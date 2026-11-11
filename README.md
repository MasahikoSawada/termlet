# termlet

A minimal terminal emulator in about 1,200 lines of Rust.

termlet opens a window, runs your shell on a PTY, and draws it with
OpenGL. It is enough to use bash and `emacs -nw`. It isn't meant to
replace your terminal; it's small enough to read in one sitting, which
is the point.

The hard part of a terminal emulator, parsing escape sequences and
keeping the screen grid, is done by
[alacritty_terminal](https://crates.io/crates/alacritty_terminal).
termlet is the code around it: the window, the PTY, drawing, key
input, and answering the application's queries.

## What works

- Colors: 16, 256 and 24-bit
- Bold, reverse video, dim, hidden
- Wide characters (CJK) from fontconfig's fallback fonts
- Resizing; the application gets `SIGWINCH`
- Scrollback with the mouse wheel
- Emacs: replies to DA1/DA2/DSR, OSC 11 (background color) and CSI 14t,
  and modifyOtherKeys, so `C-;` and friends reach Emacs

## What doesn't

- The mouse does nothing but scroll. No selection, no copy.
- No paste from the clipboard.
- No IME, so no Japanese input.
- No configuration. The font (JetBrains Mono, 15px) and colors are
  hard-coded.
- No tabs or splits.
- Drawing is one quad per cell, every frame. There is no glyph atlas
  and no damage tracking. We kept it that way on purpose: with an
  optimized renderer, a garbled screen could be a drawing bug or a
  parsing bug, and you can't tell which.

Linux only. winit supports both Wayland and X11, but termlet has only
been tested on Wayland.

## Build

On Fedora:

```sh
sudo dnf install pkgconf-pkg-config fontconfig-devel freetype-devel
cargo build --release
./target/release/termlet
```

termlet runs `$SHELL`, or `/bin/sh` if it isn't set.

`TERMLET_SNAPSHOT=frame.ppm` writes every frame to `frame.ppm`. GNOME
doesn't let other programs take a screenshot of a Wayland window, so
this is how to get one.

## Code

| File | What it does |
|---|---|
| `app.rs` | The window, the event loop, and the glue between the rest |
| `pty.rs` | Opens the PTY and starts the shell |
| `render.rs` | Fonts and OpenGL drawing |
| `color.rs` | The palette and cell colors |
| `keys.rs` | Key presses to bytes |
| `reply.rs` | Answers to the application's queries |
| `mok.rs` | modifyOtherKeys, which alacritty_terminal doesn't do |

Line counts: 1,179 without tests (everything from `#[cfg(test)]` on),
1,004 without blank lines and comments as well.
