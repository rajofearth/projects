//! Plan usage limits for the Usage page's Limits tab, after t3code's `codexUsageLimits.ts`
//! and `claudeUsageLimits.ts` (MIT License, Copyright (c) 2026 T3 Tools Inc.).
//!
//! - Codex: the `rate_limits` snapshot Codex writes into its newest session log
//!   (`~/.codex/sessions/**/rollout-*.jsonl`). `primary` is the 5-hour session window
//!   (the monthly window on free/go plans), `secondary` the weekly one.
//! - Claude: one read-only GET of Anthropic's OAuth usage endpoint with the token Claude
//!   Code keeps in `~/.claude/.credentials.json` (t3code asks the Agent SDK, which reads
//!   the same endpoint). `curl` is fed its config on stdin so the token never reaches a
//!   command line.
//!
//! Blocking; call off the UI thread.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

const SESSION_MINS: i64 = 5 * 60;
const WEEK_MINS: i64 = 7 * 24 * 60;
const MONTH_MINS: i64 = 30 * 24 * 60;

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    /// 0–100.
    pub used_pct: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
    pub minutes: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    pub provider: &'static str,
    pub windows: Vec<Window>,
    /// Why there are no windows, when there are none.
    pub error: Option<String>,
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

pub fn load() -> Vec<Limits> {
    let Some(home) = home() else { return Vec::new() };
    vec![codex(&home), claude(&home)]
}

fn unavailable(provider: &'static str, why: &str) -> Limits {
    Limits { provider, windows: Vec::new(), error: Some(why.to_string()) }
}

// ---- Codex ------------------------------------------------------------------

fn kind_label(mins: i64) -> &'static str {
    if mins >= MONTH_MINS {
        "Monthly"
    } else if mins >= WEEK_MINS {
        "Weekly"
    } else {
        "Session"
    }
}

/// t3code's `codexRateLimitsToWindows` over the snake_case snapshot in the logs.
pub fn codex_windows(rl: &Value) -> Vec<Window> {
    if rl.get("limit_id").and_then(Value::as_str).is_some_and(|id| id != "codex") {
        return Vec::new();
    }
    let monthly_plan = matches!(rl.get("plan_type").and_then(Value::as_str), Some("free" | "go"));
    let mut out = Vec::new();
    for (key, fallback) in [("primary", if monthly_plan { MONTH_MINS } else { SESSION_MINS }), ("secondary", WEEK_MINS)] {
        let Some(w) = rl.get(key).filter(|w| w.is_object()) else { continue };
        let Some(used) = w.get("used_percent").and_then(Value::as_f64) else { continue };
        let mins = w.get("window_minutes").and_then(Value::as_i64).unwrap_or(fallback);
        out.push(Window {
            label: kind_label(mins).into(),
            used_pct: used.clamp(0.0, 100.0),
            resets_at: w.get("resets_at").and_then(Value::as_i64).filter(|t| *t > 0),
            minutes: Some(mins),
        });
    }
    if let Some(ind) = rl.get("individual_limit").filter(|w| w.is_object())
        && let Some(rem) = ind.get("remaining_percent").and_then(Value::as_f64) {
            out.push(Window {
                label: "Monthly credit limit".into(),
                used_pct: (100.0 - rem).clamp(0.0, 100.0),
                resets_at: ind.get("resets_at").and_then(Value::as_i64).filter(|t| *t > 0),
                minutes: None,
            });
        }
    out
}

fn newest_logs(dir: &Path, out: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            newest_logs(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl")
            && let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                out.push((m, p));
            }
    }
}

fn codex(home: &Path) -> Limits {
    let mut logs = Vec::new();
    newest_logs(&home.join(".codex").join("sessions"), &mut logs);
    logs.sort_by_key(|l| std::cmp::Reverse(l.0));
    for (_, path) in logs.iter().take(8) {
        let Ok(file) = fs::File::open(path) else { continue };
        let mut latest: Option<Value> = None;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"rate_limits\"") {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && let Some(rl) = v.pointer("/payload/rate_limits").filter(|r| r.is_object()) {
                    latest = Some(rl.clone());
                }
        }
        if let Some(rl) = latest {
            let windows = codex_windows(&rl);
            if !windows.is_empty() {
                return Limits { provider: "codex", windows, error: None };
            }
        }
    }
    unavailable("codex", "No rate limits in recent Codex sessions")
}

// ---- Claude -----------------------------------------------------------------

