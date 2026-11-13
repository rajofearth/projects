//! The dashboard view: header, KPIs, treemap, language mix, pulse donut, momentum map,
//! composition bars and insights — mirroring the mock's layout, drawn with Rune primitives.

use rune::tok::{self, type_};
use rune::{Color, Cx, El, ScrollSpec, Shape, canvas, col, el, row, text};
use rune::{Align, Cursor, TextStyle};

use crate::charts::{squarify, tile_label_area};
use crate::model::{Project, cat_color, fmt, fit, gb, gib, lang_color_or};
use crate::State;

const CONTENT_W: f32 = 1040.0;
/// Cards pad 16 pt each side, so their content is narrower than the page.
const INNER_W: f32 = CONTENT_W - 32.0;
const MAP_H: f32 = 300.0;
const SCATTER_H: f32 = 300.0;

fn label(s: &str, c: Color) -> El {
    text(s.to_string()).font(type_::SECTION).color(c)
}

fn caption(s: &str, c: Color) -> El {
    text(s.to_string()).font(TextStyle::new(10.0, 400.0)).color(c)
}

fn card(t: &tok::Theme, title: &str, sub: &str, content: El) -> El {
    col()
        .w_full()
        .fill(t.card)
        .border(1.0, t.card_border)
        .radius(10.0)
        .px(16.0)
        .py(14.0)
        .gap(8.0)
        .child(label(title, t.text))
        .child(caption(sub, t.text_secondary))
        .child(content)
}

fn kpi(value: &str, name: &str, sub: &str, t: &tok::Theme) -> El {
    col()
        .grow(1.0)
        .fill(t.card)
        .border(1.0, t.card_border)
        .radius(10.0)
        .px(12.0)
        .py(10.0)
        .child(text(value.to_string()).font(TextStyle::new(19.0, 700.0)).color(t.text))
        .child(text(name.to_string()).font(TextStyle::new(10.0, 500.0)).color(t.text_secondary))
        .child(caption(sub, t.text_secondary))
}

// ---- aggregates ----

