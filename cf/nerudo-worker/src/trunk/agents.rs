//! Who an API request acts for. All agents share the agent token, so a name in a request
//! proves nothing by itself. Two things make it proof:
//!
//! - Each agent has its own key, `lsa1.<agent>.<gen>.<mac>`, an HMAC under the admin token
//!   (as the share and personal keys are). The first fork of an agent returns it. A
//!   request with the key in X-Nerudo-Agent-Key acts for that agent only: its `as`, `who`,
//!   fork and PRs must be that agent's.
//! - The agent token alone does one thing for an agent: its first fork, which returns its
//!   key. Once the agent has a fork (`fa:{agent}`), only its key, or the admin token, acts
//!   for it: in the API here, and in plain git (trunk/git.rs).
//! - The people of a trunk (the approvers in nerudo.toml) act through their own links and
//!   keys, never through the agent token: it can't take a person's name, the name of a
//!   person's fork, or a person's inbox.
//!
//! Storage: `ag:{agent}` the generation of the agent's key (none: 0). Rotating the key
//! raises it, and the older keys stop working.

use super::ui::{agent_key, same};
use super::*;
use serde_json::json;

/// The header that carries an agent's own key.
pub(super) const KEY_HEADER: &str = "X-Nerudo-Agent-Key";

/// The sender of an API request.
pub(super) struct Caller {
    /// The admin token, or the sequencer itself for a person it checked (the app, plain git).
    pub admin: bool,
    /// The agent that a valid agent key names.
    pub agent: Option<String>,
}

impl Caller {
    /// The app and plain git: they open forks and PRs for a person whose key they checked.
    pub fn trusted() -> Caller {
        Caller { admin: true, agent: None }
    }
}

