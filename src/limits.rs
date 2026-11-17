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
//! - Grok: `cli-chat-proxy.grok.com/v1/billing` with the `~/.grok/auth.json` session
//!   (`provider-grok/src/server/usageLimits.ts`).
//! - OpenCode Go: `opencode.ai/zen/go/v1/usage` with the key OpenCode stores in its auth
//!   file or `credential` table (`provider-opencode/src/server/usageLimits.ts`).
//!
//! Providers that aren't set up on this machine are left out. Each card also carries the
//! account's email and plan for the hover card. Blocking; call off the UI thread.

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
    pub email: Option<String>,
    pub plan: Option<String>,
}

/// The machine name, for the hover card's "Signed in".
pub fn host() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

pub fn load() -> Vec<Limits> {
    let Some(home) = home() else { return Vec::new() };
    let mut out = vec![codex(&home), claude(&home)];
    out.extend(grok(&home));
    out.extend(opencode_go(&home));
    out
}

fn unavailable(provider: &'static str, why: &str) -> Limits {
    Limits { provider, windows: Vec::new(), error: Some(why.to_string()), email: None, plan: None }
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
                let (email, plan) = codex_account(home);
                return Limits { provider: "codex", windows, error: None, email, plan };
            }
        }
    }
    unavailable("codex", "No rate limits in recent Codex sessions")
}

