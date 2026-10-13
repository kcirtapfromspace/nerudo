//! People, teams and agents talking about changes: comments with @mentions, review
//! requests, emoji reactions, and each participant's notifications.
//!
//! People are the approvers in nerudo.toml on trunk, and teams are their roles, so the
//! people and teams of a trunk change only through a sealed governance change. Agents are
//! the names of the forks this trunk made. A conversation belongs to a change id, so it
//! goes with the change from the queue to its landing.
//!
//! Storage (Durable Object key-value):
//!   cm:{change}:{ms}:{rand}   a comment
//!   rr:{change}:{reviewer}    a review request (reviewer: a name, or @team)
//!   nt:{who}:{ms}:{rand}      a notification for a person or an agent
//!   rx:{target}:{emoji}:{who} a reaction (target: c.{change} or m.{comment id})

use super::*;
use serde_json::{json, Value};

/// The longest comment, in characters.
pub(super) const MAX_BODY: usize = 4000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Comment {
    pub id: String,
    pub change_id: String,
    pub author: String,
    pub agent: bool,
    pub body: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ReviewRequest {
    pub change_id: String,
    /// A person's or agent's name, or @team.
    pub reviewer: String,
    pub by: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Note {
    /// mention | team_mention | review_request
    pub kind: String,
    pub change_id: String,
    pub from: String,
    #[serde(default)]
    pub team: Option<String>,
    pub text: String,
    pub at_ms: i64,
}

pub(super) enum Who {
    Person(String),
    Team(String),
    Agent(String),
}

/// Who can be mentioned on this trunk.
pub(super) struct Directory {
    /// Person -> roles.
    pub people: BTreeMap<String, Vec<String>>,
    pub agents: BTreeSet<String>,
}

impl Directory {
    pub fn team(&self, role: &str) -> Vec<String> {
        self.people.iter().filter(|(_, roles)| roles.iter().any(|r| r == role)).map(|(n, _)| n.clone()).collect()
    }

    pub fn teams(&self) -> BTreeSet<String> {
        self.people.values().flatten().cloned().collect()
    }

    /// A name after @: a person first, then a team, then an agent.
    pub fn resolve(&self, name: &str) -> Option<Who> {
        let name = name.trim_start_matches('@');
        if self.people.contains_key(name) {
            Some(Who::Person(name.into()))
        } else if self.teams().contains(name) {
            Some(Who::Team(name.into()))
        } else if self.agents.contains(name) {
            Some(Who::Agent(name.into()))
        } else {
            None
        }
    }
}

/// The names after @ in `text`, in order, each once.
pub(super) fn mentioned(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let starts = chars[i] == '@' && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'));
        if starts {
            let name: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_alphanumeric() || "._-".contains(**c)).collect();
            let name = name.trim_end_matches(['.', '-']).to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name.clone());
            }
            i += 1 + name.len();
        } else {
            i += 1;
        }
    }
    out
}

/// A comment as stored: no control characters except new lines and tabs, trimmed, and at
/// most MAX_BODY characters.
pub(super) fn clean(body: &str) -> String {
    let s: String = body.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
    s.replace("\r\n", "\n").trim().chars().take(MAX_BODY).collect()
}

fn stamp() -> (i64, String) {
    let ms = now() as i64;
    let rand = (js_sys::Math::random() * 65536.0) as u32;
    (ms, format!("{ms:013}{rand:04x}"))
}

impl Trunk {
    pub(super) async fn directory(&self) -> Result<Directory> {
        let rules = self.rules().await?;
        let people = rules.approvers.iter().map(|a| (a.name.clone(), a.roles.clone())).collect();
        let agents = self.list_prefix("fa:").await?.into_iter().map(|(k, _)| k["fa:".len()..].to_string()).collect();
        Ok(Directory { people, agents })
    }

