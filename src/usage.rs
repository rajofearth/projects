//! Aggregate AI coding-agent token usage and cost from local log files.
//!
//! Sources (each skipped when missing):
//! - Claude Code: `%USERPROFILE%\.claude\projects\**\*.jsonl` (assistant lines),
//!   de-duplicated by `message.id` + `requestId`.
//! - Codex: `%USERPROFILE%\.codex\sessions\**\*.jsonl` (`token_count` events).
//! - OpenCode: `%USERPROFILE%\.local\share\opencode\opencode.db` (the `message` and
//!   `session_message` tables) plus the legacy `storage\message\**\*.json` files.
//! - Pi: `%USERPROFILE%\.pi\agent\sessions\**\*.jsonl`.
//!
//! Token accounting and pricing follow t3code (https://github.com/pingdotgg/t3code),
//! MIT License, Copyright (c) 2026 T3 Tools Inc. In particular: copies of one
//! billed message are merged by taking the maximum of each token channel;
//! cache writes are billed at separate 5-minute and 1-hour rates when the log
//! distinguishes them; reasoning tokens are reported inside output and are
//! never charged a second time; models with neither a logged cost nor a list
//! price are marked `priced: false` instead of quietly costing zero.
//! Prices are the USD-per-million list rates from LiteLLM's
//! `model_prices_and_context_window.json`, the table t3code itself parses.
//! The day bucket is the local (machine timezone) YYYY-MM-DD.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

/// One aggregated row: usage summed over (provider, model, local day, project).
///
/// `input` is uncached input only. `cache_write` is the total of 5-minute and
/// 1-hour cache writes (the split is used for pricing), and `output` includes
/// any reasoning tokens, matching t3code's `outputTokens`.
pub struct Usage {
    pub provider: &'static str,
    pub model: String,
    /// YYYY-MM-DD in the machine's local timezone.
    pub day: String,
    /// Folder name of the session's cwd, or "" when unknown.
    pub project: String,
    /// Epoch hour (seconds / 3600) for usage in the last 25 hours, -1 before that, so the
    /// Past 24h view can slide by the hour as t3code's does.
    pub hour: i64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    /// Portion of `cost_usd` attributed to each token channel. A logged cost is
    /// scaled across these categories by the model's list price; when there is
    /// no list price the whole cost goes to `cost_other` (t3code's "unsplit").
    pub cost_input: f64,
    pub cost_cache_read: f64,
    pub cost_cache_write: f64,
    pub cost_output: f64,
    pub cost_other: f64,
    /// Distinct sessions first contributing to this (provider, model, day, project)
    /// bucket. Counting a session only once makes the sum over all rows equal the
    /// source's distinct-session count, as in t3code's `mergeUsage`.
    pub sessions: u32,
    /// False when some of these tokens had no price: no cost in the log and no list price
    /// for the model, so `cost_usd` undercounts them.
    pub priced: bool,
}

#[derive(Default)]
struct Acc {
    unpriced: bool,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
    cost_input: f64,
    cost_cache_read: f64,
    cost_cache_write: f64,
    cost_output: f64,
    cost_other: f64,
    sessions: HashSet<String>,
}

#[derive(PartialEq, Eq, Hash)]
struct Key {
    provider: &'static str,
    model: String,
    day: String,
    project: String,
    hour: i64,
}

/// Usage at or after this epoch second is bucketed by hour too.
static HOURLY_FROM: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MAX);

/// A single usage event before aggregation.
struct RawUse {
    provider: &'static str,
    model: String,
    day: String,
    project: String,
    /// Uncached input only (Codex's `input_tokens` includes cached tokens).
    input: u64,
    /// Output, including reasoning tokens.
    output: u64,
    cache_read: u64,
    /// Total cache writes (5-minute + 1-hour).
    cache_write: u64,
    /// The subset of `cache_write` billed at the 1-hour rate.
    cache_write_1h: u64,
    /// Reasoning tokens, a subset of `output` kept only for the token mix.
    reasoning: u64,
    /// Cost reported by the log itself, if any.
    cost: Option<f64>,
    /// Session this message belongs to, for distinct-session counts.
    session_id: String,
    /// Repeats of one billed message share this key and are merged by max.
    dedupe: Option<String>,
    /// Epoch seconds, 0 when unknown.
    ts: i64,
    /// Service tier: 0 standard, 1 fast/priority, 2 ultrafast.
    speed: u8,
}

/// One record's list-price cost split by token channel (USD, not yet per-million scaled).
#[derive(Clone, Copy, Default)]
struct Category {
    input: f64,
    cache_read: f64,
    cache_write: f64,
    output: f64,
}

impl Category {
    fn sum(self) -> f64 {
        self.input + self.cache_read + self.cache_write + self.output
    }

    fn scaled(self, k: f64) -> Self {
        Self {
            input: self.input * k,
            cache_read: self.cache_read * k,
            cache_write: self.cache_write * k,
            output: self.output * k,
        }
    }
}

/// Read every available source and aggregate. Blocking; call off the UI thread.
///
/// OpenCode's SQLite history is intentionally bounded to roughly the widest range
/// the UI offers (a quarter plus a few days of timezone slack); the transcript
/// files are small and are read in full.
pub fn load() -> Vec<Usage> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    load_inner(now_ms - 92 * 86_400_000)
}

fn load_inner(cutoff_ms: i64) -> Vec<Usage> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    HOURLY_FROM.store(now - 25 * 3600, std::sync::atomic::Ordering::Relaxed);
    let home = match std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        Some(h) => PathBuf::from(h),
        None => return Vec::new(),
    };
    let mut acc: HashMap<Key, Acc> = HashMap::new();
    // One table across all providers and files: Claude copies messages into
    // resumed/forked transcripts, and OpenCode can repeat an id across stores.
    let mut seen: HashMap<String, RawUse> = HashMap::new();
    // A session is attributed to the first bucket that contributes to it, so
    // summing `Usage.sessions` gives the source's distinct-session count.
    let mut counted_sessions: HashSet<String> = HashSet::new();
    parse_claude_logs(&home, &mut acc, &mut seen, &mut counted_sessions);
    parse_codex_logs(&home, &mut acc, &mut seen, &mut counted_sessions);
    parse_pi_logs(&home, &mut acc, &mut seen, &mut counted_sessions);
    parse_grok_logs(&home, &mut acc, &mut seen, &mut counted_sessions);
    parse_opencode_logs(&home, &mut acc, &mut seen, &mut counted_sessions, cutoff_ms);
    // Keyed copies are counted once, after every copy has been folded in.
    for raw in seen.into_values() {
        accumulate(&mut acc, raw, &mut counted_sessions);
    }

    let mut rows: Vec<Usage> = acc
        .into_iter()
        .map(|(k, a)| Usage {
            provider: k.provider,
            model: k.model,
            day: k.day,
            project: k.project,
            hour: k.hour,
            input: a.input,
            output: a.output,
            cache_read: a.cache_read,
            cache_write: a.cache_write,
            cost_usd: a.cost,
            cost_input: a.cost_input,
            cost_cache_read: a.cost_cache_read,
            cost_cache_write: a.cost_cache_write,
            cost_output: a.cost_output,
            cost_other: a.cost_other,
            sessions: a.sessions.len() as u32,
            priced: !a.unpriced,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.provider, &a.day, &a.model, &a.project).cmp(&(b.provider, &b.day, &b.model, &b.project))
    });
    rows
}

