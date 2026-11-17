//! Antigravity usage, a port of t3code's `antigravityUsageReader.ts` and `AntigravityUsage.ts`
//! (MIT License, Copyright (c) 2026 T3 Tools Inc.).
//!
//! Antigravity keeps each conversation in a SQLite file under
//! `~/.gemini/{antigravity,antigravity-cli,antigravity-ide,antigravity-backup}/conversations`
//! (or `~/.config/antigravity`). Usage sits in protobuf blobs in the `gen_metadata` and
//! `steps` tables; the same call can appear in both and in several files, so candidates
//! sharing a response id are merged (union-find, the maximum of each token channel).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

#[derive(Clone)]
enum Fv {
    Num(u64),
    Bytes(Vec<u8>),
}

type Fields = HashMap<u32, Vec<Fv>>;

fn fields(b: &[u8]) -> Option<Fields> {
    let mut off = 0usize;
    let mut out: Fields = HashMap::new();
    let varint = |off: &mut usize| -> Option<u64> {
        let mut v: u64 = 0;
        let mut shift = 0u32;
        while shift < 70 {
            let byte = *b.get(*off)?;
            *off += 1;
            if shift == 63 && byte > 1 {
                return None;
            }
            v |= ((byte & 127) as u64) << shift;
            if byte < 128 {
                return Some(v);
            }
            shift += 7;
        }
        None
    };
    while off < b.len() {
        let tag = varint(&mut off)?;
        let (num, wire) = ((tag / 8) as u32, tag % 8);
        if num == 0 {
            return None;
        }
        let v = match wire {
            0 => Fv::Num(varint(&mut off)?),
            1 | 2 | 5 => {
                let len = if wire == 2 { varint(&mut off)? as usize } else if wire == 1 { 8 } else { 4 };
                if len > b.len() - off {
                    return None;
                }
                let bytes = b[off..off + len].to_vec();
                off += len;
                if wire != 2 {
                    continue;
                }
                Fv::Bytes(bytes)
            }
            _ => return None,
        };
        out.entry(num).or_default().push(v);
    }
    Some(out)
}

fn num(f: &Fields, k: u32) -> u64 {
    match f.get(&k).and_then(|v| v.first()) {
        Some(Fv::Num(n)) if *n <= (1u64 << 53) => *n,
        _ => 0,
    }
}

fn bytes(f: &Fields, k: u32) -> Option<&[u8]> {
    match f.get(&k).and_then(|v| v.first()) {
        Some(Fv::Bytes(b)) => Some(b),
        _ => None,
    }
}

fn nested(f: &Fields, k: u32) -> Option<Fields> {
    match bytes(f, k) {
        None => Some(HashMap::new()),
        Some(b) => fields(b),
    }
}

fn text(f: &Fields, k: u32) -> Option<String> {
    match bytes(f, k) {
        None => Some(String::new()),
        Some(b) => std::str::from_utf8(b).ok().map(|s| s.trim().to_string()),
    }
}

fn timestamp(f: &Fields) -> Option<i64> {
    let s = num(f, 1);
    (s > 0).then(|| s as i64 * 1000 + (num(f, 2) / 1_000_000) as i64)
}

fn model_id_name(id: u64) -> Option<&'static str> {
    Some(match id {
        246 => "gemini-2.5-pro",
        312 => "gemini-2.5-flash",
        313 | 329 => "gemini-2.5-flash-thinking",
        330 => "gemini-2.5-flash-lite",
        281 | 282 => "claude-sonnet-4",
        290 | 291 => "claude-opus-4",
        333 | 334 => "claude-sonnet-4-5",
        340 | 341 => "claude-haiku-4-5",
        342 => "gpt-oss-120b-medium",
        1026 => "claude-opus-4-6",
        1035 => "claude-sonnet-4-6",
        1016 | 1036 | 1037 => "gemini-3.1-pro",
        1018 | 1084 | 1047 => "gemini-3-flash-preview",
        1020 => "gemini-3.5-flash-medium",
        1021 => "gemini-3.1-flash-image",
        1050 => "gemini-3.1-flash-lite",
        1071 => "gemini-3.6-flash-high",
        1072 => "gemini-3.6-flash-medium",
        1073 => "gemini-3.6-flash-low",
        1132 | 1133 => "gemini-3.5-flash-high",
        1187 => "gemini-3.5-flash-extra-low",
        1196 | 1266 => "gemini-3.6-flash",
        1298 => "gemini-3.7-flash-high",
        1299 => "gemini-3.7-flash-medium",
        1300 => "gemini-3.7-flash-low",
        1301 => "gemini-3.7-flash-tiered",
        1318 => "gemini-3.8-flash-high",
        1319 => "gemini-3.8-flash-medium",
        1320 => "gemini-3.8-flash-low",
        1322 => "gemini-3.8-flash-tiered",
        1402 => "claude-opus-5-5",
        1417..=1419 => "gemini-4-argon",
        _ => return None,
    })
}