    /// Former names of people, old → new, for display only (app.rs `renamed`). Records and
    /// signed trailers keep the name they were written with. A name that is a person of the
    /// repo now is always that person.
    pub(super) async fn aliases(&self, dir: &Directory) -> Result<BTreeMap<String, String>> {
        let mut a: BTreeMap<String, String> = self.get("aliases").await?.unwrap_or_default();
        a.retain(|old, new| !dir.people.contains_key(old) && old != new);
        Ok(a)
    }

    /// A rules change that gives an approver's key a new name makes the old name an alias of
    /// the new one. Called before the new rules are stored.
    pub(super) async fn note_renames(&self, old: &Rules, new: &Rules) -> Result<()> {
        let mut a: BTreeMap<String, String> = self.get("aliases").await?.unwrap_or_default();
        let before = a.clone();
        for n in new.approvers.iter().filter(|n| !n.key.is_empty()) {
            for o in old.approvers.iter().filter(|o| o.key == n.key && o.name != n.name) {
                if !new.approvers.iter().any(|x| x.name == o.name) {
                    a.insert(o.name.clone(), n.name.clone());
                }
            }
        }
        // An older name of a renamed person follows the person.
        let hop = a.clone();
        for to in a.values_mut() {
            if let Some(next) = hop.get(to.as_str()) {
                *to = next.clone();
            }
        }
        a.retain(|old, new| old != new);
        if a != before {
            self.put("aliases", &a).await?;
        }
        Ok(())
    }

    /// Admin: `{from, to}` shows the former name `from` as `to`; an empty `to` removes it.
    /// For renames from before aliases, and for a person who left the repo and is a user by
    /// another name.
    pub(super) async fn set_alias(&self, v: serde_json::Value) -> Result<Response> {
        let (from, to) = (v["from"].as_str().unwrap_or("").trim(), v["to"].as_str().unwrap_or("").trim());
        if from.is_empty() || from.contains(' ') || to.contains(' ') || from == to {
            return bad(400, "send {from, to}: two different names");
        }
        let mut a: BTreeMap<String, String> = self.get("aliases").await?.unwrap_or_default();
        if to.is_empty() {
            a.remove(from);
        } else {
            a.insert(from.into(), to.into());
        }
        self.put("aliases", &a).await?;
        Response::from_json(&json!({ "aliases": a }))
    }

    /// A conversation belongs to a change id or to an task id.
    async fn change_exists(&self, cid: &str) -> Result<bool> {
        Ok(self.get::<u32>(&format!("latest:{cid}")).await?.is_some() || self.task(cid).await?.is_some())
    }

    async fn notify_who(&self, who: &str, note: &Note) -> Result<()> {
        let (_, id) = stamp();
        self.put(&format!("nt:{who}:{id}"), note).await
    }

    /// Store a comment and notify everyone it mentions.
    pub(super) async fn add_comment(&self, cid: &str, author: &str, agent: bool, body: &str) -> Result<std::result::Result<Comment, String>> {
        let body = clean(body);
        if body.is_empty() {
            return Ok(Err("the comment is empty".into()));
        }
        if !self.change_exists(cid).await? {
            return Ok(Err(format!("unknown change {cid}")));
        }
        let (at_ms, id) = stamp();
        let c = Comment { id: id.clone(), change_id: cid.into(), author: author.into(), agent, body: body.clone(), at_ms };
        self.put(&format!("cm:{cid}:{id}"), &c).await?;
        let line: String = body.lines().next().unwrap_or("").chars().take(120).collect();
        self.event("comment", format!("{author} on {cid}: {line}"), Some(cid), None, Some(author)).await?;
        let dir = self.directory().await?;
        let snippet: String = body.chars().take(200).collect();
        for name in mentioned(&body) {
            match dir.resolve(&name) {
                Some(Who::Person(p)) | Some(Who::Agent(p)) if p != author => {
                    let n = Note { kind: "mention".into(), change_id: cid.into(), from: author.into(), team: None, text: snippet.clone(), at_ms };
                    self.notify_who(&p, &n).await?;
                }
                Some(Who::Team(t)) => {
                    for m in dir.team(&t).into_iter().filter(|m| m != author) {
                        let n = Note { kind: "team_mention".into(), change_id: cid.into(), from: author.into(), team: Some(t.clone()), text: snippet.clone(), at_ms };
                        self.notify_who(&m, &n).await?;
                    }
                }
                _ => {}
            }
        }
        Ok(Ok(c))
    }

