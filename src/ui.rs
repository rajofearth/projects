//! The window, ported from the upstream interface's `app.tsx` (MIT).
//!
//! Everything is one monospace face at one size, on black, with no borders and no panels:
//! hierarchy comes from colour and weight alone. The page is a top line (what the app is and
//! what the scan is doing), the totals, the dot-matrix meter, then two columns — the category
//! legend with its reclaimable summary, and the detail pane where folders open — and a bottom
//! line carrying the actions that apply right now.
//!
//! The one thing dropped from the port is the terminal: there are no key hints and no key
//! routing. Every action is a control under the pointer, and the signature "press again"
//! confirmation is gone with it, since nothing here deletes anything.

use rune::color::srgb_decode;
use rune::{el, row, text, col, Align, Color, Cursor, Cx, El, ScrollSpec, SurfaceFit, TextStyle};

use crate::browse::{Culprit, Item};
use crate::cats::{self, Cat, Label, Tier, Verdict, CATEGORIES};
use crate::model;
use crate::meter::Meter;
use crate::store;
use crate::State;

/// One terminal cell's worth of text: the only size on screen.
pub const FS: f32 = 12.0;
/// The meter's dot pitch, in points.
pub const CELL: f32 = 7.5;
/// Rows the detail pane's list draws before it stops.
const DOTS: usize = 16;

// The table's column widths, in points. The header and every row read these same numbers,
// so a heading can never drift off the column it names.
const MARK_W: f32 = FS * 1.2;

const ARROW_W: f32 = FS * 1.2;
const VERDICT_W: f32 = FS * 7.0;
const SIZE_W: f32 = FS * 8.0;
const PCT_W: f32 = FS * 3.0;
const AGE_W: f32 = FS * 6.0;
const GAP: f32 = FS * 0.9;
/// The preview strip under the list: tall enough for verdict, blame and actions.

/// The port's palette, decoded once per view.
struct Ink {
    fg: Color,
    muted: Color,
    faint: Color,
    red: Color,
    green: Color,
    amber: Color,
    row: Color,
    /// A row under the pointer, and the row that is picked: the two mouse-only cues.
    hover: Color,
    picked: Color,
}

fn rgb(v: [u8; 3]) -> Color {
    Color {
        r: srgb_decode(v[0] as f32 / 255.0),
        g: srgb_decode(v[1] as f32 / 255.0),
        b: srgb_decode(v[2] as f32 / 255.0),
        a: 1.0,
    }
}

impl Ink {
    fn new() -> Self {
        Self {
            fg: rgb([230, 230, 230]),
            muted: rgb([140, 140, 140]),
            faint: rgb([108, 108, 108]),
            red: rgb([248, 113, 113]),
            green: rgb([74, 222, 128]),
            amber: rgb([251, 191, 36]),
            row: rgb([181, 181, 181]),
            hover: rgb([22, 22, 24]),
            picked: rgb([30, 30, 33]),
        }
    }
}

fn color_of(rgb8: cats::Rgb) -> Color {
    rgb(rgb8)
}

fn tint(rgb8: cats::Rgb, alpha: f32) -> Color {
    let mut c = color_of(rgb8);
    c.a = alpha.clamp(0.0, 1.0);
    c
}

fn mono(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight).mono()
}

/// Width of n monospace characters at the one size on screen.
fn ch(n: f32) -> f32 {
    FS * 0.6 * n
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0%".to_string()
    } else {
        format!("{}%", (part as f64 / whole as f64 * 100.0).round() as u64)
    }
}

/// A small bordered control: the pointer-first replacement for the port's key hints.
fn chip(
    label: impl Into<String>,
    c: Color,
    ink: cats::Rgb,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    row()
        .px(9.0)
        .py(4.0)
        .radius(7.0)
        .border(1.0, tint(ink, 0.32))
        .fill(Color::BLACK)
        .hover(tint(ink, 0.16))
        .cursor(Cursor::Pointer)
        .items_center()
        .on_press(on_press)
        .child(text(label).font(mono(FS, 400.0)).color(c))
}

/// A run of plain monospace text: the only kind of text on screen.
fn t(content: impl Into<String>, c: Color) -> El {
    text(content).font(mono(FS, 400.0)).color(c)
}

fn tb(content: impl Into<String>, c: Color) -> El {
    text(content).font(mono(FS, 700.0)).color(c)
}

fn sep(ink: &Ink) -> El {
    t("·", ink.faint)
}

/// The "you are here" dot in front of a filter row. It keeps its width when unlit, so every
/// row's marker and name start at the same place whether or not the row is the current one.
fn marker(lit: bool, color: Color) -> El {
    row()
        .w(ch(1.0))
        .shrink()
        .child(tb(if lit { "·" } else { "" }, color))
}

