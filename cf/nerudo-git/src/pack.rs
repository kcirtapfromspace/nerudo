//! Packfiles: parse (with OFS_DELTA / REF_DELTA resolution) and write (whole objects only).

use crate::error::{corrupt, Result};
use crate::object::{Kind, Object};
use crate::oid::Oid;
use crate::store::Store;
use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;
use sha1::{Digest, Sha1};
use std::collections::HashMap;

const OFS_DELTA: u8 = 6;
const REF_DELTA: u8 = 7;

/// The largest object a pack may declare. An entry's size comes from the pack before its
/// bytes are seen, and the buffer for it is made first, so a size that no object has must
/// be refused before anything is allocated: the parser runs in a Durable Object with 128 MB.
pub const MAX_OBJECT_BYTES: usize = 32 << 20;
/// The most a pack may inflate to in all: the entries' declared sizes, summed as they are
/// read. A pack that inflates past this would not fit beside its own bytes in the object.
pub const MAX_PACK_INFLATED_BYTES: usize = 96 << 20;
/// The most entries a pack may declare. The count is in the header; the parser trusts it only
/// up to here, and allocates for at most a few thousand entries up front.
pub const MAX_PACK_OBJECTS: usize = 1 << 22;
/// Passes over the entries that resolve deltas: each pass resolves every delta whose base
/// is resolved, so a chain of depth d takes d passes. git writes chains of depth 50 by
/// default; a pack that needs more than this many passes is refused.
const MAX_DELTA_PASSES: usize = 512;

/// A varint of the pack header or of a delta header: 7 bits a byte, at most 10 bytes (a
/// 64-bit size), so no shift overflows.
const MAX_VARINT_BYTES: usize = 10;

fn too_big(what: &str, size: usize) -> crate::error::Error {
    corrupt(format!("{what} of {size} bytes is over the limit of {MAX_OBJECT_BYTES}"))
}

enum Raw {
    Whole(Kind, Vec<u8>),
    Ofs(usize, Vec<u8>),
    Ref(Oid, Vec<u8>),
}

/// Inflate one zlib stream whose uncompressed size is known; returns (data, bytes consumed).
fn inflate_known(input: &[u8], size: usize) -> Result<(Vec<u8>, usize)> {
    if size > MAX_OBJECT_BYTES {
        return Err(too_big("a pack entry", size));
    }
    let mut out = vec![0u8; size];
    let mut st = DecompressorOxide::new();
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, consumed, written) = decompress(&mut st, input, &mut out, 0, flags);
    match status {
        TINFLStatus::Done if written == size => Ok((out, consumed)),
        s => Err(corrupt(format!("inflate failed: {s:?} (wrote {written} of {size})"))),
    }
}

