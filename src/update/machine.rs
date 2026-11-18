//! The updater's state and every transition it makes, as plain functions with no I/O, so the
//! rules are tested without a network or a disk:
//!
//! - one check or download at a time;
//! - a downloaded update stays offered when a later check fails or finds nothing newer;
//! - a version that failed to start after installing is never offered again;
//! - a newer release than the downloaded one replaces it.

use std::path::PathBuf;

use super::feed::Release;
use super::version::Version;

/// A verified update on disk, waiting for a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: Version,
    pub exe: PathBuf,
    pub sha256: String,
    /// The `.sig` file's text, checked again just before installing.
    pub sig: String,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Checking,
    Downloading { version: Version, done: u64, total: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateState {
    pub current: Version,
    /// Why updates are off for this copy (not installed, or turned off by the environment).
    pub disabled: Option<String>,
    pub activity: Activity,
    pub ready: Option<Staged>,
    /// The last check's outcome: the newest release seen, if newer than this build.
    pub latest: Option<Release>,
    /// Unix seconds of the last finished check.
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    /// A version that was installed but didn't start; skipped until a newer one ships.
    pub bad: Option<Version>,
}

/// What the caller does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    Nothing,
    Download(Release),
    /// A download finished: tell the user once.
    Announce(Version),
}

impl UpdateState {
    pub fn new(current: Version) -> UpdateState {
        UpdateState {
            current,
            disabled: None,
            activity: Activity::Idle,
            ready: None,
            latest: None,
            checked_at: None,
            error: None,
            bad: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.activity != Activity::Idle
    }

    pub fn can_check(&self) -> bool {
        self.disabled.is_none() && !self.busy()
    }

    /// Keeps a staged update only if it is newer than this build and not the known-bad one.
    pub fn restore(&mut self, staged: Staged) -> bool {
        if staged.version > self.current && Some(staged.version) != self.bad {
            self.ready = Some(staged);
            true
        } else {
            false
        }
    }

    pub fn check_started(&mut self) -> bool {
        if !self.can_check() {
            return false;
        }
        self.activity = Activity::Checking;
        self.error = None;
        true
    }

    /// A check came back. `Ok(None)` is "nothing newer".
    pub fn check_finished(&mut self, result: Result<Option<Release>, String>, now: i64) -> Next {
        self.activity = Activity::Idle;
        match result {
            Err(e) => {
                self.error = Some(e);
                Next::Nothing
            }
            Ok(found) => {
                self.checked_at = Some(now);
                let offer = found.filter(|r| r.version > self.current && Some(r.version) != self.bad);
                self.latest = offer.clone();
                match offer {
                    Some(r) if self.ready.as_ref().is_none_or(|s| s.version < r.version) => {
                        self.activity = Activity::Downloading { version: r.version, done: 0, total: r.exe_size };
                        Next::Download(r)
                    }
                    _ => Next::Nothing,
                }
            }
        }
    }

    pub fn progress(&mut self, done: u64, total: u64) {
        if let Activity::Downloading { done: d, total: t, .. } = &mut self.activity {
            *d = done;
            *t = total;
        }
    }

    pub fn download_finished(&mut self, result: Result<Staged, String>) -> Next {
        self.activity = Activity::Idle;
        match result {
            Ok(staged) => {
                let v = staged.version;
                self.ready = Some(staged);
                self.error = None;
                Next::Announce(v)
            }
            Err(e) => {
                self.error = Some(e);
                Next::Nothing
            }
        }
    }

    /// One line for the sidebar and Settings.
    pub fn headline(&self) -> String {
        if let Some(why) = &self.disabled {
            return why.clone();
        }
        match &self.activity {
            Activity::Checking => return "Checking for updates".into(),
            Activity::Downloading { version, done, total } => {
                let pct = if *total > 0 { (*done as f64 / *total as f64 * 100.0).min(100.0) } else { 0.0 };
                return format!("Downloading {version} · {pct:.0}%");
            }
            Activity::Idle => {}
        }
        if let Some(s) = &self.ready {
            return format!("Version {} is ready to install", s.version);
        }
        if let Some(e) = &self.error {
            return format!("Couldn't update: {e}");
        }
        match self.checked_at {
            Some(_) => "Up to date".into(),
            None => "Not checked yet".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn release(s: &str) -> Release {
        Release { version: v(s), notes: vec![], exe_url: String::new(), exe_size: 100, sig_url: String::new() }
    }

    fn staged(s: &str) -> Staged {
        Staged { version: v(s), exe: PathBuf::new(), sha256: String::new(), sig: String::new(), notes: vec![] }
    }

    #[test]
    fn a_newer_release_is_downloaded_then_announced() {
        let mut st = UpdateState::new(v("0.2.0"));
        assert!(st.check_started());
        assert!(!st.check_started(), "one action at a time");
        assert_eq!(st.check_finished(Ok(Some(release("0.3.0"))), 1), Next::Download(release("0.3.0")));
        assert!(st.busy());
        st.progress(50, 100);
        assert_eq!(st.headline(), "Downloading 0.3.0 · 50%");
        assert_eq!(st.download_finished(Ok(staged("0.3.0"))), Next::Announce(v("0.3.0")));
        assert_eq!(st.headline(), "Version 0.3.0 is ready to install");
    }

    #[test]
    fn same_or_older_releases_are_up_to_date() {
        let mut st = UpdateState::new(v("0.2.0"));
        st.check_started();
        assert_eq!(st.check_finished(Ok(Some(release("0.2.0"))), 1), Next::Nothing);
        assert_eq!(st.headline(), "Up to date");
        assert!(st.latest.is_none());
    }

    #[test]
    fn a_failed_check_keeps_the_downloaded_update() {
        let mut st = UpdateState::new(v("0.2.0"));
        assert!(st.restore(staged("0.3.0")));
        st.check_started();
        st.check_finished(Err("offline".into()), 1);
        assert_eq!(st.ready.as_ref().unwrap().version, v("0.3.0"));
        assert_eq!(st.headline(), "Version 0.3.0 is ready to install");
    }

    #[test]
    fn the_downloaded_version_is_not_fetched_twice_but_a_newer_one_is() {
        let mut st = UpdateState::new(v("0.2.0"));
        st.restore(staged("0.3.0"));
        st.check_started();
        assert_eq!(st.check_finished(Ok(Some(release("0.3.0"))), 1), Next::Nothing);
        st.check_started();
        assert_eq!(st.check_finished(Ok(Some(release("0.4.0"))), 2), Next::Download(release("0.4.0")));
    }

    #[test]
    fn a_version_that_failed_to_start_is_skipped() {
        let mut st = UpdateState::new(v("0.2.0"));
        st.bad = Some(v("0.3.0"));
        assert!(!st.restore(staged("0.3.0")));
        st.check_started();
        assert_eq!(st.check_finished(Ok(Some(release("0.3.0"))), 1), Next::Nothing);
        st.check_started();
        assert!(matches!(st.check_finished(Ok(Some(release("0.3.1"))), 2), Next::Download(_)));
    }

    #[test]
    fn stale_or_older_staged_files_are_dropped() {
        let mut st = UpdateState::new(v("0.3.0"));
        assert!(!st.restore(staged("0.3.0")));
        assert!(!st.restore(staged("0.2.0")));
    }

    #[test]
    fn a_disabled_updater_never_starts() {
        let mut st = UpdateState::new(v("0.2.0"));
        st.disabled = Some("Updates are off".into());
        assert!(!st.check_started());
        assert_eq!(st.headline(), "Updates are off");
    }
}