// ---- the dot sparkline ----------------------------------------------------

fn dot_hash(a: usize, b: usize) -> f32 {
    let mut h = (a as u32)
        .wrapping_add(374_761_393)
        .wrapping_mul(668_265_263)
        ^ (b as u32)
            .wrapping_add(1_274_126_177)
            .wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(3_266_489_917);
    ((h ^ (h >> 16)) as f32) / 4_294_967_296.0
}

/// A small dot-matrix bar, the same pixel language as the big meter: lit dots in the category
/// colour with the odd white one, unlit dots for the rest.
fn dots(fraction: f32, color: cats::Rgb, count: usize, seed: usize, dim: bool) -> El {
    let dot = (FS * 0.26).max(2.0).round();
    let gap = (FS * 0.14).max(1.0).round();
    let lit = if fraction > 0.0 {
        ((fraction.clamp(0.0, 1.0) * count as f32).round() as usize).max(1)
    } else {
        0
    };
    row()
        .gap(gap)
        .items_center()
        .children((0..count).map(move |i| {
            let on = i < lit;
            let k = dot_hash(i + seed * 31, seed);
            let fill = if !on {
                rgb([28, 28, 28])
            } else if k > 0.9 && !dim {
                Color::WHITE
            } else {
                tint(color, if dim { 0.35 } else { 0.55 + k * 0.45 })
            };
            el().size(dot, dot).radius(dot / 2.0).fill(fill)
        }))
}

// ---- top line -------------------------------------------------------------

fn clip_path(path: &str, max_chars: usize) -> String {
    // Long absolute paths keep their tail, which is the part that says what is being read.
    let count = path.chars().count();
    if count <= max_chars {
        return path.to_string();
    }
    let tail: String = path.chars().skip(count - max_chars + 1).collect();
    format!("…{tail}")
}

fn top_line(s: &State, ink: &Ink) -> El {
    let status = if s.scanning {
        let what = if s.total > 0 {
            format!("measuring {} / {}", s.done_count, s.total)
        } else {
            "starting scan".to_string()
        };
        row()
            .items_center()
            .gap(ch(1.0))
            .child(t(what, ink.fg))
            .child(t(clip_path(&s.current, 56), ink.faint).truncate())
    } else if let Some(error) = &s.error {
        t(error.clone(), ink.red).truncate()
    } else {
        let secs = if s.scan_secs >= 0.05 {
            format!(" in {:.1}s", s.scan_secs)
        } else {
            String::new()
        };
        t(
            format!(
                "scan complete · {} folders{}",
                s.projects.len(),
                secs
            ),
            ink.faint,
        )
        .truncate()
    };

    row()
        .key("topline")
        .w_full()
        .items_center()
        .gap(ch(2.0))
        .child(
            row()
                .gap(ch(2.0))
                .items_center()
                .child(tb("projects-stats", ink.fg))
                .child(t(clip_path(&format!("{}", s.ui.root().display()), 40), ink.faint))
                .children((s.demo).then(|| t("demo disk", ink.amber))),
        )
        .child(el().grow(1.0))
        .child(status)
        .child(chip("Rescan", ink.fg, [230, 230, 230], |s: &mut State| {
            State::request_scan(s)
        }))
}

// ---- totals ---------------------------------------------------------------

fn totals(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    let total: u64 = sizes.iter().sum();
    let rebuildable = sizes[1] + sizes[2];
    let known = total > 0;
    let figure = |label: &str, value: String| -> El {
        row()
            .items_center()
            .child(tb(if known { value } else { "…".into() }, ink.fg))
            .child(t(format!(" {label}"), ink.muted))
    };
    row()
        .key("totals")
        .w_full()
        .items_center()
        .gap(ch(1.2))
        .child(figure("under the root", model::gb(total)))
        .child(sep(ink))
        .child(sep(ink))
        .child(tb(model::gb(rebuildable), ink.green))
        .child(t(" rebuildable", ink.muted))
        .child(sep(ink))
        .child(tb(model::gb(sizes[0]), ink.fg))
        .child(t(" source", ink.muted))
        .child(el().grow(1.0))
        .child(match s.ui.hover {
            // Hovering the meter names the band under the pointer, which is what a mouse
            // user needs to know before clicking a segment.
            Some(cat) => {
                let size = s.ui.category_sizes(&s.projects)[store::cat_index(cat)];
                row()
                    .key("meter-readout")
                    .items_center()
                    .gap(ch(0.6))
                    .child(t("■", color_of(cat.color())))
                    .child(tb(cat.name(), ink.fg))
                    .child(t(model::gb(size), ink.muted))
                    .child(t(pct(size, total), ink.faint))
                    .child(t("· click to open", ink.faint))
            }
            None => row()
                .key("engine")
                .children((!s.engine_line().is_empty()).then(|| {
                    // A bare truncating run shrinks to nothing in a row that is already
                    // full, so the gauge gets a floor wide enough to show its reading.
                    t(s.engine_line(), ink.faint).w(ch(26.0)).align(Align::End).truncate()
                })),
        })
}

