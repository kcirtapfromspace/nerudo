use crate::error::{corrupt, Result};
use crate::oid::Oid;
use std::cmp::Ordering;

pub const MODE_TREE: u32 = 0o040000;
pub const MODE_BLOB: u32 = 0o100644;
pub const MODE_EXEC: u32 = 0o100755;
pub const MODE_LINK: u32 = 0o120000;
pub const MODE_GITLINK: u32 = 0o160000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: u32,
    pub name: String,
    pub oid: Oid,
}

impl TreeEntry {
    pub fn is_tree(&self) -> bool {
        self.mode == MODE_TREE
    }
}

pub fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut out = vec![];
    let mut i = 0;
    while i < data.len() {
        let sp = data[i..].iter().position(|&b| b == b' ').ok_or_else(|| corrupt("tree entry without mode"))? + i;
        let mode = std::str::from_utf8(&data[i..sp])
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or_else(|| corrupt("bad tree mode"))?;
        let nul = data[sp..].iter().position(|&b| b == 0).ok_or_else(|| corrupt("tree entry without name"))? + sp;
        let name = String::from_utf8(data[sp + 1..nul].to_vec()).map_err(|_| corrupt("non-utf8 path"))?;
        let end = nul + 21;
        if end > data.len() {
            return Err(corrupt("truncated tree entry"));
        }
        out.push(TreeEntry { mode, name, oid: Oid::from_bytes(&data[nul + 1..end])? });
        i = end;
    }
    Ok(out)
}

/// Git's tree order: compare names as bytes, with a tree's name treated as "name/".
fn git_order(a: &TreeEntry, b: &TreeEntry) -> Ordering {
    let ka = a.name.as_bytes().iter().copied().chain(a.is_tree().then_some(b'/'));
    let kb = b.name.as_bytes().iter().copied().chain(b.is_tree().then_some(b'/'));
    ka.cmp(kb)
}

pub fn serialize_tree(entries: &mut [TreeEntry]) -> Vec<u8> {
    entries.sort_by(git_order);
    let mut out = vec![];
    for e in entries.iter() {
        out.extend_from_slice(format!("{:o} {}\0", e.mode, e.name).as_bytes());
        out.extend_from_slice(&e.oid.0);
    }
    out
}
