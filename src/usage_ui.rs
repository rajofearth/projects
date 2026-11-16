//! The usage page, after t3code's usage view: a hero total with its providers, a daily line
//! chart with a hover crosshair, the token totals, the split by type and a breakdown by model
//! or day. One range control and one Cost / Tokens switch drive all of it. The logs are read
//! on a worker thread, the way the scan is.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use rune::{canvas, col, el, icon, row, Align, Color, El, Icon, LineCap, ScrollSpec, Shape, Waker};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, SegmentedControl, Spinner};

use crate::fmt::{count, usd};
use crate::icons;
use crate::shell;
use crate::ui::{card, data, label, rgb, Ink, R_CTRL, S1, S2, S3, S4, S5, S6, S8};
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

pub struct UsageState {
    pub rows: Vec<Usage>,
    pub loading: bool,
    pub loaded: bool,
    pub range: Range,
    pub metric: Metric,
    /// The breakdown lists days instead of models.
    pub by_day: bool,
    /// The chart column under the pointer.
    pub hover: Option<usize>,
    /// The demo scene reads the fixture instead of the logs.
    pub demo: bool,
    rx: Option<Receiver<Vec<Usage>>>,
}

impl UsageState {
    pub fn new() -> Self {
        UsageState {
            rows: Vec::new(),
            loading: false,
            loaded: false,
            range: Range::Month,
            metric: Metric::Cost,
            by_day: false,
            hover: None,
            demo: false,
            rx: None,
        }
    }

    /// Reads the logs off the UI thread; the waker brings the answer back.
    pub fn start(&mut self, waker: Option<Waker>) {
        if self.loading {
            return;
        }
        if self.demo {
            self.rows = crate::fixture::demo_usage(today());
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

    /// True when the load finished this turn.
    pub fn drain(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(rows) => {
                self.rows = rows;
                self.done();
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.done();
                true
            }
        }
    }

    fn done(&mut self) {
        self.loading = false;
        self.loaded = true;
        self.rx = None;
    }
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
        return "0%".into();
    }
    let p = part / whole * 100.0;
    if p > 0.0 && p < 0.1 {
        "<0.1%".into()
    } else {
        format!("{p:.1}%")
    }
}

/// Rows from `back` days ago through today.
fn within(rows: &[Usage], now: i64, back: i64) -> impl Iterator<Item = &Usage> {
    rows.iter().filter(move |u| parse_day(&u.day).is_some_and(|d| d <= now && d > now - back))
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

/// A provider's name, mark and series colour.
fn provider(p: &str) -> (String, &'static Icon, Color) {
    match p {
        "claude" => ("Claude Code".into(), &icons::P_CLAUDE, rgb([222, 128, 92])),
        "codex" => ("Codex".into(), &icons::P_CODEX, rgb([214, 214, 220])),
        "opencode" => ("OpenCode".into(), &icons::P_OPENCODE, rgb([96, 165, 250])),
        "pi" => ("Pi".into(), &icons::P_PI, rgb([110, 208, 140])),
        other => (other.to_string(), &icons::CODE, rgb([160, 160, 166])),
    }
}

// ---- view ------------------------------------------------------------------

fn section(title: &str, ink: &Ink) -> El {
    label(title, 13.0, 500.0, ink.fg)
}

/// The hero total and one line per provider.
fn summary(ink: &Ink, m: Metric, total: &Sum, providers: &[(String, Sum)]) -> El {
    let whole = total.value(m);
    let unit = if m == Metric::Cost { "cost" } else { "tokens" };
    let sessions = format!("{} session{}", total.sessions, if total.sessions == 1 { "" } else { "s" });
    col()
        .key("usage-summary")
        .w(300.0)
        .shrink()
        .gap(S5)
        .child(
            col()
                .gap(S1)
                .child(label(fmt_value(m, whole), 34.0, 600.0, ink.fg))
                .child(label(
                    if m == Metric::Cost && total.unpriced { format!("{sessions} · unpriced models count as $0") } else { sessions },
                    12.0,
                    400.0,
                    ink.faint,
                )),
        )
        .child(col().gap(S4).children(providers.iter().map(|(p, s)| {
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
                        .child(el().size(6.0, 6.0).radius(3.0).fill(color))
                        .child(icon(glyph, 13.0).color(color))
                        .child(label(name, 13.0, 500.0, ink.fg))
                        .child(label(format!("{} sess.", s.sessions), 11.0, 400.0, ink.faint).truncate().shrink())
                        .child(el().grow(1.0))
                        .child(data(fmt_value(m, s.value(m)), 13.0, ink.fg)),
                )
                .child(data(format!("{} of {unit} · {other}", pct(s.value(m), whole)), 11.0, ink.faint).pl(28.0))
        })))
}

