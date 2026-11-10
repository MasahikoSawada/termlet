use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::process::Pid;
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;

pub struct Pty {
    pub master: OwnedFd,
    pub child: Child,
}

pub fn set_winsize(master: &OwnedFd, cols: u16, rows: u16) -> io::Result<()> {
    rustix::termios::tcsetwinsize(
	master,
	Winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 },
    )?;
    Ok(())
}

pub fn spawn(cols: u16, rows: u16) -> io::Result<Pty> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::pty::grantpt(&master);
    rustix::pty::unlockpt(&master);
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(&name, OFlags::RDWR, Mode::empty())?;

    // The shell reads TIOCGWINSZ once at startup, so the size has to
    // be in place before spawn.
    set_winsize(&master, cols, rows);

    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut cmd = Command::new(shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let sfd = slave.try_clone()?;
    unsafe {
	cmd.pre_exec(move || {
	    rustix::process::setsid();
	    rustix::process::ioctl_tiocsctty(&sfd)?;
	    rustix::stdio::dup2_stdin(&sfd)?;
	    rustix::stdio::dup2_stdout(&sfd)?;
	    rustix::stdio::dup2_stderr(&sfd)?;
	    Ok(())
	});
    }
    let child = cmd.spawn()?;

    // Command keeps the pre_exec closure, and with it the slave clone,
    // until it is dropped. Holding either fd here means read_master
    // never sees EIO when the shell exits.
    drop(cmd);
    drop(slave);

    // Verify the child really took the ctty. bash reacquires one on
    // its own, so this is the only palce a missing TIOCSCTTY shows up
    // with bash as the shell. pre_exec races with us, here the retry
    // with a nap.
    let want = Pid::from_raw(child.id() as i32).unwrap();
    let mut ok = false;
    for _ in 0..50 {
	if rustix::termios::tcgetpgrp(&master) == Ok(want) {
	    ok = true;
	    break;
	}
	std::thread::sleep(Duration::from_millis(10));
    }
    if !ok {
	eprintln!("termlet: child did not take the controlling terminal");
    }

    Ok(Pty { master, child })
}

// read(2) on the master returns EIO, not 0, once the last slave fd is
// closed. Map that to EOF so callers can treat it like any stream.
pub fn read_master(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
	match rustix::io::read(fd, buf) {
	    Ok(n) => return Ok(n),
	    Err(Errno::INTR) => continue,
	    Err(Errno::IO) => return Ok(0),
	    Err(e) => return Err(e.into()),
	}
    }
}
