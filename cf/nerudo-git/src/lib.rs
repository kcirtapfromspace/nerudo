//! A minimal git engine for the nerudo sequencer.
//!
//! Pure Rust and wasm-compatible: it never touches a filesystem or a socket itself.
//! Network I/O goes through the [`Transport`] trait, so the same code runs natively
//! (tests, CLI) and inside a Cloudflare Durable Object (over `fetch`).
//!
//! Scope is deliberately small: objects, trees, commits, packfiles (read with delta
//! resolution, write without deltas), pkt-lines, and the smart HTTP operations the
//! sequencer needs: protocol v2 `ls-refs` and `fetch`, and v1 `receive-pack` for push; and, for
//! the sequencer's own git endpoint, the commands and report of a push it passes on.

mod client;
mod error;
mod object;
mod oid;
mod pack;
mod pktline;
mod receive;
mod store;
mod tree;

pub use client::{fetch, fetch_body, fetch_depth, fetch_pack, fetch_with, ingest, ls_refs, push, FetchOpts, Remote, RefUpdate, PushReport, Transport};
pub use error::{Error, Result};
pub use object::{Commit, Kind, Object, Signature, Tag};
pub use oid::Oid;
pub use pack::{parse_pack, write_pack};
pub use receive::{insert_before_flush, parse_commands, parse_report, refusal, remote_lines, Command, Commands};
pub use pack::{MAX_OBJECT_BYTES, MAX_PACK_OBJECTS};
pub use store::{Diff, FileEntry, Store, MAX_TREE_DEPTH};
pub use tree::{serialize_tree, TreeEntry, MODE_BLOB, MODE_EXEC, MODE_GITLINK, MODE_LINK, MODE_TREE};
