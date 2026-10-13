//! The server side of a push, as much of it as a proxy in front of receive-pack needs: the
//! commands a client sends, the report-status it gets back, and side-band framing for
//! `remote:` lines. The pack itself is never parsed here.

use crate::client::PushReport;
use crate::error::{Error, Result};
use crate::oid::Oid;
use crate::pktline::{pkt, pkt_str, text, Pkt, PktReader, FLUSH};

/// The largest side-band packet: 65520 bytes with its 4-byte length and 1-byte band.
const MAX_BAND_DATA: usize = 65515;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub old: Oid,
    pub new: Oid,
    pub name: String,
}

impl Command {
    pub fn is_delete(&self) -> bool {
        self.new.is_zero()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Commands {
    pub list: Vec<Command>,
    /// The capabilities the client asked for, from the first command.
    pub caps: Vec<String>,
}

impl Commands {
    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }
}

/// The command section of a receive-pack request: pkt-lines up to the first flush.
/// `shallow` lines are skipped. None: `buf` ends before the flush.
pub fn parse_commands(buf: &[u8]) -> Result<Option<Commands>> {
    let mut out = Commands::default();
    let mut pos = 0;
    loop {
        let Some(len) = pkt_len(buf, pos)? else { return Ok(None) };
        if len == 0 {
            return Ok(Some(out));
        }
        let Some(d) = buf.get(pos + 4..pos + len) else { return Ok(None) };
        pos += len;
        let (line, caps) = match d.iter().position(|&b| b == 0) {
            Some(nul) => (&d[..nul], Some(text(&d[nul + 1..]))),
            None => (d, None),
        };
        let line = text(line);
        if line.starts_with("shallow ") {
            continue;
        }
        if let Some(c) = caps {
            out.caps = c.split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect();
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Error::Protocol(format!("bad push command {line:?}")));
        };
        out.list.push(Command { old: Oid::from_hex(old)?, new: Oid::from_hex(new)?, name: name.to_string() });
    }
}

/// The length of the pkt-line at `pos` (0 for a flush), or None when `buf` ends first.
fn pkt_len(buf: &[u8], pos: usize) -> Result<Option<usize>> {
    let Some(hdr) = buf.get(pos..pos + 4) else { return Ok(None) };
    let len = std::str::from_utf8(hdr).ok().and_then(|h| usize::from_str_radix(h, 16).ok());
    match len {
        Some(n) if n == 0 || n >= 4 => Ok(Some(n)),
        _ => Err(Error::Protocol(format!("bad pkt-line length {:?}", String::from_utf8_lossy(hdr)))),
    }
}

/// The report-status in a receive-pack response, unwrapped from side-band band 1 when the
/// client asked for side-band-64k.
pub fn parse_report(resp: &[u8], sideband: bool) -> Result<PushReport> {
    let status = if sideband {
        let mut r = PktReader::new(resp);
        let mut inner = Vec::new();
        while let Some(p) = r.next_pkt()? {
            match p {
                Pkt::Data(d) => match d.first() {
                    Some(1) => inner.extend_from_slice(&d[1..]),
                    Some(3) => return Err(Error::Protocol(format!("remote error: {}", text(&d[1..])))),
                    _ => {}
                },
                Pkt::Flush => break,
                _ => {}
            }
        }
        inner
    } else {
        resp.to_vec()
    };
    let mut report = PushReport::default();
    let mut r = PktReader::new(&status);
    while let Some(p) = r.next_pkt()? {
        let Pkt::Data(d) = p else { break };
        let line = text(d);
        if let Some(rest) = line.strip_prefix("unpack ") {
            report.unpack_ok = rest == "ok";
        } else if let Some(name) = line.strip_prefix("ok ") {
            report.refs.push((name.to_string(), None));
        } else if let Some(rest) = line.strip_prefix("ng ") {
            let (name, reason) = rest.split_once(' ').unwrap_or((rest, ""));
            report.refs.push((name.to_string(), Some(reason.to_string())));
        }
    }
    Ok(report)
}

/// `data` as packets of side-band `band` (1 data, 2 progress, 3 error).
pub fn sideband(band: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(MAX_BAND_DATA) {
        let mut d = Vec::with_capacity(chunk.len() + 1);
        d.push(band);
        d.extend_from_slice(chunk);
        out.extend(pkt(&d));
    }
    out
}

/// Lines git prints as `remote: <line>`: one band-2 packet each.
pub fn remote_lines(lines: &[String]) -> Vec<u8> {
    lines.iter().flat_map(|l| sideband(2, format!("{l}\n").as_bytes())).collect()
}