const PLOT_H: f32 = 220.0;
const AXIS_W: f32 = 52.0;
const TIP_W: f32 = 184.0;

/// One line per provider over the days, with a crosshair and tooltip on the hovered day.
fn chart(u: &UsageState, ink: &Ink, now: i64, providers: &[(String, Sum)]) -> El {
    let m = u.metric;
    let n = u.range.days().max(7) as usize;
    let first = now - n as i64 + 1;
    // series[p][i]: provider p on day first + i.
    let mut series: Vec<Vec<f64>> = vec![vec![0.0; n]; providers.len()];
    for r in within(&u.rows, now, n as i64) {
        let (Some(d), Some(p)) = (parse_day(&r.day), providers.iter().position(|(p, _)| p == r.provider)) else { continue };
        let v = match m {
            Metric::Cost => r.cost_usd,
            Metric::Tokens => (r.input + r.output + r.cache_read + r.cache_write) as f64,
        };
        series[p][(d - first) as usize] += v;
    }
    let top = nice_ceil(series.iter().flatten().copied().fold(0.0, f64::max));
    let y = |v: f64| (100.0 - v / top * 100.0) as f32;
    let mut shape = Shape::new().view_box(n as f32, 100.0);
    for g in [0.0, 50.0, 100.0] {
        shape = shape.line([0.0, g], [n as f32, g], 1.0, LineCap::Butt, ink.line);
    }
    // Biggest provider last, so it draws on top.
    for (p, vals) in series.iter().enumerate().rev() {
        let (_, _, color) = provider(&providers[p].0);
        let pts: Vec<[f32; 2]> = vals.iter().enumerate().map(|(i, v)| [i as f32 + 0.5, y(*v)]).collect();
        let mut area = pts.clone();
        area.push([n as f32 - 0.5, 100.0]);
        area.push([0.5, 100.0]);
        shape = shape.fill(area, color.alpha(0.08)).stroke(pts, 1.5, color);
    }
    let columns = (0..n).map(|i| {
        let on = u.hover == Some(i);
        let tip = on.then(|| {
            let total: f64 = series.iter().map(|s| s[i]).sum();
            col()
                .absolute(if i < n / 2 { S2 } else { -TIP_W - S2 }, S2)
                .w(TIP_W)
                .padding(S3)
                .gap(S1)
                .fill(ink.raised)
                .radius(R_CTRL)
                .border(1.0, ink.line_strong)
                .child(label(short_date(first + i as i64), 12.0, 500.0, ink.fg).pb(2.0))
                .children(providers.iter().zip(&series).map(|((p, _), s)| {
                    let (name, glyph, color) = provider(p);
                    row()
                        .w_full()
                        .gap(6.0)
                        .items_center()
                        .child(icon(glyph, 11.0).color(color))
                        .child(label(name, 11.5, 400.0, ink.muted).grow(1.0))
                        .child(data(fmt_value(m, s[i]), 11.5, ink.fg))
                }))
                .child(
                    row()
                        .w_full()
                        .pt(S1)
                        .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                        .child(label("Total", 11.5, 400.0, ink.muted).grow(1.0))
                        .child(data(fmt_value(m, total), 11.5, ink.fg)),
                )
        });
        row()
            .key(format!("cday/{i}"))
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .on_hover_change(move |s: &mut State, inside, _| {
                if inside {
                    s.usage.hover = Some(i);
                } else if s.usage.hover == Some(i) {
                    s.usage.hover = None;
                }
            })
            .child(el().grow(1.0))
            .child(el().w(1.0).h_full().fill(if on { ink.line_strong } else { Color::TRANSPARENT }).children(tip))
            .child(el().grow(1.0))
    });
    let ticks = [first, first + n as i64 / 2, now];
    col()
        .key("usage-chart")
        .grow(1.0)
        .min_w(0.0)
        .gap(S3)
        .child(section(if m == Metric::Cost { "Daily cost" } else { "Daily processed tokens" }, ink))
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
                        .children([top, top / 2.0, 0.0].map(|v| data(fmt_value(m, v), 10.5, ink.faint).offset(0.0, if v == top { -6.0 } else if v == 0.0 { 6.0 } else { 0.0 }))),
                )
                .child(
                    el()
                        .key("plot")
                        .grow(1.0)
                        .min_w(0.0)
                        .h_full()
                        .child(canvas(shape).w_full().h_full())
                        .child(row().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).children(columns)),
                ),
        )
        .child(
            row()
                .w_full()
                .pl(AXIS_W)
                .justify_between()
                .children(ticks.map(|d| data(short_date(d).to_uppercase(), 10.5, ink.faint))),
        )
}