    /// Ask a person, a team (`@team`) or an agent to review a change, and notify them.
    pub(super) async fn add_review_request(&self, cid: &str, reviewer: &str, by: &str) -> Result<std::result::Result<String, String>> {
        if !self.change_exists(cid).await? {
            return Ok(Err(format!("unknown change {cid}")));
        }
        let dir = self.directory().await?;
        let (key, members) = match dir.resolve(reviewer) {
            Some(Who::Person(p)) => (p.clone(), vec![p]),
            Some(Who::Agent(a)) => (a.clone(), vec![a]),
            Some(Who::Team(t)) => (format!("@{t}"), dir.team(&t)),
            None => return Ok(Err(format!("nobody is called {reviewer} on this trunk"))),
        };
        let at_ms = now() as i64;
        let rr = ReviewRequest { change_id: cid.into(), reviewer: key.clone(), by: by.into(), at_ms };
        self.put(&format!("rr:{cid}:{key}"), &rr).await?;
        self.event("review", format!("{by} asked {key} to review {cid}"), Some(cid), None, Some(by)).await?;
        let team = key.strip_prefix('@').map(str::to_string);
        for m in members.into_iter().filter(|m| m != by) {
            let n = Note { kind: "review_request".into(), change_id: cid.into(), from: by.into(), team: team.clone(), text: String::new(), at_ms };
            self.notify_who(&m, &n).await?;
        }
        Ok(Ok(key))
    }

