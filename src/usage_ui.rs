//! The usage page: tokens and cost from the coding agents' local logs. Totals with their
//! change, thirty days as a segment chart (the meter's lit-segment look), breakdowns by
//! provider, model and project, and what the prices say about which model to use for what.
//! The logs are read on a worker thread, the way the scan is.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use rune::{col, el, icon, row, Align, Color, El, ScrollSpec, Waker};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, SegmentedControl, Spinner};

use crate::fmt::{count, usd};
use crate::icons;
use crate::shell;
use crate::ui::{bar, card, data, label, Ink, S1, S2, S3, S4, S5, S6};
use crate::usage::{self, Usage};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    All,
}

impl Range {
    const ALL: [Range; 3] = [Range::Week, Range::Month, Range::All];
    fn days(self) -> i64 {
        match self {
            Range::Week => 7,
            Range::Month => 30,
            Range::All => i64::MAX / 4,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::All => "All time",
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

fn tokens(u: &Usage) -> u64 {
    u.input + u.output + u.cache_read + u.cache_write
}

#[derive(Default, Clone, Copy)]
struct Sum {
    tokens: u64,
    /// Uncached input.
    input: u64,
    /// Cached input (cache reads).
    cache_read: u64,
    /// Cache writes (5-minute and 1-hour combined).
    cache_write: u64,
    /// Output, including reasoning tokens.
    output: u64,
    cost: f64,
    /// Some tokens had no price.
    unpriced: bool,
}

impl Sum {
    fn add(&mut self, u: &Usage) {
        self.tokens += tokens(u);
        self.input += u.input;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.output += u.output;
        self.cost += u.cost_usd;
        self.unpriced |= !u.priced;
    }
    fn value(&self, m: Metric) -> f64 {
        match m {
            Metric::Cost => self.cost,
            Metric::Tokens => self.tokens as f64,
        }
    }
    /// The input / cached / cache write / output split, for totals and breakdowns.
    fn mix(&self) -> String {
        format!(
            "in {} · cached {} · cache write {} · out {}",
            count(self.input),
            count(self.cache_read),
            count(self.cache_write),
            count(self.output),
        )
    }
    /// Cost, or "unpriced" when the total is zero only because a model has no price.
    fn cost_label(&self) -> String {
        if self.cost == 0.0 && self.unpriced {
            "unpriced".into()
        } else {
            usd(self.cost)
        }
    }
}

/// Rows `from` to `to` days back (inclusive), today being 0.
fn between(rows: &[Usage], now: i64, from: i64, to: i64) -> impl Iterator<Item = &Usage> {
    rows.iter().filter(move |u| parse_day(&u.day).is_some_and(|d| d <= now - from && d >= now.saturating_sub(to)))
}

fn sum<'a>(it: impl Iterator<Item = &'a Usage>) -> Sum {
    it.fold(Sum::default(), |mut s, u| {
        s.add(u);
        s
    })
}

/// Sums grouped by a key, biggest cost first (tokens break ties and order unpriced rows).
fn group<'a>(it: impl Iterator<Item = &'a Usage>, key: impl Fn(&Usage) -> String) -> Vec<(String, Sum)> {
    let mut map: BTreeMap<String, Sum> = BTreeMap::new();
    for u in it {
        map.entry(key(u)).or_default().add(u);
    }
    let mut v: Vec<_> = map.into_iter().filter(|(_, s)| s.tokens > 0).collect();
    v.sort_by(|a, b| b.1.cost.total_cmp(&a.1.cost).then(b.1.tokens.cmp(&a.1.tokens)));
    v
}

fn change(now: f64, before: f64) -> Option<f64> {
    (before > 0.0).then(|| (now - before) / before * 100.0)
}

// ---- routing ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub model: String,
    pub tokens: u64,
    pub cost: f64,
    /// Dollars per million tokens, cache reads included; `None` when unpriced.
    pub per_m: Option<f64>,
    pub tier: Tier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Premium,
    Middle,
    Cheap,
    Free,
    Unpriced,
}