/// Email and "ChatGPT Free Subscription" from the id token in `~/.codex/auth.json`.
fn codex_account(home: &Path) -> (Option<String>, Option<String>) {
    let claims = fs::read_to_string(home.join(".codex").join("auth.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.pointer("/tokens/id_token").and_then(Value::as_str).and_then(jwt_claims));
    let Some(c) = claims else { return (None, None) };
    let email = c.get("email").and_then(Value::as_str).map(str::to_string);
    let plan = c
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
        .and_then(Value::as_str)
        .map(|p| format!("ChatGPT {} Subscription", capitalize(p)));
    (email, plan)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The payload of a JWT, without checking its signature (it only labels the card).
fn jwt_claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for ch in part.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => continue,
        };
        bits = (bits << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    serde_json::from_slice(&out).ok()
}

/// One read-only GET through `curl`, the bearer token passed on stdin. (status, body)
fn get_json(url: &str, token: &str, extra: &[&str]) -> Option<(u16, Value)> {
    if token.contains(['"', '\n', '\r', '\\']) {
        return None;
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-m", "10", "-K", "-", "-w", "\n%{http_code}"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let mut config = format!("url = \"{url}\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"Accept: application/json\"\nuser-agent = \"projects-stats\"\n");
        for h in extra {
            config.push_str(&format!("header = \"{h}\"\n"));
        }
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n')?;
    Some((code.trim().parse().ok()?, serde_json::from_str(body).unwrap_or(Value::Null)))
}

// ---- Grok ---------------------------------------------------------------------

/// t3code's `grokUsageResponseToLimits`.
pub fn grok_windows(v: &Value) -> Vec<Window> {
    let period = v.pointer("/config/currentPeriod");
    let kind = period.and_then(|p| p.get("type")).and_then(Value::as_str).map(|t| t.trim_start_matches("USAGE_PERIOD_TYPE_"));
    let label = match kind {
        Some("WEEKLY") => "Weekly",
        Some("MONTHLY") => "Monthly",
        _ => "Subscription",
    };
    let start = period.and_then(|p| p.get("start")).and_then(Value::as_str).and_then(crate::usage::epoch_secs);
    let end = period.and_then(|p| p.get("end")).and_then(Value::as_str).and_then(crate::usage::epoch_secs);
    let mut used = v.pointer("/config/creditUsagePercent").and_then(Value::as_f64);
    if used.is_none() && label != "Subscription" && matches!((start, end), (Some(a), Some(b)) if a < b) {
        used = Some(0.0);
    }
    let Some(used) = used.filter(|u| u.is_finite()) else { return Vec::new() };
    let minutes = match (start, end) {
        (Some(a), Some(b)) if b > a => Some((b - a) / 60),
        _ => None,
    };
    vec![Window { label: label.into(), used_pct: used.clamp(0.0, 100.0), resets_at: end, minutes }]
}

fn grok(home: &Path) -> Option<Limits> {
    let dir = home.join(".grok");
    for name in ["config.toml", "managed_config.toml", "requirements.toml"] {
        let config = fs::read_to_string(dir.join(name)).unwrap_or_default();
        // Custom auth or endpoints belong to the CLI, as in t3code.
        if config.lines().any(|l| {
            let t = l.trim_start().trim_start_matches(['[', '"', '\'']);
            ["auth", "grok_com_config", "endpoints"].iter().any(|k| t.starts_with(k))
        }) {
            return None;
        }
    }
    let auth: Value = serde_json::from_str(&fs::read_to_string(dir.join("auth.json")).ok()?).ok()?;
    let cred = auth
        .get("https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828")
        .or_else(|| auth.get("https://accounts.x.ai/sign-in"))?;
    if cred.get("auth_mode").and_then(Value::as_str) == Some("api_key") {
        return None;
    }
    let email = cred.get("email").and_then(Value::as_str).map(str::to_string);
    let token = cred.get("key").and_then(Value::as_str)?.trim().to_string();
    let base = Limits { provider: "grok", windows: Vec::new(), error: None, email, plan: None };
    match get_json("https://cli-chat-proxy.grok.com/v1/billing?format=credits", &token, &[]) {
        Some((200, v)) => {
            let windows = grok_windows(&v);
            if windows.is_empty() {
                return None;
            }
            Some(Limits { windows, ..base })
        }
        // t3code's limits page lists accounts with windows; a failed probe stays out of it.
        _ => None,
    }
}

// ---- OpenCode Go ----------------------------------------------------------------

fn opencode_keys(home: &Path) -> Vec<String> {
    let data = home.join(".local").join("share").join("opencode");
    let mut keys: Vec<String> = Vec::new();
    if let Ok(db) = rusqlite::Connection::open_with_flags(data.join("opencode.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        let _ = db.busy_timeout(std::time::Duration::from_millis(100));
        for id in ["opencode", "opencode-go"] {
            let row: Option<String> = db
                .query_row(
                    "SELECT value FROM credential WHERE integration_id = ?1 AND active IS NOT 0 \
                     ORDER BY active DESC, time_created DESC, id DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .ok();
            let stored = row.and_then(|v| serde_json::from_str::<Value>(&v).ok());
            let Some(stored) = stored.filter(|v| v.get("type").and_then(Value::as_str) == Some("key")) else { continue };
            if let Some(server) = stored.pointer("/metadata/server").and_then(Value::as_str) {
                if server.trim_end_matches('/') != "https://opencode.ai/console" {
                    continue;
                }
            }
            if let Some(k) = stored.get("key").and_then(Value::as_str) {
                keys.push(k.trim().to_string());
            }
        }
    }
    let auth = fs::read_to_string(data.join("auth.json")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(k) = auth.as_ref().and_then(|a| a.pointer("/opencode-go/key")).and_then(Value::as_str) {
        keys.push(k.trim().to_string());
    }
    keys.retain(|k| !k.is_empty());
    keys.dedup();
    keys
}

pub fn opencode_windows(v: &Value) -> Vec<Window> {
    [("rolling", "Go · Session", Some(SESSION_MINS)), ("weekly", "Go · Weekly", Some(WEEK_MINS)), ("monthly", "Go · Monthly", None)]
        .iter()
        .filter_map(|(k, label, mins)| {
            let w = v.pointer(&format!("/usage/{k}"))?;
            Some(Window {
                label: (*label).into(),
                used_pct: w.get("percent").and_then(Value::as_f64)?.clamp(0.0, 100.0),
                resets_at: w.get("resetsAt").and_then(Value::as_str).and_then(crate::usage::epoch_secs),
                minutes: *mins,
            })
        })
        .collect()
}

fn opencode_go(home: &Path) -> Option<Limits> {
    let keys = opencode_keys(home);
    let mut response = None;
    for key in &keys {
        response = get_json("https://opencode.ai/zen/go/v1/usage", key, &[]);
        if !matches!(response, Some((401 | 403, _))) {
            break;
        }
    }
    let base = Limits { provider: "opencode", windows: Vec::new(), error: None, email: None, plan: Some("OpenCode Go".into()) };
    match response? {
        (403, _) => None,
        (200, v) => {
            let windows = opencode_windows(&v);
            (windows.len() == 3).then_some(Limits { windows, ..base })
        }
        _ => None,
    }
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
    let Some((status, v)) = get_json("https://api.anthropic.com/api/oauth/usage", &token, &["anthropic-beta: oauth-2025-04-20"]) else {
        return unavailable("claude", "curl is not available");
    };
    if status != 200 {
        return unavailable("claude", "Anthropic did not return usage");
    }
    let windows = claude_windows(&v);
    if windows.is_empty() {
        return unavailable("claude", "No limits reported for this plan");
    }
    let plan = creds
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.pointer("/claudeAiOauth/subscriptionType").and_then(Value::as_str).map(|p| format!("Claude {} Subscription", capitalize(p))));
    let email = fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.pointer("/oauthAccount/emailAddress").and_then(Value::as_str).map(str::to_string));
    Limits { provider: "claude", windows, error: None, email, plan }
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
    fn grok_and_opencode_go_responses() {
        let g: Value = serde_json::from_str(r#"{"config":{"creditUsagePercent":12.5,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-11-10T00:00:00Z","end":"2026-11-17T00:00:00Z"}}}"#).unwrap();
        let w = grok_windows(&g);
        assert_eq!((w[0].label.as_str(), w[0].used_pct, w[0].minutes), ("Weekly", 12.5, Some(WEEK_MINS)));
        let o: Value = serde_json::from_str(r#"{"usage":{"rolling":{"percent":10,"resetsAt":"2026-11-17T05:00:00Z"},"weekly":{"percent":20,"resetsAt":"2026-11-20T00:00:00Z"},"monthly":{"percent":30,"resetsAt":"2026-12-01T00:00:00Z"}}}"#).unwrap();
        assert_eq!(opencode_windows(&o).len(), 3);
        let claims = jwt_claims("x.eyJlbWFpbCI6ImFAYi5jIn0.y").unwrap();
        assert_eq!(claims["email"], "a@b.c");
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