/// t3code's `modelName`: "Claude 4.5 Sonnet (Thinking)" → "claude-sonnet-4-5-thinking".
fn model_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let mut n = name.to_lowercase();
    if n.ends_with(')')
        && let Some(open) = n.rfind('(')
    {
        let inner = n[open + 1..n.len() - 1].to_string();
        n = format!("{}-{inner}", n[..open].trim_end());
    }
    let n: String = n.split_whitespace().collect::<Vec<_>>().join("-");
    if let Some(rest) = n.strip_prefix("claude-") {
        // claude-<version>-<family>… → claude-<family>-<version>…, dots to dashes.
        let mut parts = rest.splitn(3, '-');
        let (v, fam, tail) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next());
        let swapped = if v.chars().next().is_some_and(|c| c.is_ascii_digit()) && matches!(fam, "sonnet" | "opus" | "haiku") {
            match tail {
                Some(t) => format!("claude-{fam}-{v}-{t}"),
                None => format!("claude-{fam}-{v}"),
            }
        } else {
            n.clone()
        };
        return swapped.replace('.', "-");
    }
    n
}

fn is_agent_flash(m: &str) -> bool {
    matches!(m, "gemini-3-flash-a" | "gemini-3-flash-b" | "gemini-3-flash-agent" | "gemini-3-flash-agent-a" | "gemini-3-flash-agent-b")
}

fn is_concrete(m: &str) -> bool {
    let gem = m.strip_prefix("gemini-").is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_digit()));
    let cl = ["claude-sonnet-", "claude-opus-", "claude-haiku-"]
        .iter()
        .any(|p| m.strip_prefix(p).is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_digit())));
    (gem || cl || m.starts_with("gpt-")) && !is_agent_flash(m)
}

/// The model whose list price applies (t3code's `antigravityRateModel`).
pub fn rate_model(m: &str) -> String {
    if is_agent_flash(m) {
        return "gemini-3.5-flash".into();
    }
    if m == "gemini-3.7-flash-safety-le" {
        return "gemini-3.7-flash".into();
    }
    let mut base = m.to_string();
    for suf in ["-extra-low", "-low", "-medium", "-high", "-tiered", "-control", "-thinking", "-agent"] {
        if let Some(b) = m.strip_suffix(suf) {
            base = b.to_string();
            break;
        }
    }
    if base == "gemini-3-flash" { "gemini-3-flash-preview".into() } else { base }
}

struct Meta {
    model: String,
    model_id: u64,
    ts: Option<i64>,
    usages: Vec<Fields>,
}

fn metadata(b: &[u8], step: bool) -> Option<Meta> {
    let root = fields(b)?;
    if !step && bytes(&root, 1).is_none() {
        return None;
    }
    let data = if step { root } else { nested(&root, 1)? };
    let model = if step { nested(&data, 24)? } else { data.clone() };
    let primary = match bytes(&data, if step { 9 } else { 4 }) {
        Some(u) => Some(fields(u)?),
        None => None,
    };
    let mut usages: Vec<Fields> = primary.iter().cloned().collect();
    for retry in data.get(&(if step { 28 } else { 17 })).map(Vec::as_slice).unwrap_or(&[]) {
        let Fv::Bytes(r) = retry else { return None };
        if let Some(u) = bytes(&fields(r)?, 2) {
            usages.push(fields(u)?);
        }
    }
    let names = [model_name(&text(&model, if step { 12 } else { 19 })?), model_name(&text(&model, if step { 8 } else { 21 })?)];
    let chosen = names
        .iter()
        .find(|n| is_concrete(n))
        .or_else(|| names.iter().find(|n| !n.is_empty() && !n.starts_with("model_") && !n.starts_with("model-")))
        .cloned()
        .unwrap_or_default();
    let model_id = match num(&model, if step { 1 } else { 3 }) {
        0 => primary.as_ref().map_or(0, |p| num(p, 1)),
        id => id,
    };
    let ts = if step {
        timestamp(&nested(&data, 8)?).or(timestamp(&nested(&data, 1)?))
    } else {
        timestamp(&nested(&nested(&data, 9)?, 4)?)
    };
    Some(Meta { model: chosen, model_id, ts, usages })
}

/// One billed call, before the cross-file merge.
#[derive(Clone)]
pub struct Rec {
    pub session: String,
    pub ts_ms: i64,
    pub model: String,
    /// uncached input, cached, cache write, output, reasoning
    pub tokens: [u64; 5],
    pub dedupe: String,
    model_id: u64,
    model_quality: u8,
    keys: Vec<String>,
    ts_quality: u8,
}