impl Tier {
    pub fn job(self) -> &'static str {
        match self {
            Tier::Premium => "design, integration, review",
            Tier::Middle => "features with a clear spec",
            Tier::Cheap => "scoped modules, tests",
            Tier::Free => "parsers, fixtures, bulk edits",
            Tier::Unpriced => "no price known",
        }
    }
}

/// Every model with tokens in the range, most expensive per token first, each with the kind
/// of job its price suits.
pub fn routes(rows: &[(String, Sum)]) -> Vec<Route> {
    let mut out: Vec<Route> = rows
        .iter()
        .filter(|(_, s)| s.tokens > 0)
        .map(|(m, s)| Route {
            model: m.clone(),
            tokens: s.tokens,
            cost: s.cost,
            per_m: (!s.unpriced || s.cost > 0.0).then(|| s.cost / s.tokens as f64 * 1e6),
            tier: Tier::Unpriced,
        })
        .collect();
    out.sort_by(|a, b| {
        b.per_m.unwrap_or(-1.0).total_cmp(&a.per_m.unwrap_or(-1.0)).then(b.tokens.cmp(&a.tokens))
    });
    let paid: Vec<f64> = out.iter().filter_map(|r| r.per_m).filter(|p| *p > 0.0).collect();
    let top = paid.first().copied().unwrap_or(0.0);
    let bottom = paid.last().copied().unwrap_or(0.0);
    for r in &mut out {
        r.tier = match r.per_m {
            None => Tier::Unpriced,
            Some(p) if p <= 0.0 => Tier::Free,
            Some(p) if paid.len() == 1 || p >= top * 0.5 => Tier::Premium,
            Some(p) if p <= bottom * 1.5 => Tier::Cheap,
            Some(_) => Tier::Middle,
        };
    }
    out
}

/// One or two sentences from the routes: how far apart the prices are and where work goes.
pub fn advice(routes: &[Route]) -> String {
    let premium = routes.iter().find(|r| r.tier == Tier::Premium);
    let cheap = routes.iter().rev().find(|r| r.tier == Tier::Cheap || r.tier == Tier::Middle);
    let free = routes.iter().find(|r| r.tier == Tier::Free);
    let spend: f64 = routes.iter().map(|r| r.cost).sum();
    match (premium, cheap) {
        (Some(p), Some(c)) => {
            let ratio = p.per_m.unwrap_or(0.0) / c.per_m.unwrap_or(1.0).max(1e-9);
            let share = if spend > 0.0 { p.cost / spend * 100.0 } else { 0.0 };
            let mut line = format!(
                "{} costs {:.0}\u{00d7} {} per token and took {:.0}% of spend. Keep it for design, integration and review; hand scoped modules to {}.",
                short_model(&p.model),
                ratio.max(1.0),
                short_model(&c.model),
                share,
                short_model(&c.model)
            );
            if let Some(f) = free {
                line.push_str(&format!(" Parsers and fixtures can go to {}, which is free.", short_model(&f.model)));
            }
            line
        }
        (Some(p), None) => match free {
            Some(f) => format!(
                "Only {} is paid here. Keep it for design and review; send parsers and fixtures to {}, which is free.",
                short_model(&p.model),
                short_model(&f.model)
            ),
            None => format!("Everything ran on {}. A cheaper model could take the scoped modules.", short_model(&p.model)),
        },
        _ => "Not enough priced usage in this range to compare models.".into(),
    }
}

/// "claude-opus-4-1-20250805" → "claude-opus-4-1".
pub fn short_model(m: &str) -> String {
    let parts: Vec<&str> = m.split('-').collect();
    let keep: Vec<&str> = parts.iter().copied().filter(|p| !(p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))).collect();
    keep.join("-")
}

// ---- view ------------------------------------------------------------------