fn totals(ink: &Ink, t: &Sum) -> El {
    let cell = |k: &str, v: String| col().grow(1.0).min_w(0.0).gap(S1).child(label(k, 12.0, 400.0, ink.faint)).child(data(v, 18.0, ink.fg));
    card(ink)
        .key("usage-totals")
        .w_full()
        .padding(S5)
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
            ("Input", t.c_input, ink.fg.alpha(0.35)),
            ("Cache read", t.c_cache_read, ink.fg.alpha(0.18)),
            ("Cache write", t.c_cache_write, ink.fg.alpha(0.6)),
            ("Output", t.c_output, ink.accent),
            ("Other", t.c_other, ink.fg.alpha(0.9)),
        ],
        Metric::Tokens => vec![
            ("Input", t.input as f64, ink.fg.alpha(0.35)),
            ("Cache read", t.cache_read as f64, ink.fg.alpha(0.18)),
            ("Cache write", t.cache_write as f64, ink.fg.alpha(0.6)),
            ("Output", t.output as f64, ink.accent),
        ],
    };
    card(ink)
        .key("usage-types")
        .w_full()
        .padding(S5)
        .gap(S3)
        .child(section(if m == Metric::Cost { "Cost by type" } else { "Tokens by type" }, ink))
        .child(
            row().w_full().h(8.0).gap(2.0).radius(4.0).fill(ink.raised).children(
                parts.iter().filter(|p| p.1 > 0.0).map(|(_, v, c)| el().grow(*v as f32).min_w(3.0).h_full().radius(2.0).fill(*c)),
            ),
        )
        .child(row().w_full().gap(S5).children(parts.iter().map(|(k, v, c)| {
            row()
                .gap(6.0)
                .items_center()
                .child(el().size(8.0, 8.0).radius(2.0).fill(*c))
                .child(label(*k, 12.0, 400.0, ink.muted))
                .child(data(fmt_value(m, *v), 12.0, ink.fg))
        })))
}

const LIMIT: usize = 12;

