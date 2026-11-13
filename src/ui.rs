//! The app shell: a sidebar with the pages, a topbar with the title and Rescan, and the
//! main view. The main view (Overview) carries the highlights; each sidebar page carries
//! one kind of information to dig into — projects, languages, activity.

use rune::{Align, Color, Cx, Cursor, El, ScrollSpec, TextStyle, col, el, row, text};

use crate::charts::{scan_time, squarify};
use crate::cui::{self, Theme};
use crate::model::{Project, fmt, gb, gib};
use crate::viz;
use crate::{Page, State};

/// Sidebar width; the content column fills the rest.
pub const SIDE_W: f32 = 200.0;
const CONTENT_W: f32 = 1100.0 - SIDE_W - 48.0;

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
        bytes: 0, files: 0, loc: 0, src: 0, commits: 0, dirty: 0, repos: 0, no_git: 0,
        active: 0, dormant: 0, stale: 0, nm: 0, target: 0, other: 0,
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

/// Today as days since 1970-01-01 in local time, and the local UTC offset — shared with
/// the scanner's bucket math so heatmap dates land on the same calendar.
pub fn today_local() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (now + crate::scan::local_utc_offset_secs()) / 86_400
}

/// The inspector strip: names what the cursor is on; a click pins it.
fn inspector(s: &State, t: &Theme) -> El {
    let active = s.pinned.as_ref().or(s.hover.as_ref());
    let (lead, info) = match active.and_then(|n| s.projects.iter().find(|p| &p.name == n)) {
        Some(p) => {
            let days = match p.days {
                Some(d) => format!("last commit {}d ago", d),
                None => "no git".to_string(),
            };
            let pinned = if s.pinned.is_some() { " · pinned" } else { "" };
            (
                p.name.clone(),
                format!(
                    "{} · {} · {} files · {} LOC · {} commits · {} · {} dirty{}",
                    p.cat, gb(p.bytes), fmt(p.files), fmt(p.loc), fmt(p.commits), days, fmt(p.dirty), pinned
                ),
            )
        }
        None => (
            "Inspector".to_string(),
            "hover a tile, dot or bar for details — click to pin".to_string(),
        ),
    };
    row()
        .sticky_top(0.0)
        .w_full()
        .h(30.0)
        .items_center()
        .gap(8.0)
        .px(12.0)
        .radius(cui::R_CONTROL)
        .fill(t.raised)
        .border(1.0, t.border_subtle)
        .child(text(lead).font(TextStyle::new(10.5, 500.0)).color(t.fg))
        .child(cui::mono_text(info, 10.0, t.text3))
}

// ---- sidebar ----

fn sidebar(s: &State, t: &Theme) -> El {
    let counts = [String::new(), s.projects.len().to_string(), String::new(), String::new()];
    let mut nav = col().gap(2.0);
    for (i, (page, title)) in Page::ALL.iter().enumerate() {
        let sel = s.page == *page;
        let mut item = row()
            .key(format!("nav/{}", title))
            .h(cui::H_MD)
            .items_center()
            .gap(8.0)
            .px(10.0)
            .radius(cui::R_CONTROL)
            .cursor(Cursor::Pointer)
            .on_press({
                let page = *page;
                move |st: &mut State| {
                    st.page = page;
                    st.hover = None;
                    st.pinned = None;
                }
            });
        item = if sel {
            item.fill(t.accent.alpha(0.14)).child(text(title.to_string()).font(TextStyle::new(12.5, 500.0)).color(t.accent_strong))
        } else {
            item.hover(t.hover()).child(text(title.to_string()).font(TextStyle::new(12.5, 400.0)).color(t.text2))
        };
        if !counts[i].is_empty() {
            item = item.child(cui::mono_text(counts[i].clone(), 11.0, t.text3).grow(1.0).align(Align::End));
        }
        nav = nav.child(item);
    }
    col()
        .w(SIDE_W)
        .h_full()
        .fill(t.surface)
        .border(1.0, t.border_subtle)
        .px(12.0)
        .py(16.0)
        .gap(4.0)
        .child(
            row()
                .gap(8.0)
                .items_center()
                .px(4.0)
                .pb(12.0)
                .child(el().size(18.0, 18.0).radius(5.0).fill(t.accent.alpha(0.9)))
                .child(text("Projects").font(TextStyle::new(13.5, 500.0)).color(t.fg)),
        )
        .child(cui::note("Workspace", t.text3).px(4.0))
        .child(nav)
        .child(el().grow(1.0))
        .child(cui::hairline(t))
        .child(
            row()
                .gap(6.0)
                .items_center()
                .px(4.0)
                .pt(10.0)
                .child(cui::dot(if s.scanning { t.warning } else { t.success }, 7.0))
                .child(cui::note(scan_line(s), t.text3)),
        )
}

