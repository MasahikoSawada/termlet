use std::num::NonZeroU32;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Debug)]
struct PtyEvent(usize);

#[derive(Default)]
struct App {
    window: Option<Window>,
    gl: Option<glow::Context>,
    surface: Option<Surface<WindowSurface>>,
    context: Option<PossiblyCurrentContext>,
}

impl ApplicationHandler<PtyEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
	let (window, cfg) = DisplayBuilder::new()
	    .with_window_attributes(Some(Window::default_attributes()))
	    .build(el, ConfigTemplateBuilder::new(), |mut c| c.next().unwrap())
	    .unwrap();
	let window = window.unwrap();

	let raw = window.window_handle().unwrap().as_raw();
	let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw));
	let not_current = unsafe {
	    cfg.display().create_context(&cfg, &ctx_attrs)
	}.unwrap();

	let attrs = window.build_surface_attributes(Default::default()).unwrap();
	let surface = unsafe {
	    cfg.display().create_window_surface(&cfg, &attrs)
	}.unwrap();

	let context = not_current.make_current(&surface).unwrap();
	let gl = unsafe {
	    glow::Context::from_loader_function(|s| {
		let cs = std::ffi::CString::new(s).unwrap();
		cfg.display().get_proc_address(&cs)
	    })
	};

	window.request_redraw();
	self.window = Some(window);
	self.gl = Some(gl);
	self.surface = Some(surface);
	self.context = Some(context);
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _ev: PtyEvent) {}

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
	use glow::HasContext;

	let (Some(gl), Some(surface), Some(context)) = (
	    self.gl.as_ref(),
	    self.surface.as_ref(),
	    self.context.as_ref(),
	) else {
	    return;
	};

	match ev {
	    WindowEvent::RedrawRequested => {
		unsafe {
		    gl.clear_color(0.08, 0.09, 0.11, 1.0);
		    gl.clear(glow::COLOR_BUFFER_BIT);
		}
		surface.swap_buffers(context).unwrap();
	    }
	    WindowEvent::Resized(size) => {
		if let (Some(w), Some(h)) =
		    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
		{
		    surface.resize(context, w, h);
		    unsafe {
			gl.viewport(0, 0, w.get() as i32, h.get() as i32)
		    }
		}
	    }
	    WindowEvent::CloseRequested => el.exit(),
	    _ => {}
	}
    }
}

fn main() {
    let el = EventLoop::<PtyEvent>::with_user_event().build().unwrap();
    el.run_app(&mut App::default()).unwrap();
}
