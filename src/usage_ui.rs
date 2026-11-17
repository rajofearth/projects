//! The usage page, a port of t3code's Usage view (MIT License, Copyright (c) 2026 T3 Tools
//! Inc.): a breadcrumb with a provider filter, a range and a Cost / Tokens / Limits switch.
//! Cost and Tokens show a hero total with its providers, a smooth daily area chart with one
//! hover tooltip, the token totals, the split by type and a breakdown by model or day. Limits
//! shows each provider's plan windows. Logs and limits are read on worker threads.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use rune::{canvas, col, el, icon, row, Align, Color, El, Icon, LineCap, ScrollSpec, Shape, Side, Tx, Waker};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Menu, MenuItem, SegmentedControl, Spinner};

use crate::fmt::{count, usd};
use crate::icons;
use crate::limits::{self, Limits};
use crate::logos;
use crate::ui::{data, label, rgb, Ink, R_CTRL, S1, S2, S3, S4, S5, S6, S8};
use crate::usage::{self, Usage};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    Quarter,
}

impl Range {
    const ALL: [Range; 4] = [Range::Day, Range::Week, Range::Month, Range::Quarter];
    fn days(self) -> i64 {
        match self {
            Range::Day => 1,
            Range::Week => 7,
            Range::Month => 30,
            Range::Quarter => 90,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Range::Day => "Past 24h",
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::Quarter => "90 days",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cost,
    Tokens,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Cost,
    Tokens,
    Limits,
}

/// The providers the page reads, in t3code's menu order.
const PROVIDERS: [&str; 8] = ["codex", "claude", "grok", "cursor", "opencode", "antigravity", "pi", "muse"];

pub struct UsageState {
    pub rows: Vec<Usage>,
    pub loading: bool,
    pub loaded: bool,
    pub range: Range,
    pub tab: Tab,
    /// The breakdown lists days instead of models.
    pub by_day: bool,
    /// The chart column under the pointer.
    pub hover: Option<usize>,
    /// Providers switched off in the breadcrumb menu.
    pub hidden: Vec<&'static str>,
    pub menu: bool,
    pub limits: Vec<Limits>,
    pub limits_loading: bool,
    pub limits_loaded: bool,
    /// The limit bar under the pointer: (provider, window index).
    pub limit_hover: Option<(&'static str, usize)>,
    /// The demo scene reads the fixture instead of the logs.
    pub demo: bool,
    rx: Option<Receiver<Vec<Usage>>>,
    /// Bumped whenever `rows` changes, so the cached figures know to rebuild.
    generation: u64,
    /// Everything the Cost/Tokens view derives from the rows, built once per range, metric,
    /// filter and data change. Hovering the chart only moves the crosshair and tooltip.
    cache: RefCell<Option<Rc<Figures>>>,
    limits_rx: Option<Receiver<Vec<Limits>>>,
}

impl UsageState {
    pub fn new() -> Self {
        UsageState {
            rows: Vec::new(),
            loading: false,
            loaded: false,
            range: Range::Month,
            tab: Tab::Cost,
            by_day: false,
            hover: None,
            hidden: Vec::new(),
            menu: false,
            limits: Vec::new(),
            limits_loading: false,
            limits_loaded: false,
            limit_hover: None,
            demo: false,
            rx: None,
            generation: 0,
            cache: RefCell::new(None),
            limits_rx: None,
        }
    }

    fn metric(&self) -> Metric {
        if self.tab == Tab::Tokens { Metric::Tokens } else { Metric::Cost }
    }

    fn shown(&self, provider: &str) -> bool {
        !self.hidden.contains(&provider)
    }

    /// Reads the logs off the UI thread; the waker brings the answer back.
    pub fn start(&mut self, waker: Option<Waker>) {
        if self.loading {
            return;
        }
        if self.demo {
            self.rows = crate::fixture::demo_usage(today());
            self.generation += 1;
            self.loaded = true;
            return;
        }
        self.loading = true;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let waker: Waker = waker.unwrap_or_else(|| Arc::new(|| ()));
        std::thread::spawn(move || {
            let _ = tx.send(usage::load());
            waker();
        });
    }

    /// Reads plan limits off the UI thread (a log read and one HTTPS request).
    pub fn start_limits(&mut self, waker: Option<Waker>) {
        if self.limits_loading {
            return;
        }
        if self.demo {
            self.limits = demo_limits();
            self.limits_loaded = true;
            return;
        }
        self.limits_loading = true;
        let (tx, rx) = channel();
        self.limits_rx = Some(rx);
        let waker: Waker = waker.unwrap_or_else(|| Arc::new(|| ()));
        std::thread::spawn(move || {
            let _ = tx.send(limits::load());
            waker();
        });
    }

    /// True when a load finished this turn.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(rows) => {
                    self.rows = rows;
                    self.generation += 1;
                    self.finish_rows();
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.finish_rows();
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.limits_rx {
            match rx.try_recv() {
                Ok(l) => {
                    self.limits = l;
                    self.finish_limits();
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.finish_limits();
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        changed
    }

    fn finish_rows(&mut self) {
        self.loading = false;
        self.loaded = true;
        self.rx = None;
    }

    fn finish_limits(&mut self) {
        self.limits_loading = false;
        self.limits_loaded = true;
        self.limits_rx = None;
    }
}

fn demo_limits() -> Vec<Limits> {
    let now = limits::now_secs();
    let w = |label: &str, used: f64, left_secs: i64, mins: i64| limits::Window {
        label: label.into(),
        used_pct: used,
        resets_at: Some(now + left_secs),
        minutes: Some(mins),
    };
    vec![
        Limits {
            provider: "codex",
            windows: vec![w("Monthly", 2.0, 7 * 86_400 + 19 * 3600, 43_200)],
            error: None,
            email: Some("you@example.com".into()),
            plan: Some("ChatGPT Free Subscription".into()),
        },
        Limits {
            provider: "claude",
            windows: vec![w("Session", 5.0, 3 * 3600 + 49 * 60, 300), w("Weekly", 65.0, 2 * 86_400 + 5 * 3600, 10_080)],
            error: None,
            email: Some("you@example.com".into()),
            plan: Some("Claude Pro Subscription".into()),
        },
    ]
}

// ---- dates -----------------------------------------------------------------

/// Days since 1970-01-01 for a civil date (Howard Hinnant's days-from-civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) for a day number.
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_day(day: &str) -> Option<i64> {
    let mut it = day.split('-').map(|p| p.parse::<i64>().ok());
    Some(days_from_civil(it.next()??, it.next()??, it.next()??))
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn short_date(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{} {d}", MONTHS[(m - 1).clamp(0, 11) as usize])
}

/// Today's day number in local time (UTC off Windows).
pub fn today() -> i64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut st) };
        days_from_civil(st.wYear as i64, st.wMonth as i64, st.wDay as i64)
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        secs.div_euclid(86_400)
    }
}

// ---- figures ---------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Sum {
    tokens: u64,
    sessions: u64,
    /// Uncached input, cached input (cache reads), cache writes, output (reasoning included).
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    cost: f64,
    c_input: f64,
    c_cache_read: f64,
    c_cache_write: f64,
    c_output: f64,
    c_other: f64,
    /// What the cache reads would have cost at the uncached input rate, less what they cost.
    savings: f64,
    /// Some tokens had no price.
    unpriced: bool,
    provider: &'static str,
}

impl Sum {
    fn add(&mut self, u: &Usage) {
        self.tokens += u.input + u.output + u.cache_read + u.cache_write;
        self.sessions += u.sessions as u64;
        self.input += u.input;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.output += u.output;
        self.cost += u.cost_usd;
        self.c_input += u.cost_input;
        self.c_cache_read += u.cost_cache_read;
        self.c_cache_write += u.cost_cache_write;
        self.c_output += u.cost_output;
        self.c_other += u.cost_other;
        if u.input > 0 && u.cost_input > 0.0 {
            self.savings += (u.cache_read as f64 * u.cost_input / u.input as f64 - u.cost_cache_read).max(0.0);
        }
        self.unpriced |= !u.priced;
        self.provider = u.provider;
    }
    fn value(&self, m: Metric) -> f64 {
        match m {
            Metric::Cost => self.cost,
            Metric::Tokens => self.tokens as f64,
        }
    }
}

fn fmt_value(m: Metric, v: f64) -> String {
    match m {
        Metric::Cost => usd(v),
        Metric::Tokens => count(v.max(0.0) as u64),
    }
}

fn pct(part: f64, whole: f64) -> String {
    if whole <= 0.0 {
        return "0.0%".into();
    }
    let p = part / whole * 100.0;
    if p > 0.0 && p < 0.1 {
        "<0.1%".into()
    } else {
        format!("{p:.1}%")
    }
}

/// Epoch hour now.
fn now_hour() -> i64 {
    limits::now_secs().div_euclid(3600)
}

/// Rows of the shown providers in the range: the last 24 hours by the hour for Past 24h,
/// else from `days` days ago through today.
fn within(u: &UsageState, now: i64, hour: i64, range: Range) -> impl Iterator<Item = &Usage> {
    u.rows.iter().filter(move |r| {
        u.shown(r.provider)
            && match range {
                Range::Day => r.hour > hour - 24 && r.hour <= hour,
                _ => parse_day(&r.day).is_some_and(|d| d <= now && d > now - range.days()),
            }
    })
}

#[derive(Clone, PartialEq)]
struct FigKey {
    generation: u64,
    range: Range,
    metric: Metric,
    hidden: Vec<&'static str>,
    by_day: bool,
    now: i64,
    hour: i64,
}

/// The Cost/Tokens page's figures and chart geometry.
struct Figures {
    key: FigKey,
    total: Sum,
    providers: Vec<(String, Sum)>,
    /// series[p][i]: provider p in column i.
    series: Vec<Vec<f64>>,
    /// Tooltip title per column.
    titles: Vec<String>,
    ticks: [String; 3],
    top: f64,
    areas: Shape,
    lines: Shape,
    groups: Vec<(String, Sum)>,
    more: usize,
    whole: f64,
    best: f64,
}

fn hour_label(h: i64) -> String {
    match usage::local_parts(h * 3600) {
        Some((_, mo, d, hh, _)) => {
            let (h12, ap) = match hh {
                0 => (12, "AM"),
                1..=11 => (hh, "AM"),
                12 => (12, "PM"),
                _ => (hh - 12, "PM"),
            };
            format!("{} {d}, {h12} {ap}", MONTHS[(mo as usize).clamp(1, 12) - 1])
        }
        None => String::new(),
    }
}

fn figures(u: &UsageState) -> Rc<Figures> {
    let key = FigKey {
        generation: u.generation,
        range: u.range,
        metric: u.metric(),
        hidden: u.hidden.clone(),
        by_day: u.by_day,
        now: today(),
        hour: now_hour(),
    };
    if let Some(f) = u.cache.borrow().as_ref().filter(|f| f.key == key) {
        return f.clone();
    }
    let f = Rc::new(build_figures(u, key));
    *u.cache.borrow_mut() = Some(f.clone());
    f
}

fn build_figures(u: &UsageState, key: FigKey) -> Figures {
    let (m, now, hour, range) = (key.metric, key.now, key.hour, key.range);
    let total = sum(within(u, now, hour, range));
    let providers = group(within(u, now, hour, range), m, |r| r.provider.to_string());
    // Columns: 24 hours, or at least a week of days.
    let hourly = range == Range::Day;
    let n = if hourly { 24 } else { range.days().max(7) as usize };
    let first_day = now - n as i64 + 1;
    let first_hour = hour - 23;
    let mut series: Vec<Vec<f64>> = vec![vec![0.0; n]; providers.len()];
    let chart_rows = u.rows.iter().filter(|r| {
        u.shown(r.provider)
            && if hourly { r.hour >= first_hour && r.hour <= hour } else { parse_day(&r.day).is_some_and(|d| d >= first_day && d <= now) }
    });
    for r in chart_rows {
        let Some(p) = providers.iter().position(|(p, _)| p == r.provider) else { continue };
        let col = if hourly { r.hour - first_hour } else { parse_day(&r.day).map_or(-1, |d| d - first_day) };
        if !(0..n as i64).contains(&col) {
            continue;
        }
        series[p][col as usize] += match m {
            Metric::Cost => r.cost_usd,
            Metric::Tokens => (r.input + r.output + r.cache_read + r.cache_write) as f64,
        };
    }
    let titles: Vec<String> =
        (0..n as i64).map(|i| if hourly { hour_label(first_hour + i) } else { short_date(first_day + i) }).collect();
    let tick = |i: usize| if hourly { hour_label(first_hour + i as i64).to_uppercase() } else { short_date(first_day + i as i64).to_uppercase() };
    let ticks = [tick(0), tick(n / 2), tick(n - 1)];
    let top = nice_ceil(series.iter().flatten().copied().fold(0.0, f64::max));
    let (areas, lines) = chart_shapes(&series, &providers, top, n);

    let mut groups = if key.by_day {
        let mut g = group(within(u, now, hour, range), m, |r| r.day.clone());
        g.sort_by(|a, b| b.0.cmp(&a.0));
        g
    } else {
        group(within(u, now, hour, range), m, |r| short_model(&r.model))
    };
    let whole: f64 = groups.iter().map(|g| g.1.value(m)).sum();
    let best = groups.iter().map(|g| g.1.value(m)).fold(0.0, f64::max);
    let more = groups.len().saturating_sub(LIMIT);
    groups.truncate(LIMIT);
    Figures { key, total, providers, series, titles, ticks, top, areas, lines, groups, more, whole, best }
}

/// The chart's two layers: the areas under each provider's line, and the gridlines with the
/// lines themselves. A vertical fade between them makes the areas' gradient, as t3code's
/// SVG gradient does, so the geometry stays one polygon and one stroke per provider: cheap
/// to draw again on every hover.
fn chart_shapes(series: &[Vec<f64>], providers: &[(String, Sum)], top: f64, n: usize) -> (Shape, Shape) {
    let ink = Ink::new();
    let y = |v: f64| (100.0 - v / top * 100.0) as f32;
    let mut areas = Shape::new().view_box(n as f32, 100.0);
    let mut lines = Shape::new().view_box(n as f32, 100.0);
    for g in [0.0, 50.0] {
        lines = lines.line([0.0, g], [n as f32, g], 1.0, LineCap::Butt, ink.line);
    }
    lines = lines.line([0.0, 100.0], [n as f32, 100.0], 1.0, LineCap::Butt, ink.line_strong);
    let steps = if n > 40 { 4 } else { 8 };
    // Biggest provider last, so it draws on top.
    for (p, vals) in series.iter().enumerate().rev() {
        let (_, _, color) = provider(&providers[p].0);
        let ys: Vec<f32> = vals.iter().map(|v| y(*v)).collect();
        let line = smooth(&ys, steps);
        let mut poly = line.clone();
        poly.push([n as f32 - 0.5, 100.0]);
        poly.push([0.5, 100.0]);
        areas = areas.fill(poly, color.alpha(0.3));
        lines = lines.stroke(line, 1.75, color);
    }
    (areas, lines)
}

fn sum<'a>(it: impl Iterator<Item = &'a Usage>) -> Sum {
    it.fold(Sum::default(), |mut s, u| {
        s.add(u);
        s
    })
}