/// A whole receive-pack response that refuses every ref: `unpack ok`, then `ng <ref>
/// <reason>` for each, with `lines` as `remote:` lines when the client takes side-band.
pub fn refusal(refused: &[(String, String)], lines: &[String], sideband_64k: bool) -> Vec<u8> {
    let mut status = pkt_str("unpack ok\n");
    for (name, reason) in refused {
        status.extend(pkt_str(&format!("ng {name} {reason}\n")));
    }
    status.extend_from_slice(FLUSH);
    if !sideband_64k {
        return status;
    }
    let mut out = remote_lines(lines);
    out.extend(sideband(1, &status));
    out.extend_from_slice(FLUSH);
    out
}

/// A side-band response with `extra` packets put before its final flush (or at its end,
/// with a flush, when it has none).
pub fn insert_before_flush(resp: &[u8], extra: &[u8]) -> Vec<u8> {
    // Walk the packets to find where the last whole one ends, and whether it is a flush.
    let (mut pos, mut last_flush) = (0, None);
    while let Ok(Some(len)) = pkt_len(resp, pos) {
        let end = pos + len.max(4);
        if end > resp.len() {
            break;
        }
        last_flush = (len == 0).then_some(pos);
        pos = end;
    }
    let mut out = Vec::with_capacity(resp.len() + extra.len() + 4);
    match last_flush {
        Some(f) if f + 4 == pos => {
            out.extend_from_slice(&resp[..f]);
            out.extend_from_slice(extra);
            out.extend_from_slice(&resp[f..]);
        }
        _ => {
            out.extend_from_slice(resp);
            out.extend_from_slice(extra);
            out.extend_from_slice(FLUSH);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(c: char) -> String {
        std::iter::repeat(c).take(40).collect()
    }

    #[test]
    fn commands_and_caps() {
        let mut b = pkt_str(&format!("{} {} refs/heads/feat\0report-status side-band-64k agent=git/2\n", oid('0'), oid('a')));
        b.extend(pkt_str(&format!("{} {} refs/heads/main\n", oid('b'), oid('c'))));
        b.extend_from_slice(FLUSH);
        b.extend_from_slice(b"PACK....");
        let c = parse_commands(&b).unwrap().unwrap();
        assert_eq!(c.list.len(), 2);
        assert_eq!(c.list[0].name, "refs/heads/feat");
        assert!(c.list[0].old.is_zero() && !c.list[0].is_delete());
        assert_eq!(c.list[1].name, "refs/heads/main");
        assert!(c.has("side-band-64k") && c.has("report-status") && !c.has("atomic"));
        // Cut before the flush: not complete yet.
        assert!(parse_commands(&b[..20]).unwrap().is_none());
        assert!(parse_commands(&b[..b.len() - 12]).unwrap().is_none());
        // A probe with no commands.
        assert!(parse_commands(FLUSH).unwrap().unwrap().list.is_empty());
    }

    #[test]
    fn refusal_round_trips() {
        let refused = vec![("refs/heads/main".to_string(), "main moves only through the queue".to_string())];
        let lines = vec!["push a branch".to_string()];
        let resp = refusal(&refused, &lines, true);
        let rep = parse_report(&resp, true).unwrap();
        assert!(rep.unpack_ok);
        assert_eq!(rep.refs, vec![("refs/heads/main".to_string(), Some("main moves only through the queue".to_string()))]);
        let plain = refusal(&refused, &lines, false);
        assert_eq!(parse_report(&plain, false).unwrap().refs.len(), 1);
    }

    #[test]
    fn lines_go_before_the_final_flush() {
        let mut status = pkt_str("unpack ok\n");
        status.extend(pkt_str("ok refs/heads/feat\n"));
        status.extend_from_slice(FLUSH);
        let mut resp = sideband(1, &status);
        resp.extend_from_slice(FLUSH);
        let extra = remote_lines(&["Opened pull request #1 for feat".to_string()]);
        let out = insert_before_flush(&resp, &extra);
        assert_eq!(out.len(), resp.len() + extra.len());
        assert!(out.ends_with(FLUSH));
        assert_eq!(&out[resp.len() - 4..resp.len() - 4 + extra.len()], &extra[..]);
        assert_eq!(parse_report(&out, true).unwrap().refs, vec![("refs/heads/feat".to_string(), None)]);
        // No final flush: the lines and a flush go at the end.
        let cut = &resp[..resp.len() - 4];
        let out = insert_before_flush(cut, &extra);
        assert_eq!(out.len(), cut.len() + extra.len() + 4);
        assert!(out.ends_with(FLUSH));
    }
}
