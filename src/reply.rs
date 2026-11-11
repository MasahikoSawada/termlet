// Term answers DA1/DA2/DSR itself and hands the bytes to its
// EventListener as PtyWrite. OSC color queries and CSI 14t come as
// closures that need a value from us. All of them go to a channel that
// the owner drains into the PTY.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use alacritty_terminal::event::{Event, EventListener, WindowSize};

use crate::color::Palette;

pub struct ToPty {
    tx: Sender<Vec<u8>>,
    palette: Palette,
    size: Rc<Cell<WindowSize>>,
}

impl ToPty {
    pub fn new(tx: Sender<Vec<u8>>, palette: Palette, size: Rc<Cell<WindowSize>>) -> ToPty {
        ToPty { tx, palette, size }
    }
}

impl EventListener for ToPty {
    fn send_event(&self, ev: Event) {
        let bytes = match ev {
            Event::PtyWrite(s) => s.into_bytes(),
            // Dropping these leaves the application waiting for a timeout.
            Event::ColorRequest(i, f) => f(self.palette.index(i)).into_bytes(),
            Event::TextAreaSizeRequest(f) => f(self.size.get()).into_bytes(),
            _ => return,
        };
        let _ = self.tx.send(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi;
    use std::sync::mpsc::{Receiver, channel};

    fn size(cols: u16, lines: u16, cw: u16, ch: u16) -> WindowSize {
        WindowSize {
            num_lines: lines,
            num_cols: cols,
            cell_width: cw,
            cell_height: ch,
        }
    }

    fn term() -> (Term<ToPty>, Receiver<Vec<u8>>, Rc<Cell<WindowSize>>) {
        let (tx, rx) = channel();
        let ws = Rc::new(Cell::new(size(80, 24, 8, 16)));
        let listener = ToPty::new(tx, Palette::default(), ws.clone());
        let t = Term::new(Config::default(), &TermSize::new(80, 24), listener);
        (t, rx, ws)
    }

    fn feed(t: &mut Term<ToPty>, bytes: &[u8]) {
        let mut p: ansi::Processor = ansi::Processor::new();
        p.advance(t, bytes);
    }

    fn drain(rx: &Receiver<Vec<u8>>) -> Vec<u8> {
        rx.try_iter().flatten().collect()
    }

    #[test]
    fn da1_da2_and_dsr_are_forwarded() {
        let (mut t, rx, _) = term();
        feed(&mut t, b"\x1b[c\x1b[>c\x1b[5n\x1b[6n");
        assert_eq!(drain(&rx), b"\x1b[?6c\x1b[>0;2600;1c\x1b[0n\x1b[1;1R");
    }

    #[test]
    fn osc_11_answers_with_palette_background() {
        let (mut t, rx, _) = term();
        feed(&mut t, b"\x1b]11;?\x07");
        assert_eq!(drain(&rx), b"\x1b]11;rgb:1414/1717/1c1c\x07");
    }

    #[test]
    fn csi_14t_answers_with_text_area_pixels() {
        let (mut t, rx, ws) = term();
        ws.set(size(20, 10, 7, 15));
        feed(&mut t, b"\x1b[14t");
        assert_eq!(drain(&rx), b"\x1b[4;150;140t");
    }

    #[test]
    fn grid_is_still_driven() {
        let (mut t, _, _) = term();
        feed(&mut t, b"HELLO\x1b[>c");
        let row: String = (0..5).map(|c| t.grid()[Line(0)][Column(c)].c).collect();
        assert_eq!(row, "HELLO");
    }
}
