use crate::error::{corrupt, Result};
use crate::oid::Oid;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
            Kind::Tag => "tag",
        }
    }

    /// Pack object type number (1..4).
    pub fn pack_type(&self) -> u8 {
        match self {
            Kind::Commit => 1,
            Kind::Tree => 2,
            Kind::Blob => 3,
            Kind::Tag => 4,
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Some(match name {
            "commit" => Kind::Commit,
            "tree" => Kind::Tree,
            "blob" => Kind::Blob,
            "tag" => Kind::Tag,
            _ => return None,
        })
    }

    pub fn from_pack_type(t: u8) -> Option<Kind> {
        Some(match t {
            1 => Kind::Commit,
            2 => Kind::Tree,
            3 => Kind::Blob,
            4 => Kind::Tag,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Object {
    pub kind: Kind,
    pub data: Vec<u8>,
}

impl Object {
    pub fn new(kind: Kind, data: Vec<u8>) -> Self {
        Self { kind, data }
    }

    pub fn oid(&self) -> Oid {
        Oid::hash_object(self.kind, &self.data)
    }
}

/// "Name <email> <unix-seconds> <+hhmm>"
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub when: i64,
    pub tz: String,
}

impl Signature {
    pub fn new(name: &str, email: &str, when: i64) -> Self {
        Self { name: name.into(), email: email.into(), when, tz: "+0000".into() }
    }

    pub fn parse(s: &str) -> Result<Signature> {
        let lt = s.find('<').ok_or_else(|| corrupt(format!("bad signature {s:?}")))?;
        let gt = s[lt..].find('>').map(|i| i + lt).ok_or_else(|| corrupt(format!("bad signature {s:?}")))?;
        let name = s[..lt].trim_end().to_string();
        let email = s[lt + 1..gt].to_string();
        let mut rest = s[gt + 1..].split_whitespace();
        let when = rest.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        let tz = rest.next().unwrap_or("+0000").to_string();
        Ok(Signature { name, email, when, tz })
    }

    pub fn render(&self) -> String {
        format!("{} <{}> {} {}", self.name, self.email, self.when, self.tz)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: Signature,
    pub committer: Signature,
    /// Headers git doesn't interpret here (e.g. `gpgsig`, jj's `change-id`), kept in order.
    pub extra: Vec<(String, String)>,
    pub message: String,
}

impl Commit {
    pub fn parse(data: &[u8]) -> Result<Commit> {
        let text = std::str::from_utf8(data).map_err(|_| corrupt("commit is not utf-8"))?;
        let (head, message) = match text.find("\n\n") {
            Some(i) => (&text[..i], text[i + 2..].to_string()),
            None => (text, String::new()),
        };
        let mut tree = None;
        let mut parents = vec![];
        let mut author = None;
        let mut committer = None;
        let mut extra: Vec<(String, String)> = vec![];
        for line in head.split('\n') {
            if let Some(cont) = line.strip_prefix(' ') {
                // Continuation of a multi-line header (gpgsig).
                if let Some(last) = extra.last_mut() {
                    last.1.push('\n');
                    last.1.push_str(cont);
                }
                continue;
            }
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "tree" => tree = Some(Oid::from_hex(v)?),
                "parent" => parents.push(Oid::from_hex(v)?),
                "author" => author = Some(Signature::parse(v)?),
                "committer" => committer = Some(Signature::parse(v)?),
                _ => extra.push((k.to_string(), v.to_string())),
            }
        }
        Ok(Commit {
            tree: tree.ok_or_else(|| corrupt("commit without tree"))?,
            parents,
            author: author.ok_or_else(|| corrupt("commit without author"))?,
            committer: committer.ok_or_else(|| corrupt("commit without committer"))?,
            extra,
            message,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut s = format!("tree {}\n", self.tree);
        for p in &self.parents {
            s.push_str(&format!("parent {p}\n"));
        }
        s.push_str(&format!("author {}\n", self.author.render()));
        s.push_str(&format!("committer {}\n", self.committer.render()));
        for (k, v) in &self.extra {
            s.push_str(k);
            s.push(' ');
            s.push_str(&v.replace('\n', "\n "));
            s.push('\n');
        }
        s.push('\n');
        s.push_str(&self.message);
        s.into_bytes()
    }

    /// Value of a trailer line like "Change-Id: abc" in the message.
    pub fn trailer(&self, key: &str) -> Option<String> {
        self.trailers(key).pop()
    }

    /// Every value of a trailer that may repeat, in order. Trailers are the lines of the
    /// message's last paragraph, as git reads them: a line like "Sealed-By: ..." in the
    /// body above it is text, not a trailer.
    pub fn trailers(&self, key: &str) -> Vec<String> {
        last_paragraph_trailers(&self.message, key)
    }

    /// The message without its first line and without its trailer paragraph: what the
    /// author wrote about why.
    pub fn body(&self) -> &str {
        let rest = self.message.split_once('\n').map(|(_, r)| r).unwrap_or("").trim();
        match rest.rsplit_once("\n\n") {
            Some((body, last)) if last.lines().all(|l| l.split_once(": ").is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '))) => body.trim(),
            _ if rest.lines().all(|l| l.split_once(": ").is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '))) => "",
            _ => rest,
        }
    }
}

/// The values of `key` among the trailer lines of a message's last paragraph.
fn last_paragraph_trailers(message: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}: ");
    let last = message.trim_end().rsplit("\n\n").next().unwrap_or("");
    last.lines().filter_map(|l| l.strip_prefix(&prefix).map(|v| v.trim().to_string())).collect()
}

/// An annotated tag: a named, dated pointer to one object, with a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub object: Oid,
    pub kind: Kind,
    /// The tag's name, like "v1.2.0" (its ref is refs/tags/<name>).
    pub name: String,
    pub tagger: Option<Signature>,
    pub message: String,
}

impl Tag {
    pub fn parse(data: &[u8]) -> Result<Tag> {
        let text = std::str::from_utf8(data).map_err(|_| corrupt("tag is not utf-8"))?;
        let (head, message) = match text.find("\n\n") {
            Some(i) => (&text[..i], text[i + 2..].to_string()),
            None => (text, String::new()),
        };
        let (mut object, mut kind, mut name, mut tagger) = (None, None, None, None);
        for line in head.split('\n') {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "object" => object = Some(Oid::from_hex(v)?),
                "type" => kind = Kind::from_name(v),
                "tag" => name = Some(v.to_string()),
                "tagger" => tagger = Some(Signature::parse(v)?),
                _ => {}
            }
        }
        Ok(Tag {
            object: object.ok_or_else(|| corrupt("tag without object"))?,
            kind: kind.ok_or_else(|| corrupt("tag without a known type"))?,
            name: name.ok_or_else(|| corrupt("tag without a name"))?,
            tagger,
            message,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut s = format!("object {}\ntype {}\ntag {}\n", self.object, self.kind.name(), self.name);
        if let Some(t) = &self.tagger {
            s.push_str(&format!("tagger {}\n", t.render()));
        }
        s.push('\n');
        s.push_str(&self.message);
        s.into_bytes()
    }

    pub fn trailer(&self, key: &str) -> Option<String> {
        self.trailers(key).pop()
    }

    /// Every value of a trailer in the message's last paragraph, in order.
    pub fn trailers(&self, key: &str) -> Vec<String> {
        last_paragraph_trailers(&self.message, key)
    }
}
