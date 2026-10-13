//! Read aliases for the forge's own former public names. Stored trunk IDs stay opaque.

pub(crate) fn owner_alias(owner: &str) -> Option<&'static str> {
    (owner == "landseq").then_some("nerudo")
}

pub(crate) fn repo_alias<'a>(owner: &str, repo: &'a str) -> Option<(&'static str, &'a str)> {
    owner_alias(owner).map(|owner| (owner, if repo == "landseq" { "nerudo" } else { repo }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_former_brand_owner_is_aliased() {
        assert_eq!(repo_alias("landseq", "landseq"), Some(("nerudo", "nerudo")));
        assert_eq!(repo_alias("landseq", "demo"), Some(("nerudo", "demo")));
        assert_eq!(repo_alias("alice", "landseq"), None);
        assert_eq!(repo_alias("nerudo", "nerudo"), None);
    }
}
