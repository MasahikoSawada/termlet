// The GUI: one window and one Term fed from the PTY.

use std::cell::Cell;
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, point_to_viewport};
use alacritty_terminal::vte::ansi::{self, CursorShape, NamedColor};
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Window, WindowId};

use crate::Result;
use crate::color::{Palette, resolve};
use crate::keys;
use crate::mok::Mok;
use crate::pty;
use crate::render::Renderer;
use crate::reply::ToPty;

const FONT_PX: f32 = 15.0;

// Sent by the PTY reader thread when there is output to parse.
#[derive(Debug)]
pub struct PtyEvent;

struct Gfx {
    window: Window,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    renderer: Renderer,
}

pub struct App {
    proxy: EventLoopProxy<PtyEvent>,
    gfx: Option<Gfx>,
    term: Option<Term<ToPty>>,
    parser: ansi::Processor,
    mok: Mok,
    master: Option<OwnedFd>,
    rx: Option<Receiver<Vec<u8>>>,
    // Answers to the application's queries, from ToPty.
    reply_rx: Option<Receiver<Vec<u8>>>,
    // Shared with ToPty for CSI 14t.
    win_size: Rc<Cell<WindowSize>>,
    palette: Palette,
    mods: ModifiersState,
    cols: u16,
    rows: u16,
    // Fraction of a line left over from touchpad scrolling.
    wheel_acc: f32,
}

impl App {
    pub fn new(proxy: EventLoopProxy<PtyEvent>) -> App {
        App {
            proxy,
            gfx: None,
            term: None,
            parser: ansi::Processor::new(),
            mok: Mok::new(),
            master: None,
            rx: None,
            reply_rx: None,
            win_size: Rc::new(Cell::new(WindowSize {
                num_lines: 24,
                num_cols: 80,
                cell_width: 8,
                cell_height: 16,
            })),
            palette: Palette::default(),
            mods: ModifiersState::empty(),
            cols: 80,
            rows: 24,
            wheel_acc: 0.0,
        }
    }

    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let attrs = Window::default_attributes().with_title("termlet");
        // Wayland app_id and X11 WM_CLASS. Key remappers like xremap
        // can't exclude a window without them.
        let attrs = WindowAttributesExtWayland::with_name(attrs, "termlet", "termlet");
        let attrs = WindowAttributesExtX11::with_name(attrs, "termlet", "termlet");
        let (window, cfg) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(el, ConfigTemplateBuilder::new(), |mut c| c.next().unwrap())
            // Box<dyn Error> without Send, so io::Error::other() won't take it.
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let window = window.ok_or_else(|| std::io::Error::other("no window"))?;