struct Totals {
    bytes: u64,
    files: u64,
    loc: u64,
    commits: u64,
    dirty: u64,
    repos: usize,
    folders: usize,
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
        commits: 0,
        dirty: 0,
        repos: 0,
        folders: ps.len(),
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
        t.commits += p.commits;
        t.dirty += p.dirty;
        t.nm += p.nm;
        t.target += p.target;
        t.other += p.other;
        if p.git {
            t.repos += 1;
            match p.days {
                Some(d) if d <= 30 => t.active += 1,
                Some(d) if d <= 365 => t.dormant += 1,
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
    v.truncate(10);
    v
}

// ---- header ----

fn header(s: &State, t: &tok::Theme) -> El {
    let n = s.projects.len();
    let subtitle = if s.demo {
        format!("{n} folders · demo fixture")
    } else if s.scanning {
        if s.total > 0 {
            format!("scanning {} / {} · {}", s.done_count, s.total, s.current)
        } else {
            "starting scan…".to_string()
        }
    } else if let Some(when) = &s.done_at {
        format!("{} folders · scanned {} · {}", n, when, crate::charts::scan_time(s.scan_secs))
    } else {
        format!("{n} folders · not scanned yet")
    };

    let mut left = col()
        .gap(2.0)
        .child(text(format!("State of {}", s.root)).font(TextStyle::new(15.0, 650.0)).color(t.text))
        .child(text(subtitle).font(TextStyle::new(11.5, 400.0)).color(t.text_secondary));

    if let Some(err) = &s.error {
        left = left.child(text(err.clone()).font(type_::SECTION).color(rune::color::hex("#ef4444").unwrap()));
    }

    let refresh = el()
        .key("rescan")
        .role("button")
        .label("Rescan")
        .cursor(Cursor::Pointer)
        .h(26.0)
        .px(12.0)
        .radius(6.0)
        .fill(t.control_off)
        .hover(t.hover)
        .items_center()
        .justify_center()
        .on_press(State::request_scan)
        .child(text("Rescan").font(type_::LABEL).color(t.text));

    let mut head = col()
        .w_full()
        .gap(6.0)
        .child(row().w_full().justify_between().items_center().child(left).child(refresh));

    if s.scanning && s.total > 0 {
        head = head.child(
            row()
                .key("progress")
                .w_full()
                .h(2.0)
                .radius(1.0)
                .child(el().grow(s.done_count as f32).fill(tok::accent(0, true)))
                .child(el().grow((s.total - s.done_count) as f32).fill(t.control_track)),
        );
    }
    head
}

// ---- treemap ----

fn treemap(ps: &[Project], t: &tok::Theme) -> El {
    let mut items: Vec<(String, u64, String)> = ps
        .iter()
        .filter(|p| p.bytes > 0)
        .map(|p| (p.name.clone(), p.bytes, p.cat.to_string()))
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    let tiles = squarify(&items, 0.0, 0.0, INNER_W, MAP_H);

    let mut rects: Vec<El> = Vec::new();
    for tile in &tiles {
        let color = cat_color(&tile.cat);
        let w = (tile.w - 2.0).max(0.0);
        let h = (tile.h - 2.0).max(0.0);
        if w < 2.0 || h < 2.0 {
            continue;
        }
        let area = w * h;
        let (show_name, show_size) = tile_label_area(w, h);
        let fs = (area.sqrt() / 9.0).clamp(7.0, 12.0);
        let mut inner = col().gap(1.0);
        if show_name {
            let maxc = ((w - 10.0) / (fs * 0.58)).max(3.0) as usize;
            inner = inner.child(
                text(fit(&tile.name, maxc))
                    .font(TextStyle::new(fs, 600.0))
                    .color(t.text),
            );
        }
        if show_size {
            inner = inner.child(
                text(gb(tile.bytes))
                    .font(TextStyle::new((fs - 3.0).max(7.0), 400.0))
                    .color(t.text_secondary),
            );
        }
        rects.push(
            el()
                .key(format!("tile/{}", tile.name))
                .absolute(tile.x + 1.0, tile.y + 1.0)
                .size(w, h)
                .radius(3.0)
                .fill(color.alpha(0.22))
                .border(1.0, color.alpha(0.55))
                .help(format!("{} · {} · {} · {}", tile.name, tile.cat, gb(tile.bytes), fmt(tile.bytes)))
                .child(inner.px(5.0).py(4.0)),
        );
    }

    let mut legend = row().w_full().flex_wrap().gaps(4.0, 11.0);
    let mut cats: Vec<(&str, usize)> = Vec::new();
    for p in ps {
        match cats.iter_mut().find(|(c, _)| *c == p.cat) {
            Some((_, n)) => *n += 1,
            None => cats.push((p.cat, 1)),
        }
    }
    cats.sort_by(|a, b| b.1.cmp(&a.1));
    for (cat, n) in cats {
        legend = legend.child(
            row()
                .gap(4.0)
                .items_center()
                .child(el().size(7.0, 7.0).radius(2.0).fill(cat_color(cat)))
                .child(text(format!("{cat} {n}")).font(TextStyle::new(10.0, 400.0)).color(t.text_secondary)),
        );
    }

    col()
        .w_full()
        .gap(8.0)
        .child(el().key("treemap").w_full().h(MAP_H).children(rects))
        .child(legend)
}

// ---- language mix ----

fn lang_bars(ps: &[Project], t: &tok::Theme) -> El {
    let mix = lang_mix(ps);
    let total: u64 = mix.iter().map(|l| l.1).sum();
    let max = mix.first().map(|l| l.1).unwrap_or(1);
    let mut col_ = col().w_full().gap(6.0);
    for (lang, lines) in &mix {
        let pct = if total > 0 { *lines as f64 / total as f64 * 100.0 } else { 0.0 };
        col_ = col_.child(
            row()
                .w_full()
                .gap(6.0)
                .items_center()
                .child(text(lang.clone()).font(TextStyle::new(9.5, 400.0)).color(t.text).w(96.0))
                .child(
                    el()
                        .h(16.0)
                        .grow(*lines as f32 / max as f32)
                        .min_w(2.0)
                        .radius(3.0)
                        .fill(lang_color_or(lang)),
                )
                .child(text(format!("{pct:.1}%")).font(TextStyle::new(8.5, 400.0)).color(t.text_secondary).w(46.0)),
        );
    }
    col_
}

// ---- pulse donut ----

fn pulse_donut(t: &Totals, theme: &tok::Theme) -> El {
    let segments: Vec<(u64, &str)> = vec![
        (t.active as u64, "#22c55e"),
        (t.dormant as u64, "#4f8cff"),
        (t.stale as u64, "#ef4444"),
        (t.no_git as u64, "#64748b"),
    ];
    let total: u64 = segments.iter().map(|s| s.0).sum();
    let (cx, cy, r, ir) = (120.0f32, 95.0f32, 72.0f32, 46.0f32);
    let mut shape = Shape::new();
    let mut ang = -std::f32::consts::FRAC_PI_2;
    if total > 0 {
        for (n, hex) in &segments {
            if *n == 0 {
                continue;
            }
            let a1 = ang + (*n as f32 / total as f32) * std::f32::consts::TAU;
            let color = rune::color::hex(hex).unwrap();
            shape = crate::charts::ring(shape, cx, cy, ir, r, ang, a1, color.alpha(0.8));
            ang = a1;
        }
    }
    let labels = [
        ("built recently", t.active, "#22c55e"),
        ("dormant", t.dormant, "#4f8cff"),
        ("over a year", t.stale, "#ef4444"),
        ("no git", t.no_git, "#64748b"),
    ];
    let mut legend = row().w_full().flex_wrap().gaps(4.0, 11.0);
    for (name, n, hex) in labels {
        legend = legend.child(
            row()
                .gap(4.0)
                .items_center()
                .child(el().size(7.0, 7.0).radius(2.0).fill(rune::color::hex(hex).unwrap()))
                .child(text(format!("{name} {n}")).font(TextStyle::new(10.0, 400.0)).color(theme.text_secondary)),
        );
    }
    col()
        .w_full()
        .gap(8.0)
        .child(
            el()
                .w(240.0)
                .h(190.0)
                .self_center()
                .child(canvas(shape))
                .child(
                    col()
                        .absolute(cx - 40.0, cy - 18.0)
                        .w(80.0)
                        .items_center()
                        .child(text(fmt(t.folders as u64)).font(TextStyle::new(26.0, 700.0)).color(theme.text))
                        .child(text("folders").font(TextStyle::new(9.0, 400.0)).color(theme.text_secondary)),
                ),
        )
        .child(legend)
}

// ---- momentum scatter ----

fn scatter(ps: &[Project], t: &tok::Theme) -> El {
    const L: f32 = 44.0;
    const R: f32 = 14.0;
    const TOP: f32 = 12.0;
    const BOT: f32 = 30.0;
    let iw = INNER_W - L - R;
    let ih = SCATTER_H - TOP - BOT;
    let max_d = 660.0f64;
    let min_loc = 2000.0f64;
    let max_loc = ps.iter().map(|p| p.loc).max().unwrap_or(1).max(2000) as f64;

    let x_of = |days: u64| -> f32 {
        L + (((1.0 + days.min(660) as f64).ln() / (1.0 + max_d).ln()) * iw as f64) as f32
    };
    let y_of = |loc: u64| {
        let l = (loc as f64).max(min_loc);
        TOP as f64 + ih as f64 - ((l.ln() - min_loc.ln()) / (max_loc.ln() - min_loc.ln())) * ih as f64
    };

    let mut parts: Vec<El> = Vec::new();
    for d in [0u64, 7, 30, 90, 180, 365, 660] {
        let x = x_of(d) as f32;
        parts.push(el().absolute(x, TOP).w(1.0).h(ih).fill(t.separator));
        parts.push(
            text(if d == 660 { "660+".to_string() } else { d.to_string() })
                .font(TextStyle::new(9.0, 400.0))
                .color(t.text_secondary)
                .absolute(x - 12.0, TOP + ih + 4.0)
                .w(24.0)
                .align(Align::Center),
        );
    }
    for v in [5000u64, 20_000, 50_000, 150_000, 700_000] {
        if v as f64 > max_loc {
            continue;
        }
        let y = y_of(v) as f32;
        parts.push(el().absolute(L, y).w(iw).h(1.0).fill(t.separator));
        parts.push(
            text(if v >= 1000 { format!("{}k", v / 1000) } else { v.to_string() })
                .font(TextStyle::new(9.0, 400.0))
                .color(t.text_secondary)
                .absolute(0.0, y - 5.0)
                .w(L - 6.0)
                .align(Align::End),
        );
    }
    // One-year-stale line.
    let sx = x_of(365) as f32;
    parts.push(el().absolute(sx, TOP).w(1.0).h(ih).fill(rune::color::hex("#ef4444").unwrap().alpha(0.6)));
    parts.push(
        text("1 year stale")
            .font(TextStyle::new(8.0, 400.0))
            .color(rune::color::hex("#ef4444").unwrap())
            .absolute(sx + 4.0, TOP)
            .opacity(0.85),
    );
    parts.push(
        text("days since last commit")
            .font(TextStyle::new(9.0, 400.0))
            .color(t.text_secondary)
            .absolute(L, SCATTER_H - 14.0)
            .w(iw)
            .align(Align::Center),
    );

    let mut ranked: Vec<&Project> = ps.iter().filter(|p| p.days.is_some()).collect();
    ranked.sort_by(|a, b| b.loc.cmp(&a.loc));
    for p in ranked {
        let days = p.days.unwrap_or(0);
        let r = ((gib(p.bytes) as f64).sqrt() * 3.4 + 1.8).clamp(2.6, 14.0) as f32;
        let (x, y) = (x_of(days) as f32, y_of(p.loc) as f32);
        let color = cat_color(p.cat);
        parts.push(
            el()
                .key(format!("dot/{}", p.name))
                .absolute(x - r, y - r)
                .size(2.0 * r, 2.0 * r)
                .radius(r)
                .fill(color.alpha(0.45))
                .border(1.0, color.alpha(0.8))
                .help(format!("{} — {}, {} LOC, {}d ago", p.name, gb(p.bytes), fmt(p.loc), days)),
        );
    }

    el().key("scatter").w_full().h(SCATTER_H).children(parts)
}

// ---- composition ----

fn composition(ps: &[Project], t: &tok::Theme) -> El {
    let mut top: Vec<&Project> = ps.iter().collect();
    top.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    top.truncate(10);
    let _max = top.first().map(|p| p.bytes).unwrap_or(1).max(1) as f32;
    let parts: [(&str, &str); 4] = [
        ("src", "#4f8cff"),
        ("nm", "#f59e0b"),
        ("target", "#ef4444"),
        ("other", "#64748b"),
    ];
    let mut rows = col().w_full().gap(5.0);
    for p in &top {
        let mut seg = row().h(16.0).grow(1.0).gap(1.0);
        for (key, hex) in parts {
            let bytes = match key {
                "src" => p.src,
                "nm" => p.nm,
                "target" => p.target,
                _ => p.other,
            };
            if bytes == 0 {
                continue;
            }
            seg = seg.child(
                el()
                    .h(16.0)
                    .grow(bytes as f32)
                    .min_w(1.0)
                    .radius(2.0)
                    .fill(rune::color::hex(hex).unwrap().alpha(0.85))
                    .help(format!("{} · {key}: {:.2} GiB", p.name, gib(bytes))),
            );
        }
        rows = rows.child(
            row()
                .w_full()
                .gap(6.0)
                .items_center()
                .child(text(p.name.clone()).font(TextStyle::new(9.5, 400.0)).color(t.text).w(150.0))
                .child(seg)
                .child(text(format!("{:.1}", gib(p.bytes))).font(TextStyle::new(9.0, 400.0)).color(t.text_secondary).w(44.0)),
        );
    }
    let total = |key: &str| ps.iter().map(|p| match key { "src" => p.src, "nm" => p.nm, "target" => p.target, _ => p.other }).sum::<u64>();
    let mut legend = row().w_full().flex_wrap().gaps(4.0, 11.0);
    for (key, hex) in parts {
        legend = legend.child(
            row()
                .gap(4.0)
                .items_center()
                .child(el().size(7.0, 7.0).radius(2.0).fill(rune::color::hex(hex).unwrap()))
                .child(text(format!("{key} {:.1} GiB", gib(total(key)))).font(TextStyle::new(10.0, 400.0)).color(t.text_secondary)),
        );
    }
    col().w_full().gap(8.0).child(rows).child(legend)
}

// ---- insights ----

fn insights(ps: &[Project], t: &Totals, theme: &tok::Theme) -> El {
    let dirtiest = {
        let mut v: Vec<&Project> = ps.iter().collect();
        v.sort_by(|a, b| b.dirty.cmp(&a.dirty));
        v.truncate(3);
        v.iter().map(|p| format!("{} ({})", p.name, p.dirty)).collect::<Vec<_>>().join(", ")
    };
    let top_lang = lang_mix(ps).first().cloned();
    let biggest = ps.iter().max_by_key(|p| p.loc).cloned();
    let rebuildable = gib(t.nm + t.target + t.other);

    let cards: Vec<(String, String)> = vec![
        (
            format!("Only {} are alive", t.active),
            format!(
                "{} of {} repos touched in 30 days. {} dormant, {} past a year.",
                t.active, t.repos, t.dormant, t.stale
            ),
        ),
        (
            format!("~{rebuildable:.0} GiB is rebuildable"),
            format!(
                "{:.0} GiB target, {:.1} GiB node_modules, {:.0} GiB other build output.",
                gib(t.target),
                gib(t.nm),
                gib(t.other)
            ),
        ),
        (
            "One folder dwarfs the rest".to_string(),
            match &biggest {
                Some(p) if t.loc > 0 => format!(
                    "{} is {} lines — {:.0}% of all code here.",
                    p.name,
                    fmt(p.loc),
                    p.loc as f64 / t.loc as f64 * 100.0
                ),
                Some(p) => format!("{} is the biggest codebase, with {} lines.", p.name, fmt(p.loc)),
                None => "Nothing scanned yet.".to_string(),
            },
        ),
        (
            "What the code is written in".to_string(),
            match &top_lang {
                Some((lang, lines)) if t.loc > 0 => format!(
                    "{lang} leads with {} lines — {:.0}% of all code here.",
                    fmt(*lines),
                    *lines as f64 / t.loc as f64 * 100.0
                ),
                Some((lang, lines)) => format!("{lang} leads with {} lines.", fmt(*lines)),
                None => "Nothing scanned yet.".to_string(),
            },
        ),
        (
            format!("{} dirty files", fmt(t.dirty)),
            format!("Uncommitted work, mostly in {dirtiest}."),
        ),
        (
            if t.no_git == 1 { "1 folder has no git".to_string() } else { format!("{} folders have no git", t.no_git) },
            "Worth a `git init` before history is lost.".to_string(),
        ),
    ];

    let mut rows: Vec<El> = Vec::new();
    for pair in cards.chunks(2) {
        let mut r = row().w_full().gap(8.0);
        for (head, body) in pair {
            r = r.child(
                col()
                    .grow(1.0)
                    .fill(theme.card)
                    .radius(8.0)
                    .px(11.0)
                    .py(10.0)
                    .gap(3.0)
                    .child(text(head.clone()).font(TextStyle::new(11.5, 500.0)).color(theme.text))
                    .child(text(body.clone()).font(TextStyle::new(10.5, 400.0)).color(theme.text_secondary)),
            );
        }
        rows.push(r);
    }
    col().w_full().gap(8.0).children(rows)
}

// ---- view ----

pub fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let tt = totals(&s.projects);

    let kpis = [
        kpi(&format!("{:.1} GiB", gib(tt.bytes)), "on disk", &format!("{} files", fmt(tt.files)), &t),
        kpi(&format!("{:.2}M", tt.loc as f64 / 1e6), "lines of code", "vendored excluded", &t),
        kpi(&fmt(tt.commits), "commits", &format!("{} git repos", tt.repos), &t),
        kpi(&fmt(tt.folders as u64), "folders", &format!("{} without git", tt.no_git), &t),
        kpi(&fmt(tt.dirty), "uncommitted files", "work in progress", &t),
        kpi(&fmt(tt.active as u64), "active in 30 days", &format!("{} dormant, {} stale", tt.dormant, tt.stale), &t),
    ];
    let kpi_rows = row().w_full().gap(8.0).children(kpis.iter().take(3).cloned().collect::<Vec<El>>());
    let kpi_rows2 = row().w_full().gap(8.0).children(kpis.iter().skip(3).cloned().collect::<Vec<El>>());

    let content = col()
        .w(CONTENT_W)
        .gap(14.0)
        .child(header(s, &t))
        .child(kpi_rows)
        .child(kpi_rows2)
        .child(card(&t, "Disk footprint", "Area = bytes on disk, colour = project type.", treemap(&s.projects, &t)))
        .child(
            row()
                .w_full()
                .gap(10.0)
                .child(col().grow(1.4).child(card(&t, "Language mix", "Hand-written lines, vendored and minified code excluded.", lang_bars(&s.projects, &t))))
                .child(col().grow(1.0).child(card(&t, "Project pulse", "How recently each folder was touched.", pulse_donut(&tt, &t)))),
        )
        .child(card(&t, "Momentum map", "Left = recently touched, up = more code, bubble size = disk footprint. Dashed line = one year stale.", scatter(&s.projects, &t)))
        .child(card(&t, "What the bytes actually are", "Top 10 folders: real content vs regenerable build junk.", composition(&s.projects, &t)))
        .child(insights(&s.projects, &tt, &t))
        .child(caption("Auto-rescans every 5 minutes · built with Rune", t.text_secondary));

    col()
        .w_full()
        .h_full()
        .fill(t.bg)
        .child(
            col()
                .key("page")
                .w_full()
                .h_full()
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .items_center()
                .py(20.0)
                .child(content),
        )
}
