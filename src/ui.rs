//! The single-page dashboard: a header with the scan state and Rescan, the headline
//! readouts, then one card per kind of information — disk footprint treemap, momentum
//! map, languages, and the projects table. The content column caps its width and centers,
//! so wide windows don't stretch it and narrow ones just shrink the cards.

use rune::{col, el, row, text, Align, Color, Cx, El, ScrollSpec, TextStyle};

use crate::charts::{scan_time, squarify};
use crate::cui::{self, Theme};
use crate::model::{fmt, gb, gib, Project};
use crate::viz;
use crate::State;

/// The content column's widest size; wider windows center it instead of stretching.
pub const MAX_W: f32 = 1000.0;
/// Chart width inside a card: the column (MAX_W) minus the page padding and card padding.
pub const CHART_W: f32 = MAX_W - 40.0;
/// How many projects the table lists before it says "and N more".
const TOP_PROJECTS: usize = 10;

// ---- shared helpers ----

struct Totals {
    bytes: u64,
    files: u64,
    loc: u64,
    src: u64,
    commits: u64,
    dirty: u64,
    repos: usize,
    no_git: usize,
    active: usize,
    dormant: usize,
    stale: usize,
    nm: u64,
    target: u64,
    other: u64,
}

fn totals(ps: &[Project]) -> Totals {
    let mut t = Totals {
        bytes: 0,
        files: 0,
        loc: 0,
        src: 0,
        commits: 0,
        dirty: 0,
        repos: 0,
        no_git: 0,
        active: 0,
        dormant: 0,
        stale: 0,
        nm: 0,
        target: 0,
        other: 0,
    };
    for p in ps {
        t.bytes += p.bytes;
        t.files += p.files;
        t.loc += p.loc;
        t.src += p.src;
        t.commits += p.commits;
        t.dirty += p.dirty;
        t.nm += p.nm;
        t.target += p.target;
        t.other += p.other;
        if p.git {
            t.repos += 1;
            match p.days {
                Some(d) if d <= 30 => t.active += 1,
                Some(d) if d <= 120 => t.dormant += 1,
                Some(_) => t.stale += 1,
                None => {}
            }
        } else {
            t.no_git += 1;
        }
    }
    t
}