/// Fold a partial copy into the held copy the way t3code's `mergeUsageCopies`
/// does: the maximum of every token channel and the larger reported cost.
fn add(
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
    mut raw: RawUse,
) {
    if let Some(key) = raw.dedupe.take() {
        if let Some(held) = seen.get_mut(&key) {
            held.input = held.input.max(raw.input);
            held.output = held.output.max(raw.output);
            held.cache_read = held.cache_read.max(raw.cache_read);
            held.cache_write = held.cache_write.max(raw.cache_write);
            held.cache_write_1h = held.cache_write_1h.max(raw.cache_write_1h);
            held.reasoning = held.reasoning.max(raw.reasoning);
            held.cost = match (held.cost, raw.cost) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            return;
        }
        seen.insert(key, raw);
        return;
    }
    accumulate(acc, raw, counted);
}

/// Price one record the way t3code's `priceUsage` does: a logged cost wins, but
/// it is split across token categories in proportion to the model's list price.
/// With no list price the logged cost is "unsplit" and lands in `cost_other`.
fn accumulate(acc: &mut HashMap<Key, Acc>, raw: RawUse, counted: &mut HashSet<String>) {
    let rate = price_for(&raw.model);
    let tiered = rate.map(|r| rates_at(r, raw.speed));
    let one_hour = raw.cache_write_1h.min(raw.cache_write);
    let list = tiered.map(|r| {
        Category {
            input: raw.input as f64 * r.input,
            cache_read: raw.cache_read as f64 * r.read,
            cache_write: (raw.cache_write - one_hour) as f64 * r.write_5m
                + one_hour as f64 * r.write_1h,
            output: raw.output as f64 * r.output,
        }
        .scaled(1.0 / 1_000_000.0)
    });
    let list_total = list.map_or(0.0, Category::sum);
    let (cost, categories) = match (raw.cost, list) {
        (Some(reported), Some(c)) if list_total > 0.0 => (reported, Some(c.scaled(reported / list_total))),
        (Some(reported), _) => (reported, None),
        (None, Some(c)) if list_total > 0.0 => (list_total, Some(c)),
        _ => (0.0, None),
    };

    let key = Key {
        provider: raw.provider,
        model: raw.model,
        day: raw.day,
        project: raw.project,
        hour: if raw.ts >= HOURLY_FROM.load(std::sync::atomic::Ordering::Relaxed) { raw.ts.div_euclid(3600) } else { -1 },
    };
    let a = acc.entry(key).or_default();
    a.input += raw.input;
    a.output += raw.output;
    a.cache_read += raw.cache_read;
    a.cache_write += raw.cache_write;
    a.cost += cost;
    match categories {
        Some(c) => {
            a.cost_input += c.input;
            a.cost_cache_read += c.cache_read;
            a.cost_cache_write += c.cache_write;
            a.cost_output += c.output;
        }
        None => a.cost_other += cost,
    }
    a.unpriced |= raw.cost.is_none() && rate.is_none();
    if !raw.session_id.is_empty() && counted.insert(raw.session_id.clone()) {
        a.sessions.insert(raw.session_id);
    }
}

// ---------------------------------------------------------------- Claude Code

fn parse_claude_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
    let mut files = Vec::new();
    walk_ext(&home.join(".claude").join("projects"), "jsonl", &mut files);
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
            let Some(raws) = parse_claude_value(&v) else { continue };
            for raw in raws {
                add(acc, seen, counted, raw);
            }
        }
    }
}

