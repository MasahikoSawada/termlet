use std::io::{Write, stdin};
use std::path::PathBuf;

use rustix::termios::{OptionalActions, Termios};

use crate::Result;
use crate::pty;
use crate::spool::{Rec, Spool};

pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("termlet")
}

pub fn spool_path(id: u64) -> PathBuf {
    runtime_dir().join(format!("{id}.spool"))
}

// Puts the outer terminal back on drop, so a panic in the worker
// doesn't leave the shell in raw mode.
struct Restore(Termios);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(stdin(), OptionalActions::Now, &self.0);
    }
}

pub fn run() -> Result<()> {
    // The worker's own session, separate from the shell's setsid
    // in pre_exec. Without it the worker still belongs to the GUI's
    // session and dies with it on SIGHUP.
    let _ = rustix::process::setsid();

    let (cols, rows) = (80u16, 24u16); // default window size
    let pty = pty::spawn(cols, rows)?;

    let saved = rustix::termios::tcgetattr(stdin())?;
    let mut raw = saved.clone();
    raw.make_raw();
    rustix::termios::tcsetattr(stdin(), OptionalActions::Now, &raw)?;
    let _restore = Restore(saved);

    {
        let m = pty.master.try_clone()?;
        std::thread::spawn(move || {
            let mut b = [0u8; 1024];
            while let Ok(n) = rustix::io::read(stdin(), &mut b) {
                if n == 0 || rustix::io::write(&m, &b[..n]).is_err() {
                    break;
                }
            }
        });
    }

    let mut spool = Spool::create(&spool_path(0))?;
    spool.append(Rec::Resize { cols, rows });
    let mut buf = [0u8; 8192];

    // main loop
    loop {
        let n = pty::read_master(&pty.master, &mut buf)?;
        if n == 0 {
            break;
        }

        spool.append(Rec::Out(&buf[..n]))?;
        std::io::stdout().write_all(&buf[..n])?;
        std::io::stdout().flush()?;
    }

    Ok(())
}
