//! Thin wrapper over the git CLI. The MVP shells out rather than linking a git library.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone)]
pub struct Git {
    pub dir: PathBuf,
    pub envs: Vec<(String, String)>,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        // The parent's GIT_CONFIG_* entries stay (the bench sets credential.helper= there, so
        // a token in a mirror URL is never saved by a credential helper); ours goes after them.
        let n: usize = std::env::var("GIT_CONFIG_COUNT").ok().and_then(|c| c.parse().ok()).unwrap_or(0);
        Self {
            dir: dir.into(),
            envs: vec![
                ("GIT_COMMITTER_NAME".into(), "nerudo".into()),
                ("GIT_COMMITTER_EMAIL".into(), "nerudo@bench.invalid".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                // Never sign landed commits with whatever key the host user configured.
                ("GIT_CONFIG_COUNT".into(), (n + 1).to_string()),
                (format!("GIT_CONFIG_KEY_{n}"), "commit.gpgsign".into()),
                (format!("GIT_CONFIG_VALUE_{n}"), "false".into()),
            ],
        }
    }

    pub fn with_env(&self, k: &str, v: &str) -> Self {
        let mut g = self.clone();
        g.envs.push((k.into(), v.into()));
        g
    }

    pub fn output(&self, args: &[&str]) -> Result<Output> {
        Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .envs(self.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .with_context(|| format!("spawn git {args:?}"))
    }

    /// Run and return stdout; error on non-zero exit.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.output(args)?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn rev(&self, rev: &str) -> Result<String> {
        Ok(self.run(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?.trim().to_string())
    }
}

/// Run an arbitrary command in `dir`, returning (success, combined output).
pub fn command(dir: &Path, argv: &[String], envs: &[(String, String)]) -> Result<(bool, String)> {
    let (prog, rest) = argv.split_first().context("empty command")?;
    let out = Command::new(prog)
        .args(rest)
        .current_dir(dir)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .with_context(|| format!("spawn {argv:?}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}