/// Sums grouped by a key, biggest by `m` first (tokens break ties).
fn group<'a>(it: impl Iterator<Item = &'a Usage>, m: Metric, key: impl Fn(&Usage) -> String) -> Vec<(String, Sum)> {
    let mut map: BTreeMap<String, Sum> = BTreeMap::new();
    for u in it {
        map.entry(key(u)).or_default().add(u);
    }
    let mut v: Vec<_> = map.into_iter().filter(|(_, s)| s.tokens > 0).collect();
    v.sort_by(|a, b| b.1.value(m).total_cmp(&a.1.value(m)).then(b.1.tokens.cmp(&a.1.tokens)));
    v
}

/// The smallest 1 / 2 / 2.5 / 5 × 10ⁿ at or above `v`: the chart's top line.
fn nice_ceil(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|f| f * mag).find(|t| *t >= v).unwrap_or(10.0 * mag)
}

/// "claude-opus-4-1-20250805" → "claude-opus-4-1".
pub fn short_model(m: &str) -> String {
    m.split('-').filter(|p| !(p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))).collect::<Vec<_>>().join("-")
}

/// A provider's name, logo and series colour, as t3code's `PROVIDER_PRESENTATION` has them.
fn provider(p: &str) -> (String, &'static Icon, Color) {
    match p {
        "codex" => ("Codex".into(), &logos::CODEX, rgb([236, 236, 238])),
        "claude" => ("Claude Code".into(), &logos::CLAUDE, rgb([217, 119, 87])),
        "grok" => ("Grok Build".into(), &logos::GROK, rgb([175, 175, 180])),
        "cursor" => ("Cursor".into(), &logos::CURSOR, rgb([139, 139, 139])),
        "opencode" => ("OpenCode".into(), &logos::OPENCODE, rgb([91, 155, 189])),
        "antigravity" => ("Antigravity".into(), &logos::ANTIGRAVITY, rgb([140, 123, 209])),
        "pi" => ("Pi".into(), &logos::PI, rgb([77, 159, 138])),
        "muse" => ("Muse Code".into(), &logos::MUSE, rgb([8, 102, 255])),
        other => (other.to_string(), &icons::CODE, rgb([160, 160, 166])),
    }
}