/// Claude repeats a message for every content block, and subagent blocks can
/// grow from stream-start to final usage, so the caller merges each key by max.
/// `usage.iterations` holds advisor calls billed to a different model.
fn parse_claude_value(v: &Value) -> Option<Vec<RawUse>> {
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
    // Claude Code uses this for local placeholders that never reached the API.
    if model.is_empty() || model == "<synthetic>" {
        return None;
    }

    let id = v.pointer("/message/id").and_then(Value::as_str);
    let request = v.get("requestId").and_then(Value::as_str);
    let dedupe = if id.is_none() && request.is_none() {
        None
    } else {
        Some(format!("claude:{}:{}", id.unwrap_or(""), request.unwrap_or("")))
    };

    let day = day_from_timestamp(v.get("timestamp").and_then(Value::as_str).unwrap_or(""));
    let ts = epoch_from_iso(v.get("timestamp").and_then(Value::as_str).unwrap_or("")).unwrap_or(0);
    let project = project_from_cwd(v.get("cwd").and_then(Value::as_str).unwrap_or(""));
    let session_id = v.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
    let cost = v.get("costUSD").and_then(Value::as_f64).filter(|c| c.is_finite());
    let count = |obj: &Value, name: &str| obj.get(name).and_then(Value::as_u64).unwrap_or(0);

    let input = count(usage, "input_tokens");
    let output = count(usage, "output_tokens");
    let cache_read = count(usage, "cache_read_input_tokens");
    let cache_write = count(usage, "cache_creation_input_tokens");
    // Anthropic reports the 5m/1h split only inside `cache_creation`.
    let cache_write_1h = usage
        .pointer("/cache_creation/ephemeral_1h_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // Anthropic folds thinking into `output_tokens` and does not break it out.
    let reasoning = 0;
    let speed = u8::from(usage.get("speed").and_then(Value::as_str) == Some("fast"));

    let mut out = Vec::new();
    if input + output + cache_read + cache_write > 0 {
        out.push(RawUse {
            provider: "claude",
            model: model.to_string(),
            day: day.clone(),
            project: project.clone(),
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
            reasoning,
            cost,
            session_id: session_id.clone(),
            dedupe: dedupe.clone(),
            ts,
            speed,
        });
    }

    if let Some(iterations) = usage.get("iterations").and_then(Value::as_array) {
        for (index, iteration) in iterations.iter().enumerate() {
            if iteration.get("type").and_then(Value::as_str) != Some("advisor_message") {
                continue;
            }
            let advisor_model = iteration.get("model").and_then(Value::as_str).unwrap_or("");
            if advisor_model.is_empty() || advisor_model == "<synthetic>" {
                continue;
            }
            let ai = count(iteration, "input_tokens");
            let ao = count(iteration, "output_tokens");
            let ar = count(iteration, "cache_read_input_tokens");
            let aw = count(iteration, "cache_creation_input_tokens");
            if ai + ao + ar + aw == 0 {
                continue;
            }
            let a1h = iteration
                .pointer("/cache_creation/ephemeral_1h_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            out.push(RawUse {
                provider: "claude",
                model: advisor_model.to_string(),
                day: day.clone(),
                project: project.clone(),
                input: ai,
                output: ao,
                cache_read: ar,
                cache_write: aw,
                cache_write_1h: a1h,
                reasoning: 0,
                cost: None, // advisor calls are priced at their own model
                session_id: session_id.clone(),
                dedupe: dedupe.as_ref().map(|d| format!("{d}:advisor:{index}")),
                ts,
                speed,
            });
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

// ---------------------------------------------------------------------- Codex
//
// A port of t3code's `codexUsage.ts` scan state machine. Codex writes per-call usage in
// `token_count` events (`last_token_usage`, with the running `total_token_usage`) and, in
// newer builds, `token_usage_record` lines keyed by response id. The cumulative total is
// used to drop replays, forked-session copies and malformed events, so each billed call
// is counted once. Older sessions are compressed to `.jsonl.zst`.

/// input, cached input, cache write, output, reasoning, total.
type Tok = [u64; 6];

fn tok_valid(t: &Tok) -> bool {
    t[1] + t[2] <= t[0] && t[4] <= t[3] && t[5] >= t[0] + t[3]
}

fn read_tok(v: &Value) -> Option<Tok> {
    let o = v.as_object()?;
    let input = o.get("input_tokens")?.as_u64()?;
    let output = o.get("output_tokens")?.as_u64()?;
    let get = |k: &str, dflt: u64| match o.get(k) {
        None => Some(dflt),
        Some(x) => x.as_u64(),
    };
    let t = [
        input,
        get("cached_input_tokens", 0)?,
        get("cache_write_input_tokens", 0)?,
        output,
        get("reasoning_output_tokens", 0)?,
        get("total_tokens", input + output)?,
    ];
    tok_valid(&t).then_some(t)
}

fn tok_add(prev: Option<Tok>, last: &Tok) -> Option<Tok> {
    let p = prev.unwrap_or_default();
    let t: Tok = std::array::from_fn(|i| p[i] + last[i]);
    tok_valid(&t).then_some(t)
}

fn tok_sub(a: &Tok, b: &Tok) -> Option<Tok> {
    let mut t = [0; 6];
    for i in 0..6 {
        t[i] = a[i].checked_sub(b[i])?;
    }
    tok_valid(&t).then_some(t)
}

#[derive(Clone)]
struct CodexReq {
    ts: i64,
    model: String,
    speed: u8,
    usage: Tok,
}

#[derive(Default)]
struct CodexScan {
    model: String,
    speed: u8,
    session: String,
    cwd: String,
    last_sig: Option<Tok>,
    last_cum: Option<Tok>,
    seq_start: Option<(i64, Tok)>,
    latest_cum_at: Option<i64>,
    pending: Option<Vec<CodexReq>>,
    ledger: bool,
    saw_meta: bool,
    suppress_forks: bool,
    fork_anchor: i64,
}

impl CodexScan {
    fn date(&mut self, ts: i64) {
        self.latest_cum_at = Some(self.latest_cum_at.unwrap_or(ts).max(ts));
    }

    fn rebase(&mut self, cum: Tok, ts: i64) {
        self.last_cum = Some(cum);
        self.date(ts);
        self.pending = None;
        if cum[0] + cum[3] == 0 {
            self.seq_start = None;
        }
    }

    /// Forked sessions open by replaying the parent's events within a second of each other.
    fn fork_copy(&mut self, ts: i64) -> bool {
        if !self.suppress_forks {
            return false;
        }
        if ts - self.fork_anchor < 1000 {
            self.fork_anchor = ts;
            return true;
        }
        self.suppress_forks = false;
        self.last_sig = None;
        false
    }

    fn req(&self, ts: i64, usage: Tok) -> CodexReq {
        CodexReq { ts, model: self.model.clone(), speed: self.speed, usage }
    }

    /// One parsed line; returns the billed calls it completes (with a dedupe key or not).
    fn line(&mut self, v: &Value) -> Vec<(CodexReq, Option<String>)> {
        let Some(payload) = v.get("payload").filter(|p| p.is_object()) else { return Vec::new() };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(ms_from_iso);
        if let Some(c) = payload.get("cwd").and_then(Value::as_str)
            && (kind == "session_meta" || kind == "turn_context") {
                self.cwd = c.to_string();
            }
        if kind == "session_meta" {
            if self.saw_meta {
                return Vec::new();
            }
            self.saw_meta = true;
            if let Some(id) = payload.get("id").or_else(|| payload.get("session_id")).and_then(Value::as_str) {
                self.session = id.to_string();
            }
            let forked = payload.get("forked_from_id").is_some_and(Value::is_string)
                || payload.pointer("/source/subagent/thread_spawn/parent_thread_id").is_some_and(Value::is_string);
            if let (Some(ts), true) = (ts, forked) {
                self.suppress_forks = true;
                self.fork_anchor = ts;
            }
            return Vec::new();
        }
        if kind == "turn_context" {
            self.ledger = false;
            if let Some(m) = payload.get("model").and_then(Value::as_str) {
                self.model = m.to_string();
            }
            return Vec::new();
        }
        if kind == "token_usage_record" {
            let (Some(ts), Some(usage)) = (ts, payload.get("usage").and_then(read_tok)) else { return Vec::new() };
            if self.model.is_empty() || self.fork_copy(ts) {
                return Vec::new();
            }
            if let Some(rid) = payload.get("response_id").and_then(Value::as_str).filter(|r| !r.is_empty()) {
                let pending = self.pending.take().unwrap_or_default();
                self.ledger = true;
                for r in &pending {
                    self.last_cum = tok_add(self.last_cum, &r.usage);
                }
                self.last_cum = tok_add(self.last_cum, &usage);
                self.date(ts);
                let key = format!("codex:{}:{rid}", self.session);
                let mut out: Vec<_> = pending.into_iter().map(|r| (r, None)).collect();
                out.push((self.req(ts, usage), Some(key)));
                return out;
            }
            self.ledger = false;
            let r = self.req(ts, usage);
            self.pending.get_or_insert_with(Vec::new).push(r);
            return Vec::new();
        }
        if ptype == "thread_settings_applied" {
            if let Some(tier) = payload.pointer("/thread_settings/service_tier") {
                self.speed = match tier.as_str() {
                    Some("priority" | "fast") => 1,
                    Some("ultrafast") => 2,
                    _ => 0,
                };
            }
            return Vec::new();
        }
        if ptype != "token_count" {
            return Vec::new();
        }
        let Some(info) = payload.get("info").filter(|i| i.is_object()) else { return Vec::new() };
        let Some(ts) = ts else { return Vec::new() };
        if self.model.is_empty() {
            return Vec::new();
        }
        let last = info.get("last_token_usage").and_then(read_tok);
        let has_cum = info.get("total_token_usage").is_some_and(|c| !c.is_null());
        let cum = if has_cum { info.get("total_token_usage").and_then(read_tok) } else { None };
        if self.fork_copy(ts) {
            if let Some(c) = cum {
                self.rebase(c, ts);
            } else if let (false, Some(l)) = (has_cum, last)
                && Some(l) != self.last_sig {
                    self.last_cum = tok_add(self.last_cum, &l);
                    self.date(ts);
                }
            self.last_sig = last;
            return Vec::new();
        }
        if self.ledger {
            if let Some(c) = cum {
                self.rebase(c, ts);
            }
            return Vec::new();
        }
        let previous = self.last_cum;
        let latest_at = self.latest_cum_at;
        let pending = self.pending.clone();
        if let Some(c) = cum {
            self.rebase(c, ts);
        }
        let Some(last) = last else { return Vec::new() };
        if has_cum && cum.is_none() {
            return Vec::new();
        }
        if last[0] + last[3] == 0 {
            self.last_sig = None;
            return Vec::new();
        }
        if let (Some(c), Some(p)) = (cum, previous)
            && c == p {
                return Vec::new();
            }
        if last[5] != last[0] + last[3] {
            return Vec::new();
        }
        let mut unreported: Vec<CodexReq> = Vec::new();
        if let Some(c) = cum {
            let restarted = c == last;
            if restarted && self.seq_start == Some((ts, last)) {
                self.last_cum = previous;
                if latest_at.is_none() {
                    self.latest_cum_at = None;
                }
                return Vec::new();
            }
            let lowered = previous.is_some_and(|p| (0..6).any(|i| c[i] < p[i]));
            let rewound = lowered && latest_at.is_some_and(|at| ts > at);
            let reconciled = match previous {
                Some(p) if !rewound => (0..6).all(|i| c[i] as i128 - p[i] as i128 == last[i] as i128),
                _ => tok_sub(&c, &last).is_some(),
            };
            if !restarted && !reconciled {
                match bridge(previous, &c, &last, pending.as_deref()) {
                    Some(b) => unreported = b,
                    None => {
                        let replayed = latest_at.is_some_and(|at| ts < at || (ts == at && lowered && c[0] + c[3] > 0));
                        if replayed {
                            self.last_cum = previous;
                            if pending.is_some() {
                                self.pending = pending;
                            }
                        }
                        return Vec::new();
                    }
                }
            }
            if restarted {
                self.seq_start = Some((ts, last));
            }
        } else {
            if Some(last) == self.last_sig {
                return Vec::new();
            }
            let Some(adv) = tok_add(previous, &last) else { return Vec::new() };
            self.last_cum = Some(adv);
            self.date(ts);
        }
        self.last_sig = Some(last);
        let mut out: Vec<_> = unreported.into_iter().map(|r| (r, None)).collect();
        out.push((self.req(ts, last), None));
        out
    }
}

/// Calls logged only as `token_usage_record`s with no response id, recovered when the
/// cumulative jump equals their sum (t3code's `unreportedRequests`).
fn bridge(previous: Option<Tok>, cum: &Tok, last: &Tok, pending: Option<&[CodexReq]>) -> Option<Vec<CodexReq>> {
    let (p, pending) = (previous?, pending?);
    for i in 0..6 {
        let sum: u64 = pending.iter().map(|r| r.usage[i]).sum();
        if cum[i] as i128 - p[i] as i128 != sum as i128 {
            return None;
        }
    }
    let (fin, rest) = pending.split_last()?;
    let remainder = tok_sub(&fin.usage, last)?;
    let mut out: Vec<CodexReq> = rest.to_vec();
    out.push(CodexReq { usage: remainder, ..fin.clone() });
    out.retain(|r| r.usage[0] + r.usage[3] > 0);
    Some(out)
}

/// A session file's lines, decompressing `.jsonl.zst`.
fn open_lines(path: &Path) -> Option<Box<dyn BufRead>> {
    let file = fs::File::open(path).ok()?;
    if path.extension().and_then(|e| e.to_str()) == Some("zst") {
        let dec = ruzstd::decoding::StreamingDecoder::new(BufReader::new(file)).ok()?;
        Some(Box::new(BufReader::new(dec)))
    } else {
        Some(Box::new(BufReader::new(file)))
    }
}

fn parse_codex_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
    let mut files = Vec::new();
    let dir = home.join(".codex").join("sessions");
    walk_ext(&dir, "jsonl", &mut files);
    walk_ext(&dir, "zst", &mut files);
    for file in files {
        let Some(reader) = open_lines(&file) else { continue };
        let mut scan = CodexScan::default();
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if !(line.contains("token_count")
                || line.contains("token_usage_record")
                || line.contains("session_meta")
                || line.contains("turn_context")
                || line.contains("thread_settings_applied"))
            {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            for (r, dedupe) in scan.line(&v) {
                let u = r.usage;
                add(
                    acc,
                    seen,
                    counted,
                    RawUse {
                        provider: "codex",
                        model: r.model,
                        day: local_day_from_epoch(r.ts.div_euclid(1000)),
                        project: project_from_cwd(&scan.cwd),
                        // Codex's `input_tokens` includes the cached and cache-write portions.
                        input: u[0] - u[1] - u[2],
                        output: u[3],
                        cache_read: u[1],
                        cache_write: u[2],
                        cache_write_1h: 0,
                        reasoning: u[4],
                        cost: None,
                        session_id: scan.session.clone(),
                        dedupe,
                        ts: r.ts.div_euclid(1000),
                        speed: r.speed,
                    },
                );
            }
        }
    }
}

// ------------------------------------------------------------------- OpenCode

fn parse_opencode_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
    cutoff_ms: i64,
) {
    let root = home.join(".local").join("share").join("opencode");
    let mut projects: HashMap<String, String> = HashMap::new();

    // Current OpenCode stores history in SQLite. `opencode.db` is read first,
    // then any dated `opencode-*.db` files; ids duplicated across them or with
    // the legacy JSON files are counted once.
    if let Ok(entries) = fs::read_dir(&root) {
        let mut dbs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("opencode") && n.ends_with(".db"))
            })
            .collect();
        dbs.sort_by(|a, b| {
            let main = |p: &Path| p.file_name().and_then(|n| n.to_str()) == Some("opencode.db");
            main(b).cmp(&main(a)).then_with(|| a.cmp(b))
        });
        for db in &dbs {
            read_opencode_database(db, cutoff_ms, &mut projects, acc, seen, counted);
        }
    }

    let storage = root.join("storage");
    // sessionID -> project directory, from legacy storage/session/**/*.json.
    let mut files = Vec::new();
    walk_ext(&storage.join("session"), "json", &mut files);
    for file in &files {
        if let Some(v) = read_json(file)
            && let (Some(id), Some(dir)) = (
                v.get("id").and_then(Value::as_str),
                v.get("directory")
                    .or_else(|| v.get("cwd"))
                    .and_then(Value::as_str),
            ) {
                projects.entry(id.to_string()).or_insert_with(|| dir.to_string());
            }
    }
    files.clear();
    walk_ext(&storage.join("message"), "json", &mut files);
    for file in &files {
        let Some(v) = read_json(file) else { continue };
        // Legacy message files are named by message id; newer records carry it inline.
        let id = file
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| v.get("id").and_then(Value::as_str).map(str::to_string));
        if let Some(raw) = opencode_record(&v, id.as_deref(), None, None, &projects) {
            add(acc, seen, counted, raw);
        }
    }
}