fn scan_line(s: &State) -> String {
    if s.scanning {
        "scanning…".to_string()
    } else if s.scan_secs > 0.0 {
        format!("scanned in {}", scan_time(s.scan_secs))
    } else if s.demo {
        "demo fixture".to_string()
    } else {
        "not scanned yet".to_string()
    }
}

// ---- topbar ----

fn topbar(s: &State, t: &Theme) -> El {
    let subtitle = if s.scanning {
        if s.total > 0 {
            format!("scanning {} / {} · {}", s.done_count, s.total, s.current)
        } else {
            "starting scan…".to_string()
        }
    } else if let Some(when) = &s.done_at {
        format!("{} folders · scanned {} · {}", s.projects.len(), when, scan_time(s.scan_secs))
    } else {
        format!("{} folders", s.projects.len())
    };
    row()
        .w_full()
        .h(52.0)
        .items_center()
        .gap(10.0)
        .px(24.0)
        .border(1.0, t.border_subtle)
        .child(text(s.page.title()).font(TextStyle::new(14.0, 500.0)).color(t.fg))
        .child(cui::note(subtitle, t.text3))
        .child(el().grow(1.0))
        .child(cui::button_primary(t, "Rescan", "rescan", |st: &mut State| State::request_scan(st)))
}

// ---- overview page ----

fn overview(s: &State, t: &Theme, tt: &Totals) -> El {
    let has = !s.projects.is_empty();
    let fig = |v: String| if has { v } else { "—".to_string() };
    let rebuildable = tt.nm + tt.target + tt.other;

    let readouts = row().w_full().gap(10.0)
        .child(col().grow(1.0).child(cui::card(t, "", "", cui::readout(t, "ON DISK", &fig(format!("{:.1}", gib(tt.bytes))), Some("GiB"), Some(format!("{} files", fmt(tt.files))), 28.0, false))))
        .child(col().grow(1.0).child(cui::card(t, "", "", cui::readout(t, "LINES OF CODE", &fig(format!("{:.2}M", tt.loc as f64 / 1e6)), None, Some(format!("{} in source", fmt(tt.src))), 28.0, false))))
        .child(col().grow(1.0).child(cui::card(t, "", "", cui::readout(t, "COMMITS", &fig(fmt(tt.commits)), None, Some(format!("{} git repos", tt.repos)), 28.0, false))))
        .child(col().grow(1.0).child(cui::card(t, "", "", cui::readout(t, "REBUILDABLE", &fig(format!("{:.1}", gib(rebuildable))), Some("GiB"), Some("build output and deps".to_string()), 28.0, true))));

    // Treemap: CUI style — neutral tiles, accent only under the cursor or the selection.
    let items: Vec<(String, u64, String)> = s
        .projects
        .iter()
        .map(|p| (p.name.clone(), p.bytes, p.cat.to_string()))
        .collect();
    let tiles = squarify(&items, 0.0, 0.0, CONTENT_W - 40.0, 280.0);
    let treemap_card = cui::card(
        t,
        "Disk footprint",
        "Area = bytes on disk; click a tile to pin it in the inspector.",
        viz::treemap(t, &tiles, 280.0, s.hover.as_deref(), s.pinned.as_deref()),
    );

    let scatter_card = cui::card(t, "Momentum map", "Left = recently touched, up = more code, bubble size = disk footprint.", momentum(s, t));
    let insights_card = insights(s, tt, t);

    col()
        .w_full()
        .gap(12.0)
        .child(readouts)
        .child(treemap_card)
        .child(scatter_card)
        .child(insights_card)
}

