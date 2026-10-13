use crate::error::{corrupt, Result};
use crate::object::Kind;
use sha1::{Digest, Sha1};
use std::fmt;

/// A SHA-1 object id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Oid(pub [u8; 20]);

impl Oid {
    pub const ZERO: Oid = Oid([0; 20]);

    pub fn from_hex(s: &str) -> Result<Oid> {
        let s = s.trim();
        if s.len() != 40 {
            return Err(corrupt(format!("oid must be 40 hex chars, got {s:?}")));
        }
        let mut out = [0u8; 20];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(chunk).map_err(|_| corrupt("non-utf8 oid"))?;
            out[i] = u8::from_str_radix(hex, 16).map_err(|_| corrupt(format!("bad hex in oid {s:?}")))?;
        }
        Ok(Oid(out))
    }

    pub fn from_bytes(b: &[u8]) -> Result<Oid> {
        let arr: [u8; 20] = b.try_into().map_err(|_| corrupt("oid must be 20 bytes"))?;
        Ok(Oid(arr))
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(40);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 20]
    }

    /// The id git assigns to an object: SHA-1 over "<kind> <len>\0<data>".
    pub fn hash_object(kind: Kind, data: &[u8]) -> Oid {
        let mut h = Sha1::new();
        h.update(format!("{} {}\0", kind.name(), data.len()).as_bytes());
        h.update(data);
        Oid(h.finalize().into())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", &self.to_hex()[..12])
    }
}

impl serde::Serialize for Oid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Oid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Oid, D::Error> {
        let s = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(d)?;
        Oid::from_hex(&s).map_err(serde::de::Error::custom)
    }
}