fn remember(models: &mut HashMap<u64, Option<String>>, id: u64, model: Option<String>) {
    let prev = models.get(&id).cloned();
    models.insert(id, match prev {
        None => model,
        Some(p) if p == model => model,
        Some(_) => None,
    });
}

fn read_metadata(db: &Connection, sql: &str, step: bool) -> Option<Vec<(i64, Meta)>> {
    let mut st = db.prepare(sql).ok()?;
    let rows = st.query_map([], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).ok()?;
    let mut out = Vec::new();
    for row in rows {
        let (idx, data) = row.ok()?;
        let Some(data) = data else {
            if step {
                continue;
            }
            return None;
        };
        out.push((idx?, metadata(&data, step)?));
    }
    out.sort_by_key(|e| e.0);
    Some(out)
}

/// Model names learned per model id; `None` when two files disagree.
type Learned = HashMap<u64, Option<String>>;

fn read_database(path: &Path, fallback_ms: i64) -> Option<(Vec<Rec>, Learned)> {
    let session = path.file_stem()?.to_string_lossy().to_string();
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(100));
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .ok()?
        .query_map([], |r| r.get(0))
        .ok()?
        .filter_map(Result::ok)
        .collect();
    let has = |t: &str| tables.iter().any(|x| x == t);
    if !has("gen_metadata") && !has("steps") {
        return None;
    }
    let gens = if has("gen_metadata") { read_metadata(&db, "SELECT idx, data FROM gen_metadata ORDER BY rowid", false)? } else { Vec::new() };
    let mut traj: Option<i64> = None;
    if has("trajectory_metadata_blob") {
        let mut st = db.prepare("SELECT data FROM trajectory_metadata_blob").ok()?;
        for b in st.query_map([], |r| r.get::<_, Vec<u8>>(0)).ok()?.filter_map(Result::ok) {
            if traj.is_none() {
                traj = timestamp(&nested(&fields(&b)?, 2)?);
            }
        }
    }
    let steps = if has("steps") { read_metadata(&db, "SELECT idx, metadata FROM steps ORDER BY rowid", true)? } else { Vec::new() };
    let mut models = HashMap::new();
    for (_, m) in gens.iter().chain(&steps) {
        if m.model_id > 0 && is_concrete(&m.model) {
            remember(&mut models, m.model_id, Some(m.model.clone()));
        }
    }
    let gen_by_idx: HashMap<i64, &Meta> = gens.iter().map(|(i, m)| (*i, m)).collect();
    let mut recs = Vec::new();
    for (source, entries) in [("step", &steps), ("generation", &gens)] {
        for (index, (idx, entry)) in entries.iter().enumerate() {
            for (ui, usage) in entry.usages.iter().enumerate() {
                let model_id = match num(usage, 1) {
                    0 => entry.model_id,
                    id => id,
                };
                let matches = |m: &Meta| model_id == 0 || m.model_id == model_id;
                let generation = if source == "step" { gen_by_idx.get(idx).copied() } else { None };
                let model = if matches(entry) && !entry.model.is_empty() {
                    entry.model.clone()
                } else if let Some(g) = generation.filter(|g| matches(g)) {
                    g.model.clone()
                } else {
                    String::new()
                };
                let output = num(usage, 3).max(num(usage, 9) + num(usage, 10));
                let t = [num(usage, 2), num(usage, 5), num(usage, 4), output, output.min(num(usage, 9))];
                if t[0] + t[1] + t[2] + t[3] == 0 {
                    continue;
                }
                let keys: Vec<String> =
                    [11, 12, 7].iter().filter_map(|k| text(usage, *k).filter(|s| !s.is_empty()).map(|id| format!("antigravity:{k}:{id}"))).collect();
                let dedupe = keys.first().cloned().unwrap_or_else(|| format!("antigravity:{session}:{source}:{index}:{ui}"));
                recs.push(Rec {
                    session: session.clone(),
                    ts_ms: entry.ts.or(traj).unwrap_or(fallback_ms),
                    model_quality: if is_concrete(&model) { 4 } else if model.is_empty() { 0 } else { 1 },
                    model,
                    tokens: t,
                    dedupe,
                    model_id,
                    keys,
                    ts_quality: if entry.ts.is_some() { 2 } else if traj.is_some() { 1 } else { 0 },
                });
            }
        }
    }
    Some((recs, models))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    names.sort();
    for p in names {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("db") {
            out.push(p);
        }
    }
}