/// Momentum scatter in CUI style: status-colored bubbles with surface rings, guide lines
/// to both axes under the hovered point.
fn momentum(s: &State, t: &Theme) -> El {
    const W: f32 = CONTENT_W - 40.0;
    const H: f32 = 280.0;
    const MAX_DAYS: f64 = 660.0;
    let max_loc = s.projects.iter().map(|p| p.loc).max().unwrap_or(1).max(2000) as f64;
    let points: Vec<(String, f64, f64, f64, Color)> = s
        .projects
        .iter()
        .filter(|p| p.days.is_some())
        .map(|p| {
            (
                p.name.clone(),
                (p.days.unwrap_or(0) as f64).min(MAX_DAYS),
                p.loc as f64,
                (gib(p.bytes) as f64).sqrt() * 3.4 + 1.8,
                t.status(p.days),
            )
        })
        .collect();
    let y_ticks: Vec<(f64, String)> = [5000.0, 20000.0, 50000.0, 150000.0, 700000.0]
        .iter()
        .filter(|&&v| v <= max_loc)
        .map(|&v| (v, viz::compact(v)))
        .collect();
    let x_ticks: Vec<(f64, String)> = [(0.0, "today".into()), (30.0, "30d".into()), (120.0, "120d".into()), (365.0, "365d".into()), (MAX_DAYS, "660+".into())].to_vec();
    viz::scatter_bubble(
        t,
        &points,
        &x_ticks,
        &y_ticks,
        W,
        H,
        "days since last commit",
        s.hover.as_deref(),
        Some((365.0, "1 year stale".into())),
        MAX_DAYS,
        max_loc,
        true,
        true,
    )
}

// ---- projects page ----

/// Projects: ranked bar list by disk, a radar comparing the two biggest codebases, and
/// the composition breakdown that shows how much of each folder is real source.
fn projects_page(s: &State, t: &Theme, tt: &Totals) -> El {
    let mut ranked: Vec<&Project> = s.projects.iter().collect();
    ranked.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    let max = ranked.first().map(|p| p.bytes).unwrap_or(1).max(1) as f32;

    let mut list = col().w_full().gap(2.0);
    for p in &ranked {
        let frac = p.bytes as f32 / max;
        list = list.child(
            row()
                .key(format!("projrow/{}", p.name))
                .w_full()
                .h(cui::H_SM)
                .items_center()
                .gap(12.0)
                .px(6.0)
                .radius(cui::R_CONTROL)
                .hover(t.hover())
                .on_hover_change(viz::hover_name(p.name.clone()))
                .on_press(viz::pin_name(p.name.clone()))
                .child(cui::dot(t.status(p.days), 7.0))
                .child(text(p.name.clone()).font(TextStyle::new(12.5, 400.0)).color(t.fg).w(150.0))
                .child(
                    el()
                        .h(8.0)
                        .grow(1.0)
                        .radius(4.0)
                        .fill(t.track)
                        .child(el().h(8.0).w_frac(frac).radius(4.0).fill(t.text3)),
                )
                .child(cui::mono_text(gb(p.bytes), 11.5, t.text3).w(80.0).align(Align::End))
                .child(cui::mono_text(fmt(p.commits), 11.5, t.text3).w(70.0).align(Align::End)),
        );
    }
    let disk_card = cui::card(t, "On disk", "Ranked by bytes; the dot is the project's pulse.", list);

    // Radar: the two biggest codebases by LOC across comparable axes.
    let mut by_loc: Vec<&Project> = s.projects.iter().filter(|p| p.git).collect();
    by_loc.sort_by(|a, b| b.loc.cmp(&a.loc));
    let axes: Vec<String> = ["LOC", "Files", "Commits", "Disk", "Dirty", "Freshness"].iter().map(|s| s.to_string()).collect();
    let norm = |v: u64, max: u64| v as f64 / max.max(1) as f64 * 100.0;
    let maxes = (
        by_loc.iter().map(|p| p.loc).max().unwrap_or(1),
        by_loc.iter().map(|p| p.files).max().unwrap_or(1),
        by_loc.iter().map(|p| p.commits).max().unwrap_or(1),
        by_loc.iter().map(|p| p.bytes).max().unwrap_or(1),
        by_loc.iter().map(|p| p.dirty).max().unwrap_or(1).max(1),
    );
    let series: Vec<(String, Vec<f64>, Color)> = by_loc
        .iter()
        .take(2)
        .enumerate()
        .map(|(i, p)| {
            (
                p.name.clone(),
                vec![
                    norm(p.loc, maxes.0),
                    norm(p.files, maxes.1),
                    norm(p.commits, maxes.2),
                    norm(p.bytes, maxes.3),
                    norm(p.dirty, maxes.4),
                    100.0 - (p.days.unwrap_or(365).min(365) as f64 / 365.0 * 100.0),
                ],
                if i == 0 { t.series[0] } else { t.series[1] },
            )
        })
        .collect();
    let radar_card = cui::card(
        t,
        "Shape of a codebase",
        "The two largest git projects, each axis scaled to the workspace max.",
        if series.len() == 2 {
            viz::radar(t, &axes, &series, 240.0, 100.0)
        } else {
            cui::note("Needs two git projects to compare.", t.text3)
        },
    );

    // Composition: what the bytes actually are, per top folder.
    let parts: [(&str, usize); 4] = [("src", 0), ("nm", 1), ("target", 2), ("other", 3)];
    let mut comp_rows = col().w_full().gap(4.0);
    for p in ranked.iter().take(10) {
        let mut seg = row().h(16.0).grow(1.0).gap(1.0);
        for (key, si) in parts {
            let bytes = match key {
                "src" => p.src,
                "nm" => p.nm,
                "target" => p.target,
                _ => p.other,
            };
            if bytes == 0 {
                continue;
            }
            seg = seg.child(el().h(16.0).grow(bytes as f32).min_w(1.0).radius(2.0).fill(t.series[si].alpha(0.85)));
        }
        comp_rows = comp_rows.child(
            row()
                .w_full()
                .gap(8.0)
                .items_center()
                .on_hover_change(viz::hover_name(p.name.clone()))
                .child(text(p.name.clone()).font(TextStyle::new(10.0, 400.0)).color(t.text2).w(150.0))
                .child(seg)
                .child(cui::mono_text(format!("{:.1}", gib(p.bytes)), 10.0, t.text3).w(44.0).align(Align::End)),
        );
    }
    let comp_card = cui::card(
        t,
        "What the bytes actually are",
        "Top 10 folders: source vs regenerable build output.",
        col().w_full().gap(6.0).child(comp_rows).child(
            row()
                .w_full()
                .gap(14.0)
                .children(parts.iter().map(|(key, si)| {
                    let total = match *key {
                        "src" => tt.src,
                        "nm" => tt.nm,
                        "target" => tt.target,
                        _ => tt.other,
                    };
                    row()
                        .gap(5.0)
                        .items_center()
                        .child(cui::dot(t.series[*si], 7.0))
                        .child(cui::note(format!("{key} {:.1} GiB", gib(total)), t.text3))
                }).collect::<Vec<El>>()),
        ),
    );

    col()
        .w_full()
        .gap(12.0)
        .child(disk_card)
        .child(row().w_full().gap(12.0).child(col().grow(1.0).child(radar_card)).child(col().grow(1.2).child(comp_card)))
}

