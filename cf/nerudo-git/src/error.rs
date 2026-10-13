use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Malformed object, pack, or protocol data.
    Corrupt(String),
    /// An object the operation needs is not in the store.
    Missing(String),
    /// The remote answered, but not with what the protocol requires.
    Protocol(String),
    /// Transport-level failure: no answer, or a lost connection.
    Transport(String),
    /// The remote answered with an HTTP status other than 200.
    Status(u16, String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Corrupt(m) => write!(f, "corrupt: {m}"),
            Error::Missing(m) => write!(f, "missing object: {m}"),
            Error::Protocol(m) => write!(f, "protocol: {m}"),
            Error::Transport(m) | Error::Status(_, m) => write!(f, "transport: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A fetch whose answer had no packfile section: the answer was cut or empty. A remote that
/// lacks a wanted object says so with ERR instead. Artifacts gave this under load in the
/// 10,000-trunk test (cf/scale), and the same fetch then worked.
pub(crate) const NO_PACKFILE: &str = "fetch returned no packfile";

impl Error {
    /// The same request can succeed later: the connection failed, the remote answered 408,
    /// 429 or 5xx, or a fetch came back with no packfile. Any other answer (a 4xx, an ERR line,
    /// bad data) gives the same result again.
    pub fn transient(&self) -> bool {
        match self {
            Error::Transport(_) => true,
            Error::Status(s, _) => *s >= 500 || *s == 408 || *s == 429,
            Error::Protocol(m) => m == NO_PACKFILE,
            _ => false,
        }
    }
}

pub(crate) fn corrupt(m: impl Into<String>) -> Error {
    Error::Corrupt(m.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fetch_with_no_packfile_is_transient() {
        assert!(Error::Protocol(NO_PACKFILE.into()).transient());
        assert_eq!(Error::Protocol(NO_PACKFILE.into()).to_string(), "protocol: fetch returned no packfile");
    }

    #[test]
    fn transient() {
        assert!(Error::Transport("Network connection lost.".into()).transient());
        for s in [408, 429, 500, 502, 503] {
            assert!(Error::Status(s, format!("HTTP {s}")).transient(), "{s}");
        }
        for s in [401, 403, 404] {
            assert!(!Error::Status(s, format!("HTTP {s}")).transient(), "{s}");
        }
        assert!(!Error::Protocol("ERR not our ref".into()).transient());
        assert!(!Error::Protocol("remote error: upload-pack: not our ref".into()).transient());
        assert!(!Error::Corrupt("bad pack header".into()).transient());
        assert_eq!(Error::Status(503, "POST git-upload-pack: HTTP 503: down".into()).to_string(), "transport: POST git-upload-pack: HTTP 503: down");
    }
}
