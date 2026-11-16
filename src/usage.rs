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
}

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
    let one_hour = raw.cache_write_1h.min(raw.cache_write);
    let list = rate.map(|r| {
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
            });
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

// ---------------------------------------------------------------------- Codex

fn parse_codex_logs(
    home: &Path,
    acc: &mut HashMap<Key, Acc>,
    seen: &mut HashMap<String, RawUse>,
    counted: &mut HashSet<String>,
) {
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
        let mut session_id = String::new();
        let mut prev_total: Option<[u64; 5]> = None;
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
                    if session_id.is_empty() {
                        session_id = v
                            .pointer("/payload/id")
                            .or_else(|| v.pointer("/payload/session_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
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
                                seen,
                                counted,
                                RawUse {
                                    provider: "codex",
                                    model: if model.is_empty() { "unknown".into() } else { model.clone() },
                                    day: day_from_timestamp(
                                        v.get("timestamp").and_then(Value::as_str).unwrap_or(""),
                                    ),
                                    project: project_from_cwd(&cwd),
                                    // Codex's `input_tokens` is inclusive of the cached and
                                    // cache-write portions; bill those only once.
                                    input: toks[0].saturating_sub(toks[2]).saturating_sub(toks[3]),
                                    output: toks[1],
                                    cache_read: toks[2],
                                    cache_write: toks[3],
                                    cache_write_1h: 0,
                                    // Reported inside `output_tokens`, never charged twice.
                                    reasoning: toks[4],
                                    cost: None, // Codex logs carry no cost
                                    session_id: session_id.clone(),
                                    dedupe: None,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Field order: input, output, cache_read (cached_input), cache_write, reasoning.
fn codex_tokens(info: &Value, prev_total: &mut Option<[u64; 5]>) -> Option<[u64; 5]> {
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
            read(obj, "reasoning_output_tokens"),
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
        Some(prev) => std::array::from_fn(|i| toks[i].saturating_sub(prev[i])),
        None => toks, // first cumulative event: assume it is all we have seen
    };
    *prev_total = Some(toks);
    Some(delta)
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
        if let Some(v) = read_json(&file) {
            if let (Some(id), Some(dir)) = (
                v.get("id").and_then(Value::as_str),
                v.get("directory")
                    .or_else(|| v.get("cwd"))
                    .and_then(Value::as_str),
            ) {
                projects.entry(id.to_string()).or_insert_with(|| dir.to_string());
            }
        }
    }
    files.clear();
    walk_ext(&storage.join("message"), "json", &mut files);
    for file in &files {
        let Some(v) = read_json(&file) else { continue };
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
    let day = match v.pointer("/time/created") {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|ms| ms as i64))
            .map(|ms| local_day_from_epoch(ms / 1000)),
        Some(Value::String(s)) => Some(day_from_timestamp(s)),
        _ => None,
    }
    .or_else(|| fallback_created.map(|ms| local_day_from_epoch(ms / 1000)))?;
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
        if let Some(m) = v.pointer(pointer).and_then(Value::as_str) {
            if !m.is_empty() {
                return m.to_string();
            }
        }
    }
    for key in ["modelID", "model"] {
        if let Some(m) = v.get(key).and_then(Value::as_str) {
            if !m.is_empty() {
                return m.to_string();
            }
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

// ----------------------------------------------------------------- cost table

/// USD per million tokens. `write_5m`/`write_1h` are the cache-creation rates;
/// when a model has no distinct 1-hour rate it equals `write_5m`.
#[derive(Clone, Copy)]
struct Price {
    input: f64,
    output: f64,
    read: f64,
    write_5m: f64,
    write_1h: f64,
}

const fn p(input: f64, output: f64, read: f64, write_5m: f64, write_1h: f64) -> Price {
    Price { input, output, read, write_5m, write_1h }
}

/// Matched case-insensitively by substring, most specific first; unknown
/// models are unpriced rather than free. These are the first-party list rates
/// from LiteLLM, the source t3code parses for its price table.
const PRICES: &[(&str, Price)] = &[
    // Claude 5 family
    ("claude-opus-5-5", p(4.0, 20.0, 0.2, 5.0, 8.0)),
    ("claude-sonnet-5-5", p(2.0, 10.0, 0.1, 2.5, 4.0)),
    ("claude-haiku-5-5", p(0.1, 0.5, 0.01, 0.125, 0.2)),
    ("claude-fable-5-2", p(8.0, 40.0, 0.2, 10.0, 16.0)),
    ("claude-opus-5", p(5.0, 25.0, 0.5, 6.25, 10.0)),
    ("claude-sonnet-5", p(2.0, 10.0, 0.2, 2.5, 4.0)),
    // GPT-6 family (OpenAI has no distinct 1-hour cache-write rate)
    ("gpt-5.6-luna", p(0.2, 1.2, 0.02, 0.25, 0.25)),
    ("gpt-6-astra-law", p(10.0, 50.0, 1.0, 10.0, 10.0)),
    ("gpt-6-rosalind", p(15.0, 75.0, 1.5, 18.75, 18.75)),
    ("gpt-6-cyber", p(20.0, 100.0, 2.0, 25.0, 25.0)),
    ("gpt-6-astra", p(10.0, 50.0, 1.0, 12.5, 12.5)),
    ("gpt-6-luna", p(0.1, 0.5, 0.01, 0.125, 0.125)),
    ("gpt-6.1-terra", p(1.0, 5.0, 0.05, 1.25, 1.25)),
    ("gpt-6.1-sol", p(2.0, 10.0, 0.1, 2.5, 2.5)),
    ("gpt-6-sol", p(2.0, 10.0, 0.2, 2.5, 2.5)),
    // Older Claude (Anthropic list prices)
    ("claude-haiku-4", p(1.0, 5.0, 0.1, 1.25, 1.25)),
    ("claude-haiku", p(0.8, 4.0, 0.08, 1.0, 1.0)),
    ("claude-opus", p(15.0, 75.0, 1.5, 18.75, 18.75)),
    ("claude-sonnet", p(3.0, 15.0, 0.3, 3.75, 3.75)),
    // Older OpenAI
    ("gpt-5-nano", p(0.05, 0.4, 0.005, 0.05, 0.05)),
    ("gpt-5-mini", p(0.25, 2.0, 0.025, 0.25, 0.25)),
    ("gpt-5", p(1.25, 10.0, 0.125, 1.25, 1.25)),
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

/// Whether the built-in table has a list price for this model.
pub fn has_price(model: &str) -> bool {
    price_for(model).is_some()
}

/// Estimated cost in USD for the given usage at list prices, 0.0 if unknown.
/// Reasoning is already inside `output` and is deliberately not charged again.
fn price(
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
) -> f64 {
    match price_for(model) {
        Some(r) => (input as f64 * r.input
            + output as f64 * r.output
            + cache_read as f64 * r.read
            + cache_write_5m as f64 * r.write_5m
            + cache_write_1h as f64 * r.write_1h)
            / 1_000_000.0,
        None => 0.0,
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
        let cost = price(&raw.model, raw.input, raw.output, raw.cache_read, raw.cache_write, 0);
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
        let computed = price(&raws[0].model, raws[0].input, raws[0].output, 0, 40, 60);
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
        };
        let mut counted = HashSet::new();
        add(&mut acc, &mut seen, &mut counted, mk(10, 20));
        add(&mut acc, &mut seen, &mut counted, mk(5, 40)); // partial copy, higher output
        for raw in seen.into_values() {
            accumulate(&mut acc, raw, &mut counted);
        }
        let row = &acc[&Key { provider: "claude", model: "claude-opus-5-5".into(), day: "2026-11-16".into(), project: "app".into() }];
        assert_eq!((row.input, row.output), (10, 40));
        assert!(row.cost > 0.0);
    }

    #[test]
    fn new_models_are_priced_and_unknowns_are_not() {
        for m in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"] {
            assert!(has_price(m), "{m} should be priced");
        }
        for m in ["some-future-model", "opus", "<synthetic>", "unknown"] {
            assert!(!has_price(m), "{m} should be unpriced");
        }
    }

    #[test]
    fn parses_codex_last_token_usage() {
        let mut state: Option<[u64; 5]> = None;
        let info: Value = serde_json::from_str(
            r#"{"total_token_usage":{"input_tokens":21491,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":47,"reasoning_output_tokens":10,"total_tokens":21538},"last_token_usage":{"input_tokens":21491,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":47,"reasoning_output_tokens":10,"total_tokens":21538}}"#,
        )
        .unwrap();
        let first = codex_tokens(&info, &mut state).unwrap();
        assert_eq!(first, [21491, 47, 0, 0, 10]);
        // Next event: cumulative grows, last_token_usage covers only the new call.
        let info2: Value = serde_json::from_str(
            r#"{"total_token_usage":{"input_tokens":24991,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":347},"last_token_usage":{"input_tokens":3500,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":300}}"#,
        )
        .unwrap();
        let second = codex_tokens(&info2, &mut state).unwrap();
        assert_eq!(second, [3500, 300, 0, 0, 0]);
    }

    #[test]
    fn codex_totals_delta_when_last_missing() {
        let mut state: Option<[u64; 5]> = None;
        let mk = |i: u64, o: u64| {
            serde_json::from_str::<Value>(&format!(
                r#"{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":{o}}}}}"#
            ))
            .unwrap()
        };
        assert_eq!(codex_tokens(&mk(100, 20), &mut state).unwrap(), [100, 20, 0, 0, 0]);
        assert_eq!(codex_tokens(&mk(150, 70), &mut state).unwrap(), [50, 50, 0, 0, 0]);
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
        for provider in ["opencode", "claude", "codex", "pi"] {
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