/// Monotone cubic interpolation (Fritsch–Carlson, d3's `curveMonotoneX`) sampled densely:
/// a smooth line through every point that never overshoots, so a zero day stays on the axis.
fn smooth(ys: &[f32], steps: usize) -> Vec<[f32; 2]> {
    let n = ys.len();
    if n < 2 {
        return ys.iter().enumerate().map(|(i, y)| [i as f32 + 0.5, *y]).collect();
    }
    let d: Vec<f32> = ys.windows(2).map(|w| w[1] - w[0]).collect();
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for k in 1..n - 1 {
        m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { (d[k - 1] + d[k]) / 2.0 };
    }
    for k in 0..n - 1 {
        if d[k] == 0.0 {
            m[k] = 0.0;
            m[k + 1] = 0.0;
            continue;
        }
        let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
        let h = a * a + b * b;
        if h > 9.0 {
            let t = 3.0 / h.sqrt();
            m[k] = t * a * d[k];
            m[k + 1] = t * b * d[k];
        }
    }
    let mut out = Vec::with_capacity((n - 1) * steps + 1);
    for k in 0..n - 1 {
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let y = (2.0 * t3 - 3.0 * t2 + 1.0) * ys[k] + (t3 - 2.0 * t2 + t) * m[k] + (-2.0 * t3 + 3.0 * t2) * ys[k + 1] + (t3 - t2) * m[k + 1];
            out.push([k as f32 + 0.5 + t, y]);
        }
    }
    out.push([n as f32 - 0.5, ys[n - 1]]);
    out
}