fn stat(ink: &Ink, key: &str, title: &str, s: Sum, delta: Option<(f64, &str)>) -> El {
    card(ink)
        .key(format!("stat/{key}"))
        .grow(1.0)
        .min_w(0.0)
        .h(116.0)
        .padding(S4)
        .gap(S1)
        .child(label(title, 12.0, 400.0, ink.faint))
        .child(
            row()
                .w_full()
                .items_end()
                .gap(S2)
                .child(data(s.cost_label(), 24.0, ink.fg).shrink())
                .child(el().grow(1.0))
                .children(delta.map(|(pct, vs)| {
                    let up = pct >= 0.0;
                    col()
                        .items_end()
                        .gap(1.0)
                        .child(data(format!("{}{:.0}%", if up { "+" } else { "\u{2212}" }, pct.abs()), 12.0, ink.muted))
                        .child(data(format!("vs {vs}"), 10.5, ink.faint))
                })),
        )
        .child(
            row()
                .w_full()
                .gap(S2)
                .items_center()
                .child(data(format!("{} tokens", count(s.tokens)), 11.0, ink.muted))
                .children(s.unpriced.then(|| {
                    data("+ unpriced", 11.0, ink.faint).help("Some tokens ran on models with no known price, so the cost undercounts them")
                })),
        )
        .child(
            data(s.mix(), 10.5, ink.faint)
                .truncate()
                .help("Uncached input / cached input / cache writes (5m + 1h) / output. Reasoning is inside output and is not billed twice."),
        )
}

const SEGMENTS: usize = 24;

fn chart(s: &State, ink: &Ink, k: &Kit, now: i64) -> El {
    let u = &s.usage;
    let metric = u.metric;
    let mut per_day: BTreeMap<i64, Sum> = BTreeMap::new();
    for r in between(&u.rows, now, 0, 29) {
        if let Some(d) = parse_day(&r.day) {
            per_day.entry(d).or_default().add(r);
        }
    }
    let max = per_day.values().map(|v| v.value(metric)).fold(0.0, f64::max);
    let peak_day = per_day.iter().max_by(|a, b| a.1.value(metric).total_cmp(&b.1.value(metric))).map(|(d, _)| *d);
    let total: f64 = per_day.values().map(|v| v.value(metric)).sum();
    let fmt_v = |v: f64| match metric {
        Metric::Cost => usd(v),
        Metric::Tokens => count(v as u64),
    };
    let unlit = Color::WHITE.alpha(0.05);
    let columns = (0..30).map(|i| {
        let day = now - 29 + i;
        let v = per_day.get(&day).copied().unwrap_or_default();
        let value = v.value(metric);
        let lit = if max > 0.0 && value > 0.0 { ((value / max) * SEGMENTS as f64).ceil().max(1.0) as usize } else { 0 };
        let color = if day == now { ink.accent } else { ink.fg.alpha(if Some(day) == peak_day { 0.85 } else { 0.5 }) };
        col()
            .key(format!("day/{i}"))
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .gap(2.0)
            .hover(Color::WHITE.alpha(0.03))
            .radius(3.0)
            .help(format!(
                "{} · {} · {} tokens{} · {}",
                if day == now { "Today".into() } else { short_date(day) },
                v.cost_label(),
                count(v.tokens),
                if v.unpriced { " · some unpriced" } else { "" },
                v.mix()
            ))
            .children((0..SEGMENTS).rev().map(move |seg| {
                el().w_full().grow(1.0).min_h(2.0).radius(1.5).fill(if seg < lit { color } else { unlit })
            }))
    });
    let axis = (0..30).map(|i| {
        let day = now - 29 + i;
        let show = i % 7 == 1 || day == now;
        el().grow(1.0).min_w(0.0).child(
            data(if day == now { "today".into() } else if show { short_date(day) } else { String::new() }, 10.5, ink.faint)
                .align(Align::Center)
                .w_full(),
        )
    });
    let picks = ["Cost", "Tokens"];
    card(ink)
        .key("chart")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .padding(S5)
        .gap(S4)
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(
                    col()
                        .gap(2.0)
                        .grow(1.0)
                        .min_w(0.0)
                        .child(label("Last 30 days", 13.0, 500.0, ink.fg))
                        .child(data(
                            format!(
                                "{} total · peak {}{}",
                                fmt_v(total),
                                fmt_v(max),
                                peak_day.map(|d| format!(" on {}", short_date(d))).unwrap_or_default()
                            ),
                            11.0,
                            ink.faint,
                        )
                        .truncate()),
                )
                .child(
                    SegmentedControl::new(&picks, Some(if metric == Metric::Cost { 0 } else { 1 }))
                        .label("Chart shows")
                        .fit()
                        .on_pick(|s: &mut State, i, _| s.usage.metric = if i == 0 { Metric::Cost } else { Metric::Tokens })
                        .build(k),
                ),
        )
        .child(row().key("bars").w_full().grow(1.0).min_h(120.0).gap(3.0).children(columns))
        .child(row().w_full().gap(3.0).children(axis))
}