/// The name of a person's fork: the person's name with anything but [A-Za-z0-9] as `_`.
pub(super) fn person_fork(person: &str) -> String {
    person.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// What to do without the key of an agent that has a fork.
pub(super) fn remedy(trunk: &str, agent: &str) -> String {
    format!("Ask the admin: nerudo agent-key --trunk {trunk} --agent {agent}")
}

impl Trunk {
    /// The sender of an API request that has the agent token or the admin token. A key that
    /// does not verify is refused (Err), never ignored: a rotated key works nowhere.
    pub(super) async fn caller(&self, req: &Request, admin: bool) -> Result<std::result::Result<Caller, String>> {
        let Some(key) = crate::request_header(&req, KEY_HEADER)?.filter(|k| !k.is_empty()) else {
            return Ok(Ok(Caller { admin, agent: None }));
        };
        Ok(self.key_agent(&key).await?.map(|agent| Caller { admin, agent: Some(agent) }))
    }

    /// The agent that `key` names, when the key is valid on this trunk now.
    pub(super) async fn key_agent(&self, key: &str) -> Result<std::result::Result<String, String>> {
        let mut parts = key.splitn(4, '.');
        let (Some("lsa1"), Some(agent), Some(gen), Some(_)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Ok(Err(format!("{KEY_HEADER} is not an agent key (lsa1.<agent>.<gen>.<mac>)")));
        };
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return Ok(Err("no such trunk".into()));
        };
        let refused = || {
            let name = if host::valid_agent(agent) { agent } else { "<agent>" };
            Err(format!(
                "this agent key is not valid on trunk {}: it is for another trunk, or the admin rotated it. {}",
                cfg.trunk,
                remedy(&cfg.trunk, name)
            ))
        };
        let Ok(gen) = gen.parse::<u64>() else { return Ok(refused()) };
        if !host::valid_agent(agent) || gen != self.key_gen(agent).await? {
            return Ok(refused());
        }
        // A person's name, or a person's fork name, is never an agent's: a rules change that
        // adds a person with an agent's name retires that agent's key.
        if self.is_person(agent).await? {
            return Ok(Err(format!("{agent} is a person on trunk {}, or the name of a person's fork; an agent key does not act for a person", cfg.trunk)));
        }
        if !same(key.as_bytes(), agent_key(&self.env, &cfg.trunk, agent, gen)?.as_bytes()) {
            return Ok(refused());
        }
        Ok(Ok(agent.to_string()))
    }

    /// The generation of the agent's key now.
    pub(super) async fn key_gen(&self, agent: &str) -> Result<u64> {
        Ok(self.get::<u64>(&format!("ag:{agent}")).await?.unwrap_or(0))
    }

    /// `GET .../agents/{agent}/key` (admin): the agent's key. `POST` (admin): a new key; the
    /// old one stops working at once.
    pub(super) async fn api_agent_key(&self, agent: &str, rotate: bool) -> Result<Response> {
        if !host::valid_agent(agent) || self.get::<Fork>(&format!("fa:{agent}")).await?.is_none() {
            return bad(404, format!("{agent:?} has no fork on this trunk. Its first POST /forks returns its key."));
        }
        let cfg = self.cfg().await?;
        let mut gen = self.key_gen(agent).await?;
        if rotate {
            gen += 1;
            self.put(&format!("ag:{agent}"), &gen).await?;
        }
        Response::from_json(&json!({ "agent": agent, "gen": gen, "agent_key": agent_key(&self.env, &cfg.trunk, agent, gen)? }))
    }

    /// `name` is a person of this trunk, or the name of a person's fork.
    pub(super) async fn is_person(&self, name: &str) -> Result<bool> {
        let Some(rules) = self.get::<Rules>("rules").await? else { return Ok(false) };
        Ok(rules.approvers.iter().any(|a| a.name == name || person_fork(&a.name) == name))
    }

    /// The agent named in `field` (`as`, `who`, `by`), when the caller may act for it: it has
    /// a fork on this trunk, and the caller has its key or the admin token. The agent token
    /// alone acts for no agent. Err: an HTTP status and why.
    pub(super) async fn acting_agent(&self, caller: &Caller, field: &str, name: &str) -> Result<std::result::Result<String, (u16, String)>> {
        if let Some(me) = caller.agent.as_deref() {
            if me != name {
                return Ok(Err((403, format!("{field}: {name:?} is not {me}. Your agent key acts only for {me}."))));
            }
        } else if !caller.admin {
            if self.is_person(name).await? {
                return Ok(Err((403, format!("{field}: {name} is a person on this trunk. The agent token acts only for agents."))));
            }
            if !name.is_empty() && self.get::<Fork>(&format!("fa:{name}")).await?.is_some() {
                return Ok(Err((403, self.needs_key(name).await?)));
            }
        }
        if name.is_empty() || self.get::<Fork>(&format!("fa:{name}")).await?.is_none() {
            return Ok(Err((400, format!("{field}: {name:?} is not an agent with a fork on this trunk; call POST /forks first"))));
        }
        Ok(Ok(name.to_string()))
    }

    /// Why the agent token alone does not act for `agent`, an agent with a fork.
    pub(super) async fn needs_key(&self, agent: &str) -> Result<String> {
        let trunk = self.cfg().await?.trunk;
        Ok(format!(
            "agent {agent} has a fork on trunk {trunk}. Only its agent key, or the admin token, acts for it: \
             send the key in {KEY_HEADER}. The agent token alone only makes the first fork of a new agent. {}",
            remedy(&trunk, agent)
        ))
    }

    /// The name of the fork of the caller's agent, when the caller has an agent key.
    pub(super) async fn caller_fork(&self, caller: &Caller) -> Result<Option<String>> {
        match caller.agent.as_deref() {
            Some(me) => Ok(self.get::<Fork>(&format!("fa:{me}")).await?.map(|f| f.name)),
            None => Ok(None),
        }
    }

    /// The agent whose fork is named `fork`, if one is.
    pub(super) async fn fork_owner(&self, fork: &str) -> Result<Option<String>> {
        for (k, v) in self.list_prefix("fa:").await? {
            if serde_json::from_str::<Fork>(&v).is_ok_and(|f| f.name == fork) {
                return Ok(Some(k["fa:".len()..].to_string()));
            }
        }
        Ok(None)
    }
}
