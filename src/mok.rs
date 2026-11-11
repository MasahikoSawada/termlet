// modifyOtherKeys. Term implements 66 of Handler's 71 methods, and
// set_modify_other_keys and report_modify_other_keys aren't among them,
// so we pick the sequences out of the output before the parser sees it.
// A sequence can be split across two reads, hence the state machine.

#[derive(Clone, Copy)]
enum State {
    Ground,
    Esc,
    Csi,
}

pub struct Mok {
    mode: u8,
    state: State,
    buf: Vec<u8>,
}

impl Mok {
    pub fn new() -> Mok {
        Mok {
            mode: 0,
            state: State::Ground,
            buf: Vec::new(),
        }
    }

    // 0 = off, 1 = keys without a byte of their own (C-;), 2 = all keys.
    pub fn mode(&self) -> u8 {
        self.mode
    }

    // Returns the bytes to write back to the PTY: the answer to 'CSI ? 4 m'.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        for &b in bytes {
            match (self.state, b) {
                (_, 0x1b) => {
                    self.state = State::Esc;
                    self.buf.clear();
                }
                (State::Esc, b'[') => self.state = State::Csi,
                (State::Esc, _) => self.state = State::Ground,
                (State::Csi, 0x40..=0x7e) => {
                    if b == b'm' {
                        self.finish(&mut reply);
                    }
                    self.state = State::Ground;
                }
                (State::Csi, _) if self.buf.len() < 16 => self.buf.push(b),
                (State::Csi, _) => self.state = State::Ground,
                (State::Ground, _) => {}
            }
        }
        reply
    }

    fn finish(&mut self, reply: &mut Vec<u8>) {
        match self.buf.as_slice() {
            b">4" | b">4;0" => self.mode = 0,
            b">4;1" => self.mode = 1,
            b">4;2" => self.mode = 2,
            b"?4" => reply.extend_from_slice(format!("\x1b[>4;{}m", self.mode).as_bytes()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_changes_mode_and_replies_nothing() {
        let mut m = Mok::new();
        assert_eq!(m.scan(b"\x1b[>4;2m"), b"");
        assert_eq!(m.mode(), 2);
    }

    #[test]
    fn sequence_split_across_reads() {
        let mut m = Mok::new();
        m.scan(b"abc\x1b[>4");
        assert_eq!(m.mode(), 0);
        m.scan(b";1mdef");
        assert_eq!(m.mode(), 1);
    }

    #[test]
    fn missing_parameter_and_zero_both_reset() {
        let mut m = Mok::new();
        m.scan(b"\x1b[>4;2m\x1b[>4m");
        assert_eq!(m.mode(), 0);
        m.scan(b"\x1b[>4;2m\x1b[>4;0m");
        assert_eq!(m.mode(), 0);
    }

    #[test]
    fn query_replies_with_current_mode() {
        let mut m = Mok::new();
        m.scan(b"\x1b[>4;1m");
        assert_eq!(m.scan(b"\x1b[?4m"), b"\x1b[>4;1m");
    }

    #[test]
    fn unrelated_sequences_are_ignored() {
        let mut m = Mok::new();
        m.scan(b"\x1b[>4;2m");
        assert_eq!(
            m.scan(b"\x1b[31mred\x1b[?25h\x1b[>c\x1b]0;t\x07\x1b[4m"),
            b""
        );
        assert_eq!(m.mode(), 2);
    }

    #[test]
    fn esc_inside_a_sequence_restarts_it() {
        let mut m = Mok::new();
        m.scan(b"\x1b[>4;\x1b[>4;2m");
        assert_eq!(m.mode(), 2);
    }
}