// ---- languages page ----

fn languages_page(s: &State, t: &Theme, tt: &Totals) -> El {
    let mix = lang_mix(s.projects.as_slice());
    let total: u64 = mix.iter().map(|l| l.1).sum();
    let max = mix.first().map(|l| l.1).unwrap_or(1).max(1) as f32;
    let mut list = col().w_full().gap(2.0);
    for (i, (lang, lines)) in mix.iter().take(12).enumerate() {
        list = list.child(
            cui::bar_list_row(t, lang, *lines as f32 / max, &format!("{} · {:.1}%", fmt(*lines), *lines as f64 / total.max(1) as f64 * 100.0), i == 0, 110.0),
        );
    }
    let bars_card = cui::card(t, "Lines by language", "Hand-written code only; vendored and minified excluded.", list);

    // Donut of the language share, series colors from the theme.
    let segments: Vec<(String, u64, Color)> = mix
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, (l, n))| (l.clone(), *n, t.series[i % t.series.len()]))
        .collect();
    let donut_card = cui::card(
        t,
        "Share of code",
        "The six largest languages; the ring's middle names the leader.",
        viz::donut(t, &segments, 190.0, s.hover.as_deref()),
    );

    col()
        .w_full()
        .gap(12.0)
        .child(bars_card)
        .child(row().w_full().gap(12.0).child(col().grow(1.0).child(donut_card)).child(col().grow(1.0).child(lang_facts(t, tt, &mix))))
}

fn lang_facts(t: &Theme, tt: &Totals, mix: &[(String, u64)]) -> El {
    let mut facts = col().gap(2.0);
    let lead = mix.first();
    let row_of = |k: &str, v: String| {
        row()
            .w_full()
            .py(9.0)
            .gap(12.0)
            .border(1.0, t.border_subtle)
            .child(cui::note(k.to_string(), t.text3).w(130.0))
            .child(cui::mono_text(v, 11.0, t.fg))
    };
    facts = facts
        .child(row_of("languages", mix.len().to_string()))
        .child(row_of("total lines", fmt(tt.loc)))
        .child(row_of(
            "leader",
            lead.map(|(l, n)| format!("{l} · {} lines", fmt(*n))).unwrap_or_else(|| "—".into()),
        ))
        .child(row_of(
            "leader share",
            lead.map(|(_, n)| format!("{:.0}%", *n as f64 / tt.loc.max(1) as f64 * 100.0)).unwrap_or_else(|| "—".into()),
        ));
    cui::card(t, "At a glance", "The shape of the code in this workspace.", facts)
}