    /// A change parked for a seal asks the teams that its quorums need for a review.
    pub(super) async fn auto_review(&self, rec: &ChangeRec) -> Result<()> {
        let mut asked = BTreeSet::new();
        for t in rec.reason.as_ref().and_then(|r| r["quorum"].as_array()).into_iter().flatten() {
            let roles: Vec<String> = t["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            let targets: Vec<String> = if roles.is_empty() {
                self.directory().await?.people.keys().cloned().collect()
            } else {
                roles.iter().map(|r| format!("@{r}")).collect()
            };
            for who in targets {
                if asked.insert(who.clone()) {
                    let _ = self.add_review_request(&rec.req.change_id, &who, "nerudo").await?;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn comments(&self, cid: &str) -> Result<Vec<Comment>> {
        Ok(self.list_prefix(&format!("cm:{cid}:")).await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect())
    }

    pub(super) async fn reviews(&self, cid: &str) -> Result<Vec<ReviewRequest>> {
        Ok(self.list_prefix(&format!("rr:{cid}:")).await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect())
    }

    /// Every review request on the trunk.
    pub(super) async fn all_reviews(&self) -> Result<Vec<ReviewRequest>> {
        Ok(self.list_prefix("rr:").await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect())
    }

    /// Notifications for `who`, newest first.
    pub(super) async fn notes(&self, who: &str, limit: usize) -> Result<Vec<Note>> {
        let mut v: Vec<Note> = self.list_prefix(&format!("nt:{who}:")).await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
        v.reverse();
        v.truncate(limit);
        Ok(v)
    }

    /// Reactions on a target: emoji -> who, in the order they reacted.
    pub(super) async fn reactions(&self, target: &str) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, _) in self.list_prefix(&format!("rx:{target}:")).await? {
            let rest = &k[format!("rx:{target}:").len()..];
            if let Some((emoji, who)) = rest.rsplit_once(':') {
                out.entry(emoji.to_string()).or_default().push(who.to_string());
            }
        }
        Ok(out)
    }

    /// The target of a reaction on `cid`: its own thread (`c.{cid}`), or one of its comments
    /// (`m.{id}`), which must be there. A comment id is what `stamp` makes, so no other text
    /// becomes part of a storage key.
    pub(super) async fn reaction_target(&self, cid: &str, comment: Option<&str>) -> Result<std::result::Result<String, String>> {
        let Some(id) = comment.filter(|c| !c.is_empty()) else {
            return Ok(Ok(format!("c.{cid}")));
        };
        let shaped = id.len() == 17 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !shaped || self.get::<Comment>(&format!("cm:{cid}:{id}")).await?.is_none() {
            return Ok(Err(format!("no comment {id} on {cid}")));
        }
        Ok(Ok(format!("m.{id}")))
    }

    /// Add `who`'s reaction, or take it away if it is there. Returns whether it is on now.
    pub(super) async fn toggle_reaction(&self, target: &str, emoji: &str, who: &str) -> Result<bool> {
        let key = format!("rx:{target}:{emoji}:{who}");
        if self.get::<i64>(&key).await?.is_some() {
            self.state.storage().delete(&key).await?;
            Ok(false)
        } else {
            self.put(&key, &(now() as i64)).await?;
            Ok(true)
        }
    }

    // ------------------------------------------------------------------ API for agents

    pub(super) async fn api_comment(&self, caller: &super::agents::Caller, body: Value) -> Result<Response> {
        let who = match self.acting_agent(caller, "as", body["as"].as_str().unwrap_or_default()).await? {
            Ok(w) => w,
            Err((status, e)) => return bad(status, e),
        };
        match self.add_comment(body["change_id"].as_str().unwrap_or_default(), &who, true, body["body"].as_str().unwrap_or_default()).await? {
            Ok(c) => Response::from_json(&c),
            Err(e) => bad(400, e),
        }
    }

    pub(super) async fn api_comments(&self, req: &Request) -> Result<Response> {
        let cid = req.url()?.query_pairs().find(|(k, _)| k == "change").map(|(_, v)| v.into_owned()).unwrap_or_default();
        let comments = self.comments(&cid).await?;
        let reviews = self.reviews(&cid).await?;
        let reactions = self.reactions(&format!("c.{cid}")).await?;
        Response::from_json(&json!({ "change_id": cid, "comments": comments, "review_requests": reviews, "reactions": reactions }))
    }

    pub(super) async fn api_review(&self, caller: &super::agents::Caller, body: Value) -> Result<Response> {
        let who = match self.acting_agent(caller, "as", body["as"].as_str().unwrap_or_default()).await? {
            Ok(w) => w,
            Err((status, e)) => return bad(status, e),
        };
        match self.add_review_request(body["change_id"].as_str().unwrap_or_default(), body["reviewer"].as_str().unwrap_or_default(), &who).await? {
            Ok(r) => Response::from_json(&json!({ "requested": r })),
            Err(e) => bad(400, e),
        }
    }

    pub(super) async fn api_react(&self, caller: &super::agents::Caller, body: Value) -> Result<Response> {
        let who = match self.acting_agent(caller, "as", body["as"].as_str().unwrap_or_default()).await? {
            Ok(w) => w,
            Err((status, e)) => return bad(status, e),
        };
        let cid = body["change_id"].as_str().unwrap_or_default();
        if !self.change_exists(cid).await? {
            return bad(400, format!("unknown change {cid}"));
        }
        let Some(emoji) = normalize_emoji(body["emoji"].as_str().unwrap_or_default()) else {
            return bad(400, "emoji: give one emoji, or a :shortcode:");
        };
        let target = match self.reaction_target(cid, body["comment"].as_str()).await? {
            Ok(t) => t,
            Err(e) => return bad(400, e),
        };
        let on = self.toggle_reaction(&target, &emoji, &who).await?;
        Response::from_json(&json!({ "emoji": emoji, "on": on }))
    }

    /// `GET .../notes?who=<name>`. With the agent token, only an agent's notes; the admin
    /// reads anyone's.
    pub(super) async fn api_notes(&self, caller: &super::agents::Caller, req: &Request) -> Result<Response> {
        let who = req.url()?.query_pairs().find(|(k, _)| k == "who").map(|(_, v)| v.into_owned()).unwrap_or_default();
        if !caller.admin {
            if let Err((status, e)) = self.acting_agent(caller, "who", &who).await? {
                return bad(status, e);
            }
        }
        Response::from_json(&self.notes(&who, 50).await?)
    }
}

// ---------------------------------------------------------------------- emoji

/// Shortcodes this trunk understands, as in `:tada:`. A curated set of common ones; any
/// emoji typed as itself also works.
pub(super) const SHORTCODES: &[(&str, &str)] = &[
    ("+1", "👍"), ("thumbsup", "👍"), ("-1", "👎"), ("thumbsdown", "👎"), ("ok_hand", "👌"), ("clap", "👏"),
    ("raised_hands", "🙌"), ("pray", "🙏"), ("muscle", "💪"), ("wave", "👋"), ("point_up", "☝️"), ("point_right", "👉"),
    ("v", "✌️"), ("handshake", "🤝"), ("eyes", "👀"), ("brain", "🧠"), ("smile", "😄"), ("grin", "😁"),
    ("joy", "😂"), ("rofl", "🤣"), ("slightly_smiling_face", "🙂"), ("wink", "😉"), ("blush", "😊"), ("heart_eyes", "😍"),
    ("thinking", "🤔"), ("neutral_face", "😐"), ("expressionless", "😑"), ("confused", "😕"), ("worried", "😟"),
    ("cry", "😢"), ("sob", "😭"), ("scream", "😱"), ("exploding_head", "🤯"), ("sweat_smile", "😅"), ("sunglasses", "😎"),
    ("nerd_face", "🤓"), ("upside_down_face", "🙃"), ("relieved", "😌"), ("sleeping", "😴"), ("facepalm", "🤦"),
    ("shrug", "🤷"), ("rage", "😡"), ("skull", "💀"), ("ghost", "👻"), ("robot", "🤖"), ("alien", "👽"),
    ("heart", "❤️"), ("orange_heart", "🧡"), ("yellow_heart", "💛"), ("green_heart", "💚"), ("blue_heart", "💙"),
    ("purple_heart", "💜"), ("broken_heart", "💔"), ("sparkling_heart", "💖"), ("100", "💯"), ("fire", "🔥"),
    ("sparkles", "✨"), ("star", "⭐"), ("star2", "🌟"), ("zap", "⚡"), ("boom", "💥"), ("tada", "🎉"),
    ("confetti_ball", "🎊"), ("balloon", "🎈"), ("gift", "🎁"), ("trophy", "🏆"), ("medal", "🏅"), ("crown", "👑"),
    ("rocket", "🚀"), ("airplane", "✈️"), ("ship", "🚢"), ("anchor", "⚓"), ("construction", "🚧"), ("rotating_light", "🚨"),
    ("warning", "⚠️"), ("no_entry", "⛔"), ("stop_sign", "🛑"), ("x", "❌"), ("white_check_mark", "✅"),
    ("heavy_check_mark", "✔️"), ("ballot_box_with_check", "☑️"), ("question", "❓"), ("exclamation", "❗"),
    ("bulb", "💡"), ("memo", "📝"), ("pencil2", "✏️"), ("book", "📖"), ("books", "📚"), ("bookmark", "🔖"),
    ("link", "🔗"), ("paperclip", "📎"), ("pushpin", "📌"), ("calendar", "📅"), ("chart_with_upwards_trend", "📈"),
    ("chart_with_downwards_trend", "📉"), ("bar_chart", "📊"), ("clipboard", "📋"), ("package", "📦"), ("mailbox", "📫"),
    ("email", "📧"), ("inbox_tray", "📥"), ("outbox_tray", "📤"), ("lock", "🔒"), ("unlock", "🔓"), ("key", "🔑"),
    ("shield", "🛡️"), ("hammer", "🔨"), ("wrench", "🔧"), ("hammer_and_wrench", "🛠️"), ("gear", "⚙️"), ("nut_and_bolt", "🔩"),
    ("magnet", "🧲"), ("test_tube", "🧪"), ("microscope", "🔬"), ("mag", "🔍"), ("computer", "💻"), ("keyboard", "⌨️"),
    ("floppy_disk", "💾"), ("cd", "💿"), ("satellite", "📡"), ("bell", "🔔"), ("no_bell", "🔕"), ("loudspeaker", "📢"),
    ("speech_balloon", "💬"), ("thought_balloon", "💭"), ("hourglass", "⌛"), ("stopwatch", "⏱️"), ("alarm_clock", "⏰"),
    ("recycle", "♻️"), ("wastebasket", "🗑️"), ("broom", "🧹"), ("bug", "🐛"), ("ant", "🐜"), ("bee", "🐝"),
    ("snail", "🐌"), ("turtle", "🐢"), ("rabbit", "🐇"), ("unicorn", "🦄"), ("dragon", "🐉"), ("octopus", "🐙"),
    ("whale", "🐳"), ("crab", "🦀"), ("snake", "🐍"), ("cat", "🐱"), ("dog", "🐶"), ("seedling", "🌱"),
    ("evergreen_tree", "🌲"), ("deciduous_tree", "🌳"), ("herb", "🌿"), ("fallen_leaf", "🍂"), ("sunflower", "🌻"),
    ("rainbow", "🌈"), ("sunny", "☀️"), ("cloud", "☁️"), ("snowflake", "❄️"), ("ocean", "🌊"), ("earth_americas", "🌎"),
    ("coffee", "☕"), ("tea", "🍵"), ("beer", "🍺"), ("pizza", "🍕"), ("cake", "🍰"), ("cookie", "🍪"),
    ("apple", "🍎"), ("lemon", "🍋"), ("hot_pepper", "🌶️"), ("popcorn", "🍿"), ("checkered_flag", "🏁"), ("dart", "🎯"),
    ("game_die", "🎲"), ("art", "🎨"), ("musical_note", "🎵"), ("headphones", "🎧"), ("movie_camera", "🎥"),
    ("camera", "📷"), ("tv", "📺"), ("telephone", "☎️"), ("arrow_right", "➡️"), ("arrow_left", "⬅️"), ("arrow_up", "⬆️"),
    ("arrow_down", "⬇️"), ("arrows_counterclockwise", "🔄"), ("new", "🆕"), ("free", "🆓"), ("cool", "🆒"), ("sos", "🆘"),
    ("information_source", "ℹ️"), ("red_circle", "🔴"), ("green_circle", "🟢"), ("yellow_circle", "🟡"),
    ("large_blue_circle", "🔵"), ("white_circle", "⚪"), ("black_circle", "⚫"), ("tm", "™️"), ("copyright", "©️"),
];

pub(super) fn shortcode(name: &str) -> Option<&'static str> {
    SHORTCODES.iter().find(|(n, _)| *n == name).map(|(_, e)| *e)
}

/// One reaction: a known :shortcode: (with or without the colons), or an emoji typed as
/// itself (no ASCII at all, so no markup and no separator; at most 32 bytes: enough for ZWJ
/// sequences and skin tones).
pub(super) fn normalize_emoji(input: &str) -> Option<String> {
    let s = input.trim();
    let name = s.trim_matches(':');
    if let Some(e) = shortcode(name) {
        return Some(e.to_string());
    }
    let ok = !s.is_empty() && s.len() <= 32 && s.chars().all(|c| !c.is_ascii() && !c.is_whitespace() && !c.is_control());
    ok.then(|| s.to_string())
}

/// `text` with each known :shortcode: replaced by its emoji.
pub(super) fn with_emoji(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(':') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let name_len = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-')).unwrap_or(after.len());
        if name_len > 0 && after[name_len..].starts_with(':') {
            if let Some(e) = shortcode(&after[..name_len]) {
                out.push_str(e);
                rest = &after[name_len + 1..];
                continue;
            }
        }
        out.push(':');
        rest = after;
    }
    out.push_str(rest);
    out
}
