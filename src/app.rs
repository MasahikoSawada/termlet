// The GUI: one window and one Term fed from the PTY.

use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use alacritty_terminal::event::VoidListener;
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
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::Result;
use crate::color::{Palette, resolve};
use crate::keys;
use crate::pty;
use crate::render::Renderer;

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
    term: Option<Term<VoidListener>>,
    parser: ansi::Processor,
    master: Option<OwnedFd>,
    rx: Option<Receiver<Vec<u8>>>,
    palette: Palette,
    mods: ModifiersState,
    cols: u16,
    rows: u16,
}

impl App {
    pub fn new(proxy: EventLoopProxy<PtyEvent>) -> App {
        App {
            proxy,
            gfx: None,
            term: None,
            parser: ansi::Processor::new(),
            master: None,
            rx: None,
            palette: Palette::default(),
            mods: ModifiersState::empty(),
            cols: 80,
            rows: 24,
        }
    }

    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let attrs = Window::default_attributes().with_title("termlet");
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

        let pty = pty::spawn(cols, rows)?;
        let size = TermSize::new(cols as usize, rows as usize);
        self.term = Some(Term::new(Config::default(), &size, VoidListener));

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

    fn pump(&mut self, el: &ActiveEventLoop) {
        let (Some(rx), Some(term)) = (self.rx.as_ref(), self.term.as_mut()) else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(chunk) => self.parser.advance(term, &chunk),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    el.exit();
                    return;
                }
            }
        }
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
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
        match keys::encode(&ev.logical_key, self.mods, *term.mode()) {
            Some(bytes) => write_all(m, &bytes),
            None => Ok(()),
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
        self.pump(el);
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
            WindowEvent::CloseRequested => el.exit(),
            _ => {}
        }
    }
}