/// Parse one OpenCode assistant message. `fallback_id`/`fallback_session`/
/// `fallback_created` carry values held in SQL columns rather than in `data`.
fn opencode_record(
    v: &Value,
    fallback_id: Option<&str>,
    fallback_session: Option<&str>,
    fallback_created: Option<i64>,
    projects: &HashMap<String, String>,
) -> Option<RawUse> {
    if v.get("role").is_some_and(|r| r.as_str() != Some("assistant")) {
        return None;
    }
    let tokens = v.get("tokens")?;
    let count = |obj: &Value, key: &str| obj.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = count(tokens, "input");
    let cache_read = tokens.pointer("/cache/read").and_then(Value::as_u64).unwrap_or(0);
    let cache_write = tokens.pointer("/cache/write").and_then(Value::as_u64).unwrap_or(0);
    // OpenCode keeps reasoning separate from `output`; t3code adds it in.
    let reasoning = count(tokens, "reasoning");
    let output = count(tokens, "output") + reasoning;
    if input + output + cache_read + cache_write == 0 {
        return None;
    }
    let model = opencode_model(v);
    if model.is_empty() {
        return None;
    }
    let provider_id = v
        .pointer("/model/providerID")
        .or_else(|| v.get("providerID"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let secs = match v.pointer("/time/created") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|ms| ms as i64)).map(|ms| ms / 1000),
        Some(Value::String(s)) => epoch_from_iso(s),
        _ => None,
    }
    .or_else(|| fallback_created.map(|ms| ms / 1000))?;
    let day = local_day_from_epoch(secs);
    let session_id = v
        .get("sessionID")
        .and_then(Value::as_str)
        .or(fallback_session)
        .unwrap_or("")
        .to_string();
    // OpenCode records zero for unknown rates and paid subscriptions alike, so
    // only a positive cost (or one of its own gateways' free models) is a report.
    let cost = match v.get("cost").and_then(Value::as_f64) {
        Some(c) if c > 0.0 => Some(c),
        Some(0.0)
            if matches!(provider_id, "opencode" | "opencode-go")
                && model.ends_with("-free") =>
        {
            Some(0.0)
        }
        _ => None,
    };
    let project = projects
        .get(&session_id)
        .map(|d| project_from_cwd(d))
        .unwrap_or_default();
    let id = fallback_id
        .filter(|s| !s.is_empty())
        .or_else(|| v.get("id").and_then(Value::as_str));
    Some(RawUse {
        provider: "opencode",
        model,
        day,
        project,
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: 0,
        reasoning,
        cost,
        session_id,
        dedupe: id.map(|id| format!("opencode:{id}")),
        ts: secs,
        speed: 0,
    })
}

