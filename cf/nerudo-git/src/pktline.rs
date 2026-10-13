//! pkt-line framing used by the smart protocol.

use crate::error::{Error, Result};

pub const FLUSH: &[u8] = b"0000";
pub const DELIM: &[u8] = b"0001";

pub fn pkt(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data);
    v
}

pub fn pkt_str(s: &str) -> Vec<u8> {
    pkt(s.as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pkt<'a> {
    Data(&'a [u8]),
    Flush,
    Delim,
    ResponseEnd,
}

pub struct PktReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PktReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn next_pkt(&mut self) -> Result<Option<Pkt<'a>>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let hdr = self.buf.get(self.pos..self.pos + 4).ok_or_else(|| Error::Protocol("truncated pkt-line".into()))?;
        let len = usize::from_str_radix(std::str::from_utf8(hdr).unwrap_or("x"), 16)
            .map_err(|_| Error::Protocol(format!("bad pkt-line length {:?}", String::from_utf8_lossy(hdr))))?;
        self.pos += 4;
        Ok(Some(match len {
            0 => Pkt::Flush,
            1 => Pkt::Delim,
            2 => Pkt::ResponseEnd,
            n if n < 4 => return Err(Error::Protocol(format!("pkt-line length {n}"))),
            n => {
                let d = self.buf.get(self.pos..self.pos + n - 4).ok_or_else(|| Error::Protocol("truncated pkt-line body".into()))?;
                self.pos += n - 4;
                Pkt::Data(d)
            }
        }))
    }
}

/// Text of a data pkt without its trailing newline.
pub fn text(d: &[u8]) -> String {
    let s = String::from_utf8_lossy(d);
    s.strip_suffix('\n').unwrap_or(&s).to_string()
}