// ---- meter ----------------------------------------------------------------

fn meter(s: &State, _cx: &mut Cx<State>) -> El {
    el()
        .key("meter")
        .w_full()
        .h(Meter::height(CELL))
        .native_surface(s.meter.surface(), SurfaceFit::Fill)
        .surface_anchor(Align::Start, Align::Start)
        .cursor(Cursor::Pointer)
        .on_layout(|s: &mut State, r: [f32; 4]| s.meter_resize(r[2], r[3]))
        .on_pointer_move_local(|s: &mut State, p, _| {
            let hit = s.meter_hit(p.x, p.y);
            s.ui.set_hover(hit);
        })
        .on_hover_change(|s: &mut State, inside, _| {
            if !inside {
                s.ui.set_hover(None);
            }
        })
        .on_press(|s: &mut State| {
            if let Some(cat) = s.ui.hover {
                s.pick(cat);
            }
        })
}

// ---- left column: categories ----------------------------------------------

fn category_line(s: &State, ink: &Ink, category: &cats::Category, sizes: &[u64; 4], largest: u64) -> El {
    let selected = s.ui.filter.cat() == Some(category.id);
    let dimmed = s.ui.hover.is_some() && s.ui.hover != Some(category.id) && !selected;
    let bytes = sizes[store::cat_index(category.id)];
    let fraction = if largest > 0 {
        bytes as f32 / largest as f32
    } else {
        0.0
    };
    let id = category.id;
    row()
        .key(format!("cat/{}", category.id.label()))
        .w_full()
        .h(FS * 2.0)
        .px(ch(0.5))
        .radius(4.0)
        .items_center()
        .gap(ch(0.6))
        .cursor(Cursor::Pointer)
        .fill(if selected { ink.picked } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_hover_change(move |s: &mut State, inside, _| {
            if inside {
                s.ui.set_hover(Some(id));
            } else if s.ui.hover == Some(id) {
                s.ui.set_hover(None);
            }
        })
        .on_press(move |s: &mut State| s.pick(id))
        .child(marker(selected, color_of(category.color)))
        .child(t("■", color_of(category.color)).w(ch(1.0)))
        .child(
            text(category.name)
                .font(mono(FS, if selected { 700.0 } else { 400.0 }))
                .color(if selected {
                    ink.fg
                } else if dimmed {
                    ink.faint
                } else {
                    ink.row
                })
                .w(ch(11.0)),
        )
        .child(
            text(model::gb(bytes))
                .font(mono(FS, 400.0))
                .color(if selected { ink.fg } else { ink.muted })
                .w(ch(9.0))
                .align(Align::End),
        )
        .child(dots(
            fraction,
            category.color,
            14,
            category.id.label().len() * 7,
            dimmed,
        ))
}

/// The one thing to do next in this view: how much space a reinstall or rebuild gives back,
/// and the single folder worth clearing first. A press on that folder shows it in the file
/// manager; the app never deletes anything itself.
fn reclaim_line(s: &State, ink: &Ink) -> El {
    let Some((item, label)) = s.ui.biggest_reclaimable(&s.projects) else {
        return col()
            .key("reclaim")
            .w_full()
            .gap(ch(0.2))
            .child(section_label("space you can free", ink))
            .child(t("nothing rebuildable in this view", ink.faint));
    };
    let (count, bytes) = s.ui.reclaim_summary(&s.projects);
    col()
        .key("reclaim")
        .w_full()
        .gap(ch(0.3))
        .child(section_label("space you can free", ink))
        .child(
            row()
                .items_center()
                .child(tb(model::gb(bytes), ink.green))
                .child(t(" reclaimable", ink.muted)),
        )
        .child(
            t(
                format!(
                    "{} {} here can be deleted and rebuilt",
                    count,
                    if count == 1 { "folder" } else { "folders" }
                ),
                ink.faint,
            )
            .truncate(),
        )
        .child(section_label("biggest win", ink).pt(ch(0.5)))
        .child(
            row()
                .w_full()
                .items_center()
                .gap(ch(0.6))
                .cursor(Cursor::Pointer)
                .hover(ink.hover)
                .radius(4.0)
                .on_press(|s: &mut State| s.reveal())
                .child(t(format!("→ {}", item.name), ink.amber).grow(1.0).min_w(0.0).truncate())
                .child(t(model::gb(item.size), ink.muted).shrink()),
        )
        .children(
            // Why clearing it is safe: "npm install brings it back".
            (!label.reason.is_empty()).then(|| t(label.reason.clone(), ink.faint).pl(ch(2.0)).truncate()),
        )
}

/// The whole disk's rebuildable bytes, split by what brings them back. A press filters the
/// list to that category.
fn quick_wins(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    if s.scanning && s.projects.is_empty() {
        return t("looking for space you can get back…", ink.faint);
    }
    let row_for = |cat: Cat, label: &str, how: &str, bytes: u64| -> El {
        let id = cat;
        row()
            .key(format!("win/{}", id.label()))
            .w_full()
            .items_center()
            .gap(ch(0.6))
            .cursor(Cursor::Pointer)
            .hover(ink.hover)
            .radius(4.0)
            .on_press(move |s: &mut State| s.pick(id))
            .child(t("■", color_of(id.color())))
            .child(t(label.to_string(), ink.muted).w(ch(12.0)))
            .child(t(model::gb(bytes), ink.muted).w(ch(9.0)).align(Align::End))
            .child(t(how.to_string(), ink.faint).pl(ch(0.4)).truncate())
    };
    col()
        .gap(ch(0.3))
        .child(section_label("across the whole disk", ink).pt(ch(0.5)))
        .child(row_for(Cat::Deps, "dependencies", "reinstall", sizes[1]))
        .child(row_for(Cat::Build, "build output", "rebuild", sizes[2]))
}

/// The whole disk in one row: the default view, and the way back out of any filter.
/// It carries the same parts as a category row (marker, square, name, size, dot bar) so it
/// reads as one more option in the column rather than a heading above them. Its bar is full
/// width because every byte on the disk is what it stands for, and it has no hue of its own:
/// nothing is a category here, so the square is the neutral foreground.
fn all_line(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    let active = s.ui.filter.cat().is_none();
    let total: u64 = sizes.iter().sum();
    row()
        .key("filter/all")
        .w_full()
        .h(FS * 2.0)
        .px(ch(0.5))
        .radius(4.0)
        .items_center()
        .gap(ch(0.6))
        .cursor(Cursor::Pointer)
        .fill(if active { ink.picked } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_hover_change(|s: &mut State, inside, _| {
            // Hovering the whole-disk row names no band, so the meter and the category rows
            // stop dimming: nothing is being singled out.
            if inside {
                s.ui.set_hover(None);
            }
        })
        .on_press(|s: &mut State| s.filter_to(store::Filter::All))
        .child(marker(active, ink.fg))
        .child(t("■", if active { ink.fg } else { ink.muted }).w(ch(1.0)))
        .child(
            text("All projects")
                .font(mono(FS, if active { 700.0 } else { 400.0 }))
                .color(if active { ink.fg } else { ink.row })
                .w(ch(11.0)),
        )
        .child(
            text(model::gb(total))
                .font(mono(FS, if active { 700.0 } else { 400.0 }))
                .color(if active { ink.fg } else { ink.muted })
                .w(ch(9.0))
                .align(Align::End),
        )
        .child(dots(1.0, [230, 230, 230], 14, 0, !active))
}

fn categories(s: &State, ink: &Ink, sizes: &[u64; 4], width: f32) -> El {
    let largest = sizes.iter().copied().max().unwrap_or(0);
    col()
        .key("categories")
        .w(width)
        .shrink()
        .gap(ch(0.2))
        // What the highlighted row is comes first, the way the sidebar in the design leads
        // with the thing being looked at rather than the list of views.
        .child(details(s, ink))
        .child(
            t("categories", ink.faint)
                .pl(ch(2.0))
                .pt(ch(1.1))
                .pb(ch(0.4)),
        )
        .child(all_line(s, ink, sizes))
        .children(
            CATEGORIES
                .iter()
                .map(|c| category_line(s, ink, c, sizes, largest)),
        )
        .child(el().grow(1.0))
        .child(
            col()
                .gap(ch(0.5))
                .pl(ch(1.2))
                .child(reclaim_line(s, ink))
                .child(quick_wins(s, ink, sizes)),
        )
}

// ---- right column: what is inside -----------------------------------------

/// One crumb in the path: every level but the last is a target that jumps straight to it.
fn crumb(
    label: String,
    color: Color,
    ink: &Ink,
    active: bool,
    level: usize,
) -> El {
    let hint = label.clone();
    // The handler sits on a box around the text, not on the text: a run of text is a
    // selection target first, and a press on one is easier to lose than one on a box.
    let mut cell = row()
        .key(format!("crumb/{level}"))
        .items_center()
        // Shrink to the label: a crumb that stretched would swallow its neighbours' clicks.
        .shrink()
        .h(FS * 1.7)
        .px(ch(0.45))
        .radius(4.0)
        .child(
            text(label)
                .font(mono(FS, if active { 700.0 } else { 400.0 }))
                .color(color),
        );
    if active {
        // Where you are: a surface, not an arrow. Not a target.
        cell = cell.fill(ink.picked);
    } else {
        cell = cell
            .cursor(Cursor::Pointer)
            .hover(ink.hover)
            .on_press(move |s: &mut State| s.jump(level))
            .help(format!("Go up to {hint}"));
    }
    cell
}

fn breadcrumb(s: &State, ink: &Ink) -> El {
    // The trail starts at whatever the list is showing: every project, or one category.
    let accent = s.ui.filter.color();
    let depth = s.ui.stack.len();
    let mut bar = row().items_center().shrink().gap(ch(0.15));
    bar = bar.child(crumb(
        s.ui.filter.name().to_string(),
        color_of(accent),
        ink,
        depth == 0,
        0,
    ));
    for (i, level) in s.ui.stack.iter().enumerate() {
        let last = i + 1 == depth;
        bar = bar.child(t("/", ink.faint).px(ch(0.3)));
        bar = bar.child(crumb(
            format!("{}{}", level.name, if level.dir { "/" } else { "" }),
            if last { color_of(accent) } else { ink.muted },
            ink,
            last,
            i + 1,
        ));
    }
    bar
}

fn inside_header(s: &State, ink: &Ink, total: u64) -> El {
    let category = s.ui.filter.cat().unwrap_or(cats::Cat::Source).category();
    let scope = s.ui.scope_size(&s.projects);
    row()
        .w_full()
        .items_center()
        .gap(ch(2.0))
        // The trail comes first and keeps its own width; the summary gives way to it.
        .child(breadcrumb(s, ink))
        .child(el().grow(1.0))
        .child(
            row()
                .items_center()
                .gap(ch(2.0))
                .shrink()
                .child(tb(model::gb(scope), ink.fg))
                .child(t(format!("{} of disk", pct(scope, total)), ink.muted))
                .child(t(
                    cats::tier_note(category.tier),
                    match category.tier {
                        Tier::Safe => ink.green,
                        Tier::Review => ink.amber,
                        Tier::Keep => ink.muted,
                    },
                )),
        )
}

fn blurb_line(s: &State, ink: &Ink) -> El {
    let line = match s.ui.stack.last() {
        Some(level) => level.path.display().to_string(),
        None => match s.ui.filter.cat() {
            Some(cat) => cat.category().blurb.to_string(),
            None => "Every project under this folder. Pick a category to narrow it down."
                .to_string(),
        },
    };
    row()
        .w_full()
        .items_center()
        .gap(ch(2.0))
        .child(t(line, ink.faint).truncate())
        .child(el().grow(1.0))
        .children(
            s.ui.listing
                .then(|| t("measuring…", ink.muted).shrink()),
        )
}

fn project_age(s: &State, item: &Item) -> Option<u64> {
    let name = if s.ui.stack.is_empty() {
        item.name.as_str()
    } else {
        s.ui.stack[0].name.as_str()
    };
    s.projects
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.days)
}

fn item_line(ctx: &Ctx, item: &Item, index: usize) -> El {
    let s = ctx.s;
    let ink = ctx.ink;
    let selected = s.ui.row == index;
    let category = s.ui.filter.cat().unwrap_or(cats::Cat::Source).category();
    let label = s.ui.label(item, &s.projects);
    let days = project_age(s, item);
    let stale = days.is_some_and(|d| d > 180);
    let scope = s.ui.scope_size(&s.projects).max(1);
    let share = s.ui.share(item, &s.projects);
    let disk = s.ui.disk_size(&s.projects).max(1);
    let disk_share = item.size as f32 / disk as f32;

    row()
        .key(format!("row/{index}"))
        .w_full()
        .h(FS * 2.0)
        .px(ch(0.5))
        .radius(4.0)
        .items_center()
        .gap(GAP)
        .cursor(Cursor::Pointer)
        // A picked row keeps a surface, so the selection is visible without a keystroke.
        .fill(if selected { ink.picked } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_press(move |s: &mut State| {
            s.ui.set_row(index);
        })
        .on_double_press(|s: &mut State| s.open())
        .child(marker(selected, color_of(category.color)))
        .child(
            // The disclosure arrow is its own target: one click opens the folder, the way a
            // tree in a file manager does.
            t(
                if item.dir { "▸" } else { "" },
                if item.dir {
                    color_of(category.color)
                } else {
                    ink.faint
                },
            )
            .w(ARROW_W)
            .cursor(if item.dir {
                Cursor::Pointer
            } else {
                Cursor::Default
            })
            .hover(color_of(category.color))
            .on_press(move |s: &mut State| {
                s.ui.set_row(index);
                s.open();
            }),
        )
        .child(
            // The arrow already marks a folder, and every row shares the parent the header
            // shows, so the name stands alone: no trailing slash, no repeated path.
            text(item.name.clone())
                .font(mono(FS, if selected { 700.0 } else { 400.0 }))
                .color(if selected { ink.fg } else { ink.row })
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(
            t(label.verdict.tag(), color_of(label.verdict.color())).w(VERDICT_W),
        )
        .child(
            text(model::gb(item.size))
                .font(mono(FS, if selected { 700.0 } else { 400.0 }))
                .color(if selected { ink.fg } else { ink.muted })
                .w(SIZE_W)
                .align(Align::End),
        )
        .child(
            // Every row's dots share one denominator, the whole disk, so a column of dots
            // reads as an amount of data rather than as a per-category ranking.
            dots(
                disk_share,
                if label.verdict == Verdict::Yours {
                    category.color
                } else {
                    label.verdict.color()
                },
                DOTS,
                index + 3,
                !selected,
            ),
        )
        .child(
            text(pct(share, scope))
                .font(mono(FS, 400.0))
                .color(if selected { ink.muted } else { ink.faint })
                .w(PCT_W),
        )
        .child(
            text(days.map(cats::short_age).unwrap_or_default())
                .font(mono(FS, 400.0))
                .color(if stale { ink.amber } else if selected { ink.muted } else { ink.faint })
                .w(AGE_W)
                .align(Align::End),
        )
        // The pointer tooltip says the same thing the card says, in one line: what this is,
        // how big, and what that means. It used to end in "last commit known", which repeated
        // the age column beside it without telling the reader anything.
        .help(format!(
            "{} · {}{}",
            item.name,
            model::gb(item.size),
            if label.reason.is_empty() {
                String::new()
            } else {
                format!(" · {}", label.reason)
            },
        ))
}

/// A faint lowercase heading, the same voice as "categories" in the sidebar.
fn section_label(text_: &str, ink: &Ink) -> El {
    t(text_, ink.faint).pb(ch(0.2))
}

/// Where the folder's space sits, drawn like a category row: name, size, share and a dot
/// bar in the verdict's colour, so it reads as part of the sidebar rather than a footnote.
fn culprit_line(ink: &Ink, item: &Item, culprit: &Culprit, color: cats::Rgb) -> El {
    let Some(last) = culprit.chain.last() else {
        if culprit.top.len() <= 1 {
            return el();
        }
        // No single thing dominates: name the three biggest instead.
        let spread = culprit
            .top
            .iter()
            .map(|t| format!("{} {}", t.name, model::gb(t.size)))
            .collect::<Vec<_>>()
            .join(" · ");
        return col()
            .w_full()
            .gap(ch(0.2))
            .child(section_label("spread across", ink))
            .child(t(spread, ink.muted).truncate());
    };
    let whole = culprit.root_size.max(item.size).max(last.size);
    let fraction = if whole > 0 {
        last.size as f32 / whole as f32
    } else {
        0.0
    };
    // Bucket names from the scan already end in a slash; a real folder needs one added.
    let name = if last.dir && !last.name.ends_with('/') {
        format!("{}/", last.name)
    } else {
        last.name.clone()
    };
    col()
        .w_full()
        .gap(ch(0.3))
        .child(section_label("most of it is in", ink))
        .child(
            row()
                .w_full()
                .items_center()
                .gap(ch(0.6))
                .child(t(name, ink.row).grow(1.0).min_w(0.0).truncate())
                .child(t(model::gb(last.size), ink.muted).shrink())
                .child(t(pct(last.size, whole), ink.faint).w(ch(4.0)).align(Align::End)),
        )
        .child(dots(fraction, color, 14, last.name.len() * 7, false))
}

/// What the highlighted row is, where its space is, and what you can do with it. It is
/// laid out like the rest of the sidebar: flat on the background, a faint heading, bold
/// name and figure, a coloured square for the verdict, dot bars and the shared chips.
fn item_details(ctx: &Ctx, item: &Item) -> El {
    let s = ctx.s;
    let ink = ctx.ink;
    let label = s.ui.label(item, &s.projects);
    let culprit = s.ui.culprit(item, &s.projects);
    let pending = s.ui.culprit_pending(item);
    let can_open = item.dir && s.ui.stack.len() < 12;
    let has_culprit = culprit.as_ref().is_some_and(|c| c.chain.len() > 1);

    let category = s.ui.filter.cat().unwrap_or(cats::Cat::Source).category();
    let share = s.ui.share(item, &s.projects);
    let verdict_rgb = label.verdict.color();

    col()
        .key("detail")
        .w_full()
        .gap(ch(0.5))
        .pl(ch(2.0))
        .pr(ch(0.5))
        .child(section_label("selected", ink))
        .child(
            row()
                .w_full()
                .items_center()
                .gap(ch(0.6))
                .child(
                    tb(
                        format!("{}{}", item.name, if item.dir { "/" } else { "" }),
                        ink.fg,
                    )
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
                )
                .child(tb(model::gb(item.size), ink.fg).shrink()),
        )
        .children(
            // Which part of the folder is being counted, when the list is filtered to one
            // category and the row's number is bigger than that category's slice.
            (s.ui.stack.is_empty() && share != item.size).then(|| {
                row()
                    .items_center()
                    .gap(ch(0.6))
                    .child(t("■", color_of(category.color)))
                    .child(
                        t(
                            format!("{} in {}", model::gb(share), category.name),
                            ink.muted,
                        )
                        .truncate(),
                    )
            }),
        )
        .children(verdict_block(&label, color_of(verdict_rgb), ink))
        .children(pending.then(|| t("finding what is taking the space…", ink.faint)))
        .children(
            culprit
                .as_ref()
                .filter(|c| !c.chain.is_empty() || c.top.len() > 1)
                .map(|c| culprit_line(ink, item, c, verdict_rgb).pt(ch(0.4))),
        )
        .child(
            // The buttons wrap rather than shrink: two fit side by side in the column, and a
            // third starts a new line instead of poking out of it. Same chip as Rescan.
            row()
                .w_full()
                .wrap()
                .items_center()
                .gap(ch(0.6))
                .pt(ch(0.4))
                .children(can_open.then(|| {
                    chip("Open folder", ink.fg, [230, 230, 230], |s: &mut State| s.open())
                }))
                .children(has_culprit.then(|| {
                    chip("Culprit", ink.fg, [230, 230, 230], |s: &mut State| {
                        s.go_to_culprit()
                    })
                }))
                .child(chip("Reveal", ink.fg, [230, 230, 230], |s: &mut State| {
                    s.reveal()
                })),
        )
}

/// The verdict and its reason: a coloured square and the verdict word, the way categories
/// are marked, with the reason on its own line so the narrow column never cuts it short.
fn verdict_block(label: &Label, coloured: Color, ink: &Ink) -> Option<El> {
    if label.tag_line().is_empty() && label.reason.is_empty() {
        return None;
    }
    Some(
        col()
            .w_full()
            .gap(ch(0.2))
            .child(
                row()
                    .items_center()
                    .gap(ch(0.6))
                    .child(t("■", coloured))
                    .child(t(label.tag_line(), coloured)),
            )
            .children(
                (!label.reason.is_empty())
                    .then(|| t(label.reason.clone(), ink.muted).pl(ch(1.6)).truncate()),
            ),
    )
}

/// A column header: clicking it orders the list by that column, clicking it again flips it.
fn header_cell(
    s: &State,
    ink: &Ink,
    sort: store::Sort,
    label: &str,
    width: f32,
    numeric: bool,
) -> El {
    let active = sort != store::Sort::None && s.ui.sort == sort;
    let arrow = if !active {
        ""
    } else if s.ui.sort_down {
        "↓"
    } else {
        "↑"
    };
    // The arrow never widens the heading: it sits in a slot the row reserves too, so the
    // numbers stay under the word that names them.
    let mut cell = text(format!("{label}{arrow}"))
        .font(mono(FS, if active { 700.0 } else { 400.0 }))
        .color(if active { ink.row } else { ink.faint })
        .align(if numeric { Align::End } else { Align::Start });
    // The growing column takes whatever is left; the rest keep the row's exact widths.
    cell = if numeric { cell.w(width) } else { cell.grow(1.0).min_w(0.0) };
    if sort == store::Sort::None {
        return cell;
    }
    cell.px(0.0)
        .cursor(Cursor::Pointer)
        .hover(Color::WHITE)
        .on_press(move |s: &mut State| s.ui.set_sort(sort))
        .help(if active {
            "Reverse the order"
        } else {
            "Sort by this column"
        })
}

/// Width of one row's dot block, so the header can reserve exactly that much.
fn dots_width() -> f32 {
    let dot = (FS * 0.26).max(2.0).round();
    let gap = (FS * 0.14).max(1.0).round();
    DOTS as f32 * dot + (DOTS as f32 - 1.0) * gap
}

/// What the highlighted row is, where its space went, and what to do about it: the detail
/// block in the sidebar, above the categories. It reads the row from the list rather than
/// taking it, so the sidebar and the table can never disagree about which row is current.
fn details(s: &State, ink: &Ink) -> El {
    let ctx = Ctx { s, ink };
    let Some(item) = s.ui.entries(&s.projects).into_iter().nth(s.ui.row) else {
        return col().key("details").w_full();
    };
    col()
        .key("details")
        .w_full()
        .gap(ch(0.1))
        .child(item_details(&ctx, &item))
}

fn empty(s: &State, ink: &Ink) -> El {
    let line = if s.ui.listing {
        "measuring what is inside…"
    } else if s.scanning && s.ui.stack.is_empty() {
        "still measuring…"
    } else if !s.ui.stack.is_empty() {
        "empty, or could not be read"
    } else if s.projects.is_empty() {
        "nothing under this folder yet"
    } else {
        "nothing in this category"
    };
    t(line, ink.faint).pt(ch(0.6)).pl(ch(4.0))
}

fn inside(ctx: &Ctx) -> El {
    let s = ctx.s;
    let ink = ctx.ink;
    let entries = s.ui.entries(&s.projects);
    // The count line is the only thing that needs the untruncated list, so it asks for the
    // length rather than building a second sorted copy of every row every frame.
    let listing = s.ui.listing_count(&s.projects);
    let total = s.ui.total(&s.projects);

    let mut rows = col()
        .key("rows")
        .w_full()
        .gap(ch(0.15))
        // Every row is one line, always: the selected row's detail lives in the preview strip
        // below the list, in the same place for every row, instead of pushing the list around.
        .children(
            entries
                .iter()
                .enumerate()
                .map(|(index, item)| item_line(ctx, item, index)),
        );

    rows = rows.children((listing > store::MAX_ROWS).then(|| {
        t(
            format!("… and {} smaller", listing - store::MAX_ROWS),
            ink.faint,
        )
            .pt(ch(0.4))
            .pl(ch(2.4))
    }));

    let mut list = col()
        .grow(1.0)
        .min_h(0.0)
        .gap(ch(0.2))
        .child(inside_header(s, ink, total))
        .child(blurb_line(s, ink))
        .child(
            // Laid out with the row's own widths and gaps, so every heading sits over the
            // column it names: the name cell is the one that grows, exactly as in a row.
            row()
                .w_full()
                .items_center()
                .gap(GAP)
                .pt(ch(0.7))
                .pb(ch(0.15))
                // Same inset as a row's own padding, so the two grids coincide.
                .px(ch(0.5))
                .child(el().size(MARK_W, 0.0))
                .child(el().size(ARROW_W, 0.0))
                .child(header_cell(s, ink, store::Sort::Name, "folder", 0.0, true))
                .child(header_cell(s, ink, store::Sort::None, "", VERDICT_W, false))
                .child(header_cell(s, ink, store::Sort::Size, "size", SIZE_W, true))
                .child(el().size(dots_width(), 0.0))
                .child(el().size(PCT_W, 0.0))
                .child(header_cell(s, ink, store::Sort::Age, "last commit", AGE_W, true)),
        );

    if entries.is_empty() {
        list = list.child(empty(s, ink));
    } else {
        list = list.child(
            col()
                .key("scroll")
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(26.0)
                .child(rows),
        );
    }
    list.key("inside")
}

// ---- bottom line -----------------------------------------------------------

fn bottom_line(s: &State, ink: &Ink) -> El {
    // No back button: the breadcrumb above the list is the way up, and it names where it
    // goes. The action slot is kept so the line's layout stays fixed when chips come back.
    row()
        .key("bottom")
        .w_full()
        .items_center()
        .gap(ch(2.0))
        .child(row().key("actions").items_center().gap(ch(1.4)))
        .child(el().grow(1.0))
        .children(s.ui.note.clone().map(|note| t(note, ink.muted).truncate()))
}

// ---- view ------------------------------------------------------------------

/// The left column's width: the marker, the square, the name, the size, then the dot bar.
pub fn left_width() -> f32 {
    let dot = (FS * 0.26).max(2.0).round();
    let gap = (FS * 0.14).max(1.0).round();
    ch(1.0) + ch(1.0) + ch(11.0) + ch(9.0) + 14.0 * (dot + gap) + ch(4.6)
}

struct Ctx<'a> {
    s: &'a State,
    ink: &'a Ink,
}

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let ink = Ink::new();
    let ctx = Ctx { s, ink: &ink };
    let sizes = s.ui.category_sizes(&s.projects);
    col()
        .key("page")
        .w_full()
        .h_full()
        .fill(Color::BLACK)
        .px(FS * 1.2)
        .py(FS * 0.8)
        .gap(ch(0.5))
        .child(top_line(s, &ink))
        .child(el().h(ch(0.4)))
        .child(totals(s, &ink, &sizes))
        .child(meter(s, cx))
        .child(
            row()
                .grow(1.0)
                .min_h(0.0)
                .gap(ch(3.0))
                .pt(ch(0.8))
                .child(categories(s, &ink, &sizes, left_width()))
                .child(inside(&ctx)),
        )
        .child(bottom_line(s, &ink))
}