// ---- view ------------------------------------------------------------------

fn section(title: &str, ink: &Ink) -> El {
    label(title, 14.0, 600.0, ink.fg)
}

/// The hero total and one line per provider.
fn summary(ink: &Ink, m: Metric, total: &Sum, providers: &[(String, Sum)]) -> El {
    let whole = total.value(m);
    let unit = if m == Metric::Cost { "cost" } else { "tokens" };
    let plural = |n: u64| if n == 1 { "" } else { "s" };
    let sessions = format!("{} session{} · API estimate", total.sessions, plural(total.sessions));
    col()
        .key("usage-summary")
        .w(300.0)
        .shrink()
        .gap(S5)
        .child(
            col()
                .gap(S1)
                .child(label(fmt_value(m, whole), 36.0, 700.0, ink.fg))
                .child(label(
                    if m == Metric::Cost && total.unpriced { format!("{sessions} · some models unpriced") } else { sessions },
                    12.0,
                    400.0,
                    ink.muted,
                )),
        )
        .child(col().gap(S5).children(providers.iter().map(|(p, s)| {
            let (name, glyph, color) = provider(p);
            let other = match m {
                Metric::Cost => format!("{} tokens", count(s.tokens)),
                Metric::Tokens => usd(s.cost),
            };
            col()
                .key(format!("prov/{p}"))
                .w_full()
                .gap(3.0)
                .child(
                    row()
                        .w_full()
                        .gap(S2)
                        .items_center()
                        .child(el().size(7.0, 7.0).radius(3.5).fill(color))
                        .child(icon(glyph, 14.0).color(color))
                        .child(label(name, 14.0, 500.0, ink.fg))
                        .child(label(format!("{} session{}", s.sessions, plural(s.sessions)), 11.0, 400.0, ink.faint).truncate().shrink())
                        .child(el().grow(1.0))
                        .child(data(fmt_value(m, s.value(m)), 14.0, ink.fg)),
                )
                .child(label(format!("{} of {unit} · {other}", pct(s.value(m), whole)), 12.0, 400.0, ink.muted))
        })))
}

const PLOT_H: f32 = 216.0;
const AXIS_W: f32 = 56.0;
const TIP_W: f32 = 200.0;

