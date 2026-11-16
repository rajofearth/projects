//! Aggregate AI coding-agent token usage and cost from local log files.
//!
//! Sources (each skipped when missing):
//! - Claude Code: `%USERPROFILE%\.claude\projects\**\*.jsonl` (assistant lines),
//!   de-duplicated by `message.id` + `requestId`.
//! - Codex: `%USERPROFILE%\.codex\sessions\**\*.jsonl` (`token_count` events).
//! - OpenCode: `%USERPROFILE%\.local\share\opencode\storage\message\**\*.json`.
//!
//! Cost uses the log's own cost when present, otherwise a small built-in price
//! table (USD per million tokens, substring match); unknown models cost 0.
//! The day bucket is the local (machine timezone) YYYY-MM-DD.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One aggregated row: usage summed over (provider, model, local day, project).
pub struct Usage {
    pub provider: &'static str,
    pub model: String,
    /// YYYY-MM-DD in the machine's local timezone.
    pub day: String,
    /// Folder name of the session's cwd, or "" when unknown.
    pub project: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Copy, Default)]
struct Acc {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
}

#[derive(PartialEq, Eq, Hash)]
struct Key {
    provider: &'static str,
    model: String,
    day: String,
    project: String,
}

/// A single usage event before aggregation.
struct RawUse {
    model: String,
    day: String,
    project: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    /// Cost reported by the log itself, if any.
    cost: Option<f64>,
}

/// Read every available source and aggregate. Blocking; call off the UI thread.
pub fn load() -> Vec<Usage> {
    let home = match std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        Some(h) => PathBuf::from(h),
        None => return Vec::new(),
    };
    let mut acc: HashMap<Key, Acc> = HashMap::new();
    parse_claude_logs(&home, &mut acc);
    parse_codex_logs(&home, &mut acc);
    parse_opencode_logs(&home, &mut acc);

    let mut rows: Vec<Usage> = acc
        .into_iter()
        .map(|(k, a)| Usage {
            provider: k.provider,
            model: k.model,
            day: k.day,
            project: k.project,
            input: a.input,
            output: a.output,
            cache_read: a.cache_read,
            cache_write: a.cache_write,
            cost_usd: a.cost,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.provider, &a.day, &a.model, &a.project).cmp(&(b.provider, &b.day, &b.model, &b.project))
    });
    rows
}

fn add(acc: &mut HashMap<Key, Acc>, raw: RawUse, provider: &'static str) {
    let cost = raw.cost.unwrap_or_else(|| {
        price(
            &raw.model,
            raw.input,
            raw.output,
            raw.cache_read,
            raw.cache_write,
        )
    });
    let key = Key {
        provider,
        model: raw.model,
        day: raw.day,
        project: raw.project,
    };
    let a = acc.entry(key).or_default();
    a.input += raw.input;
    a.output += raw.output;
    a.cache_read += raw.cache_read;
    a.cache_write += raw.cache_write;
    a.cost += cost;
}

// ---------------------------------------------------------------- Claude Code

fn parse_claude_logs(home: &Path, acc: &mut HashMap<Key, Acc>) {
    let mut files = Vec::new();
    walk_ext(&home.join(".claude").join("projects"), "jsonl", &mut files);
    // Assistant responses are streamed as several lines sharing message.id +
    // requestId; only the first carries the usage we should count.
    let mut seen: HashSet<String> = HashSet::new();
    for file in files {
        let Ok(reader) = fs::File::open(&file).map(BufReader::new) else {
            continue;
        };
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            // Cheap pre-filter before paying for a JSON parse.
            if !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let id = v
                .pointer("/message/id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let request = v
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !seen.insert(format!("{id}\u{0}{request}")) {
                continue;
            }
            if let Some(raw) = parse_claude_value(&v) {
                add(acc, raw, "claude-code");
            }
        }
    }
}

fn parse_claude_value(v: &Value) -> Option<RawUse> {
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
    let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if input + output + cache_read + cache_write == 0 {
        return None; // interim / synthetic assistant lines carry no usage
    }
    Some(RawUse {
        model: model.to_string(),
        day: day_from_timestamp(v.get("timestamp").and_then(Value::as_str).unwrap_or("")),
        project: project_from_cwd(v.get("cwd").and_then(Value::as_str).unwrap_or("")),
        input,
        output,
        cache_read,
        cache_write,
        cost: v.get("costUSD").and_then(Value::as_f64),
    })
}

