//! The usage page: tokens and cost from the coding agents' local logs, as totals for
//! today, 7 and 30 days, a dot bar per day, and breakdowns by provider, model and project.
//! The logs are read on a worker thread, the way the scan is.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use rune::{col, el, row, Align, El, ScrollSpec, Waker};

use crate::ui::{ch, chip, dots, section_label, t, tb, Ink};
use crate::usage::{self, Usage};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Today,
    Week,
    Month,
}

impl Range {
    const ALL: [Range; 3] = [Range::Today, Range::Week, Range::Month];
    fn days(self) -> i64 {
        match self {
            Range::Today => 1,
            Range::Week => 7,
            Range::Month => 30,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::Week => "7 days",
            Range::Month => "30 days",
        }
    }
}

pub struct UsageState {
    pub rows: Vec<Usage>,
    pub loading: bool,
    pub loaded: bool,
    pub range: Range,
    rx: Option<Receiver<Vec<Usage>>>,
}

impl UsageState {
    pub fn new() -> Self {
        UsageState { rows: Vec::new(), loading: false, loaded: false, range: Range::Week, rx: None }
    }

    /// Reads the logs off the UI thread; the waker brings the answer back.
    pub fn start(&mut self, waker: Option<Waker>) {
        if self.loading {
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

fn parse_day(day: &str) -> Option<i64> {
    let mut it = day.split('-').map(|p| p.parse::<i64>().ok());
    Some(days_from_civil(it.next()??, it.next()??, it.next()??))
}

/// Today's day number in local time (UTC off Windows).
fn today() -> i64 {
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

/// 1.2M / 340k style count.
fn count(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

fn usd(c: f64) -> String {
    if c >= 100.0 { format!("${c:.0}") } else { format!("${c:.2}") }
}

#[derive(Default, Clone, Copy)]
struct Sum {
    tokens: u64,
    cost: f64,
}

/// Rows within the last `days` days, today included.
fn within(rows: &[Usage], now: i64, days: i64) -> impl Iterator<Item = &Usage> {
    rows.iter().filter(move |u| parse_day(&u.day).is_some_and(|d| d <= now && now - d < days))
}

fn sum<'a>(it: impl Iterator<Item = &'a Usage>) -> Sum {
    it.fold(Sum::default(), |mut s, u| {
        s.tokens += tokens(u);
        s.cost += u.cost_usd;
        s
    })
}

/// Sums grouped by a key, biggest cost first.
fn group<'a>(it: impl Iterator<Item = &'a Usage>, key: impl Fn(&Usage) -> String) -> Vec<(String, Sum)> {
    let mut map: BTreeMap<String, Sum> = BTreeMap::new();
    for u in it {
        let e = map.entry(key(u)).or_default();
        e.tokens += tokens(u);
        e.cost += u.cost_usd;
    }
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cost.total_cmp(&a.1.cost).then(b.1.tokens.cmp(&a.1.tokens)));
    v
}

// ---- view ------------------------------------------------------------------

const PROVIDER: [u8; 3] = [96, 165, 250];
const MODEL: [u8; 3] = [192, 132, 252];
const PROJECT: [u8; 3] = [74, 222, 128];
const DAY: [u8; 3] = [251, 191, 36];

fn figure(ink: &Ink, label: &str, s: Sum) -> El {
    col()
        .gap(ch(0.2))
        .child(section_label(label, ink))
        .child(row().items_center().child(tb(usd(s.cost), ink.fg)).child(t(" cost", ink.muted)))
        .child(row().items_center().child(tb(count(s.tokens), ink.fg)).child(t(" tokens", ink.muted)))
}

fn bar_line(ink: &Ink, key: String, name: String, s: Sum, max: f64, color: [u8; 3], seed: usize, name_w: f32) -> El {
    row()
        .key(key)
        .w_full()
        .items_center()
        .gap(ch(1.0))
        .child(t(name, ink.row).w(name_w).truncate())
        .child(t(usd(s.cost), ink.fg).w(ch(8.0)).align(Align::End))
        .child(t(count(s.tokens), ink.muted).w(ch(7.0)).align(Align::End))
        .child(dots(if max > 0.0 { (s.cost / max) as f32 } else { 0.0 }, color, 16, seed, false))
}

fn breakdown(ink: &Ink, title: &str, groups: &[(String, Sum)], color: [u8; 3], limit: usize) -> El {
    let max = groups.first().map(|g| g.1.cost).unwrap_or(0.0);
    col()
        .key(format!("by/{title}"))
        .w_full()
        .gap(ch(0.3))
        .child(section_label(title, ink))
        .children((groups.is_empty()).then(|| t("nothing yet", ink.faint)))
        .children(groups.iter().take(limit).enumerate().map(|(i, (name, s))| {
            bar_line(ink, format!("{title}/{i}"), name.clone(), *s, max, color, i + title.len(), ch(22.0))
        }))
}

pub fn view(s: &State, ink: &Ink) -> El {
    let u = &s.usage;
    if !u.loaded {
        return col()
            .key("usage")
            .w_full()
            .child(t(if u.loading { "reading usage logs…" } else { "no usage read yet" }, ink.faint));
    }
    let now = today();
    let rows = &u.rows;
    let range = u.range.days();

    // One line per day, newest first, over the last 30.
    let mut per_day: BTreeMap<i64, Sum> = BTreeMap::new();
    for r in within(rows, now, 30) {
        if let Some(d) = parse_day(&r.day) {
            let e = per_day.entry(d).or_default();
            e.tokens += tokens(r);
            e.cost += r.cost_usd;
        }
    }
    let day_max = per_day.values().map(|s| s.cost).fold(0.0, f64::max);
    let day_name: BTreeMap<i64, String> = rows.iter().filter_map(|r| parse_day(&r.day).map(|d| (d, r.day.clone()))).collect();
    let days = col()
        .key("days")
        .w_full()
        .gap(ch(0.3))
        .child(section_label("per day, last 30", ink))
        .children(per_day.iter().rev().map(|(d, sm)| {
            let label = if *d == now { "today".to_string() } else { day_name.get(d).cloned().unwrap_or_default() };
            bar_line(ink, format!("day/{d}"), label, *sm, day_max, DAY, *d as usize, ch(11.0))
        }));

    let scoped = || within(rows, now, range);
    let providers = group(scoped(), |r| r.provider.to_string());
    let models = group(scoped(), |r| r.model.clone());
    let projects = group(scoped(), |r| if r.project.is_empty() { "(none)".into() } else { r.project.clone() });

    col()
        .key("usage")
        .w_full()
        .gap(ch(0.6))
        .child(
            row()
                .key("usage-totals")
                .w_full()
                .gap(ch(6.0))
                .items_start()
                .child(figure(ink, "today", sum(within(rows, now, 1))))
                .child(figure(ink, "7 days", sum(within(rows, now, 7))))
                .child(figure(ink, "30 days", sum(within(rows, now, 30))))
                .child(figure(ink, "all time", sum(rows.iter())))
                .child(el().grow(1.0))
                .child(chip("Reload", ink.fg, [230, 230, 230], |s: &mut State| {
                    let waker = s.waker.clone();
                    s.usage.start(waker);
                })),
        )
        .child(
            row()
                .grow(1.0)
                .min_h(0.0)
                .gap(ch(4.0))
                .pt(ch(0.8))
                .child(
                    col()
                        .key("days-scroll")
                        .w(ch(52.0))
                        .min_h(0.0)
                        .scroll(ScrollSpec { stick_bottom: false })
                        .scroll_edge_fade(26.0)
                        .child(days),
                )
                .child(
                    col()
                        .key("breakdowns-scroll")
                        .grow(1.0)
                        .min_h(0.0)
                        .scroll(ScrollSpec { stick_bottom: false })
                        .scroll_edge_fade(26.0)
                        .child(
                            col()
                                .w_full()
                                .gap(ch(1.2))
                                .child(
                                    row()
                                        .items_center()
                                        .gap(ch(0.8))
                                        .child(t("breakdown for", ink.faint))
                                        .children(Range::ALL.into_iter().map(|r| {
                                            let on = u.range == r;
                                            let c = chip(r.label(), if on { ink.fg } else { ink.muted }, [230, 230, 230], move |s: &mut State| {
                                                s.usage.range = r
                                            });
                                            if on { c.fill(ink.picked) } else { c }
                                        })),
                                )
                                .child(breakdown(ink, "by provider", &providers, PROVIDER, 8))
                                .child(breakdown(ink, "by model", &models, MODEL, 10))
                                .child(breakdown(ink, "top projects", &projects, PROJECT, 10)),
                        ),
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
        assert_eq!(count(1_500_000), "1.5M");
    }
}
