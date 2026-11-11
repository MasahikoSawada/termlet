// The GUI: one window and one Term fed from the PTY.

use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi;
use glow::HasContext;
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::Result;
use crate::pty;

// Sent by the PTY reader thread when there is output to parse.
#[derive(Debug)]
pub struct PtyEvent;

struct Gfx {
    window: Window,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    gl: glow::Context,
}

pub struct App {
    proxy: EventLoopProxy<PtyEvent>,
    gfx: Option<Gfx>,
    term: Option<Term<VoidListener>>,
    parser: ansi::Processor,
    master: Option<OwnedFd>,
    rx: Option<Receiver<Vec<u8>>>,
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

        let (cols, rows) = (80, 24);
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
            gl,
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

    fn draw(&self) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        unsafe {
            gfx.gl.clear_color(0.08, 0.09, 0.11, 1.0);
            gfx.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        if let Err(e) = gfx.surface.swap_buffers(&gfx.context) {
            eprintln!("termlet: swap_buffers: {e}");
        }
    }

    fn on_resize(&self, w: u32, h: u32) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        // A minimized window can report 0x0.
        let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        gfx.surface.resize(&gfx.context, w, h);
        unsafe { gfx.gl.viewport(0, 0, w.get() as i32, h.get() as i32) }
    }
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
            WindowEvent::Resized(size) => self.on_resize(size.width, size.height),
            WindowEvent::CloseRequested => el.exit(),
            _ => {}
        }
    }
}