/// The cached chart, with a crosshair column under the pointer and one tooltip for it.
fn chart(u: &UsageState, ink: &Ink, f: &Figures, layer: &mut Vec<El>) -> El {
    let m = f.key.metric;
    let n = f.titles.len();
    let columns = (0..n).map(|i| {
        let on = u.hover == Some(i);
        row()
            .key(format!("cday/{i}"))
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .on_hover_change(move |s: &mut State, inside, tx: &mut Tx| {
                // Swap the one tooltip in place: no enter/exit fades to overlap.
                tx.instant();
                if inside {
                    s.usage.hover = Some(i);
                } else if s.usage.hover == Some(i) {
                    s.usage.hover = None;
                }
            })
            .child(el().grow(1.0))
            .child(el().key(format!("cline/{i}")).w(1.0).h_full().fill(if on { ink.line_strong } else { Color::TRANSPARENT }))
            .child(el().grow(1.0))
    });
    if let Some(i) = u.hover.filter(|i| *i < n) {
        let total: f64 = f.series.iter().map(|s| s[i]).sum();
        let tip = col()
            .key("usage-tip")
            .anchored(format!("cline/{i}"), Side::Right, Align::Start, S2)
            .instant_changes()
            .w(TIP_W)
            .padding(S3)
            .gap(S1)
            .fill(ink.raised)
            .radius(R_CTRL)
            .border(1.0, ink.line_strong)
            .child(label(f.titles[i].clone(), 12.0, 600.0, ink.fg).pb(2.0))
            .children(f.providers.iter().zip(&f.series).map(|((p, _), s)| {
                let (name, glyph, color) = provider(p);
                row()
                    .w_full()
                    .gap(6.0)
                    .items_center()
                    .child(icon(glyph, 11.0).color(color))
                    .child(label(name, 12.0, 400.0, ink.muted).grow(1.0))
                    .child(data(fmt_value(m, s[i]), 12.0, ink.fg))
            }))
            .child(
                row()
                    .w_full()
                    .pt(S1)
                    .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                    .child(label("Total", 12.0, 500.0, ink.muted).grow(1.0))
                    .child(data(fmt_value(m, total), 12.0, ink.fg)),
            );
        layer.push(tip);
    }
    let top = f.top;
    let title = match (m, f.key.range) {
        (Metric::Cost, Range::Day) => "Hourly cost",
        (Metric::Tokens, Range::Day) => "Hourly tokens",
        (Metric::Cost, _) => "Daily cost",
        (Metric::Tokens, _) => "Daily tokens",
    };
    col()
        .key("usage-chart")
        .grow(1.0)
        .min_w(0.0)
        .gap(S3)
        .child(section(title, ink))
        .child(
            row()
                .w_full()
                .h(PLOT_H)
                .child(
                    col()
                        .w(AXIS_W)
                        .h_full()
                        .pr(S2)
                        .justify_between()
                        .items_end()
                        .children([top, top / 2.0, 0.0].map(|v| {
                            data(if v == 0.0 { "0".into() } else { fmt_value(m, v) }, 10.5, ink.faint)
                                .offset(0.0, if v == top { -6.0 } else if v == 0.0 { 6.0 } else { 0.0 })
                        })),
                )
                .child(
                    el().key("plot").grow(1.0).min_w(0.0).h_full().child(canvas(f.areas.clone()).w_full().h_full())
                        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).gradient(ink.bg.alpha(0.0), ink.bg.alpha(0.92)))
                        .child(canvas(f.lines.clone()).pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))).child(
                        row().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).children(columns),
                    ),
                ),
        )
        .child(row().w_full().pl(AXIS_W).justify_between().children(f.ticks.iter().map(|t| data(t.clone(), 10.5, ink.faint))))
}

fn totals(ink: &Ink, t: &Sum) -> El {
    let cell = |k: &str, v: String| col().grow(1.0).min_w(0.0).gap(S1).child(label(k, 12.0, 400.0, ink.muted)).child(data(v, 18.0, ink.fg));
    col()
        .key("usage-totals")
        .w_full()
        .gap(S3)
        .child(section("Totals", ink))
        .child(
            row()
                .w_full()
                .gap(S4)
                .child(cell("Processed tokens", count(t.tokens)))
                .child(cell("Cached input", count(t.cache_read)))
                .child(cell("Uncached input", count(t.input)))
                .child(cell("Output", count(t.output)))
                .child(cell("Cache savings", usd(t.savings))),
        )
}

fn by_type(ink: &Ink, m: Metric, t: &Sum) -> El {
    let parts: Vec<(&str, f64, Color)> = match m {
        Metric::Cost => vec![
            ("Input", t.c_input, ink.fg.alpha(0.55)),
            ("Cache read", t.c_cache_read, ink.fg.alpha(0.3)),
            ("Cache write", t.c_cache_write, ink.fg.alpha(0.7)),
            ("Output", t.c_output, ink.fg),
            ("Other", t.c_other, ink.fg.alpha(0.42)),
        ],
        Metric::Tokens => vec![
            ("Input", t.input as f64, ink.fg.alpha(0.55)),
            ("Cache read", t.cache_read as f64, ink.fg.alpha(0.3)),
            ("Cache write", t.cache_write as f64, ink.fg.alpha(0.7)),
            ("Output", t.output as f64, ink.fg),
        ],
    };
    col()
        .key("usage-types")
        .w_full()
        .gap(S3)
        .child(section(if m == Metric::Cost { "Cost by type" } else { "Tokens by type" }, ink))
        .child(
            row().w_frac(0.5).h(8.0).gap(3.0).children(
                parts.iter().filter(|p| p.1 > 0.0).map(|(_, v, c)| el().grow(*v as f32).min_w(3.0).h_full().radius(2.0).fill(*c)),
            ),
        )
        .child(row().w_frac(0.6).gap(S4).wrap().children(parts.iter().map(|(k, v, c)| {
            row()
                .gap(6.0)
                .items_center()
                .child(el().size(8.0, 8.0).radius(2.0).fill(*c))
                .child(label(*k, 12.0, 400.0, ink.muted))
                .child(data(fmt_value(m, *v), 12.0, ink.fg))
        })))
}

const LIMIT: usize = 12;