/// Project only the fields pricing needs, in SQLite, so the often-huge message
/// bodies never cross the FFI boundary (the same projection t3code uses).
const OPENCODE_PROJECTION: &str = "json_object(\
    'tokens', data->'$.tokens', 'cost', data->'$.cost', \
    'model', data->'$.model', 'modelID', data->'$.modelID', \
    'providerID', data->'$.providerID', 'time', data->'$.time', \
    'id', data->'$.id', 'sessionID', data->'$.sessionID')";

fn read_opencode_database(
    path: &Path,
    cutoff_ms: i64,
    projects: &mut HashMap<String, String>,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
    // Open read-only; WAL commits stay visible through the -shm file. If a fresh
    // database has no -shm yet, a read-write handle with query_only recovers it
    // without ever modifying a row.
    let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
            Ok(c) => {
                let _ = c.pragma_update(None, "query_only", true);
                c
            }
            Err(_) => return,
        },
    };
    let _ = conn.busy_timeout(Duration::from_millis(250));

    for table in ["session", "session_v2"] {
        let sql = format!("SELECT id, directory FROM {table}");
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        }) else {
            continue;
        };
        for row in rows.flatten() {
            if !row.0.is_empty() && !row.1.is_empty() {
                projects.entry(row.0).or_insert(row.1);
            }
        }
    }

    let queries = [
        format!(
            "SELECT id, session_id, time_created, {OPENCODE_PROJECTION} FROM message \
             WHERE time_created >= ?1"
        ),
        format!(
            "SELECT id, session_id, time_created, {OPENCODE_PROJECTION} FROM session_message \
             WHERE type IN ('assistant','compaction') AND time_created >= ?1"
        ),
    ];
    for sql in &queries {
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let Ok(rows) = stmt.query_map([cutoff_ms], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ))
        }) else {
            continue;
        };
        for row in rows.flatten() {
            let (id, session_id, created, data) = row;
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            if let Some(raw) =
                opencode_record(&v, Some(&id), Some(&session_id), created, projects)
            {
                add(acc, seen, counted, raw);
            }
        }
    }
}

/// Newer OpenCode logs nest the model under `model: { id, providerID, ... }`;
/// older ones use a top-level `modelID` or `model` string.
fn opencode_model(v: &Value) -> String {
    for pointer in ["/model/id", "/model/modelID"] {
        if let Some(m) = v.pointer(pointer).and_then(Value::as_str)
            && !m.is_empty() {
                return m.to_string();
            }
    }
    for key in ["modelID", "model"] {
        if let Some(m) = v.get(key).and_then(Value::as_str)
            && !m.is_empty() {
                return m.to_string();
            }
    }
    String::new()
}

// ------------------------------------------------------------------------- Pi

fn parse_pi_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
    let mut files = Vec::new();
    walk_ext(&home.join(".pi").join("agent").join("sessions"), "jsonl", &mut files);
    for file in files {
        let Ok(reader) = fs::File::open(&file).map(BufReader::new) else { continue };
        let mut session_id = String::new();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if !line.contains("\"usage\"") && !line.contains("\"session\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v.get("type").and_then(Value::as_str).unwrap_or("") {
                "session" => {
                    session_id = v.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                }
                "message" | "usage" | "compaction" | "branch_summary" => {
                    let (usage, provider, model, response_model) = if v.get("type").and_then(Value::as_str) == Some("message") {
                        let m = &v["message"];
                        (
                            m.get("usage"),
                            m.get("provider").and_then(Value::as_str).unwrap_or(""),
                            m.get("model").and_then(Value::as_str).unwrap_or(""),
                            m.get("responseModel").and_then(Value::as_str).unwrap_or(""),
                        )
                    } else {
                        (
                            v.get("usage"),
                            v.get("provider").and_then(Value::as_str).unwrap_or(""),
                            v.get("model").and_then(Value::as_str).unwrap_or(""),
                            "",
                        )
                    };
                    let Some(usage) = usage else { continue };
                    let count = |key: &str| {
                        usage.get(key)
                            .and_then(Value::as_u64)
                            .or_else(|| usage.get(key).and_then(Value::as_i64).map(|n| n.max(0) as u64))
                            .unwrap_or(0)
                    };
                    let input = count("input");
                    let cache_read = count("cacheRead");
                    let cache_write = count("cacheWrite");
                    let output = count("output");
                    let reasoning = count("reasoning").min(output);
                    let cost = usage
                        .pointer("/cost/total")
                        .and_then(Value::as_f64)
                        .filter(|c| c.is_finite() && *c > 0.0);
                    if input + output + cache_read + cache_write == 0 && cost.is_none() {
                        continue;
                    }
                    let model = if provider.is_empty() {
                        "Tools/summaries".to_string()
                    } else {
                        format!("{provider}/{}", if response_model.is_empty() { model } else { response_model })
                    };
                    let id = v.get("id").and_then(Value::as_str).unwrap_or("");
                    let ts = v.get("timestamp").and_then(Value::as_str).unwrap_or("");
                    add(
                        acc,
                        seen,
                        counted,
                        RawUse {
                            provider: "pi",
                            model,
                            day: day_from_timestamp(ts),
                            ts: epoch_from_iso(ts).unwrap_or(0),
                            project: project_from_cwd(
                                v.get("cwd").and_then(Value::as_str).unwrap_or(""),
                            ),
                            input,
                            output,
                            cache_read,
                            cache_write,
                            cache_write_1h: 0,
                            reasoning,
                            cost,
                            session_id: session_id.clone(),
                            dedupe: Some(format!("pi:{id}:{ts}")),
                            speed: 0,
                        },
                    );
                }
                _ => {}
            }
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

// ----------------------------------------------------------------------- Grok
//
// t3code's `provider-grok/src/server/usage.ts`: `~/.grok/sessions/**/updates.jsonl`,
// `turn_completed` updates with per-model usage and a cost in 1e-10 USD ticks.

const GROK_TICKS_PER_USD: f64 = 10_000_000_000.0;

fn token_count(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0).map_or(0, |n| n.trunc() as u64)
}

/// (uncached input, cached, cache write, output, reasoning, cost ticks)
fn grok_totals(v: &Value) -> Option<([u64; 5], Option<f64>)> {
    let o = v.as_object()?;
    let input = token_count(o.get("inputTokens"));
    let cached = token_count(o.get("cachedReadTokens"));
    let write = token_count(o.get("cacheCreationTokens"));
    let output = token_count(o.get("outputTokens"));
    let reasoning = token_count(o.get("reasoningTokens")).min(output);
    let ticks = o.get("costUsdTicks").and_then(Value::as_f64).filter(|t| t.is_finite());
    Some(([input.saturating_sub(cached + write), cached, write, output, reasoning], ticks))
}

fn grok_usd(ticks: Option<f64>) -> Option<f64> {
    ticks.filter(|t| *t >= 0.0).map(|t| t / GROK_TICKS_PER_USD)
}

