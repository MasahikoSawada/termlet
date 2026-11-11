use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

pub enum Rec<'a> {
    Out(&'a [u8]),
    Resize { cols: u16, rows: u16 },
}

const KIND_OUT: u8 = 0;
const KIND_RESIZE: u8 = 1;

pub fn encode(rec: &Rec, out: &mut Vec<u8>) {
    match *rec {
        Rec::Out(b) => {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.push(KIND_OUT);
            out.extend_from_slice(b);
        }
        Rec::Resize { cols, rows } => {
            out.extend_from_slice(&4u32.to_le_bytes());
            out.push(KIND_RESIZE);
            out.extend_from_slice(&cols.to_le_bytes());
            out.extend_from_slice(&rows.to_le_bytes());
        }
    }
}

fn bad(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

// Decodes every record in 'buf, borrowing payloads from it. A record
// cut off at the end of 'buf' is an error, not a silent stop: the
// caller passed a length it believed to be a record boundary.
pub fn decode_all(buf: &[u8], mut f: impl FnMut(Rec<'_>)) -> io::Result<()> {
    let mut pos = 0;
    while pos < buf.len() {
        let hdr = buf
            .get(pos..pos + 5)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let len = u32::from_le_bytes(hdr[..4].try_into().unwrap()) as usize;
        let kind = hdr[4];
        let payload = buf
            .get(pos + 5..pos + 5 + len)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        match kind {
            KIND_OUT => f(Rec::Out(payload)),
            KIND_RESIZE => {
                if len != 4 {
                    return Err(bad(format!("resize record of length {len}")));
                }
                f(Rec::Resize {
                    cols: u16::from_le_bytes([payload[0], payload[1]]),
                    rows: u16::from_le_bytes([payload[2], payload[3]]),
                });
            }
            k => return Err(bad(format!("unknown record kind {k}"))),
        }

        pos += 5 + len;
    }

    Ok(())
}

pub struct Spool {
    file: File,
    len: u64,
    buf: Vec<u8>,
}

impl Spool {
    // Creates or truncates the spool at 'path'.
    pub fn create(path: &Path) -> io::Result<Spool> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Spool {
            file: File::create(path)?,
            len: 0,
            buf: Vec::new(),
        })
    }

    // Opens an existing spool for reading.
    pub fn open(path: &Path) -> io::Result<Spool> {
        let file = OpenOptions::new().read(true).open(path)?;
        let len = file.metadata()?.len();
        Ok(Spool {
            file,
            len,
            buf: Vec::new(),
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    // Write the given 'rec' to our 'buf'.
    //
    // One write_all per record, so a reader never sees a header
    // without its payload unless the writer died mid-write.
    pub fn append(&mut self, rec: Rec) -> io::Result<()> {
        self.buf.clear();
        encode(&rec, &mut self.buf);
        self.file.write_all(&self.buf)?;
        self.len += self.buf.len() as u64;
        Ok(())
    }

    // Replays records in the first 'upto' bytes. Reading a fixed prefix
    // lets an attaching GUI replay exactly what the worker's hello
    // promised while the worker keeps appending.
    pub fn for_each_to(&mut self, upto: u64, f: impl FnMut(Rec<'_>)) -> io::Result<()> {
        let mut buf = vec![0u8; upto as usize];
        self.file.read_exact_at(&mut buf, 0)?;
        decode_all(&buf, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Owned {
        Out(Vec<u8>),
        Resize(u16, u16),
    }

    fn collect(buf: &[u8]) -> io::Result<Vec<Owned>> {
        let mut v = Vec::new();
        decode_all(buf, |r| {
            v.push(match r {
                Rec::Out(b) => Owned::Out(b.to_vec()),
                Rec::Resize { cols, rows } => Owned::Resize(cols, rows),
            })
        })?;
        Ok(v)
    }

    #[test]
    fn out_record_encodes_as_len_kind_payload() {
        let mut buf = Vec::new();
        encode(&Rec::Out(b"ab"), &mut buf);
        assert_eq!(buf, [2, 0, 0, 0, 0, b'a', b'b']);
    }

    #[test]
    fn resize_record_encodes_cols_rows_le() {
        let mut buf = Vec::new();
        encode(&Rec::Resize { cols: 80, rows: 24 }, &mut buf);
        assert_eq!(buf, [4, 0, 0, 0, 1, 80, 0, 24, 0]);
    }

    #[test]
    fn decode_round_trips_mixed_records() {
        let mut buf = Vec::new();
        encode(&Rec::Resize { cols: 80, rows: 24 }, &mut buf);
        encode(&Rec::Out(b"hello"), &mut buf);
        encode(&Rec::Out(b""), &mut buf);
        assert_eq!(
            collect(&buf).unwrap(),
            [
                Owned::Resize(80, 24),
                Owned::Out(b"hello".to_vec()),
                Owned::Out(Vec::new())
            ]
        );
    }

    #[test]
    fn decode_of_empty_input_yields_nothing() {
        assert_eq!(collect(&[]).unwrap(), []);
    }

    #[test]
    fn decode_reports_truncated_record() {
        let mut buf = Vec::new();
        encode(&Rec::Out(b"hello"), &mut buf);
        buf.truncate(buf.len() - 2);
        assert_eq!(
            collect(&buf).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn decode_rejects_unknown_kind() {
        assert_eq!(
            collect(&[0, 0, 0, 0, 7]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn spool_file_round_trip_up_to_len() {
        let path = std::env::temp_dir().join(format!("termlet-spool-test-{}", std::process::id()));
        let mut w = Spool::create(&path).unwrap();
        w.append(Rec::Resize { cols: 80, rows: 24 }).unwrap();
        w.append(Rec::Out(b"abc")).unwrap();
        let upto = w.len();
        w.append(Rec::Out(b"later")).unwrap();

        let mut r = Spool::open(&path).unwrap();
        assert_eq!(r.len(), upto + 5 + 5);
        let mut got = Vec::new();
        r.for_each_to(upto, |rec| {
            got.push(match rec {
                Rec::Out(b) => Owned::Out(b.to_vec()),
                Rec::Resize { cols, rows } => Owned::Resize(cols, rows),
            })
        })
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(got, [Owned::Resize(80, 24), Owned::Out(b"abc".to_vec())]);
    }
}