// ---- activity page ----

/// Activity: the calendar heatmap of commits, the weekly bar chart with annotations, the
/// weekday-hour heatmap and the activity rings.
fn activity_page(s: &State, t: &Theme, tt: &Totals) -> El {
    // Sum every project's buckets into workspace-wide ones.
    let mut per_day = vec![0u32; 364];
    let mut by_hour = vec![0u32; 168];
    for p in &s.projects {
        if let Some(a) = &p.activity {
            for (i, &n) in a.per_day.iter().enumerate() {
                if i < 364 {
                    per_day[i] += n;
                }
            }
            for (i, &n) in a.by_hour.iter().enumerate() {
                if i < 168 {
                    by_hour[i] += n;
                }
            }
        }
    }
    let today = today_local();

    let cal_card = cui::card(
        t,
        "Commit calendar",
        "One square a day over the trailing year; hover a day for its count.",
        viz::calendar_heatmap(t, &per_day, today, s.hover.as_deref()),
    );

    // Weekly commits, oldest first, with the average as a threshold line and the peak
    // week marked as a point annotation.
    let weeks = 26;
    let mut weekly: Vec<f64> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut peak = (0usize, 0.0f64);
    for w in 0..weeks {
        let start = per_day.len() - (weeks - w) * 7;
        let sum: u32 = per_day[start..start + 7].iter().sum();
        weekly.push(sum as f64);
        labels.push(format!("w-{}", weeks - w));
        if sum as f64 > peak.1 {
            peak = (w, sum as f64);
        }
    }
    let avg = weekly.iter().sum::<f64>() / weekly.len().max(1) as f64;
    let mut annotations = vec![viz::Annotation::Threshold { y: avg, label: "avg".into(), accent: false }];
    if peak.1 > 0.0 {
        annotations.push(viz::Annotation::Point { index: peak.0, y: peak.1, label: format!("peak {}", peak.1 as u64) });
    }
    let bar_card = cui::card(
        t,
        "Commits per week",
        "Last 26 weeks; the dashed line is the weekly average.",
        viz::bar_chart(t, &weekly, &labels, CONTENT_W - 40.0, 200.0, t.accent, &annotations, s.hover.as_deref()),
    );

    let hour_card = cui::card(
        t,
        "When the work happens",
        "Commits by weekday and hour, local time.",
        viz::hour_heatmap(t, &by_hour, s.hover.as_deref()),
    );

    // Rings: this week against the 26-week average, active repos against all repos, and
    // the share of disk that is real source.
    let this_week: f64 = per_day[per_day.len() - 7..].iter().map(|&n| n as f64).sum();
    let rings = [
        ("this week vs avg".to_string(), if avg > 0.0 { this_week / avg } else { 0.0 }, t.series[0]),
        ("repos active".to_string(), tt.active as f64 / tt.repos.max(1) as f64, t.series[2]),
        ("source of disk".to_string(), tt.src as f64 / tt.bytes.max(1) as f64, t.series[3]),
    ];
    let rings_card = cui::card(
        t,
        "Pulse",
        "This week's commits against the weekly average; active repos; how much of the disk is real source.",
        viz::activity_rings(t, &rings, 170.0),
    );

    // Cumulative commits across the year, with the peak month annotated.
    let mut cum: Vec<f64> = Vec::new();
    let mut run = 0.0f64;
    for &n in &per_day {
        run += n as f64;
        cum.push(run);
    }
    // One labelled tick per 30 days, then "now" at the right edge.
    let mut cum_labels: Vec<String> = (0..cum.len()).map(|i| if i % 30 == 0 { i.to_string() } else { String::new() }).collect();
    cum_labels.push("now".into());
    let peak_day = per_day
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let cum_annotations = vec![viz::Annotation::Point {
        index: peak_day,
        y: cum.get(peak_day).cloned().unwrap_or(0.0),
        label: format!("{} commits in one day", per_day[peak_day]),
    }];
    let trend_card = cui::card(
        t,
        "Total commits over the year",
        &format!("{} commits, cumulative.", fmt(tt.commits)),
        viz::line_chart(t, &[("commits".into(), cum, t.accent)], &cum_labels, CONTENT_W - 40.0, 220.0, &cum_annotations, s.hover.as_deref()),
    );

    col()
        .w_full()
        .gap(12.0)
        .child(cal_card)
        .child(row().w_full().gap(12.0).child(col().grow(1.4).child(bar_card)).child(col().grow(1.0).child(rings_card)))
        .child(trend_card)
        .child(hour_card)
}