/// Every Antigravity call since `since_ms`, merged across files.
pub fn read(home: &Path, since_ms: i64) -> Vec<Rec> {
    let roots: Vec<PathBuf> = match std::env::var("ANTIGRAVITY_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty()).collect(),
        _ => ["antigravity", "antigravity-cli", "antigravity-ide", "antigravity-backup"]
            .iter()
            .map(|n| home.join(".gemini").join(n))
            .chain([home.join(".config").join("antigravity")])
            .collect(),
    };
    let mut files = Vec::new();
    let mut seen_dirs = Vec::new();
    for root in roots {
        let nested = root.join("conversations");
        let dir = if nested.exists() { nested } else { root };
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !seen_dirs.contains(&dir) {
            walk(&dir, &mut files);
            seen_dirs.push(dir);
        }
    }
    let mut models: HashMap<u64, Option<String>> = HashMap::new();
    let mut pending = Vec::new();
    for f in &files {
        let mtime = std::fs::metadata(f)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        if let Some((recs, m)) = read_database(f, mtime) {
            for (id, name) in m {
                remember(&mut models, id, name);
            }
            pending.push(recs);
        }
    }
    // Union-find over shared response ids.
    let mut groups: Vec<(Rec, usize, usize, usize)> = Vec::new(); // rec, parent, size, owner
    let mut ids: HashMap<String, usize> = HashMap::new();
    fn find(g: &mut [(Rec, usize, usize, usize)], mut i: usize) -> usize {
        let mut root = i;
        while g[root].1 != root {
            root = g[root].1;
        }
        while i != root {
            let p = g[i].1;
            g[i].1 = root;
            i = p;
        }
        root
    }
    for recs in pending {
        for mut c in recs {
            let learned = models.get(&c.model_id).cloned();
            let fallback = if matches!(learned, Some(None)) { None } else { model_id_name(c.model_id).map(str::to_string) };
            let learned = learned.flatten();
            let model = if c.model_quality == 4 { Some(c.model.clone()) } else { None }
                .or(learned.clone())
                .or(fallback.clone())
                .or((!c.model.is_empty()).then(|| c.model.clone()))
                .unwrap_or_else(|| if c.model_id > 0 { format!("antigravity-model-{}", c.model_id) } else { "antigravity-unknown".into() });
            c.model_quality = if c.model_quality == 4 { 4 } else if learned.is_some() { 3 } else if fallback.is_some() { 2 } else { c.model_quality };
            c.model = model;
            let index = groups.len();
            let keys = c.keys.clone();
            groups.push((c, index, 1, index));
            for k in keys {
                if let Some(&existing) = ids.get(&k) {
                    let (mut a, mut b) = (find(&mut groups, index), find(&mut groups, existing));
                    if a != b {
                        if groups[a].2 < groups[b].2 {
                            std::mem::swap(&mut a, &mut b);
                        }
                        let (t, s) = (groups[a].clone(), groups[b].clone());
                        let first = if t.3 < s.3 { &t } else { &s };
                        let best_model = if s.0.model_quality == t.0.model_quality { first } else if s.0.model_quality > t.0.model_quality { &s } else { &t };
                        let best_time = if s.0.ts_quality > t.0.ts_quality || (s.0.ts_quality == t.0.ts_quality && s.0.ts_ms < t.0.ts_ms) { &s } else { &t };
                        let mut rec = first.0.clone();
                        rec.model = best_model.0.model.clone();
                        rec.model_quality = best_model.0.model_quality;
                        rec.ts_ms = best_time.0.ts_ms;
                        rec.ts_quality = best_time.0.ts_quality;
                        rec.tokens = std::array::from_fn(|i| t.0.tokens[i].max(s.0.tokens[i]));
                        let owner = first.3;
                        groups[a].0 = rec;
                        groups[a].3 = owner;
                        groups[a].2 += s.2;
                        groups[b].1 = a;
                    }
                }
                ids.insert(k, index);
            }
        }
    }
    groups.iter().enumerate().filter(|(i, g)| g.1 == *i && g.0.ts_ms >= since_ms).map(|(_, g)| g.0.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_rate_models() {
        assert_eq!(model_name("Claude 4.5 Sonnet (Thinking)"), "claude-sonnet-4-5-thinking");
        assert_eq!(model_name("Gemini 3.1 Pro (High)"), "gemini-3.1-pro-high");
        assert_eq!(rate_model("gemini-3.1-pro-high"), "gemini-3.1-pro");
        assert_eq!(rate_model("gemini-3-flash-agent"), "gemini-3.5-flash");
        assert!(is_concrete("gemini-3.1-pro") && !is_concrete("gemini-3-flash-a"));
        // field 1 varint 150, field 2 bytes "hi"
        let f = fields(&[0x08, 0x96, 0x01, 0x12, 0x02, b'h', b'i']).unwrap();
        assert_eq!(num(&f, 1), 150);
        assert_eq!(text(&f, 2).unwrap(), "hi");
    }
}