fn lang_mix(ps: &[Project]) -> Vec<(String, u64)> {
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for p in ps {
        for (lang, lines) in &p.langs {
            *map.entry(lang.clone()).or_insert(0) += lines;
        }
    }
    let mut v: Vec<(String, u64)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Compact axis figure: 720k / 1.2M style, so log-scale ticks stay inside the gutter.
fn compact(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

/// One line per project for tooltips: everything worth knowing, nothing more.
fn project_tip(p: &Project) -> String {
    let age = match p.days {
        Some(d) => format!("last commit {}d ago", d),
        None => "no git".to_string(),
    };
    format!(
        "{} · {} · {} files · {} LOC · {} commits · {} · {} dirty",
        p.cat,
        gb(p.bytes),
        fmt(p.files),
        fmt(p.loc),
        fmt(p.commits),
        age,
        fmt(p.dirty)
    )
}

// ---- header ----

fn header(s: &State, t: &Theme) -> El {
    let subtitle = if s.scanning {
        if s.total > 0 {
            format!("scanning {} / {} · {}", s.done_count, s.total, s.current)
        } else {
            "starting scan…".to_string()
        }
    } else if let Some(when) = &s.done_at {
        format!(
            "{} folders · scanned {} · {}",
            s.projects.len(),
            when,
            scan_time(s.scan_secs)
        )
    } else if s.demo {
        format!("{} folders · demo fixture", s.projects.len())
    } else {
        format!("{} folders", s.projects.len())
    };
    row()
        .key("header")
        .w_full()
        .h(44.0)
        .items_center()
        .gap(10.0)
        .px(12.0)
        // Sticky and frosted: content scrolls underneath and blurs behind the bar.
        .sticky_top(0.0)
        .raise()
        .backdrop_blur(12.0)
        .fill(t.bg.alpha(0.72))
        .border(1.0, t.border_subtle)
        .radius(12.0)
        .child(
            text("Projects")
                .font(TextStyle::new(16.0, 500.0))
                .color(t.fg),
        )
        .child(cui::note(subtitle, t.text3))
        .child(el().grow(1.0))
        .child(cui::button_primary(
            t,
            "Rescan",
            "rescan",
            |st: &mut State| State::request_scan(st),
        ))
}

// ---- overview ----

fn overview(s: &State, t: &Theme, tt: &Totals) -> El {
    let has = !s.projects.is_empty();
    let fig = |v: String| if has { v } else { "—".to_string() };
    let rebuildable = tt.nm + tt.target + tt.other;

    let readouts = row()
        .w_full()
        .gap(10.0)
        .child(col().grow(1.0).child(cui::card(
            t,
            "",
            "",
            cui::readout(
                t,
                "ON DISK",
                &fig(format!("{:.1}", gib(tt.bytes))),
                Some("GiB"),
                Some(format!("{} files", fmt(tt.files))),
                28.0,
                false,
            ),
        )))
        .child(col().grow(1.0).child(cui::card(
            t,
            "",
            "",
            cui::readout(
                t,
                "LINES OF CODE",
                &fig(format!("{:.2}M", tt.loc as f64 / 1e6)),
                None,
                Some(format!("{:.1} GiB of source", gib(tt.src))),
                28.0,
                false,
            ),
        )))
        .child(col().grow(1.0).child(cui::card(
            t,
            "",
            "",
            cui::readout(
                t,
                "COMMITS",
                &fig(fmt(tt.commits)),
                None,
                Some(format!("{} git repos", tt.repos)),
                28.0,
                false,
            ),
        )))
        .child(col().grow(1.0).child(cui::card(
            t,
            "",
            "",
            cui::readout(
                t,
                "REBUILDABLE",
                &fig(format!("{:.1}", gib(rebuildable))),
                Some("GiB"),
                Some("build output and deps".to_string()),
                28.0,
                true,
            ),
        )));

    // Treemap: CUI style — neutral tiles, a soft accent tint under the cursor, tooltips.
    let items: Vec<(String, u64, String)> = s
        .projects
        .iter()
        .map(|p| (p.name.clone(), p.bytes, p.cat.to_string()))
        .collect();
    let tiles = squarify(&items, 0.0, 0.0, CHART_W, 280.0);
    let treemap_card = cui::card(
        t,
        "Disk footprint",
        "Area = bytes on disk; rest on a tile for the details.",
        viz::treemap(t, &tiles, CHART_W, 280.0, |tile| {
            s.projects
                .iter()
                .find(|p| p.name == tile.name)
                .map(project_tip)
                .unwrap_or_else(|| format!("{} · {}", tile.name, gb(tile.bytes)))
        }),
    );

    let scatter_card = cui::card(
        t,
        "Momentum map",
        "Left = recently touched, up = more code, bubble size = disk footprint; the dashed line marks a year stale.",
        momentum(s, t),
    );

    // Languages: the ranked lines bar list and the share donut, side by side.
    let mix = lang_mix(s.projects.as_slice());
    let lang_total: u64 = mix.iter().map(|l| l.1).sum();
    let max_lang = mix.first().map(|l| l.1).unwrap_or(1).max(1) as f32;
    let mut lang_rows = col().w_full().gap(2.0);
    for (i, (lang, lines)) in mix.iter().take(8).enumerate() {
        lang_rows = lang_rows.child(
            cui::bar_list_row(
                t,
                &format!("lang/{lang}"),
                lang,
                *lines as f32 / max_lang,
                &fmt(*lines),
                &format!("{:.0}%", *lines as f64 / lang_total.max(1) as f64 * 100.0),
                i == 0,
                110.0,
            )
            .help(format!(
                "{lang}: {} lines of hand-written code",
                fmt(*lines)
            )),
        );
    }
    let languages_card = cui::card(
        t,
        "Languages",
        "Hand-written code only; vendored and minified excluded.",
        lang_rows,
    );

    let segments: Vec<(String, u64, Color)> = mix
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, (l, n))| (l.clone(), *n, t.series[i % t.series.len()]))
        .collect();
    let share_card = cui::card(
        t,
        "Share of code",
        "The six largest languages; hover a wedge or a row to name it.",
        viz::donut(t, &segments, 190.0, s.hover_seg),
    );

    col()
        .w_full()
        .gap(12.0)
        .child(readouts)
        .child(treemap_card)
        .child(scatter_card)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .child(col().grow(1.35).child(languages_card))
                .child(col().grow(1.0).child(share_card)),
        )
        .child(projects_card(s, t))
}

/// Momentum scatter in CUI style: status-colored bubbles with surface rings.
fn momentum(s: &State, t: &Theme) -> El {
    const W: f32 = CHART_W;
    const H: f32 = 280.0;
    const MAX_DAYS: f64 = 660.0;
    let max_loc = s
        .projects
        .iter()
        .map(|p| p.loc)
        .max()
        .unwrap_or(1)
        .max(2000) as f64;
    let points: Vec<(String, f64, f64, f64, Color)> = s
        .projects
        .iter()
        .filter(|p| p.days.is_some())
        .map(|p| {
            (
                p.name.clone(),
                (p.days.unwrap_or(0) as f64).min(MAX_DAYS),
                p.loc as f64,
                (p.bytes.max(1) as f64).sqrt(),
                t.status(p.days),
            )
        })
        .collect();
    let max_size = points.iter().map(|p| p.3).fold(1.0f64, f64::max);
    let points: Vec<(String, f64, f64, f64, Color)> = points
        .iter()
        .map(|(n, x, y, sz, c)| (n.clone(), *x, *y, 3.0 + *sz / max_size * 13.0, *c))
        .collect();
    // One tooltip line per bubble, same order, with the folder's real footprint.
    let tips: Vec<String> = s
        .projects
        .iter()
        .filter(|p| p.days.is_some())
        .map(|p| {
            format!(
                "{} — {} LOC · {}d · {} on disk",
                p.name,
                fmt(p.loc),
                p.days.unwrap_or(0),
                gb(p.bytes)
            )
        })
        .collect();
    // Y ticks evenly spaced in log space from the floor up to the max, labelled compactly.
    let floor = 2000.0f64;
    let step = (floor / max_loc).powf(0.25);
    let y_ticks: Vec<(f64, String)> = (0..=4)
        .map(|k| {
            let v = max_loc * step.powi(k);
            (v, compact(v))
        })
        .collect();
    let x_ticks: Vec<(f64, String)> = [
        (0.0, "today".into()),
        (30.0, "30d".into()),
        (120.0, "120d".into()),
        (365.0, "365d".into()),
        (MAX_DAYS, "660+".into()),
    ]
    .to_vec();
    viz::scatter_bubble(
        t,
        &points,
        &x_ticks,
        &y_ticks,
        W,
        H,
        "days since last commit",
        Some((365.0, "1 year stale".into())),
        MAX_DAYS,
        max_loc,
        true,
        true,
        &tips,
    )
}

