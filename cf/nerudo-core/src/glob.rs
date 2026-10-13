//! Tiny path glob: `*` matches within one path segment, `**` matches across segments.

pub fn matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(b'*'), _) if p.get(1) == Some(&b'*') => {
                let rest = if p.get(2) == Some(&b'/') { &p[3..] } else { &p[2..] };
                (0..=s.len()).any(|i| go(rest, &s[i..]))
            }
            (Some(b'*'), _) => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| go(&p[1..], &s[i..])),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn globs() {
        assert!(matches("ci/**", "ci/check.py"));
        assert!(matches("ci/**", "ci/a/b.py"));
        assert!(matches("*.toml", "nerudo.toml"));
        assert!(!matches("*.toml", "a/nerudo.toml"));
        assert!(matches("**/*.sql", "migrations/0001_init.sql"));
        assert!(matches("targets/*", "targets/t01"));
        assert!(!matches("targets/*", "targets/t01/src.txt"));
        assert!(matches(".github/**", ".github/workflows/ci.yml"));
    }
}