fn breakdown(u: &UsageState, ink: &Ink, k: &Kit, f: &Figures) -> El {
    let m = f.key.metric;
    let (groups, whole, best, more) = (&f.groups, f.whole, f.best, f.more);
    let head = |t: &str, w: f32| label(t, 12.0, 400.0, ink.muted).w(w).align(Align::End);
    col()
        .key("usage-breakdown")
        .w_full()
        .gap(S2)
        .child(
            row().w_full().items_center().child(section("Breakdown", ink).grow(1.0)).child(
                SegmentedControl::new(&["Model", "Day"], Some(u.by_day as usize))
                    .label("Breakdown by")
                    .fit()
                    .on_pick(|s: &mut State, i, _| s.usage.by_day = i == 1)
                    .build(k),
            ),
        )
        .child(
            row()
                .w_full()
                .h(32.0)
                .items_center()
                .child(label("#", 12.0, 400.0, ink.muted).w(32.0))
                .child(label(if u.by_day { "Day" } else { "Model" }, 12.0, 400.0, ink.muted).grow(1.0))
                .child(head("Cost", 96.0))
                .child(head("Share", 80.0))
                .child(head("Tokens", 88.0)),
        )
        .children(groups.iter().enumerate().map(|(i, (key, s))| {
            let (_, glyph, color) = provider(s.provider);
            let name = if u.by_day { parse_day(key).map(short_date).unwrap_or_else(|| key.clone()) } else { key.clone() };
            let frac = if best > 0.0 { (s.value(m) / best) as f32 } else { 0.0 };
            let unpriced = s.cost == 0.0 && s.unpriced;
            row()
                .key(format!("brk/{i}"))
                .w_full()
                .h(49.0)
                .items_center()
                .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                .child(data((i + 1).to_string(), 12.0, ink.faint).w(32.0))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(6.0)
                        .child(
                            row()
                                .gap(S2)
                                .items_center()
                                .children((!u.by_day).then(|| icon(glyph, 13.0).color(color)))
                                .child(data(name, 13.5, ink.fg).truncate()),
                        )
                        .child(
                            row().w_frac(0.4).h(2.0).child(
                                el().h_full().w_frac(frac).min_w(4.0).radius(1.0).fill(if u.by_day { ink.fg.alpha(0.6) } else { color }),
                            ),
                        ),
                )
                .child(
                    data(if unpriced { "Unpriced".into() } else { usd(s.cost) }, 13.0, if unpriced { ink.faint } else { ink.fg })
                        .w(96.0)
                        .align(Align::End),
                )
                .child(data(pct(s.value(m), whole), 13.0, ink.muted).w(80.0).align(Align::End))
                .child(data(count(s.tokens), 13.0, ink.muted).w(88.0).align(Align::End))
        }))
        .children((more > 0).then(|| data(format!("+{more} more"), 11.5, ink.faint).pt(S2)))
}

// ---- limits -----------------------------------------------------------------

const HATCH_W: f32 = 2400.0;
const BAR_H: f32 = 32.0;