// ---- insights ----

fn insights(s: &State, tt: &Totals, t: &Theme) -> El {
    let dirtiest = {
        let mut v: Vec<&Project> = s.projects.iter().collect();
        v.sort_by(|a, b| b.dirty.cmp(&a.dirty));
        v.truncate(3);
        v.iter().map(|p| format!("{} ({})", p.name, p.dirty)).collect::<Vec<_>>().join(", ")
    };
    let top_lang = lang_mix(s.projects.as_slice()).first().cloned();
    let biggest = s.projects.iter().max_by_key(|p| p.loc).cloned();
    let rebuildable = gib(tt.nm + tt.target + tt.other);

    let cards: Vec<(String, String)> = vec![
        (
            format!("Only {} are alive", tt.active),
            format!("{} of {} repos touched in 30 days. {} dormant, {} past a year.", tt.active, tt.repos, tt.dormant, tt.stale),
        ),
        (
            format!("~{rebuildable:.0} GiB is rebuildable"),
            format!("{:.0} GiB target, {:.1} GiB node_modules, {:.0} GiB other build output.", gib(tt.target), gib(tt.nm), gib(tt.other)),
        ),
        (
            "One folder dwarfs the rest".to_string(),
            match &biggest {
                Some(p) if tt.loc > 0 => format!("{} is {} lines — {:.0}% of all code here.", p.name, fmt(p.loc), p.loc as f64 / tt.loc as f64 * 100.0),
                Some(p) => format!("{} is the biggest codebase, with {} lines.", p.name, fmt(p.loc)),
                None => "Nothing scanned yet.".to_string(),
            },
        ),
        (
            "What the code is written in".to_string(),
            match &top_lang {
                Some((lang, lines)) if tt.loc > 0 => format!("{lang} leads with {} lines — {:.0}% of all code here.", fmt(*lines), *lines as f64 / tt.loc as f64 * 100.0),
                Some((lang, lines)) => format!("{lang} leads with {} lines.", fmt(*lines)),
                None => "Nothing scanned yet.".to_string(),
            },
        ),
        (
            format!("{} dirty files", fmt(tt.dirty)),
            format!("Uncommitted work, mostly in {dirtiest}."),
        ),
        (
            if tt.no_git == 1 { "1 folder has no git".to_string() } else { format!("{} folders have no git", tt.no_git) },
            "Worth a `git init` before history is lost.".to_string(),
        ),
    ];

    let mut rows: Vec<El> = Vec::new();
    for pair in cards.chunks(2) {
        let mut r = row().w_full().gap(10.0);
        for (head, body) in pair {
            r = r.child(
                col()
                    .grow(1.0)
                    .fill(t.surface)
                    .border(1.0, t.border_subtle)
                    .radius(cui::R_PANEL)
                    .px(14.0)
                    .py(12.0)
                    .gap(4.0)
                    .child(text(head.clone()).font(TextStyle::new(11.5, 500.0)).color(t.fg))
                    .child(text(body.clone()).font(TextStyle::new(10.5, 400.0)).color(t.text3)),
            );
        }
        rows.push(r);
    }
    col().w_full().gap(10.0).children(rows)
}

// ---- view ----

pub fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let t = cui::theme();
    let tt = totals(&s.projects);

    let page_el = match s.page {
        Page::Overview => overview(s, &t, &tt),
        Page::Projects => projects_page(s, &t, &tt),
        Page::Languages => languages_page(s, &t, &tt),
        Page::Activity => activity_page(s, &t, &tt),
    };

    col()
        .w_full()
        .h_full()
        .fill(t.bg)
        .child(
            row()
                .w_full()
                .h_full()
                .child(sidebar(s, &t))
                .child(
                    col()
                        .grow(1.0)
                        .h_full()
                        .min_h(0.0)
                        .child(topbar(s, &t))
                        .child(
                            col()
                                .key("page")
                                .grow(1.0)
                                .min_h(0.0)
                                .scroll(ScrollSpec { stick_bottom: false })
                                .px(24.0)
                                .py(16.0)
                                .gap(12.0)
                                .child(inspector(s, &t))
                                .child(page_el),
                        ),
                ),
        )
}
