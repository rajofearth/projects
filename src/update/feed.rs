//! The newest release, from GitHub's `releases/latest` endpoint. That endpoint already skips
//! drafts and pre-releases; the checks here repeat that, and refuse a release whose assets don't
//! sit on this repository's download path or claim an implausible size.

use serde::Deserialize;

use super::verify::{ASSET_EXE, ASSET_SIG};
use super::version::Version;

pub const REPO: &str = "rajofearth/projects";

pub fn latest_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// No release of this app comes near this; a bigger asset is refused before downloading.
pub const MAX_EXE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// One line per change, from the release notes' bullet list.
    pub notes: Vec<String>,
    pub exe_url: String,
    pub exe_size: u64,
    pub sig_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// The release the JSON describes, or why it can't be offered.
pub fn parse(json: &str) -> Result<Release, String> {
    let r: GhRelease = serde_json::from_str(json).map_err(|e| format!("GitHub sent an unexpected reply ({e})"))?;
    if r.draft || r.prerelease {
        return Err("the latest release is a draft or pre-release".into());
    }
    let version = Version::parse(&r.tag_name).ok_or_else(|| format!("the latest tag, {}, isn't a version", r.tag_name))?;
    let prefix = format!("https://github.com/{REPO}/releases/download/");
    let asset = |name: &str| -> Result<&GhAsset, String> {
        let a = r.assets.iter().find(|a| a.name == name).ok_or_else(|| format!("release {version} has no {name}"))?;
        if !a.browser_download_url.starts_with(&prefix) {
            return Err(format!("{name} isn't on this repository's download path"));
        }
        Ok(a)
    };
    let exe = asset(ASSET_EXE)?;
    let sig = asset(ASSET_SIG)?;
    if exe.size == 0 || exe.size > MAX_EXE_BYTES {
        return Err(format!("{ASSET_EXE} claims {} bytes", exe.size));
    }
    Ok(Release {
        version,
        notes: notes(r.body.as_deref().unwrap_or("")),
        exe_url: exe.browser_download_url.clone(),
        exe_size: exe.size,
        sig_url: sig.browser_download_url.clone(),
    })
}

/// The bullet lines of GitHub's generated notes, without the author and PR link:
/// `* Fix the scan by @someone in https://…/pull/4` becomes `Fix the scan`.
pub fn notes(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| l.trim().strip_prefix("* ").or_else(|| l.trim().strip_prefix("- ")))
        .map(|l| {
            let l = l.split(" by @").next().unwrap_or(l);
            let l = l.split(" in https://").next().unwrap_or(l);
            unlink(l).trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .take(8)
        .collect()
}

/// `[text](url)` becomes `text`.
fn unlink(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|i| open + i) else { break };
        let Some(end) = rest[close..].find(')').map(|i| close + i) else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(tag: &str, url_base: &str, size: u64) -> String {
        format!(
            r###"{{"tag_name":"{tag}","draft":false,"prerelease":false,
            "body":"## What's Changed\n* Group node_modules by package manager by @a in https://github.com/x/y/pull/9\n* Fix [scan](https://x) stalls\n\n**Full Changelog**: https://x",
            "assets":[{{"name":"{ASSET_EXE}","browser_download_url":"{url_base}/{tag}/{ASSET_EXE}","size":{size}}},
                      {{"name":"{ASSET_SIG}","browser_download_url":"{url_base}/{tag}/{ASSET_SIG}","size":200}}]}}"###
        )
    }

    const BASE: &str = "https://github.com/rajofearth/projects/releases/download";

    #[test]
    fn reads_the_version_assets_and_notes() {
        let r = parse(&json("v0.3.0", BASE, 14_000_000)).unwrap();
        assert_eq!(r.version.to_string(), "0.3.0");
        assert_eq!(r.exe_size, 14_000_000);
        assert!(r.sig_url.ends_with(ASSET_SIG));
        assert_eq!(r.notes, vec!["Group node_modules by package manager", "Fix scan stalls"]);
    }

    #[test]
    fn refuses_foreign_urls_bad_sizes_and_prerelease_tags() {
        assert!(parse(&json("v0.3.0", "https://evil.example/releases/download", 10)).is_err());
        assert!(parse(&json("v0.3.0", BASE, 0)).is_err());
        assert!(parse(&json("v0.3.0", BASE, MAX_EXE_BYTES + 1)).is_err());
        assert!(parse(&json("v0.3.0-rc.1", BASE, 10)).is_err());
        assert!(parse("{}").is_err());
    }
}