fn breakdown(ink: &Ink, key: &str, title: &str, groups: &[(String, Sum)], limit: usize) -> El {
    let max = groups.iter().map(|g| g.1.cost).fold(0.0, f64::max);
    let max_tokens = groups.iter().map(|g| g.1.tokens).max().unwrap_or(0);
    card(ink)
        .key(format!("by/{key}"))
        .grow(1.0)
        .min_w(0.0)
        .padding(S5)
        .gap(S3)
        .child(
            row()
                .w_full()
                .items_center()
                .child(label(title, 13.0, 500.0, ink.fg).grow(1.0))
                .child(data(groups.len().to_string(), 11.0, ink.faint)),
        )
        .children(groups.is_empty().then(|| label("Nothing in this range", 12.0, 400.0, ink.faint)))
        .children(groups.iter().take(limit).enumerate().map(|(i, (name, s))| {
            // Bars compare cost, or tokens when nothing here is priced.
            let f = if max > 0.0 { s.cost / max } else if max_tokens > 0 { s.tokens as f64 / max_tokens as f64 } else { 0.0 };
            col()
                .key(format!("{key}/{i}"))
                .w_full()
                .gap(6.0)
                .child(
                    row()
                        .w_full()
                        .gap(S2)
                        .items_center()
                        .child(label(if key == "model" { short_model(name) } else { name.clone() }, 12.0, 400.0, ink.row).grow(1.0).min_w(0.0).truncate())
                        .child(data(count(s.tokens), 11.0, ink.faint))
                        .child(
                            data(
                                s.cost_label(),
                                11.0,
                                if s.cost == 0.0 && s.unpriced { ink.faint } else { ink.fg },
                            )
                            .w(56.0)
                            .align(Align::End),
                        ),
                )
                .child(data(s.mix(), 10.5, ink.faint).truncate())
                .child(bar(f as f32, if i == 0 { ink.fg.alpha(0.8) } else { ink.fg.alpha(0.4) }, ink))
        }))
        .children((groups.len() > limit).then(|| data(format!("+{} more", groups.len() - limit), 11.0, ink.faint)))
}