fn parse_grok_value(v: &Value) -> Vec<(String, [u64; 5], Option<f64>, Option<String>, i64, String)> {
    let Some(params) = v.get("params").filter(|p| p.is_object()) else { return Vec::new() };
    let Some(update) = params.get("update").filter(|u| u.is_object()) else { return Vec::new() };
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("turn_completed") {
        return Vec::new();
    }
    let Some(usage) = update.get("usage").filter(|u| u.is_object()) else { return Vec::new() };
    let session = params.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
    let prompt = update.get("prompt_id").and_then(Value::as_str);
    let ts = params
        .pointer("/_meta/agentTimestampMs")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite())
        .or_else(|| v.get("timestamp").and_then(Value::as_f64).filter(|t| t.is_finite()).map(|t| if t > 1e12 { t } else { t * 1000.0 }));
    let Some(ts) = ts else { return Vec::new() };
    let ts = ts as i64;
    let Some((top, top_ticks)) = grok_totals(usage) else { return Vec::new() };
    let key = |model: &str| prompt.map(|p| format!("grok:{session}:{p}:{ts}:{model}"));
    let total = |t: &[u64; 5]| t[0] + t[1] + t[2] + t[3];
    let entries: Vec<(String, [u64; 5], Option<f64>)> = usage
        .get("modelUsage")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| !k.is_empty())
                .filter_map(|(k, raw)| grok_totals(raw).map(|(t, c)| (k.clone(), t, c)))
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        if total(&top) == 0 {
            return Vec::new();
        }
        return vec![("grok".into(), top, grok_usd(top_ticks), key("grok"), ts, session)];
    }
    let top_usd = grok_usd(top_ticks);
    let (mut ticked, mut unticked) = (0.0, 0u64);
    for (_, t, c) in &entries {
        if total(t) == 0 {
            continue;
        }
        match c {
            Some(_) => ticked += grok_usd(*c).unwrap_or(0.0),
            None => unticked += total(t),
        }
    }
    let remaining = top_usd.map(|u| (u - ticked).max(0.0));
    entries
        .into_iter()
        .filter(|(_, t, _)| total(t) > 0)
        .map(|(model, t, c)| {
            let mut cost = grok_usd(c);
            if let (None, Some(r), true) = (cost, remaining, unticked > 0) {
                cost = Some(r * total(&t) as f64 / unticked as f64);
            }
            let k = key(&model);
            (model, t, cost, k, ts, session.clone())
        })
        .collect()
}

fn parse_grok_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
    let mut files = Vec::new();
    walk_ext(&home.join(".grok").join("sessions"), "jsonl", &mut files);
    files.retain(|f| f.file_name().and_then(|n| n.to_str()) == Some("updates.jsonl"));
    for file in files {
        let Ok(reader) = fs::File::open(&file).map(BufReader::new) else { continue };
        for line in reader.lines().map_while(Result::ok) {
            if !line.contains("\"turn_completed\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            for (model, t, cost, dedupe, ts, session) in parse_grok_value(&v) {
                add(
                    acc,
                    seen,
                    counted,
                    RawUse {
                        provider: "grok",
                        model,
                        day: local_day_from_epoch(ts.div_euclid(1000)),
                        ts: ts.div_euclid(1000),
                        project: String::new(),
                        input: t[0],
                        output: t[3],
                        cache_read: t[1],
                        cache_write: t[2],
                        cache_write_1h: 0,
                        reasoning: t[4],
                        cost,
                        session_id: session,
                        dedupe,
                        speed: 0,
                    },
                );
            }
        }
    }
}

// ----------------------------------------------------------------- cost table

/// USD per million tokens. `write_5m`/`write_1h` are the cache-creation rates;
/// when a model has no distinct 1-hour rate it equals `write_5m`.
///
/// `fast`/`ultra` scale every rate for priority service tiers (LiteLLM's
/// `provider_specific_entry.fast` or `*_priority` / `*_ultrafast` rates).
///
/// The shipped t3code build bills every cache write at the single
/// `cache_creation_input_token_cost` and has no long-context tiers, so
/// `rates_at` does the same and `write_1h` is kept only as table data.
#[derive(Clone, Copy)]
struct Price {
    input: f64,
    output: f64,
    read: f64,
    write_5m: f64,
    write_1h: f64,
    fast: f64,
    ultra: f64,
}

const fn p(input: f64, output: f64, read: f64, write_5m: f64, write_1h: f64) -> Price {
    Price { input, output, read, write_5m, write_1h, fast: 1.0, ultra: 1.0 }
}

impl Price {
    const fn fast(mut self, k: f64, ultra: f64) -> Price {
        self.fast = k;
        self.ultra = ultra;
        self
    }
}

/// OpenAI lists one cache-write rate.
const fn gpt(input: f64, output: f64, read: f64, write: f64) -> Price {
    p(input, output, read, write, write)
}

/// Matched case-insensitively by substring, most specific first; unknown
/// models are unpriced rather than free. These are the first-party list rates
/// from LiteLLM, the source t3code parses for its price table.
const PRICES: &[(&str, Price)] = &[
    // Claude 5 family
    ("claude-opus-5-5", p(4.0, 20.0, 0.2, 5.0, 8.0).fast(2.0, 2.0)),
    ("claude-sonnet-5-5", p(2.0, 10.0, 0.1, 2.5, 4.0)),
    ("claude-haiku-5-5", p(0.1, 0.5, 0.01, 0.125, 0.2)),
    ("claude-fable-5-2", p(8.0, 40.0, 0.2, 10.0, 16.0).fast(2.0, 2.0)),
    ("claude-opus-5", p(5.0, 25.0, 0.5, 6.25, 10.0).fast(2.0, 2.0)),
    ("claude-sonnet-5", p(2.0, 10.0, 0.2, 2.5, 4.0)),
    // GPT-6 family (OpenAI has no distinct 1-hour cache-write rate)
    ("gpt-5.6-luna", gpt(0.2, 1.2, 0.02, 0.25).fast(2.0, 2.0)),
    ("gpt-6-astra-law", p(10.0, 50.0, 1.0, 10.0, 10.0)),
    ("gpt-6-rosalind", gpt(15.0, 75.0, 1.5, 18.75)),
    ("gpt-6-cyber", gpt(20.0, 100.0, 2.0, 25.0)),
    ("gpt-6-astra", gpt(10.0, 50.0, 1.0, 12.5).fast(2.0, 6.0)),
    ("gpt-6-luna", gpt(0.1, 0.5, 0.01, 0.125).fast(2.0, 2.0)),
    ("gpt-6.1-terra", gpt(1.0, 5.0, 0.05, 1.25).fast(2.0, 2.0)),
    ("gpt-6.1-sol", gpt(2.0, 10.0, 0.1, 2.5).fast(2.0, 6.0)),
    ("gpt-6-sol", gpt(2.0, 10.0, 0.2, 2.5).fast(2.0, 2.0)),
    // Older Claude (Anthropic list prices)
    ("claude-haiku-4", p(1.0, 5.0, 0.1, 1.25, 2.0)),
    ("claude-haiku", p(0.8, 4.0, 0.08, 1.0, 1.6)),
    ("claude-opus", p(15.0, 75.0, 1.5, 18.75, 30.0)),
    ("claude-sonnet", p(3.0, 15.0, 0.3, 3.75, 6.0)),
    // Older OpenAI
    ("gpt-5-nano", p(0.05, 0.4, 0.005, 0.05, 0.05)),
    ("gpt-5-mini", p(0.25, 2.0, 0.025, 0.25, 0.25).fast(1.8, 1.8)),
    ("gpt-5", p(1.25, 10.0, 0.125, 1.25, 1.25).fast(2.0, 2.0)),
    ("gpt-4.1-mini", p(0.4, 1.6, 0.1, 0.4, 0.4)),
    ("gpt-4.1", p(2.0, 8.0, 0.5, 2.0, 2.0)),
    ("gpt-4o-mini", p(0.15, 0.6, 0.075, 0.15, 0.15)),
    ("gpt-4o", p(2.5, 10.0, 1.25, 2.5, 2.5)),
    ("o4-mini", p(1.1, 4.4, 0.275, 1.1, 1.1)),
    ("o3-mini", p(1.1, 4.4, 0.275, 1.1, 1.1)),
    ("o3", p(2.0, 8.0, 0.5, 2.0, 2.0)),
    // Google
    ("gemini-3-pro", p(2.0, 12.0, 0.2, 2.0, 2.0)),
    ("gemini-2.5-pro", p(1.25, 10.0, 0.31, 1.25, 1.25)),
    ("gemini-2.5-flash", p(0.3, 2.5, 0.075, 0.3, 0.3)),
    ("gemini-2.0-flash", p(0.1, 0.4, 0.025, 0.1, 0.1)),
    ("gemini-1.5-pro", p(1.25, 5.0, 0.3125, 1.25, 1.25)),
    ("gemini-1.5-flash", p(0.075, 0.3, 0.01875, 0.075, 0.075)),
];