/// t3code's `claudeUsageResponseToLimits`: the 5-hour and 7-day windows, then any
/// model-scoped weekly windows. `utilization` is already a percentage.
pub fn claude_windows(v: &Value) -> Vec<Window> {
    let mut out = Vec::new();
    let window = |label: String, w: &Value, mins: i64| -> Option<Window> {
        let used = w.get("utilization").and_then(Value::as_f64)?;
        Some(Window {
            label,
            used_pct: used.clamp(0.0, 100.0),
            resets_at: w.get("resets_at").and_then(Value::as_str).and_then(crate::usage::epoch_secs),
            minutes: Some(mins),
        })
    };
    for (key, label, mins) in [("five_hour", "Session", SESSION_MINS), ("seven_day", "Weekly", WEEK_MINS)] {
        if let Some(w) = v.get(key).and_then(|w| window(label.into(), w, mins)) {
            out.push(w);
        }
    }
    for (key, name) in [("seven_day_opus", "Opus"), ("seven_day_sonnet", "Sonnet")] {
        if let Some(w) = v.get(key).and_then(|w| window(format!("Weekly · {name}"), w, WEEK_MINS)) {
            out.push(w);
        }
    }
    out
}

fn claude(home: &Path) -> Limits {
    let creds = fs::read_to_string(home.join(".claude").join(".credentials.json")).ok();
    let token = creds
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.pointer("/claudeAiOauth/accessToken").and_then(Value::as_str).map(str::to_string));
    let Some(token) = token else { return unavailable("claude", "Not signed in to Claude Code") };
    if token.contains(['"', '\n', '\r']) {
        return unavailable("claude", "Unreadable Claude Code credentials");
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-f", "-m", "10", "-K", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let Ok(mut child) = cmd.spawn() else { return unavailable("claude", "curl is not available") };
    if let Some(mut stdin) = child.stdin.take() {
        let config = format!(
            "url = \"https://api.anthropic.com/api/oauth/usage\"\n\
             header = \"Authorization: Bearer {token}\"\n\
             header = \"anthropic-beta: oauth-2025-04-20\"\n\
             header = \"Accept: application/json\"\n\
             user-agent = \"projects-stats\"\n"
        );
        let _ = stdin.write_all(config.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else { return unavailable("claude", "Usage request failed") };
    if !out.status.success() {
        return unavailable("claude", "Anthropic did not return usage");
    }
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return unavailable("claude", "Unexpected usage response");
    };
    let windows = claude_windows(&v);
    if windows.is_empty() {
        return unavailable("claude", "No limits reported for this plan");
    }
    Limits { provider: "claude", windows, error: None }
}

/// "3h 49m", "2d 5h", "12m".
pub fn countdown(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{}m", m.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_free_plan_primary_is_monthly() {
        let rl: Value = serde_json::from_str(r#"{"limit_id":"codex","primary":{"used_percent":2.0,"window_minutes":43200,"resets_at":1795585652},"secondary":null,"plan_type":"free"}"#).unwrap();
        let w = codex_windows(&rl);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].label, "Monthly");
        assert_eq!(w[0].used_pct, 2.0);
        assert_eq!(w[0].resets_at, Some(1795585652));
    }

    #[test]
    fn codex_paid_plan_has_session_and_weekly() {
        let rl: Value = serde_json::from_str(r#"{"primary":{"used_percent":40},"secondary":{"used_percent":10,"window_minutes":10080},"plan_type":"plus"}"#).unwrap();
        let labels: Vec<_> = codex_windows(&rl).into_iter().map(|w| w.label).collect();
        assert_eq!(labels, ["Session", "Weekly"]);
    }

    #[test]
    fn claude_usage_response() {
        let v: Value = serde_json::from_str(r#"{"five_hour":{"utilization":5.0,"resets_at":"2026-11-17T14:00:00.000000+00:00"},"seven_day":{"utilization":65.0,"resets_at":"2026-11-19T20:00:00+00:00"},"seven_day_opus":null}"#).unwrap();
        let w = claude_windows(&v);
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].label.as_str(), w[0].used_pct), ("Session", 5.0));
        assert!(w[1].resets_at.is_some());
        assert_eq!(countdown(2 * 86_400 + 5 * 3600 + 60), "2d 5h");
        assert_eq!(countdown(3 * 3600 + 49 * 60), "3h 49m");
    }

    /// Reads the real limits. Run with: cargo test -- --ignored --nocapture print_limits
    #[test]
    #[ignore]
    fn print_limits() {
        for l in load() {
            println!("{} {:?} {:?}", l.provider, l.error, l.windows);
        }
    }
}