fn routing(ink: &Ink, models: &[(String, Sum)], range: Range, width: f32) -> El {
    let routes = routes(models);
    card(ink)
        .key("routing")
        .w(width)
        .shrink()
        .h_full()
        .padding(S5)
        .gap(S3)
        .child(
            row()
                .gap(S2)
                .items_center()
                .child(icon(&icons::ROUTE, 15.0).color(ink.accent))
                .child(label("Which model for which job", 13.0, 500.0, ink.fg)),
        )
        .child(label(advice(&routes), 12.0, 400.0, ink.row).wrap().w_full())
        .child(el().grow(1.0))
        .child(
            row()
                .w_full()
                .gap(S2)
                .child(label("Model", 11.0, 400.0, ink.faint).grow(1.0))
                .child(label("per 1M tokens", 11.0, 400.0, ink.faint).align(Align::End))
                .help("Cost divided by all tokens, cache reads included"),
        )
        .child(col().w_full().gap(2.0).children(routes.iter().take(4).map(|r| {
            let rate = match (r.tier, r.per_m) {
                (Tier::Free, _) => "free".to_string(),
                (_, Some(p)) => usd(p),
                (_, None) => "unpriced".to_string(),
            };
            row()
                .w_full()
                .h(36.0)
                .gap(S2)
                .items_center()
                .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(2.0)
                        .child(data(short_model(&r.model), 12.0, ink.fg).truncate())
                        .child(label(r.tier.job(), 11.0, 400.0, ink.faint).truncate()),
                )
                .child(
                    data(rate, 12.0, if r.tier == Tier::Premium { ink.accent } else { ink.muted })
                        .w(72.0)
                        .align(Align::End),
                )
        })))
        .children((routes.len() > 4).then(|| data(format!("+{} more in {}", routes.len() - 4, range.label().to_lowercase()), 10.5, ink.faint)))
}