/// Bare family names are generation-ambiguous or synthetic placeholders and
/// stay unpriced, as in t3code's `UNPRICEABLE_MODELS`.
fn unpriceable(model: &str) -> bool {
    let bare = model
        .trim()
        .to_ascii_lowercase()
        .split('[')
        .next()
        .unwrap_or(model)
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_string();
    matches!(
        bare.as_str(),
        "" | "unknown" | "<synthetic>" | "synthetic" | "opus" | "sonnet" | "haiku" | "fable"
    )
}

fn price_for(model: &str) -> Option<Price> {
    if unpriceable(model) {
        return None;
    }
    let m = model.to_ascii_lowercase();
    PRICES.iter().find_map(|(needle, rates)| m.contains(needle).then_some(*rates))
}

/// The rates one record pays at its speed tier.
fn rates_at(r: Price, speed: u8) -> Price {
    let k = match speed {
        1 => r.fast,
        2 => r.ultra,
        _ => 1.0,
    };
    Price {
        input: r.input * k,
        output: r.output * k,
        read: r.read * k,
        write_5m: r.write_5m * k,
        write_1h: r.write_5m * k,
        ..r
    }
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

/// Epoch milliseconds for an ISO-8601 timestamp, keeping up to three fraction digits.
fn ms_from_iso(ts: &str) -> Option<i64> {
    let secs = epoch_from_iso(ts)?;
    let frac = match ts.as_bytes().get(19) {
        Some(b'.') => {
            let digits: String = ts[20..].chars().take_while(char::is_ascii_digit).take(3).collect();
            format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
        }
        _ => 0,
    };
    Some(secs * 1000 + frac)
}

/// Epoch seconds for an ISO-8601 timestamp; the limits page reads reset times with it.
pub(crate) fn epoch_secs(ts: &str) -> Option<i64> {
    epoch_from_iso(ts)
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
    let (y, m, d, _, _) = local_parts(secs)?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Local (year, month, day, hour, minute) of epoch seconds; UTC off Windows.
pub(crate) fn local_parts(secs: i64) -> Option<(i64, u32, u32, u32, u32)> {
    #[cfg(not(windows))]
    {
        let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
        let r = secs.rem_euclid(86_400);
        return Some((y, m, d, (r / 3600) as u32, (r % 3600 / 60) as u32));
    }
    #[cfg(windows)]
    local_parts_windows(secs)
}

#[cfg(windows)]
fn local_parts_windows(secs: i64) -> Option<(i64, u32, u32, u32, u32)> {
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
    Some((local.wYear as i64, local.wMonth as u32, local.wDay as u32, local.wHour as u32, local.wMinute as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_assistant_line() {
        let line = r#"{"parentUuid":"x","type":"assistant","timestamp":"2026-11-16T16:11:00.206Z","cwd":"C:\\Users\\me\\my-app","requestId":"req_1","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":200,"cache_read_input_tokens":300,"cache_creation_input_tokens":40}}}"#;
        let v: Value = serde_json::from_str(line).unwrap();
        let raws = parse_claude_value(&v).expect("should parse");
        assert_eq!(raws.len(), 1);
        let raw = &raws[0];
        assert_eq!(raw.model, "claude-sonnet-4-5");
        assert_eq!((raw.input, raw.output), (10, 200));
        assert_eq!((raw.cache_read, raw.cache_write), (300, 40));
        assert_eq!(raw.project, "my-app");
        assert_eq!(raw.reasoning, 0);
        // No logged cost: priced from the table (10*3 + 200*15 + 300*0.3 + 40*3.75)/1e6.
        assert!(raw.cost.is_none());
        let cost = list_cost(&raw.model, raw.input, raw.output, raw.cache_read, raw.cache_write, 0);
        assert!((cost - 0.00327).abs() < 1e-9);
        assert_eq!(raw.day.len(), 10);
        assert_eq!(&raw.day[4..5], "-");
    }

    #[test]
    fn claude_1h_cache_writes_and_advisor_iteration() {
        let line = r#"{"type":"assistant","timestamp":"2026-11-16T16:11:00Z","cwd":"C:\\x\\app","requestId":"r","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":40,"ephemeral_1h_input_tokens":60},"iterations":[{"type":"advisor_message","model":"claude-haiku-5-5","input_tokens":5,"output_tokens":7,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}]}}}"#;
        let v: Value = serde_json::from_str(line).unwrap();
        let raws = parse_claude_value(&v).unwrap();
        assert_eq!(raws.len(), 2);
        assert_eq!(raws[0].cache_write_1h, 60);
        assert_eq!(raws[0].dedupe.as_deref(), Some("claude:m:r"));
        assert_eq!(raws[1].model, "claude-haiku-5-5");
        assert_eq!(raws[1].cost, None);
        assert_eq!(raws[1].dedupe.as_deref(), Some("claude:m:r:advisor:0"));
        // 5m at $5/M and 1h at $8/M, output at $20/M.
        let cost = 10.0 / 1e6 * 4.0 + 20.0 / 1e6 * 20.0 + 40.0 / 1e6 * 5.0 + 60.0 / 1e6 * 8.0;
        let computed = list_cost(&raws[0].model, raws[0].input, raws[0].output, 0, 40, 60);
        assert!((computed - cost).abs() < 1e-12);
    }

    #[test]
    fn dedupe_keeps_max_of_each_channel() {
        let mut acc = HashMap::new();
        let mut seen = HashMap::new();
        let mk = |i: u64, o: u64| RawUse {
            provider: "claude",
            model: "claude-opus-5-5".into(),
            day: "2026-11-16".into(),
            project: "app".into(),
            input: i,
            output: o,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 0,
            cost: None,
            session_id: "s1".into(),
            dedupe: Some("claude:m:r".into()),
            ts: 0,
            speed: 0,
        };
        let mut counted = HashSet::new();
        add(&mut acc, &mut seen, &mut counted, mk(10, 20));
        add(&mut acc, &mut seen, &mut counted, mk(5, 40)); // partial copy, higher output
        for raw in seen.into_values() {
            accumulate(&mut acc, raw, &mut counted);
        }
        let row = &acc[&Key { provider: "claude", model: "claude-opus-5-5".into(), day: "2026-11-16".into(), project: "app".into(), hour: -1 }];
        assert_eq!((row.input, row.output), (10, 40));
        assert!(row.cost > 0.0);
    }

    #[test]
    fn new_models_are_priced_and_unknowns_are_not() {
        for m in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
            assert!(price_for(m).is_some(), "{m} should be priced");
        }
        for m in ["some-future-model", "opus", "<synthetic>", "unknown"] {
            assert!(price_for(m).is_none(), "{m} should be unpriced");
        }
    }

    fn list_cost(model: &str, i: u64, o: u64, r: u64, w5: u64, w1: u64) -> f64 {
        let p = price_for(model).unwrap();
        (i as f64 * p.input + o as f64 * p.output + r as f64 * p.read + w5 as f64 * p.write_5m + w1 as f64 * p.write_1h) / 1e6
    }

    fn codex_lines(lines: &[&str]) -> Vec<(CodexReq, Option<String>)> {
        let mut scan = CodexScan::default();
        lines.iter().flat_map(|l| scan.line(&serde_json::from_str::<Value>(l).unwrap())).collect()
    }

    const META: &str = r#"{"timestamp":"2026-11-13T06:21:30.163Z","type":"session_meta","payload":{"id":"s1","cwd":"C:\\x\\app"}}"#;
    const TURN: &str = r#"{"timestamp":"2026-11-13T06:21:31.000Z","type":"turn_context","payload":{"model":"gpt-6-sol","turn_id":"t1"}}"#;

    fn count(ts: &str, total: [u64; 2], last: [u64; 2]) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{},"output_tokens":{}}},"last_token_usage":{{"input_tokens":{},"output_tokens":{}}}}}}}}}"#,
            total[0], total[1], last[0], last[1]
        )
    }

    #[test]
    fn codex_counts_each_call_once() {
        let a = count("2026-11-13T06:22:00.000Z", [100, 10], [100, 10]);
        let again = count("2026-11-13T06:22:01.000Z", [100, 10], [100, 10]); // repeat: total unchanged
        let b = count("2026-11-13T06:23:00.000Z", [150, 30], [50, 20]);
        let recs = codex_lines(&[META, TURN, &a, &again, &b]);
        let got: Vec<[u64; 2]> = recs.iter().map(|(r, _)| [r.usage[0], r.usage[3]]).collect();
        assert_eq!(got, vec![[100, 10], [50, 20]]);
        assert_eq!(recs[0].0.model, "gpt-6-sol");
    }

    #[test]
    fn codex_forked_session_skips_the_replayed_parent() {
        let meta = r#"{"timestamp":"2026-11-13T06:21:30.000Z","type":"session_meta","payload":{"id":"s2","forked_from_id":"s1"}}"#;
        let turn = r#"{"timestamp":"2026-11-13T06:21:30.100Z","type":"turn_context","payload":{"model":"gpt-6-sol"}}"#;
        let copy = count("2026-11-13T06:21:30.200Z", [100, 10], [100, 10]);
        let own = count("2026-11-13T06:25:00.000Z", [160, 15], [60, 5]);
        let recs = codex_lines(&[meta, turn, &copy, &own]);
        assert_eq!(recs.len(), 1);
        assert_eq!([recs[0].0.usage[0], recs[0].0.usage[3]], [60, 5]);
    }

    #[test]
    fn fast_tier_costs_more() {
        let r = price_for("gpt-6-sol").unwrap();
        assert_eq!(rates_at(r, 0).input, 2.0);
        assert_eq!(rates_at(r, 1).output, 20.0);
        assert_eq!(rates_at(price_for("claude-sonnet-5-5").unwrap(), 1).output, 10.0);
        assert_eq!(ms_from_iso("2026-11-13T06:21:30.163Z").unwrap() % 1000, 163);
    }

    #[test]
    fn grok_splits_the_turn_cost_across_models() {
        let v: Value = serde_json::from_str(r#"{"params":{"sessionId":"s","_meta":{"agentTimestampMs":1794865335210},"update":{"sessionUpdate":"turn_completed","prompt_id":"p","usage":{"inputTokens":100,"outputTokens":10,"costUsdTicks":20000000000,"modelUsage":{"grok-4":{"inputTokens":60,"cachedReadTokens":20,"outputTokens":5},"grok-mini":{"inputTokens":40,"outputTokens":5}}}}}}"#).unwrap();
        let recs = parse_grok_value(&v);
        assert_eq!(recs.len(), 2);
        let cost: f64 = recs.iter().map(|r| r.2.unwrap()).sum();
        assert!((cost - 2.0).abs() < 1e-9);
        assert_eq!(recs.iter().find(|r| r.0 == "grok-4").unwrap().1, [40, 20, 0, 5, 0]);
    }

    #[test]
    fn opencode_nested_model_and_reasoning() {
        let v: Value = serde_json::from_str(
            r#"{"role":"assistant","id":"msg_x","model":{"id":"claude-sonnet-5-5","providerID":"anthropic"},"time":{"created":1763323260206},"tokens":{"input":100,"output":50,"reasoning":10,"cache":{"read":20,"write":5}},"cost":0.001}"#,
        )
        .unwrap();
        assert_eq!(opencode_model(&v), "claude-sonnet-5-5");
        let tokens = v.get("tokens").unwrap();
        let reasoning = tokens.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
        let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0) + reasoning;
        assert_eq!(output, 60);
    }

    /// Prints the same per-provider 90-day figures t3code's Usage page shows.
    /// Run with: cargo test -- --ignored --nocapture print_90_day_usage
    #[test]
    #[ignore]
    fn print_90_day_usage() {
        let rows = load();
        let today = local_day_from_epoch(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
        );
        let cutoff = {
            let (y, m, d) = (
                today[0..4].parse::<i64>().unwrap(),
                today[5..7].parse::<u32>().unwrap(),
                today[8..10].parse::<u32>().unwrap(),
            );
            let (y, m, d) = civil_from_days(days_from_civil(y, m, d) - 89);
            format!("{y:04}-{m:02}-{d:02}")
        };
        let rows: Vec<&Usage> = rows
            .iter()
            .filter(|u| u.day >= cutoff && u.day <= today)
            .collect();
        for provider in ["opencode", "claude", "codex", "pi", "grok"] {
            let rs: Vec<&&Usage> = rows.iter().filter(|u| u.provider == provider).collect();
            if rs.is_empty() { continue; }
            let sum = |f: fn(&Usage) -> u64| rs.iter().map(|u| f(u)).sum::<u64>();
            let cost: f64 = rs.iter().map(|u| u.cost_usd).sum();
            println!(
                "{provider:8} sessions={:5} tokens={:>13} cost=${:.2}",
                rs.iter().map(|u| u.sessions as u64).sum::<u64>(),
                sum(|u| u.input + u.output + u.cache_read + u.cache_write),
                cost,
            );
        }
        println!(
            "TOTALS cached_read={} uncached_input={} output={} cache_write={}",
            sum_all(&rows, |u| u.cache_read),
            sum_all(&rows, |u| u.input),
            sum_all(&rows, |u| u.output),
            sum_all(&rows, |u| u.cache_write),
        );
        println!(
            "COST input=${:.2} cache_read=${:.2} cache_write=${:.2} output=${:.2} other=${:.2}",
            rows.iter().map(|u| u.cost_input).sum::<f64>(),
            rows.iter().map(|u| u.cost_cache_read).sum::<f64>(),
            rows.iter().map(|u| u.cost_cache_write).sum::<f64>(),
            rows.iter().map(|u| u.cost_output).sum::<f64>(),
            rows.iter().map(|u| u.cost_other).sum::<f64>(),
        );
    }

    fn sum_all(rows: &[&Usage], f: fn(&Usage) -> u64) -> u64 {
        rows.iter().map(|u| f(u)).sum()
    }
}