        let raw = window
            .window_handle()
            .map_err(std::io::Error::other)?
            .as_raw();
        let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw));
        let not_current = unsafe { cfg.display().create_context(&cfg, &ctx_attrs) }
            .map_err(std::io::Error::other)?;
        let sattrs = window
            .build_surface_attributes(Default::default())
            .map_err(std::io::Error::other)?;
        let surface = unsafe { cfg.display().create_window_surface(&cfg, &sattrs) }
            .map_err(std::io::Error::other)?;
        let context = not_current
            .make_current(&surface)
            .map_err(std::io::Error::other)?;
        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let cs = std::ffi::CString::new(s).unwrap();
                cfg.display().get_proc_address(&cs)
            })
        };

        let renderer = Renderer::new(gl, FONT_PX * window.scale_factor() as f32)?;
        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let (cols, rows) = grid_size(&renderer, size.width, size.height);
        self.cols = cols;
        self.rows = rows;
        self.win_size.set(window_size(&renderer, cols, rows));

        let pty = pty::spawn(cols, rows)?;
        let size = TermSize::new(cols as usize, rows as usize);
        let (reply_tx, reply_rx) = channel();
        let listener = ToPty::new(reply_tx, self.palette.clone(), self.win_size.clone());
        self.term = Some(Term::new(Config::default(), &size, listener));
        self.reply_rx = Some(reply_rx);

        // winit can't wait on an fd, so a thread reads the PTY and wakes
        // the event loop.
        let (tx, rx) = channel();
        let m = pty.master.try_clone()?;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 8192];
            loop {
                let n = match pty::read_master(&m, &mut b) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("termlet: pty read: {e}");
                        0
                    }
                };
                if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                    break;
                }
                let _ = proxy.send_event(PtyEvent);
            }
            // Dropping 'tx' is how pump() learns that the shell is gone.
            drop(tx);
            let _ = proxy.send_event(PtyEvent);
        });
        self.rx = Some(rx);
        self.master = Some(pty.master);

        window.request_redraw();
        self.gfx = Some(Gfx {
            window,
            surface,
            context,
            renderer,
        });
        Ok(())
    }

    fn pump(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let (Some(rx), Some(term)) = (self.rx.as_ref(), self.term.as_mut()) else {
            return Ok(());
        };
        let mut got = false;
        loop {
            match rx.try_recv() {
                Ok(chunk) => {
                    let r = self.mok.scan(&chunk);
                    if !r.is_empty()
                        && let Some(m) = self.master.as_ref()
                    {
                        write_all(m, &r)?;
                    }
                    self.parser.advance(term, &chunk);
                    got = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    el.exit();
                    return Ok(());
                }
            }
        }
        // New output brings the view back to the bottom.
        if got && term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
        if let (Some(rx), Some(m)) = (self.reply_rx.as_ref(), self.master.as_ref()) {
            for b in rx.try_iter() {
                write_all(m, &b)?;
            }
        }
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
        Ok(())
    }

    fn draw(&mut self) {
        let (Some(gfx), Some(term)) = (self.gfx.as_mut(), self.term.as_ref()) else {
            return;
        };
        let r = &mut gfx.renderer;
        let (cw, ch) = (r.cell_w as i32, r.cell_h as i32);
        let content = term.renderable_content();
        // Colors the application changed with OSC 4/10/11.
        let colors = content.colors;
        let named = |n: NamedColor| colors[n].unwrap_or(self.palette.get(n));
        let default_bg = named(NamedColor::Background);
        r.clear(default_bg);

        let offset = content.display_offset;
        let cursor = (content.cursor.shape != CursorShape::Hidden)
            .then(|| point_to_viewport(offset, content.cursor.point))
            .flatten();

        // Backgrounds first, then all glyphs blended on top.
        let mut glyphs = Vec::new();
        for cell in content.display_iter {
            let line = cell.point.line.0 + offset as i32;
            let col = cell.point.column.0;
            let (x, y) = (col as i32 * cw, line * ch);
            let c = cell.cell;
            let (mut fg, mut bg) = resolve(c.fg, c.bg, c.flags, &self.palette, colors);
            if cursor.is_some_and(|p| p.line == line as usize && p.column.0 == col) {
                fg = bg;
                bg = named(NamedColor::Cursor);
            }
            if bg != default_bg {
                let w = if c.flags.contains(Flags::WIDE_CHAR) {
                    2 * cw
                } else {
                    cw
                };
                r.fill(x, y, w, ch, bg);
            }
            if !c.flags.contains(Flags::WIDE_CHAR_SPACER) {
                glyphs.push((c.c, c.flags.contains(Flags::BOLD), x, y, fg));
            }
        }
        r.begin_glyphs();
        for (c, bold, x, y, fg) in glyphs {
            r.glyph(c, bold, x, y, fg);
        }
        if let Err(e) = gfx.surface.swap_buffers(&gfx.context) {
            eprintln!("termlet: swap_buffers: {e}");
        }
    }

    fn on_key(&self, ev: &KeyEvent) -> Result<()> {
        let (Some(term), Some(m)) = (self.term.as_ref(), self.master.as_ref()) else {
            return Ok(());
        };
        match keys::encode(&ev.logical_key, self.mods, *term.mode(), self.mok.mode()) {
            Some(bytes) => write_all(m, &bytes),
            None => Ok(()),
        }
    }

    // The alternate screen has no scrollback, so under vim or less the
    // wheel does nothing.
    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let (Some(gfx), Some(term)) = (self.gfx.as_ref(), self.term.as_mut()) else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
            MouseScrollDelta::PixelDelta(p) => {
                self.wheel_acc += p.y as f32 / gfx.renderer.cell_h as f32;
                let l = self.wheel_acc.trunc();
                self.wheel_acc -= l;
                l as i32
            }
        };
        if lines != 0 {
            term.scroll_display(Scroll::Delta(lines));
            gfx.window.request_redraw();
        }
    }

    fn on_resize(&mut self, w: u32, h: u32) -> Result<()> {
        let Some(gfx) = self.gfx.as_ref() else {
            return Ok(());
        };
        // A minimized window can report 0x0.
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return Ok(());
        };
        gfx.surface.resize(&gfx.context, nw, nh);
        gfx.renderer.resize(w, h);
        let (cols, rows) = grid_size(&gfx.renderer, w, h);
        if (cols, rows) == (self.cols, self.rows) {
            return Ok(());
        }
        self.cols = cols;
        self.rows = rows;
        self.win_size.set(window_size(&gfx.renderer, cols, rows));
        // Both are needed: the grid for us, and TIOCSWINSZ so the
        // application gets SIGWINCH.
        if let Some(term) = self.term.as_mut() {
            term.resize(TermSize::new(cols as usize, rows as usize));
        }
        if let Some(m) = self.master.as_ref() {
            pty::set_winsize(m, cols, rows)?;
        }
        Ok(())
    }
}

fn write_all(fd: &OwnedFd, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let n = rustix::io::write(fd, bytes)?;
        bytes = &bytes[n..];
    }
    Ok(())
}

fn window_size(r: &Renderer, cols: u16, rows: u16) -> WindowSize {
    WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: r.cell_w as u16,
        cell_height: r.cell_h as u16,
    }
}

fn grid_size(r: &Renderer, w: u32, h: u32) -> (u16, u16) {
    let cols = (w / r.cell_w).clamp(2, u16::MAX as u32) as u16;
    let rows = (h / r.cell_h).clamp(1, u16::MAX as u32) as u16;
    (cols, rows)
}

impl ApplicationHandler<PtyEvent> for App {
    // Wayland can't create a window before this.
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        if let Err(e) = self.init(el) {
            eprintln!("termlet: {e}");
            el.exit();
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _ev: PtyEvent) {
        if let Err(e) = self.pump(el) {
            eprintln!("termlet: {e}");
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(size) => {
                if let Err(e) = self.on_resize(size.width, size.height) {
                    eprintln!("termlet: {e}");
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Err(e) = self.on_key(&event) {
                    eprintln!("termlet: {e}");
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::CloseRequested => el.exit(),
            _ => {}
        }
    }
}