/// Share of the window's time still to run, 0–1.
fn time_left(w: &limits::Window, now: i64) -> Option<f32> {
    match (w.resets_at, w.minutes) {
        (Some(t), Some(mins)) if mins > 0 => Some(((t - now) as f32 / (mins as f32 * 60.0)).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// t3code's pace: spending faster than the window elapses, with it, or under it.
fn pace_icon(w: &limits::Window, now: i64) -> Option<&'static Icon> {
    let elapsed = 100.0 * (1.0 - time_left(w, now)? as f64);
    Some(if w.used_pct > elapsed + 5.0 {
        &icons::TREND_UP
    } else if w.used_pct < elapsed - 5.0 {
        &icons::TREND_DOWN
    } else {
        &icons::GAUGE
    })
}

/// "11/25 11:17 AM".
fn local_stamp(secs: i64) -> String {
    let Some((_, mo, d, h, mi)) = usage::local_parts(secs) else { return String::new() };
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{mo}/{d} {h12}:{mi:02} {ampm}")
}

/// The hover card t3code shows over a limit bar: account, plan, machine, then the window.
fn limit_card(ink: &Ink, l: &Limits, w: &limits::Window, now: i64, anchor: String) -> El {
    let (name, glyph, color) = provider(l.provider);
    let name = if l.provider == "claude" { "Claude".to_string() } else { name };
    let row_ = |k: &str, v: String| {
        row().w_full().gap(S3).child(label(k, 12.0, 400.0, ink.muted).w(76.0)).child(label(v, 12.0, 500.0, ink.fg).truncate().shrink())
    };
    let host = limits::host();
    let left = (100.0 - w.used_pct).clamp(0.0, 100.0);
    let mut first = col().w_full().gap(S1).pt(S3).border_sides([1.0, 0.0, 0.0, 0.0], ink.line);
    if let Some(p) = &l.plan {
        first = first.child(row_("Plan", p.clone()));
    }
    if !host.is_empty() {
        first = first.child(row_("Signed in", host));
    }
    let mut second = col().w_full().gap(S1).pt(S3).border_sides([1.0, 0.0, 0.0, 0.0], ink.line).child(row_("Left", format!("{left:.0}%")));
    if let Some(t) = w.resets_at {
        second = second.child(row_("Resets", format!("{} · in {}", local_stamp(t), limits::countdown(t - now))));
    }
    if let Some(f) = time_left(w, now) {
        second = second.child(row_("Even pace", format!("{:.0}% left by now", f * 100.0)));
    }
    if w.used_pct.round() > 0.0 && w.resets_at.is_some() {
        second = second.child(row_("Restores", format!("+{:.0}% of pool", w.used_pct)));
    }
    col()
        .key("limit-card")
        .anchored(anchor, Side::Bottom, Align::Center, S2)
        .instant_changes()
        .w(300.0)
        .padding(S4)
        .gap(S3)
        .fill(ink.raised)
        .radius(R_CTRL + 2.0)
        .border(1.0, ink.line_strong)
        .child(
            col()
                .gap(2.0)
                .child(row().gap(S2).items_center().child(icon(glyph, 15.0).color(color)).child(label(name, 14.0, 600.0, ink.fg)))
                .children(l.email.as_ref().map(|e| data(e.clone(), 11.0, ink.muted))),
        )
        .child(first)
        .child(second)
}

/// One plan window: "NN% left" with its pace, then a bar filled to what is left, hatched where
/// it is used, with a marker at the share of the window's time still to run and the reset
/// countdown. Hovering the bar opens the account card.
fn limit_window(u: &UsageState, ink: &Ink, l: &Limits, i: usize, now: i64, layer: &mut Vec<El>) -> El {
    let w = &l.windows[i];
    let (_, _, color) = provider(l.provider);
    let key = format!("lim/{}/{i}", l.provider);
    let left = (100.0 - w.used_pct).clamp(0.0, 100.0);
    let mut hatch = Shape::new();
    let mut x = -BAR_H;
    while x < HATCH_W {
        hatch = hatch.line([x, BAR_H], [x + BAR_H, 0.0], 1.0, LineCap::Butt, color.alpha(0.35));
        x += 7.0;
    }
    let remaining = w.resets_at.map(|t| t - now);
    let pill = remaining.map(|r| {
        row()
            .h(20.0)
            .px(6.0)
            .gap(4.0)
            .radius(5.0)
            .items_center()
            .fill(ink.bg)
            .child(icon(&icons::REFRESH, 10.0).color(ink.fg))
            .child(data(limits::countdown(r), 11.0, ink.fg))
    });
    let provider_id = l.provider;
    let bar_key = format!("{key}/bar");
    if u.limit_hover == Some((provider_id, i)) {
        layer.push(limit_card(ink, l, w, now, bar_key.clone()));
    }
    let bar = el()
        .key(bar_key)
        .grow(1.0)
        .min_w(0.0)
        .h(BAR_H)
        .clip_rounded(6.0)
        .on_hover_change(move |s: &mut State, inside, tx: &mut Tx| {
            tx.instant();
            if inside {
                s.usage.limit_hover = Some((provider_id, i));
            } else if s.usage.limit_hover == Some((provider_id, i)) {
                s.usage.limit_hover = None;
            }
        })
        .child(canvas(hatch).absolute(0.0, 0.0).size(HATCH_W, BAR_H))
        .child(
            row()
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .child(el().w_frac(left as f32 / 100.0).h_full().fill(ink.card).child(el().w_full().h_full().fill(color.alpha(0.42)))),
        )
        .children(time_left(w, now).map(|f| {
            row()
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .child(el().w_frac(f).h_full())
                .child(el().w(2.0).h_full().fill(ink.fg))
        }))
        .children(pill.map(|p| row().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pr(6.0).items_center().justify_end().child(p)));
    row()
        .key(key)
        .w_full()
        .padding(S4)
        .gap(S6)
        .items_center()
        .fill(ink.card)
        .radius(R_CTRL + 2.0)
        .border(1.0, ink.line)
        .child(
            col().w(180.0).gap(2.0).child(label(w.label.clone(), 13.0, 600.0, ink.fg)).child(
                row()
                    .gap(S2)
                    .items_end()
                    .child(label(format!("{left:.0}%"), 30.0, 700.0, ink.fg))
                    .child(label("left", 13.0, 400.0, ink.muted).pb(5.0))
                    .children(pace_icon(w, now).map(|g| icon(g, 13.0).color(ink.muted).mb(7.0))),
            ),
        )
        .child(bar)
}

fn limits_view(u: &UsageState, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    if !u.limits_loaded {
        return col().w_full().pt(120.0).gap(S3).items_center().child(Spinner::new().build(k)).child(label("Reading plan limits", 15.0, 500.0, ink.fg));
    }
    let now = limits::now_secs();
    col().key("usage-limits").w_full().gap(S6).children(u.limits.iter().filter(|l| u.shown(l.provider)).map(|l| {
        let (name, glyph, color) = provider(l.provider);
        let name = if l.provider == "claude" { "Claude".to_string() } else { name };
        col()
            .key(format!("lim/{}", l.provider))
            .w_full()
            .gap(S3)
            .child(row().gap(S2).items_center().child(icon(glyph, 15.0).color(color)).child(label(name, 14.0, 600.0, ink.fg)))
            .children((0..l.windows.len()).map(|i| limit_window(u, ink, l, i, now, layer)).collect::<Vec<_>>())
            .children(l.error.as_ref().map(|e| {
                row()
                    .w_full()
                    .padding(S4)
                    .gap(S2)
                    .items_center()
                    .fill(ink.card)
                    .radius(R_CTRL + 2.0)
                    .border(1.0, ink.line)
                    .child(label("Unavailable", 13.0, 600.0, ink.fg))
                    .child(label(e.clone(), 12.0, 400.0, ink.muted))
            }))
    }))
}

// ---- page -------------------------------------------------------------------

fn reload(s: &mut State) {
    let waker = s.waker.clone();
    if s.usage.tab == Tab::Limits {
        s.usage.start_limits(waker);
    } else {
        s.usage.start(waker);
    }
}

fn toggle_provider(s: &mut State, p: &'static str) {
    let h = &mut s.usage.hidden;
    if let Some(i) = h.iter().position(|x| *x == p) {
        h.remove(i);
    } else if h.len() + 1 < PROVIDERS.len() {
        h.push(p);
    }
    s.usage.hover = None;
}

/// "Usage / All providers ▾" with the provider checklist.
fn crumbs(u: &UsageState, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    let shown: Vec<&str> = PROVIDERS.iter().copied().filter(|p| u.shown(p)).collect();
    let title = match shown.as_slice() {
        all if all.len() == PROVIDERS.len() => "All providers".to_string(),
        [one] => provider(one).0,
        some => format!("{} providers", some.len()),
    };
    let trigger = row()
        .key("usage-prov-btn")
        .h(30.0)
        .px(S2)
        .gap(S1)
        .radius(R_CTRL)
        .items_center()
        .fill(if u.menu { ink.hover } else { Color::TRANSPARENT })
        .on_press(|s: &mut State| s.usage.menu = !s.usage.menu)
        .child(label(title, 14.0, 600.0, ink.fg))
        .child(icon(&icons::DOWN, 12.0).color(ink.muted));
    let all = shown.len() == PROVIDERS.len();
    let mut menu = Menu::new("usage-prov")
        .trigger(trigger)
        .open(u.menu)
        .min_width(200.0)
        .item(MenuItem::new("All providers").checked(all).on_select(|s: &mut State| s.usage.hidden.clear()))
        .separator();
    for p in PROVIDERS {
        let (name, glyph, color) = provider(p);
        menu = menu.item(
            MenuItem::new(name)
                .checked(u.shown(p))
                .lead(icon(glyph, 14.0).color(color))
                .on_select(move |s: &mut State| toggle_provider(s, p)),
        );
    }
    let menu = menu.on_close(|s: &mut State| s.usage.menu = false).build(k, layer);
    row()
        .gap(S2)
        .items_center()
        .child(label("Usage", 14.0, 400.0, ink.muted))
        .child(label("/", 14.0, 400.0, ink.faint))
        .child(menu)
}

pub fn view(s: &State, ink: &Ink, k: &Kit, layer: &mut Vec<El>) -> El {
    let u = &s.usage;
    let mut trailing = Vec::new();
    if u.tab != Tab::Limits {
        trailing.push(
            SegmentedControl::new(&Range::ALL.map(|r| r.label()), Range::ALL.iter().position(|r| *r == u.range))
                .label("Range")
                .fit()
                .on_pick(|s: &mut State, i, _| {
                    s.usage.range = Range::ALL[i];
                    s.usage.hover = None;
                })
                .build(k),
        );
    }
    trailing.push(
        SegmentedControl::new(&["Cost", "Tokens", "Limits"], Some(u.tab as usize))
            .label("Show")
            .fit()
            .on_pick(|s: &mut State, i, _| {
                s.usage.tab = [Tab::Cost, Tab::Tokens, Tab::Limits][i];
                s.usage.hover = None;
                if s.usage.tab == Tab::Limits && !s.usage.limits_loaded {
                    let waker = s.waker.clone();
                    s.usage.start_limits(waker);
                }
            })
            .build(k),
    );
    trailing.push(
        IconButton::new(&icons::REFRESH)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Large)
            .label("Refresh")
            .help(if u.tab == Tab::Limits { "Read plan limits again" } else { "Read the logs again" })
            .on_press(reload)
            .build(k),
    );
    let header = row()
        .key("topline")
        .w_full()
        .h(56.0)
        .shrink()
        .px(S6)
        .gap(S3)
        .items_center()
        .border_sides([0.0, 0.0, 1.0, 0.0], ink.line)
        .child(crumbs(u, ink, k, layer))
        .child(el().grow(1.0))
        .child(row().gap(S2).items_center().children(trailing));

    let page = |body: El| {
        col().key("usage").w_full().h_full().child(header.clone()).child(
            col()
                .key("usage-scroll")
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(16.0)
                .items_center()
                .child(col().w_full().max_w(1000.0).px(S6).pt(S6).pb(S8).gap(S8).child(body)),
        )
    };
    if u.tab == Tab::Limits {
        return page(limits_view(u, ink, k, layer));
    }
    let empty = |title: &str, body: &str, button: bool| {
        page(
            col()
                .w_full()
                .pt(96.0)
                .gap(S3)
                .items_center()
                .children((!button).then(|| Spinner::new().build(k)))
                .child(label(title, 15.0, 500.0, ink.fg))
                .child(label(body, 13.0, 400.0, ink.muted))
                .children(button.then(|| Button::new("Read again").variant(ButtonVariant::Neutral).on_press(reload).build(k))),
        )
    };
    if !u.loaded {
        return empty(if u.loading { "Reading the logs" } else { "Not read yet" }, "Claude Code, Codex, OpenCode and Pi", false);
    }
    if u.rows.is_empty() {
        return empty("No usage found", "Looked in .claude, .codex, .local\\share\\opencode and .pi under your user folder.", true);
    }
    let f = figures(u);
    let m = f.key.metric;
    let top = row().key("usage-top").w_full().gap(S8).items_start().child(summary(ink, m, &f.total, &f.providers)).child(chart(u, ink, &f, layer));
    page(col().w_full().gap(S8).child(top).child(totals(ink, &f.total)).child(by_type(ink, m, &f.total)).child(breakdown(u, ink, k, &f)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_parse_and_count_back() {
        assert_eq!(parse_day("1970-01-01"), Some(0));
        assert_eq!(parse_day("2026-11-17").unwrap() - parse_day("2026-11-10").unwrap(), 7);
        assert_eq!(parse_day("bad"), None);
        let d = parse_day("2026-11-17").unwrap();
        assert_eq!(civil_from_days(d), (2026, 11, 17));
        assert_eq!(short_date(d), "Nov 17");
    }

    #[test]
    fn chart_tops_are_round() {
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(83.0), 100.0);
        assert_eq!(nice_ceil(118.7), 200.0);
        assert_eq!(nice_ceil(2.2e6), 2.5e6);
        assert_eq!(pct(0.0001, 1.0), "<0.1%");
        assert_eq!(pct(1.0, 4.0), "25.0%");
    }

    #[test]
    fn smooth_line_never_overshoots() {
        let ys = [100.0, 100.0, 20.0, 100.0, 100.0, 60.0, 0.0];
        let pts = smooth(&ys, 8);
        assert!(pts.iter().all(|[_, y]| (0.0..=100.0).contains(y)));
        assert_eq!(pts.first().unwrap()[1], 100.0);
        assert_eq!(pts.last().unwrap()[1], 0.0);
    }

    #[test]
    fn dated_model_names_drop_the_date() {
        assert_eq!(short_model("claude-opus-4-1-20250805"), "claude-opus-4-1");
        assert_eq!(short_model("gpt-5-codex"), "gpt-5-codex");
    }
}