/// Parse a packfile. Objects whose REF_DELTA base isn't in the pack are resolved
/// against `external` (thin packs); everything resolved is returned whole.
pub fn parse_pack(data: &[u8], external: Option<&Store>) -> Result<Vec<Object>> {
    if data.len() < 32 || &data[..4] != b"PACK" {
        return Err(corrupt("not a packfile"));
    }
    let version = u32::from_be_bytes(data[4..8].try_into().unwrap());
    if version != 2 && version != 3 {
        return Err(corrupt(format!("pack version {version}")));
    }
    let count = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
    if count > MAX_PACK_OBJECTS {
        return Err(corrupt(format!("pack declares {count} objects, over the limit of {MAX_PACK_OBJECTS}")));
    }
    let body_end = data.len() - 20;
    let mut h = Sha1::new();
    h.update(&data[..body_end]);
    let sum: [u8; 20] = h.finalize().into();
    if sum != data[body_end..] {
        return Err(corrupt("pack checksum mismatch"));
    }

    // Every byte read below is checked against the body: the pack came from a remote, and a
    // panic here would take the whole sequencer down with it.
    let body = &data[..body_end];
    let byte = |pos: usize| body.get(pos).copied().ok_or_else(|| corrupt("truncated pack entry"));
    let mut pos = 12;
    let mut raws: Vec<(usize, Raw)> = Vec::with_capacity(count.min(1 << 12));
    let mut inflated = 0usize;
    for _ in 0..count {
        let start = pos;
        let mut b = byte(pos)?;
        pos += 1;
        let typ = (b >> 4) & 7;
        let mut size = (b & 0x0f) as usize;
        let mut shift = 4;
        let mut bytes = 1;
        while b & 0x80 != 0 {
            b = byte(pos)?;
            pos += 1;
            bytes += 1;
            if bytes > MAX_VARINT_BYTES {
                return Err(corrupt("pack entry size varint too long"));
            }
            size |= ((b & 0x7f) as usize) << shift;
            shift += 7;
        }
        if size > MAX_OBJECT_BYTES {
            return Err(too_big("a pack entry", size));
        }
        inflated += size;
        if inflated > MAX_PACK_INFLATED_BYTES {
            return Err(corrupt(format!("the pack inflates to more than {MAX_PACK_INFLATED_BYTES} bytes")));
        }
        let raw = match typ {
            OFS_DELTA => {
                let mut b = byte(pos)?;
                pos += 1;
                let mut off = (b & 0x7f) as usize;
                let mut bytes = 1;
                while b & 0x80 != 0 {
                    b = byte(pos)?;
                    pos += 1;
                    bytes += 1;
                    if bytes > MAX_VARINT_BYTES {
                        return Err(corrupt("ofs-delta offset varint too long"));
                    }
                    off = ((off + 1) << 7) | (b & 0x7f) as usize;
                }
                let base = start.checked_sub(off).ok_or_else(|| corrupt("ofs-delta before pack start"))?;
                let (d, used) = inflate_known(body.get(pos..).ok_or_else(|| corrupt("truncated pack entry"))?, size)?;
                pos += used;
                Raw::Ofs(base, d)
            }
            REF_DELTA => {
                let base = Oid::from_bytes(body.get(pos..pos + 20).ok_or_else(|| corrupt("truncated ref-delta base"))?)?;
                pos += 20;
                let (d, used) = inflate_known(body.get(pos..).ok_or_else(|| corrupt("truncated pack entry"))?, size)?;
                pos += used;
                Raw::Ref(base, d)
            }
            t => {
                let kind = Kind::from_pack_type(t).ok_or_else(|| corrupt(format!("pack object type {t}")))?;
                let (d, used) = inflate_known(body.get(pos..).ok_or_else(|| corrupt("truncated pack entry"))?, size)?;
                pos += used;
                Raw::Whole(kind, d)
            }
        };
        raws.push((start, raw));
    }

    // Resolve deltas. Bases can themselves be deltas, so resolve on demand with memoization.
    // Whole objects move out of `raws` and bases are read in place: a big pack is in memory
    // once inflated, not twice.
    let by_offset: HashMap<usize, usize> = raws.iter().enumerate().map(|(i, (o, _))| (*o, i)).collect();
    let mut resolved: Vec<Option<Object>> = (0..raws.len()).map(|_| None).collect();
    let mut by_oid: HashMap<Oid, usize> = HashMap::new();
    for (i, (_, r)) in raws.iter_mut().enumerate() {
        if let Raw::Whole(k, d) = r {
            let o = Object::new(*k, std::mem::take(d));
            by_oid.insert(o.oid(), i);
            resolved[i] = Some(o);
        }
    }
    let mut progress = true;
    let mut passes = 0;
    while progress {
        progress = false;
        passes += 1;
        if passes > MAX_DELTA_PASSES {
            return Err(corrupt(format!("delta chain deeper than {MAX_DELTA_PASSES}")));
        }
        for i in 0..raws.len() {
            if resolved[i].is_some() {
                continue;
            }
            let (base, delta): (Option<&Object>, &Vec<u8>) = match &raws[i].1 {
                Raw::Ofs(off, d) => (by_offset.get(off).and_then(|&j| resolved[j].as_ref()), d),
                Raw::Ref(oid, d) => (by_oid.get(oid).and_then(|&j| resolved[j].as_ref()).or_else(|| external.and_then(|s| s.get(oid).ok())), d),
                Raw::Whole(..) => unreachable!(),
            };
            if let Some(base) = base {
                let o = Object::new(base.kind, apply_delta(&base.data, delta)?);
                by_oid.insert(o.oid(), i);
                resolved[i] = Some(o);
                raws[i].1 = Raw::Whole(Kind::Blob, Vec::new()); // frees the delta
                progress = true;
            }
        }
    }
    resolved
        .into_iter()
        .enumerate()
        .map(|(i, o)| o.ok_or_else(|| corrupt(format!("unresolvable delta at entry {i}"))))
        .collect()
}

fn read_varint(d: &[u8], pos: &mut usize) -> Result<usize> {
    let mut n = 0usize;
    let mut shift = 0;
    for _ in 0..MAX_VARINT_BYTES {
        let b = *d.get(*pos).ok_or_else(|| corrupt("truncated delta header"))?;
        *pos += 1;
        n |= ((b & 0x7f) as usize) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(n);
        }
    }
    Err(corrupt("delta header varint too long"))
}

pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0;
    let base_size = read_varint(delta, &mut pos)?;
    let result_size = read_varint(delta, &mut pos)?;
    if base_size != base.len() {
        return Err(corrupt("delta base size mismatch"));
    }
    if result_size > MAX_OBJECT_BYTES {
        return Err(too_big("a delta result", result_size));
    }
    let byte = |pos: usize| delta.get(pos).copied().ok_or_else(|| corrupt("truncated delta"));
    let mut out = Vec::with_capacity(result_size);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut off = 0usize;
            let mut len = 0usize;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    off |= (byte(pos)? as usize) << (8 * i);
                    pos += 1;
                }
            }
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    len |= (byte(pos)? as usize) << (8 * i);
                    pos += 1;
                }
            }
            if len == 0 {
                len = 0x10000;
            }
            let src = off.checked_add(len).and_then(|end| base.get(off..end)).ok_or_else(|| corrupt("delta copy out of range"))?;
            if out.len() + src.len() > result_size {
                return Err(corrupt("delta result longer than declared"));
            }
            out.extend_from_slice(src);
        } else if op != 0 {
            let n = op as usize;
            let src = delta.get(pos..pos + n).ok_or_else(|| corrupt("delta insert out of range"))?;
            if out.len() + n > result_size {
                return Err(corrupt("delta result longer than declared"));
            }
            out.extend_from_slice(src);
            pos += n;
        } else {
            return Err(corrupt("delta opcode 0"));
        }
    }
    if out.len() != result_size {
        return Err(corrupt("delta result size mismatch"));
    }
    Ok(out)
}

/// Write a version-2 pack of whole objects (no deltas).
pub fn write_pack(objects: &[&Object]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for o in objects {
        let mut size = o.data.len();
        let mut b = (o.kind.pack_type() << 4) | (size & 0x0f) as u8;
        size >>= 4;
        while size > 0 {
            out.push(b | 0x80);
            b = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(b);
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&o.data, 6));
    }
    let mut h = Sha1::new();
    h.update(&out);
    let sum: [u8; 20] = h.finalize().into();
    out.extend_from_slice(&sum);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack with one declared entry made of `entry`, with a right checksum.
    fn pack(count: u32, entry: &[u8]) -> Vec<u8> {
        let mut out = b"PACK".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(entry);
        // The header check wants at least 32 bytes; pad the body with what parse never reads.
        let mut h = Sha1::new();
        h.update(&out);
        let sum: [u8; 20] = h.finalize().into();
        out.extend_from_slice(&sum);
        out
    }

    fn err(data: &[u8]) -> String {
        match parse_pack(data, None) {
            Ok(_) => panic!("parsed"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn a_truncated_entry_is_an_error_not_a_panic() {
        // A ref-delta header with no base id and no data (this one panicked in the slice).
        assert!(err(&pack(1, &[0x70])).contains("truncated"));
        // An ofs-delta with no offset byte and no data.
        assert!(err(&pack(1, &[0x60])).contains("truncated"));
        // A whole blob whose zlib stream is missing.
        assert!(err(&pack(1, &[0x35])).contains("inflate failed"));
        // More entries declared than there are.
        assert!(err(&pack(3, &[0x30, 0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01])).contains("truncated"));
        // A size varint that never ends, and one that declares more than any object has.
        assert!(err(&pack(1, &[0xbf; 12])).contains("varint too long"));
        let mut huge = vec![0xbf, 0xff, 0xff, 0xff, 0xff];
        huge.push(0x0f);
        assert!(err(&pack(1, &huge)).contains("over the limit"));
        // A count no pack has: refused before anything is made for it.
        assert!(err(&pack(u32::MAX, &[0x30])).contains("over the limit"));
    }

    #[test]
    fn a_delta_that_lies_about_its_size_is_refused_before_it_grows() {
        // result_size as a 10-byte varint far over the limit.
        let mut d = vec![0x00];
        d.extend_from_slice(&[0xff; 9]);
        d.push(0x7f);
        assert!(apply_delta(b"", &d).unwrap_err().to_string().contains("over the limit"));
        // A copy whose offset and length overflow when added.
        let d = [0x00, 0x02, 0x9f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert!(apply_delta(b"", &d).unwrap_err().to_string().contains("out of range"));
        // A copy op cut in the middle of its arguments.
        assert!(apply_delta(b"ab", &[0x02, 0x02, 0x91]).unwrap_err().to_string().contains("truncated"));
        // An insert that would pass the declared result size.
        assert!(apply_delta(b"", &[0x00, 0x01, 0x02, b'a', b'b']).unwrap_err().to_string().contains("longer than declared"));
        // A good delta still works.
        assert_eq!(apply_delta(b"hello", &[0x05, 0x07, 0x90, 0x05, 0x02, b'!', b'?']).unwrap(), b"hello!?");
    }

    #[test]
    fn whole_objects_round_trip() {
        let o = Object::new(Kind::Blob, b"some text\n".to_vec());
        let p = write_pack(&[&o]);
        let back = parse_pack(&p, None).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].oid(), o.oid());
    }
}
