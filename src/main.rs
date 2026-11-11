use winit::event_loop::EventLoop;

mod app;
mod color;
mod keys;
mod mok;
mod pty;
mod render;
mod reply;

use app::{App, PtyEvent};

pub type Result<T> = std::io::Result<T>;

fn main() {
    if let Err(e) = gui() {
        eprintln!("termlet: {e}");
        std::process::exit(1);
    }
}

fn gui() -> Result<()> {
    let el = EventLoop::<PtyEvent>::with_user_event()
        .build()
        .map_err(std::io::Error::other)?;
    let proxy = el.create_proxy();
    el.run_app(&mut App::new(proxy))
        .map_err(std::io::Error::other)?;
    Ok(())
}