fn breakdown(u: &UsageState, ink: &Ink, k: &Kit, now: i64) -> El {
    let m = u.metric;
    let scoped = || within(&u.rows, now, u.range.days());
    let mut groups = if u.by_day {
        let mut g = group(scoped(), m, |r| r.day.clone());
        g.sort_by(|a, b| b.0.cmp(&a.0));
        g
    } else {
        group(scoped(), m, |r| short_model(&r.model))
    };
    let whole: f64 = groups.iter().map(|g| g.1.value(m)).sum();
    let best = groups.iter().map(|g| g.1.value(m)).fold(0.0, f64::max);
    let more = groups.len().saturating_sub(LIMIT);
    groups.truncate(LIMIT);
    let head = |t: &str, w: f32| label(t, 11.5, 400.0, ink.faint).w(w).align(Align::End);
    card(ink)
        .key("usage-breakdown")
        .w_full()
        .padding(S5)
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
                .child(label("#", 11.5, 400.0, ink.faint).w(32.0))
                .child(label(if u.by_day { "Day" } else { "Model" }, 11.5, 400.0, ink.faint).grow(1.0))
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
                .h(48.0)
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
                                .children((!u.by_day).then(|| icon(glyph, 12.0).color(color)))
                                .child(data(name, 13.0, ink.fg).truncate()),
                        )
                        .child(
                            row().w_frac(0.5).h(2.0).child(
                                el().h_full().w_frac(frac).min_w(2.0).radius(1.0).fill(if u.by_day { ink.accent } else { color }),
                            ),
                        ),
                )
                .child(data(if unpriced { "Unpriced".into() } else { usd(s.cost) }, 13.0, if unpriced { ink.faint } else { ink.fg }).w(96.0).align(Align::End))
                .child(data(pct(s.value(m), whole), 13.0, ink.muted).w(80.0).align(Align::End))
                .child(data(count(s.tokens), 13.0, ink.muted).w(88.0).align(Align::End))
        }))
        .children((more > 0).then(|| data(format!("+{more} more"), 11.5, ink.faint).pt(S2)))
}

fn reload(s: &mut State) {
    let waker = s.waker.clone();
    s.usage.start(waker);
}

pub fn view(s: &State, ink: &Ink, k: &Kit) -> El {
    let u = &s.usage;
    let range_pick = SegmentedControl::new(&Range::ALL.map(|r| r.label()), Range::ALL.iter().position(|r| *r == u.range))
        .label("Range")
        .fit()
        .on_pick(|s: &mut State, i, _| {
            s.usage.range = Range::ALL[i];
            s.usage.hover = None;
        })
        .build(k);
    let mode_pick = SegmentedControl::new(&["Cost", "Tokens"], Some((u.metric == Metric::Tokens) as usize))
        .label("Show")
        .fit()
        .on_pick(|s: &mut State, i, _| s.usage.metric = if i == 0 { Metric::Cost } else { Metric::Tokens })
        .build(k);
    let refresh = IconButton::new(&icons::REFRESH)
        .variant(IconButtonVariant::Ghost)
        .control_size(ControlSize::Large)
        .label("Reload")
        .help("Read the logs again")
        .on_press(reload)
        .build(k);
    let header = shell::header("Usage", None, vec![range_pick, mode_pick, refresh], ink);
    let empty = |title: &str, body: &str, button: bool| {
        col().key("usage").w_full().h_full().child(header.clone()).child(
            col()
                .w_full()
                .pt(120.0)
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
        return empty(
            "No usage found",
            "Looked in .claude, .codex, .local\\share\\opencode and .pi under your user folder.",
            true,
        );
    }
    let now = today();
    let m = u.metric;
    let total = sum(within(&u.rows, now, u.range.days()));
    let providers = group(within(&u.rows, now, u.range.days()), m, |r| r.provider.to_string());
    let top = card(ink)
        .key("usage-top")
        .w_full()
        .padding(S6)
        .child(row().w_full().gap(S8).items_start().child(summary(ink, m, &total, &providers)).child(chart(u, ink, now, &providers)));
    col().key("usage").w_full().h_full().child(header).child(
        col()
            .key("usage-scroll")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(16.0)
            .child(
                col()
                    .w_full()
                    .px(S6)
                    .pb(S6)
                    .gap(S4)
                    .child(top)
                    .child(totals(ink, &total))
                    .child(by_type(ink, m, &total))
                    .child(breakdown(u, ink, k, now)),
            ),
    )
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
    fn dated_model_names_drop_the_date() {
        assert_eq!(short_model("claude-opus-4-1-20250805"), "claude-opus-4-1");
        assert_eq!(short_model("gpt-5-codex"), "gpt-5-codex");
    }
}