// ---- projects table ----

/// The projects table: a header row so every column says what it means, the biggest
/// folders ranked by disk, and a note when there are more.
fn projects_card(s: &State, t: &Theme) -> El {
    let mut ranked: Vec<&Project> = s.projects.iter().collect();
    ranked.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let max = ranked.first().map(|p| p.bytes).unwrap_or(1).max(1) as f32;
    let shown = ranked.len().min(TOP_PROJECTS);

    // Column widths, shared by the header and the rows.
    const NAME_W: f32 = 190.0;
    const NUM_W: f32 = 78.0;
    const AGE_W: f32 = 96.0;

    let cell = |label: &str, w: f32| {
        text(label.to_string())
            .font(TextStyle::new(9.5, 500.0).mono())
            .color(t.text3)
            .w(w)
            .align(Align::End)
    };
    let head = row()
        .w_full()
        .h(24.0)
        .items_center()
        .gap(12.0)
        .px(6.0)
        .child(
            text("PROJECT")
                .font(TextStyle::new(9.5, 500.0).mono())
                .color(t.text3)
                .w(NAME_W),
        )
        .child(text("").grow(1.0))
        .child(cell("DISK", NUM_W))
        .child(cell("LOC", NUM_W))
        .child(cell("COMMITS", NUM_W))
        .child(cell("LAST COMMIT", AGE_W));

    let mut rows = col().w_full().gap(2.0);
    for p in ranked.iter().take(shown) {
        let age = match p.days {
            Some(d) => format!("{d}d"),
            None => "—".into(),
        };
        rows = rows.child(
            row()
                .key(format!("projrow/{}", p.name))
                .w_full()
                .h(cui::H_SM)
                .items_center()
                .gap(12.0)
                .px(6.0)
                .radius(cui::R_CONTROL)
                .hover(t.hover())
                .help(project_tip(p))
                .child(
                    row()
                        .w(NAME_W)
                        .gap(8.0)
                        .items_center()
                        .child(cui::dot(t.status(p.days), 7.0))
                        .child(
                            text(p.name.clone())
                                .font(TextStyle::new(12.5, 400.0))
                                .color(t.fg),
                        ),
                )
                .child(
                    el().h(8.0).grow(1.0).radius(4.0).fill(t.track).child(
                        el().h(8.0)
                            .w_frac(p.bytes as f32 / max)
                            .radius(4.0)
                            .fill(t.text3),
                    ),
                )
                .child(
                    cui::mono_text(gb(p.bytes), 11.5, t.text2)
                        .w(NUM_W)
                        .align(Align::End),
                )
                .child(
                    cui::mono_text(fmt(p.loc), 11.5, t.text3)
                        .w(NUM_W)
                        .align(Align::End),
                )
                .child(
                    cui::mono_text(fmt(p.commits), 11.5, t.text3)
                        .w(NUM_W)
                        .align(Align::End),
                )
                .child(
                    cui::mono_text(age, 11.5, t.text3)
                        .w(AGE_W)
                        .align(Align::End),
                ),
        );
    }
    let more = ranked.len() - shown;
    let foot = (more > 0).then(|| {
        cui::note(
            format!("… and {more} more folders, smallest last — the treemap above has them all"),
            t.text3,
        )
    });

    cui::card(
        t,
        "Projects",
        "The biggest folders by disk; the dot is the project's pulse (green active, blue dormant, red stale, grey no git).",
        col()
            .w_full()
            .gap(4.0)
            .child(head)
            .child(rows)
            .children(foot),
    )
}

// ---- view ----

pub fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let t = cui::theme();
    let tt = totals(&s.projects);

    col()
        .key("page")
        .w_full()
        .h_full()
        .fill(t.bg)
        .px(24.0)
        .py(14.0)
        .items_center()
        .child(
            col()
                .w_full()
                .h_full()
                .max_w(MAX_W)
                .min_h(0.0)
                .gap(12.0)
                .child(
                    col()
                        .grow(1.0)
                        .min_h(0.0)
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .gap(12.0)
                        .child(header(s, &t))
                        .child(overview(s, &t, &tt)),
                ),
        )
}
