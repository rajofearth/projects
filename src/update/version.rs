//! Release versions: `MAJOR.MINOR.PATCH`, with an optional leading `v` as tags carry it.
//! Pre-release and build suffixes don't parse, so a `v0.3.0-rc.1` tag is never offered.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            // No signs, no leading zeros beyond "0", digits only.
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                return None;
            }
            p.parse().ok()
        };
        let v = Version { major: next()?, minor: next()?, patch: next()? };
        parts.next().is_none().then_some(v)
    }

    /// This build's version, from `Cargo.toml`.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version is MAJOR.MINOR.PATCH")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_and_orders_numerically() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(v("v1.2.3"), Version { major: 1, minor: 2, patch: 3 });
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("0.2.0").to_string(), "0.2.0");
    }

    #[test]
    fn refuses_prereleases_and_junk() {
        for s in ["0.3.0-rc.1", "0.3", "0.3.0.1", "", "v", "01.0.0", "+1.0.0", "1.0.0+build", "a.b.c"] {
            assert_eq!(Version::parse(s), None, "{s}");
        }
    }

    #[test]
    fn the_current_build_parses() {
        let _ = Version::current();
    }
}