// ---------------------------------------------------------------------- Codex

fn parse_codex_logs(home: &Path, acc: &mut HashMap<Key, Acc>) {
    let mut files = Vec::new();
    walk_ext(&home.join(".codex").join("sessions"), "jsonl", &mut files);
    for file in files {
        let Ok(reader) = fs::File::open(&file).map(BufReader::new) else {
            continue;
        };
        // Model/cwd arrive in session_meta and turn_context lines; token_count
        // events carry either a per-call `last_token_usage` or cumulative
        // `total_token_usage`, from which we take deltas.
        let mut model = String::new();
        let mut cwd = String::new();
        let mut prev_total: Option<[u64; 4]> = None;
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if !line.contains("token_count") && !line.contains("session_meta") && !line.contains("turn_context") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
            match kind {
                "session_meta" => {
                    if let Some(c) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                    }
                }
                "turn_context" => {
                    if let Some(m) = v.pointer("/payload/model").and_then(Value::as_str) {
                        if !m.is_empty() {
                            model = m.to_string();
                        }
                    }
                    if let Some(c) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                        cwd = c.to_string();
                    }
                }
                "event_msg" => {
                    if v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") {
                        let info = v.pointer("/payload/info").unwrap_or(&v["payload"]);
                        if let Some(toks) = codex_tokens(info, &mut prev_total) {
                            add(
                                acc,
                                RawUse {
                                    model: if model.is_empty() { "unknown".into() } else { model.clone() },
                                    day: day_from_timestamp(
                                        v.get("timestamp").and_then(Value::as_str).unwrap_or(""),
                                    ),
                                    project: project_from_cwd(&cwd),
                                    input: toks[0],
                                    output: toks[1],
                                    cache_read: toks[2],
                                    cache_write: toks[3],
                                    cost: None, // Codex logs carry no cost
                                },
                                "codex",
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Field order: input, output, cache_read (cached_input), cache_write.
fn codex_tokens(info: &Value, prev_total: &mut Option<[u64; 4]>) -> Option<[u64; 4]> {
    let read = |obj: &Value, name: &str| {
        obj.get(name)
            .and_then(Value::as_u64)
            .or_else(|| obj.get(name).and_then(Value::as_i64).map(|n| n.max(0) as u64))
            .unwrap_or(0)
    };
    let total = |obj: &Value| {
        [
            read(obj, "input_tokens"),
            read(obj, "output_tokens"),
            read(obj, "cached_input_tokens"),
            read(obj, "cache_write_input_tokens"),
        ]
    };
    if let Some(last) = info.get("last_token_usage") {
        let toks = total(last);
        if let Some(t) = info.get("total_token_usage") {
            *prev_total = Some(total(t));
        }
        return Some(toks);
    }
    let totals = info.get("total_token_usage")?;
    let toks = total(totals);
    let delta = match *prev_total {
        Some(prev) => [
            toks[0].saturating_sub(prev[0]),
            toks[1].saturating_sub(prev[1]),
            toks[2].saturating_sub(prev[2]),
            toks[3].saturating_sub(prev[3]),
        ],
        None => toks, // first cumulative event: assume it is all we have seen
    };
    *prev_total = Some(toks);
    Some(delta)
}

// ------------------------------------------------------------------- OpenCode

fn parse_opencode_logs(home: &Path, acc: &mut HashMap<Key, Acc>) {
    let storage = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("storage");
    // sessionID -> project directory, from storage/session/**/*.json.
    let mut sessions: HashMap<String, String> = HashMap::new();
    let mut files = Vec::new();
    walk_ext(&storage.join("session"), "json", &mut files);
    for file in &files {
        if let Some(v) = read_json(&file) {
            if let (Some(id), Some(dir)) = (
                v.get("id").and_then(Value::as_str),
                v.get("directory")
                    .or_else(|| v.get("cwd"))
                    .and_then(Value::as_str),
            ) {
                sessions.insert(id.to_string(), dir.to_string());
            }
        }
    }
    files.clear();
    walk_ext(&storage.join("message"), "json", &mut files);
    for file in &files {
        let Some(v) = read_json(&file) else { continue };
        if v.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(tokens) = v.get("tokens") else { continue };
        let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
        let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
        let cache_read = tokens
            .pointer("/cache/read")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write = tokens
            .pointer("/cache/write")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input + output + cache_read + cache_write == 0 {
            continue;
        }
        // time.created is epoch milliseconds (number) or an ISO string.
        let day = match v.pointer("/time/created") {
            Some(Value::Number(n)) => n
                .as_f64()
                .map(|ms| local_day_from_epoch((ms / 1000.0) as i64))
                .unwrap_or_default(),
            Some(Value::String(s)) => day_from_timestamp(s),
            _ => String::new(),
        };
        let project = sessions
            .get(v.get("sessionID").and_then(Value::as_str).unwrap_or(""))
            .map(|d| project_from_cwd(d))
            .unwrap_or_default();
        add(
            acc,
            RawUse {
                model: v
                    .get("modelID")
                    .or_else(|| v.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                day,
                project,
                input,
                output,
                cache_read,
                cache_write,
                cost: v.get("cost").and_then(Value::as_f64),
            },
            "opencode",
        );
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

// ----------------------------------------------------------------- cost table

/// (needle, [input, output, cache_read, cache_write] USD per million tokens).
/// Matched case-insensitively by substring, most specific first; unknown
/// models price at 0. Cache writes bill at the input rate for OpenAI/Gemini
/// (no surcharge) and 1.25x input for Claude.
const PRICES: &[(&str, [f64; 4])] = &[
    // Claude (Anthropic list prices)
    ("claude-haiku-4", [1.0, 5.0, 0.1, 1.25]),
    ("claude-haiku", [0.8, 4.0, 0.08, 1.0]),
    ("claude-opus", [15.0, 75.0, 1.5, 18.75]),
    ("claude-sonnet", [3.0, 15.0, 0.3, 3.75]),
    // OpenAI
    ("gpt-5-nano", [0.05, 0.4, 0.005, 0.05]),
    ("gpt-5-mini", [0.25, 2.0, 0.025, 0.25]),
    ("gpt-5", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-4.1-mini", [0.4, 1.6, 0.1, 0.4]),
    ("gpt-4.1", [2.0, 8.0, 0.5, 2.0]),
    ("gpt-4o-mini", [0.15, 0.6, 0.075, 0.15]),
    ("gpt-4o", [2.5, 10.0, 1.25, 2.5]),
    ("o4-mini", [1.1, 4.4, 0.275, 1.1]),
    ("o3-mini", [1.1, 4.4, 0.275, 1.1]),
    ("o3", [2.0, 8.0, 0.5, 2.0]),
    // Google
    ("gemini-3-pro", [2.0, 12.0, 0.2, 2.0]),
    ("gemini-2.5-pro", [1.25, 10.0, 0.31, 1.25]),
    ("gemini-2.5-flash", [0.3, 2.5, 0.075, 0.3]),
    ("gemini-2.0-flash", [0.1, 0.4, 0.025, 0.1]),
    ("gemini-1.5-pro", [1.25, 5.0, 0.3125, 1.25]),
    ("gemini-1.5-flash", [0.075, 0.3, 0.01875, 0.075]),
];

/// Estimated cost in USD for the given usage at list prices, 0.0 if unknown.
fn price(model: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
    let m = model.to_ascii_lowercase();
    for (needle, rates) in PRICES {
        if m.contains(needle) {
            return (input as f64 * rates[0]
                + output as f64 * rates[1]
                + cache_read as f64 * rates[2]
                + cache_write as f64 * rates[3])
                / 1_000_000.0;
        }
    }
    0.0
}

// --------------------------------------------------------------------- helpers

fn project_from_cwd(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

fn walk_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_ext(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

// ------------------------------------------------------- local-day conversion

/// Local calendar day (YYYY-MM-DD) of the given epoch-seconds timestamp.
fn local_day_from_epoch(secs: i64) -> String {
    #[cfg(windows)]
    if let Some(day) = local_day_windows(secs) {
        return day;
    }
    utc_day_from_epoch(secs)
}

/// Day for an ISO-8601 timestamp ("2026-11-16T16:11:00.206Z"), honoring a
/// trailing UTC offset if present. Empty string when unparseable.
fn day_from_timestamp(ts: &str) -> String {
    match epoch_from_iso(ts) {
        Some(secs) => local_day_from_epoch(secs),
        None => String::new(),
    }
}

/// Parse "YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]" to epoch seconds (UTC).
fn epoch_from_iso(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |a: usize, e: usize| ts.get(a..e).and_then(|s| s.parse::<i64>().ok());
    let (y, mo, d) = (num(0, 4)?, num(5, 7)? as u32, num(8, 10)? as u32);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut epoch = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s;
    // Optional fraction then zone: Z, +HH:MM, -HH:MM.
    let mut i = 19;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        let sign = if b[i] == b'+' { 1 } else { -1 };
        let oh: i64 = ts.get(i + 1..i + 3)?.parse().ok()?;
        let om: i64 = ts.get(i + 4..i + 6)?.parse().ok()?;
        epoch -= sign * (oh * 3600 + om * 60);
    }
    Some(epoch)
}

/// Howard Hinnant's civil-date algorithms (public domain).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m + 9) % 12) as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_097 * 4) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (((mp + 2) % 12) + 1) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn utc_day_from_epoch(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Windows: apply the user's timezone rules via SystemTimeToTzSpecificLocalTime.
#[cfg(windows)]
fn local_day_windows(secs: i64) -> Option<String> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
    if secs < 0 {
        return None;
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    let utc = SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        // 1970-01-01 was a Thursday; SYSTEMTIME wants Sunday = 0.
        wDayOfWeek: ((days.rem_euclid(7) + 4) % 7) as u16,
        wDay: d as u16,
        wHour: (rem / 3600) as u16,
        wMinute: ((rem % 3600) / 60) as u16,
        wSecond: (rem % 60) as u16,
        wMilliseconds: 0,
    };
    let mut local = unsafe { std::mem::zeroed::<SYSTEMTIME>() };
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    if ok == 0 {
        return None;
    }
    Some(format!("{:04}-{:02}-{:02}", local.wYear, local.wMonth, local.wDay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_assistant_line() {
        let line = r#"{"parentUuid":"x","type":"assistant","timestamp":"2026-11-16T16:11:00.206Z","cwd":"C:\\Users\\me\\my-app","requestId":"req_1","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":200,"cache_read_input_tokens":300,"cache_creation_input_tokens":40}}}"#;
        let v: Value = serde_json::from_str(line).unwrap();
        let raw = parse_claude_value(&v).expect("should parse");
        assert_eq!(raw.model, "claude-sonnet-4-5");
        assert_eq!((raw.input, raw.output), (10, 200));
        assert_eq!((raw.cache_read, raw.cache_write), (300, 40));
        assert_eq!(raw.project, "my-app");
        // No logged cost: priced from the table (10*3 + 200*15 + 300*0.3 + 40*3.75)/1e6.
        assert!(raw.cost.is_none());
        let cost = price(&raw.model, raw.input, raw.output, raw.cache_read, raw.cache_write);
        assert!((cost - 0.00327).abs() < 1e-9);
        assert_eq!(raw.day.len(), 10);
        assert_eq!(&raw.day[4..5], "-");
    }

    #[test]
    fn parses_codex_last_token_usage() {
        let mut state: Option<[u64; 4]> = None;
        let info: Value = serde_json::from_str(
            r#"{"total_token_usage":{"input_tokens":21491,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":47,"reasoning_output_tokens":0,"total_tokens":21538},"last_token_usage":{"input_tokens":21491,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":47,"reasoning_output_tokens":0,"total_tokens":21538}}"#,
        )
        .unwrap();
        let first = codex_tokens(&info, &mut state).unwrap();
        assert_eq!(first, [21491, 47, 0, 0]);
        // Next event: cumulative grows, last_token_usage covers only the new call.
        let info2: Value = serde_json::from_str(
            r#"{"total_token_usage":{"input_tokens":24991,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":347},"last_token_usage":{"input_tokens":3500,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":300}}"#,
        )
        .unwrap();
        let second = codex_tokens(&info2, &mut state).unwrap();
        assert_eq!(second, [3500, 300, 0, 0]);
    }

    #[test]
    fn codex_totals_delta_when_last_missing() {
        let mut state: Option<[u64; 4]> = None;
        let mk = |i: u64, o: u64| {
            serde_json::from_str::<Value>(&format!(
                r#"{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":{o}}}}}"#
            ))
            .unwrap()
        };
        assert_eq!(codex_tokens(&mk(100, 20), &mut state).unwrap(), [100, 20, 0, 0]);
        assert_eq!(codex_tokens(&mk(150, 70), &mut state).unwrap(), [50, 50, 0, 0]);
    }
}
