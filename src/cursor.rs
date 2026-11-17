//! Cursor account usage and plan limits, after t3code's `provider-cursor`
//! (`accountUsage.ts`, `usageLimits.ts`, `apiKey.ts`; MIT License, Copyright (c) 2026
//! T3 Tools Inc.). Cursor keeps usage on its servers, so both come from its dashboard API
//! (`api2.cursor.sh`, read-only RPCs).
//!
//! The token: `CURSOR_AUTH_TOKEN`, else `CURSOR_API_KEY` traded for one, as t3code does;
//! failing both, the access token the Cursor app keeps in its `state.vscdb` (opened
//! read-only). Blocking; call off the UI thread. With no token nothing is shown.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::limits::{http_json, Window};

const API: &str = "https://api2.cursor.sh";
const PAGE: usize = 1000;

/// One billed event from the dashboard.
pub struct Event {
    pub ts_ms: i64,
    pub model: String,
    pub session: String,
    /// uncached input, cache read, cache write, output
    pub tokens: [u64; 4],
    pub cost_usd: Option<f64>,
    pub dedupe: String,
}

fn rpc(token: &str, method: &str, body: &Value) -> Option<Value> {
    let url = format!("{API}/aiserver.v1.DashboardService/{method}");
    let (status, v) = http_json("POST", &url, token, &["connect-protocol-version: 1", "x-cursor-client-type: cli"], Some(&body.to_string()))?;
    let ok = (200..300).contains(&status) && v.is_object() && !["error", "message", "code"].iter().any(|k| v.get(k).is_some());
    ok.then_some(v)
}

/// The access token Cursor's RPCs accept, or None when Cursor isn't signed in here.
pub fn token(home: &Path) -> Option<String> {
    if let Some(t) = std::env::var("CURSOR_AUTH_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        return Some(t);
    }
    if let Some(key) = std::env::var("CURSOR_API_KEY").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        let (status, v) = http_json("POST", &format!("{API}/auth/exchange_user_api_key"), &key, &[], Some("{}"))?;
        return (status == 200).then(|| v.get("accessToken")?.as_str().map(str::to_string)).flatten();
    }
    let appdata = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
    let db = rusqlite::Connection::open_with_flags(
        appdata.join("Cursor").join("User").join("globalStorage").join("state.vscdb"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(200));
    let v: String = db.query_row("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'", [], |r| r.get(0)).ok()?;
    let v = v.trim().trim_matches('"').to_string();
    (!v.is_empty()).then_some(v)
}

/// The signed-in email Cursor caches, for the hover card.
pub fn email(home: &Path) -> Option<String> {
    let appdata = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
    let db = rusqlite::Connection::open_with_flags(
        appdata.join("Cursor").join("User").join("globalStorage").join("state.vscdb"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let v: String = db.query_row("SELECT value FROM ItemTable WHERE key = 'cursorAuth/cachedEmail'", [], |r| r.get(0)).ok()?;
    Some(v.trim().trim_matches('"').to_string()).filter(|e| !e.is_empty())
}

/// t3code's `cursorUsageResponseToLimits`.
pub fn limit_windows(v: &Value) -> Vec<Window> {
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str()?.parse().ok()));
    let end = num("billingCycleEnd").filter(|e| *e > 0.0);
    let start = num("billingCycleStart").filter(|s| *s > 0.0);
    let minutes = match (start, end) {
        (Some(s), Some(e)) if e > s => Some(((e - s) / 60_000.0).round() as i64),
        _ => None,
    };
    let Some(plan) = v.get("planUsage") else { return Vec::new() };
    [("totalPercentUsed", "Overall"), ("autoPercentUsed", "Cursor Models"), ("apiPercentUsed", "Other Models")]
        .iter()
        .filter_map(|(id, label)| {
            let used = plan.get(*id)?.as_f64().filter(|u| u.is_finite())?;
            Some(Window { label: (*label).into(), used_pct: used.clamp(0.0, 100.0), resets_at: end.map(|e| (e / 1000.0) as i64), minutes })
        })
        .collect()
}

pub fn limits(token: &str) -> Vec<Window> {
    rpc(token, "GetCurrentPeriodUsage", &json!({})).map(|v| limit_windows(&v)).unwrap_or_default()
}

fn canonical(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        Value::Object(o) => {
            let mut kv: Vec<_> = o.iter().collect();
            kv.sort_by(|a, b| a.0.cmp(b.0));
            format!("{{{}}}", kv.iter().map(|(k, v)| format!("{}:{}", Value::from(k.as_str()), canonical(v))).collect::<Vec<_>>().join(","))
        }
        other => other.to_string(),
    }
}

/// Longest suffix of `previous` that is a prefix of `current` (KMP), the rows a page repeats.
pub fn boundary_overlap(previous: &[String], current: &[String]) -> usize {
    let seq: Vec<&str> = current.iter().map(String::as_str).chain([""]).chain(previous.iter().map(String::as_str)).collect();
    let mut lens = vec![0usize; seq.len()];
    for i in 1..seq.len() {
        let mut l = lens[i - 1];
        while l > 0 && seq[i] != seq[l] {
            l = lens[l - 1];
        }
        if seq[i] == seq[l] {
            l += 1;
        }
        lens[i] = l;
    }
    *lens.last().unwrap_or(&0)
}

