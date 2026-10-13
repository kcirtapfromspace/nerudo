//! Landing rules, read from `nerudo.toml` on trunk.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub footprint: Footprint,
    #[serde(default)]
    pub commutative: Vec<Commutative>,
    #[serde(default)]
    pub derived: Vec<Derived>,
    #[serde(default)]
    pub allocate: Vec<Allocate>,
    pub ci: Ci,
}

#[derive(Debug, Default, Deserialize)]
pub struct Footprint {
    /// Glob-ish patterns like "targets/*": each child of the prefix is one unit.
    #[serde(default)]
    pub units: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Commutative {
    pub path: String,
    /// Only "append" is implemented.
    pub rule: String,
}

#[derive(Debug, Deserialize)]
pub struct Derived {
    pub path: String,
    pub command: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Allocate {
    pub dir: String,
    pub pending_prefix: String,
    pub width: usize,
}

#[derive(Debug, Deserialize)]
pub struct Ci {
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Config {
    /// Paths a change never writes directly: merged by rule or regenerated.
    pub fn is_special(&self, path: &str) -> bool {
        self.commutative.iter().any(|c| c.path == path)
            || self.derived.iter().any(|d| d.path == path)
            || self.allocate.iter().any(|a| {
                path.strip_prefix(&a.dir)
                    .and_then(|r| r.strip_prefix('/'))
                    .is_some_and(|name| name.starts_with(&a.pending_prefix))
            })
    }

    /// Map a changed path to its footprint unit.
    pub fn unit_of(&self, path: &str) -> String {
        for pat in &self.footprint.units {
            if let Some(prefix) = pat.strip_suffix("/*") {
                if let Some(rest) = path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
                    let child = rest.split('/').next().unwrap_or(rest);
                    return format!("{prefix}/{child}");
                }
            }
        }
        path.to_string()
    }
}
