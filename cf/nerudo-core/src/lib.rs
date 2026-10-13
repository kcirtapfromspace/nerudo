//! The sequencer's logic, independent of where it runs.
//!
//! Everything here is pure: given objects in a [`nerudo_git::Store`], the trunk's
//! [`Rules`], and a change, it decides what lands and produces the new commit. The
//! Durable Object only adds I/O (fetch, push) and durable state around it.

pub mod auth;
pub mod decide;
pub mod gates;
pub mod glob;
pub mod log;
pub mod merge;
pub mod quorum;
pub mod release;
pub mod rules;

pub use auth::{Allowance, AllowanceSpec, Seal, SealSpec};
pub use gates::{check_files, seal_boundaries, trunk_checks};
pub use log::LogEntry;
pub use merge::{apply_change, Applied, ChangeInput, Ejection};
pub use quorum::{verify_landing, Tally};
pub use rules::{Approver, Rules, RULES_PATH};