pub fn view(s: &State, ink: &Ink, k: &Kit) -> El {
    let u = &s.usage;
    let range_pick = SegmentedControl::new(&Range::ALL.map(|r| r.label()), Range::ALL.iter().position(|r| *r == u.range))
        .label("Range")
        .width(Some(220.0))
        .on_pick(|s: &mut State, i, _| s.usage.range = Range::ALL[i])
        .build(k);
    let reload = IconButton::new(&icons::REFRESH)
        .variant(IconButtonVariant::Ghost)
        .control_size(ControlSize::Large)
        .label("Reload")
        .help("Read the logs again")
        .on_press(|s: &mut State| {
            let waker = s.waker.clone();
            s.usage.start(waker);
        })
        .build(k);
    let header = shell::header(
        "Usage",
        Some("Claude Code, Codex and OpenCode logs on this machine".into()),
        vec![range_pick, reload],
        ink,
    );
    if !u.loaded {
        return col().key("usage").w_full().h_full().child(header).child(
            col()
                .w_full()
                .pt(120.0)
                .gap(S3)
                .items_center()
                .child(Spinner::new().build(k))
                .child(label(if u.loading { "Reading the logs" } else { "Not read yet" }, 13.0, 400.0, ink.muted)),
        );
    }
    if u.rows.is_empty() {
        return col().key("usage").w_full().h_full().child(header).child(
            col()
                .w_full()
                .pt(120.0)
                .gap(S3)
                .items_center()
                .child(label("No usage found", 15.0, 500.0, ink.fg))
                .child(label(
                    "Looked in .claude\\projects, .codex\\sessions and .local\\share\\opencode under your user folder.",
                    13.0,
                    400.0,
                    ink.muted,
                ))
                .child(
                    Button::new("Read again")
                        .variant(ButtonVariant::Neutral)
                        .on_press(|s: &mut State| {
                            let waker = s.waker.clone();
                            s.usage.start(waker);
                        })
                        .build(k),
                ),
        );
    }
    let now = today();
    let rows = &u.rows;
    let today_s = sum(between(rows, now, 0, 0));
    let yesterday = sum(between(rows, now, 1, 1));
    let week = sum(between(rows, now, 0, 6));
    let prev_week = sum(between(rows, now, 7, 13));
    let month = sum(between(rows, now, 0, 29));
    let prev_month = sum(between(rows, now, 30, 59));
    let all = sum(rows.iter());

    let span = u.range.days();
    let scoped = || between(rows, now, 0, span - 1);
    let providers = group(scoped(), |r| r.provider.to_string());
    let models = group(scoped(), |r| r.model.clone());
    let projects = group(scoped(), |r| if r.project.is_empty() { "(no folder)".into() } else { r.project.clone() });

    // A narrow window keeps each stat to its figure; the change vs the period before goes.
    let wide = s.window_w >= 1100.0;
    let stats = row()
        .key("usage-totals")
        .w_full()
        .gap(S4)
        .child(stat(ink, "today", "Today", today_s, change(today_s.cost, yesterday.cost).filter(|_| wide && !today_s.unpriced).map(|p| (p, "yesterday"))))
        .child(stat(ink, "week", "Last 7 days", week, change(week.cost, prev_week.cost).filter(|_| wide && !week.unpriced).map(|p| (p, "prior 7"))))
        .child(stat(ink, "month", "Last 30 days", month, change(month.cost, prev_month.cost).filter(|_| wide && !month.unpriced).map(|p| (p, "prior 30"))))
        .child(stat(ink, "all", "All time", all, None));
    let middle = row()
        .w_full()
        .h(356.0)
        .gap(S4)
        .items_stretch()
        .child(chart(s, ink, k, now))
        .child(routing(ink, &models, u.range, if s.window_w < 1100.0 { 300.0 } else { 380.0 }));
    let bottom = row()
        .w_full()
        .gap(S4)
        .items_stretch()
        .child(breakdown(ink, "provider", "By tool", &providers, 5))
        .child(breakdown(ink, "model", "By model", &models, 5))
        .child(breakdown(ink, "project", "By project", &projects, 5));
    col().key("usage").w_full().h_full().child(header).child(
        col()
            .key("usage-scroll")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(16.0)
            .child(col().w_full().px(S6).pb(S6).gap(S4).child(stats).child(middle).child(bottom)),
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
        assert_eq!(count(1_500_000), "1.5M");
        let d = parse_day("2026-11-17").unwrap();
        assert_eq!(civil_from_days(d), (2026, 11, 17));
        assert_eq!(short_date(d), "Nov 17");
    }

    fn sum_of(tokens: u64, cost: f64, priced: bool) -> Sum {
        Sum {
            tokens,
            input: tokens / 2,
            cache_read: tokens / 4,
            cache_write: 0,
            output: tokens - tokens / 2 - tokens / 4,
            cost,
            unpriced: !priced,
        }
    }

    #[test]
    fn routing_ranks_by_price_and_names_unpriced_models() {
        let rows = vec![
            ("claude-opus-4-1".to_string(), sum_of(10_000_000, 60.0, true)),
            ("claude-sonnet-4-5".to_string(), sum_of(40_000_000, 30.0, true)),
            ("big-pickle".to_string(), sum_of(5_000_000, 0.0, true)),
            ("mystery-model".to_string(), sum_of(1_000_000, 0.0, false)),
        ];
        let r = routes(&rows);
        assert_eq!(r[0].model, "claude-opus-4-1");
        assert_eq!(r[0].tier, Tier::Premium);
        assert_eq!(r.iter().find(|x| x.model == "claude-sonnet-4-5").unwrap().tier, Tier::Cheap);
        assert_eq!(r.iter().find(|x| x.model == "big-pickle").unwrap().tier, Tier::Free);
        assert_eq!(r.iter().find(|x| x.model == "mystery-model").unwrap().tier, Tier::Unpriced);
        let a = advice(&r);
        assert!(a.contains("claude-opus-4-1") && a.contains("claude-sonnet-4-5") && a.contains("big-pickle"), "{a}");
        assert!(a.contains("8\u{00d7}"), "opus is 8x sonnet per token here: {a}");
    }

    #[test]
    fn dated_model_names_drop_the_date() {
        assert_eq!(short_model("claude-opus-4-1-20250805"), "claude-opus-4-1");
        assert_eq!(short_model("gpt-5-codex"), "gpt-5-codex");
    }
}

#[allow(dead_code)]
fn unused() -> f32 {
    S6
}