fn tokens(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0).map_or(0, |n| n.trunc() as u64)
}

/// One scope (personal or a team) of `GetFilteredUsageEvents`, pages de-overlapped.
fn read_scope(token: &str, since_ms: i64, end_ms: i64, team: Option<i64>, user: i64) -> Option<Vec<Event>> {
    let mut normalized: Vec<Option<Event>> = Vec::new();
    let mut pages: Vec<(usize, usize, usize)> = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    let mut total: Option<usize> = None;
    for page in 1.. {
        if page > total.map_or(1000, |t| t.div_ceil(PAGE) * 2 + 1) {
            return None;
        }
        let mut body = json!({"page": page, "pageSize": PAGE, "startDate": since_ms.to_string(), "endDate": end_ms.to_string()});
        if let Some(t) = team {
            body["teamId"] = json!(t);
            body["userId"] = json!(user);
        }
        let v = rpc(token, "GetFilteredUsageEvents", &body)?;
        if let Some(c) = v.get("totalUsageEventsCount") {
            total = Some(c.as_u64()? as usize);
        }
        let events: Vec<Value> = v.get("usageEventsDisplay").and_then(Value::as_array).cloned().unwrap_or_default();
        let start = normalized.len();
        let canon: Vec<String> = events.iter().map(canonical).collect();
        let first_ts = events.first().and_then(|e| e.get("timestamp")).cloned();
        let may_overlap = first_ts.as_ref().is_none_or(|t| !(t.is_string() || t.is_number()))
            || events.first().is_some_and(|f| previous_has_ts(&previous, f));
        let overlap = if may_overlap { boundary_overlap(&previous, &canon) } else { 0 };
        previous = canon;
        for e in &events {
            let Some(usage) = e.get("tokenUsage").filter(|u| !u.is_null()) else {
                normalized.push(None);
                continue;
            };
            let ts = match e.get("timestamp") {
                Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
                Some(Value::Number(n)) => n.as_f64(),
                _ => None,
            }?;
            let model = e.get("model").and_then(Value::as_str).filter(|m| !m.is_empty())?.to_string();
            let ts = ts as i64;
            if ts < since_ms || ts > end_ms {
                normalized.push(None);
                continue;
            }
            let t = [tokens(usage.get("inputTokens")), tokens(usage.get("cacheReadTokens")), tokens(usage.get("cacheWriteTokens")), tokens(usage.get("outputTokens"))];
            let cost = usage.get("totalCents").and_then(Value::as_f64).map(|c| c / 100.0);
            let session = e.get("conversationId").and_then(Value::as_str).unwrap_or("").to_string();
            let identity = json!([team, ts, model, session, t, cost]).to_string();
            normalized.push(Some(Event { ts_ms: ts, model, session, tokens: t, cost_usd: cost, dedupe: identity }));
        }
        pages.push((start, normalized.len(), overlap));
        if events.len() < PAGE {
            break;
        }
    }
    let raw = normalized.len();
    if total.is_some_and(|t| raw < t) {
        return None;
    }
    let mut removals = total.map_or(0, |t| raw - t);
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (start, end, overlap) in pages {
        let drop = removals.min(overlap);
        removals -= drop;
        for (i, rec) in normalized.iter_mut().enumerate().take(end).skip(start) {
            let rec = rec.take();
            if i < start + drop {
                continue;
            }
            let Some(mut rec) = rec else { continue };
            let n = seen.entry(rec.dedupe.clone()).or_insert(0);
            rec.dedupe = format!("cursor:{}:{}", rec.dedupe, n);
            *n += 1;
            out.push(rec);
        }
    }
    (removals == 0).then_some(out)
}

fn previous_has_ts(previous: &[String], first: &Value) -> bool {
    let Some(ts) = first.get("timestamp") else { return true };
    let needle = format!("\"timestamp\":{ts}");
    previous.iter().any(|p| p.contains(&needle))
}

/// Every billed event since `since_ms`, personal and per team. None when Cursor isn't
/// reachable or answers inconsistently (then nothing is shown, as in t3code).
pub fn usage(token: &str, since_ms: i64, end_ms: i64) -> Option<Vec<Event>> {
    let me = rpc(token, "GetMe", &json!({}))?;
    let user = me.get("userId").and_then(Value::as_i64).filter(|u| *u > 0)?;
    let teams = rpc(token, "GetTeams", &json!({}))?;
    let mut ids: Vec<i64> = teams.get("teams").and_then(Value::as_array).map(|t| t.iter().filter_map(|x| x.get("id")?.as_i64()).collect()).unwrap_or_default();
    ids.dedup();
    let mut out = read_scope(token, since_ms, end_ms, None, user)?;
    for id in ids {
        out.extend(read_scope(token, since_ms, end_ms, Some(id), user)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_and_limits() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(boundary_overlap(&s(&["a", "b", "c"]), &s(&["b", "c", "d"])), 2);
        assert_eq!(boundary_overlap(&s(&["a"]), &s(&["b"])), 0);
        let v: Value = serde_json::from_str(r#"{"billingCycleStart":"1793000000000","billingCycleEnd":"1795592000000","planUsage":{"totalPercentUsed":12,"autoPercentUsed":3}}"#).unwrap();
        let w = limit_windows(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label, "Overall");
        assert_eq!(w[0].minutes, Some(43_200));
    }
}
