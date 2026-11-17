//! Storage: where the projects folder's bytes are. Totals up top, one segmented bar of every
//! byte by category, then the folder list beside what the picked row is, the categories it can
//! be narrowed to, and what can be cleared. Nothing here deletes anything: Reveal shows a
//! folder in Explorer and the reader decides.

use rune::{col, el, icon, row, Align, Color, Cursor, Cx, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Spinner};

use crate::browse::{Culprit, Item};
use crate::cats::{self, Cat, Tier, Verdict, CATEGORIES};
use crate::fmt;
use crate::icons;
use crate::model;
use crate::shell;
use crate::store;
use crate::ui::{bar, card, color_of, data, label, tint, Ink, R_CARD, R_CTRL, S1, S2, S3, S4, S5, S6};
use crate::State;

/// The left column: the picked row, the categories, what can be cleared.
const SIDE_W: f32 = 300.0;
const SIDE_W_COMPACT: f32 = 256.0;

/// A small window: the table drops its verdict words and share column, and the side column
/// narrows, so names keep their room.
fn compact(s: &State) -> bool {
    s.window_w < 1100.0
}

fn verdict_w(s: &State) -> f32 {
    if compact(s) { 8.0 } else { VERDICT_W }
}

fn bar_w(s: &State) -> f32 {
    if compact(s) { 48.0 } else { BAR_W }
}
// The table's column widths. The header and every row read these same numbers, so a heading
// can never drift off the column it names.
const ARROW_W: f32 = 16.0;
const VERDICT_W: f32 = 104.0;
const SIZE_W: f32 = 76.0;
const BAR_W: f32 = 96.0;
const PCT_W: f32 = 36.0;
const AGE_W: f32 = 64.0;
const GAP: f32 = 12.0;
const ROW_H: f32 = 36.0;

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0%".to_string()
    } else {
        format!("{}%", (part as f64 / whole as f64 * 100.0).round() as u64)
    }
}

fn small_dot(c: Color) -> El {
    el().size(8.0, 8.0).radius(2.5).fill(c)
}

/// A soft pill in a colour: a verdict, a tier. Proportional type on a faint wash of the colour.
fn chip(text_: impl Into<String>, rgb8: cats::Rgb) -> El {
    row()
        .h(20.0)
        .px(S2)
        .radius(10.0)
        .items_center()
        .shrink()
        .min_w(0.0)
        .fill(tint(rgb8, 0.13))
        .child(label(text_, 11.0, 500.0, color_of(rgb8)).truncate())
}

/// A thin bar held to a fixed width, for a column.
fn bar_at(width: f32, fraction: f32, color: Color, ink: &Ink) -> El {
    row().w(width).items_center().child(bar(fraction, color, ink))
}

// ---- totals -------------------------------------------------------------------

/// The biggest folder a command brings back across the whole folder, whatever the table is
/// narrowed to or opened into: the hero card never changes as you browse.
pub fn disk_win(s: &State) -> Option<(Item, cats::Label)> {
    store::Ui::new(s.ui.root().to_path_buf()).biggest_reclaimable(&s.projects)
}

fn stat(ink: &Ink, key: &str, title: &str, value: String, value_color: Color, foot: El) -> El {
    card(ink)
        .key(format!("total/{key}"))
        .grow(1.0)
        .min_w(0.0)
        .h(88.0)
        .padding(S4)
        .gap(S1)
        .child(label(title, 12.0, 400.0, ink.faint))
        .child(data(value, 22.0, value_color))
        .child(foot)
}

fn totals(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    let total: u64 = sizes.iter().sum();
    let known = total > 0 || !s.scanning;
    let v = |b: u64| if known { model::gb(b) } else { "…".into() };
    let rebuildable = sizes[1] + sizes[2];
    let win = disk_win(s);
    let win_card = match win {
        Some((item, label_)) => card(ink)
            .key("total/win")
            .grow(1.3)
            .min_w(0.0)
            .h(88.0)
            .padding(S4)
            .gap(S1)
            .cursor(Cursor::Pointer)
            .hover(ink.hover)
            .on_press(move |s: &mut State| s.reveal_biggest())
            .help("Reveal it in Explorer")
            .child(label("Clear first", 12.0, 400.0, ink.faint))
            .child(
                row()
                    .w_full()
                    .gap(S2)
                    .items_end()
                    .child(label(item.name.clone(), 18.0, 500.0, ink.fg).min_w(0.0).shrink().truncate())
                    .child(data(model::gb(item.size), 13.0, ink.green).pb(2.0)),
            )
            .child(label(label_.reason.clone(), 12.0, 400.0, ink.muted).truncate()),
        None => card(ink)
            .key("total/win")
            .grow(1.3)
            .min_w(0.0)
            .h(88.0)
            .padding(S4)
            .gap(S1)
            .child(label("Clear first", 12.0, 400.0, ink.faint))
            .child(label(if s.scanning { "Measuring" } else { "Nothing rebuildable" }, 15.0, 500.0, ink.muted)),
    };
    row()
        .key("totals")
        .w_full()
        .gap(S4)
        .child(stat(
            ink,
            "all",
            "On disk",
            v(total),
            ink.fg,
            label(format!("{} folders", s.projects.len()), 12.0, 400.0, ink.muted),
        ))
        .child(stat(
            ink,
            "rebuildable",
            "Rebuildable",
            v(rebuildable),
            ink.green,
            label(
                if compact(s) { "deps and builds".to_string() } else { format!("{} deps + {} builds", model::gb(sizes[1]), model::gb(sizes[2])) },
                12.0,
                400.0,
                ink.muted,
            )
            .truncate(),
        ))
        .child(stat(
            ink,
            "source",
            "Your source",
            v(sizes[0]),
            ink.fg,
            label(format!("{} of the disk", pct(sizes[0], total)), 12.0, 400.0, ink.muted),
        ))
        .child(win_card)
}

// ---- the meter ----------------------------------------------------------------

/// Every byte under the root as one segmented bar, a segment per category, with a legend of
/// the same categories under it. Pointing at either names the category; a click narrows to it.
fn meter_card(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    let total: u64 = sizes.iter().sum();
    let lit = s.ui.hover.or(if s.ui.stack.is_empty() { s.ui.filter.cat() } else { None });
    let readout: El = match s.ui.hover {
        Some(cat) => {
            let size = sizes[store::cat_index(cat)];
            row()
                .key("meter-readout")
                .items_center()
                .gap(S2)
                .child(label(cat.name(), 12.0, 500.0, ink.fg))
                .child(data(format!("{} · {}", model::gb(size), pct(size, total)), 12.0, ink.muted))
                .child(label("Click to filter", 12.0, 400.0, ink.faint))
        }
        None => row()
            .key("meter-readout")
            .items_center()
            .gap(S2)
            .child(data(if total > 0 || !s.scanning { model::gb(total) } else { "…".into() }, 12.0, ink.muted))
            .children(s.scanning.then(|| label("measuring", 12.0, 400.0, ink.faint))),
    };
    let segments = CATEGORIES.iter().filter(|c| sizes[store::cat_index(c.id)] > 0).map(|c| {
        let id = c.id;
        let bytes = sizes[store::cat_index(id)];
        let on = lit.is_none_or(|l| l == id);
        el().key(format!("seg/{}", id.label()))
            .h_full()
            .grow(bytes as f32 / total.max(1) as f32)
            .min_w(4.0)
            .radius(4.0)
            .fill(if on { color_of(c.color) } else { tint(c.color, 0.28) })
            .cursor(Cursor::Pointer)
            .on_hover_change(move |s: &mut State, inside, _| {
                if inside {
                    s.ui.set_hover(Some(id));
                } else if s.ui.hover == Some(id) {
                    s.ui.set_hover(None);
                }
            })
            .on_press(move |s: &mut State| s.pick(id))
            .help(c.blurb)
    });
    let bar_row = row()
        .key("meter")
        .w_full()
        .h(14.0)
        .gap(crate::meter::GAP)
        .children(segments)
        .children((total == 0).then(|| el().h_full().grow(1.0).radius(4.0).fill(ink.raised)));
    let legend = row().key("meter-legend").w_full().gap(S2).flex_wrap().children(CATEGORIES.iter().map(|c| {
        let id = c.id;
        let bytes = sizes[store::cat_index(id)];
        let on = s.ui.filter.cat() == Some(id);
        row()
            .key(format!("legend/{}", id.label()))
            .h(28.0)
            .px(S3)
            .gap(S2)
            .radius(14.0)
            .items_center()
            .cursor(Cursor::Pointer)
            .fill(if on { ink.raised } else { Color::TRANSPARENT })
            .border(1.0, if on { ink.line_strong } else { ink.line })
            .hover(ink.hover)
            .on_hover_change(move |s: &mut State, inside, _| {
                if inside {
                    s.ui.set_hover(Some(id));
                } else if s.ui.hover == Some(id) {
                    s.ui.set_hover(None);
                }
            })
            .on_press(move |s: &mut State| s.pick(id))
            .child(small_dot(color_of(c.color)))
            .child(label(c.name, 12.0, if on { 500.0 } else { 400.0 }, if on { ink.fg } else { ink.row }))
            .child(data(model::gb(bytes), 11.0, ink.muted))
            .child(data(pct(bytes, total), 11.0, ink.faint))
    }));
    card(ink)
        .key("meter-card")
        .w_full()
        .padding(S5)
        .gap(S3)
        .child(
            row()
                .w_full()
                .items_center()
                .child(label("Every byte, by what made it", 13.0, 500.0, ink.fg).grow(1.0))
                .child(readout),
        )
        .child(bar_row)
        .child(legend)
}

// ---- left column ----------------------------------------------------------------

fn heading(t: &str, ink: &Ink) -> El {
    label(t, 12.0, 500.0, ink.faint)
}

fn culprit_line(ink: &Ink, item: &Item, culprit: &Culprit, color: cats::Rgb) -> El {
    let Some(last) = culprit.chain.last() else {
        if culprit.top.len() <= 1 {
            return el();
        }
        let spread = culprit
            .top
            .iter()
            .map(|t| format!("{} {}", t.name, model::gb(t.size)))
            .collect::<Vec<_>>()
            .join(" · ");
        return col()
            .w_full()
            .gap(S1)
            .child(label("Spread across", 12.0, 400.0, ink.faint))
            .child(label(spread, 12.0, 400.0, ink.muted).truncate());
    };
    let whole = culprit.root_size.max(item.size).max(last.size);
    let fraction = if whole > 0 { last.size as f32 / whole as f32 } else { 0.0 };
    let name = if last.dir && !last.name.ends_with('/') { format!("{}/", last.name) } else { last.name.clone() };
    col()
        .w_full()
        .gap(6.0)
        .child(label("Most of it is in", 12.0, 400.0, ink.faint))
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S2)
                .child(data(name, 12.0, ink.row).grow(1.0).min_w(0.0).truncate())
                .child(data(model::gb(last.size), 11.0, ink.muted).shrink())
                .child(data(pct(last.size, whole), 11.0, ink.faint).w(32.0).align(Align::End)),
        )
        .child(bar(fraction, color_of(color), ink))
}

/// What the highlighted row is, where its space is, and what you can do with it.
fn details(s: &State, ink: &Ink, k: &Kit) -> El {
    let Some(item) = s.ui.entries(&s.projects).into_iter().nth(s.ui.row) else {
        return card(ink)
            .key("details")
            .w_full()
            .padding(S4)
            .child(col().key("detail").w_full().gap(S1).child(heading("Selected", ink)).child(label(
                if s.scanning { "Measuring" } else { "Pick a row to see where its space is" },
                12.0,
                400.0,
                ink.muted,
            )));
    };
    let label_ = s.ui.label(&item, &s.projects);
    let culprit = s.ui.culprit(&item, &s.projects);
    let pending = s.ui.culprit_pending(&item);
    let can_open = item.dir && s.ui.stack.len() < 12;
    let has_culprit = culprit.as_ref().is_some_and(|c| c.chain.len() > 1);
    let category = s.ui.filter.cat().unwrap_or(Cat::Source).category();
    let share = s.ui.share(&item, &s.projects);
    let verdict_rgb = label_.verdict.color();
    let days = project_age(s, &item);
    let mut body = col()
        .key("detail")
        .w_full()
        .gap(S3)
        .child(
            row()
                .w_full()
                .items_end()
                .gap(S2)
                .child(
                    label(format!("{}{}", item.name, if item.dir { "/" } else { "" }), 15.0, 500.0, ink.fg)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(data(model::gb(item.size), 13.0, ink.fg).shrink()),
        );
    let mut facts = row().w_full().gap(S3).items_center().flex_wrap();
    if !label_.tag_line().is_empty() {
        facts = facts.child(
            row()
                .gap(6.0)
                .items_center()
                .child(small_dot(color_of(verdict_rgb)))
                .child(label(label_.tag_line(), 12.0, 400.0, color_of(verdict_rgb))),
        );
    }
    if s.ui.stack.is_empty() && share != item.size {
        facts = facts.child(
            row()
                .gap(6.0)
                .items_center()
                .child(small_dot(color_of(category.color)))
                .child(label(format!("{} {}", model::gb(share), category.name.to_lowercase()), 12.0, 400.0, ink.muted)),
        );
    }
    if let Some(d) = days {
        facts = facts.child(label(format!("Last commit {}", fmt::days_ago(Some(d))), 12.0, 400.0, ink.faint));
    }
    body = body
        .child(facts)
        .children((!label_.reason.is_empty()).then(|| label(label_.reason.clone(), 12.0, 400.0, ink.muted).wrap().w_full()))
        .children(pending.then(|| {
            row().gap(S2).items_center().child(Spinner::new().build(k)).child(label("Finding what takes the space", 12.0, 400.0, ink.faint))
        }))
        .children(
            culprit
                .as_ref()
                .filter(|c| !c.chain.is_empty() || c.top.len() > 1)
                .map(|c| culprit_line(ink, &item, c, verdict_rgb)),
        )
        .child(
            row()
                .w_full()
                .flex_wrap()
                .gap(S2)
                .children(can_open.then(|| {
                    Button::new("Open")
                        .variant(ButtonVariant::Neutral)
                        .icon(&icons::FOLDER)
                        .on_press(|s: &mut State| s.open())
                        .help("List what is inside")
                        .build(k)
                        .key("open-folder")
                }))
                .children(has_culprit.then(|| {
                    Button::new("Go to culprit")
                        .variant(ButtonVariant::Neutral)
                        .on_press(|s: &mut State| s.go_to_culprit())
                        .help("Open every folder down to where the space is")
                        .build(k)
                        .key("culprit")
                }))
                .child(
                    Button::new("Reveal")
                        .variant(ButtonVariant::GhostMuted)
                        .icon(&icons::EXTERNAL)
                        .on_press(|s: &mut State| s.reveal())
                        .help("Show it in Explorer")
                        .build(k)
                        .key("reveal"),
                ),
        );
    card(ink).key("details").w_full().padding(S4).gap(S2).child(heading("Selected", ink)).child(body)
}

fn filter_line(
    ink: &Ink,
    key: String,
    name: &str,
    color: Color,
    bytes: u64,
    fraction: f32,
    rgb8: cats::Rgb,
    selected: bool,
    dimmed: bool,
) -> El {
    row()
        .key(key)
        .w_full()
        .h(34.0)
        .px(S2)
        .gap(S2)
        .radius(R_CTRL)
        .items_center()
        .cursor(Cursor::Pointer)
        .fill(if selected { ink.raised } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .child(small_dot(color))
        .child(
            label(name, 13.0, if selected { 500.0 } else { 400.0 }, if selected { ink.fg } else if dimmed { ink.faint } else { ink.row })
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(data(model::gb(bytes), 11.0, if selected { ink.fg } else { ink.muted }).w(64.0).align(Align::End))
        .child(bar_at(56.0, fraction, if dimmed { tint(rgb8, 0.3) } else { color_of(rgb8) }, ink))
}

fn categories_card(s: &State, ink: &Ink, sizes: &[u64; 4]) -> El {
    let total: u64 = sizes.iter().sum();
    let largest = sizes.iter().copied().max().unwrap_or(0).max(1);
    let all_on = s.ui.filter.cat().is_none();
    card(ink)
        .key("filters")
        .w_full()
        .padding(S2)
        .pt(S3)
        .pb(S3)
        .gap(2.0)
        .child(heading("Show", ink).px(S2).pb(S1))
        .child(
            filter_line(ink, "filter/all".into(), "All projects", ink.fg, total, 1.0, [236, 236, 238], all_on, false)
                .on_hover_change(|s: &mut State, inside, _| {
                    if inside {
                        s.ui.set_hover(None);
                    }
                })
                .on_press(|s: &mut State| s.filter_to(store::Filter::All)),
        )
        .children(CATEGORIES.iter().map(|c| {
            let selected = s.ui.filter.cat() == Some(c.id);
            let dimmed = s.ui.hover.is_some() && s.ui.hover != Some(c.id) && !selected;
            let bytes = sizes[store::cat_index(c.id)];
            let id = c.id;
            filter_line(
                ink,
                format!("cat/{}", c.id.label()),
                c.name,
                color_of(c.color),
                bytes,
                bytes as f32 / largest as f32,
                c.color,
                selected,
                dimmed,
            )
            .on_hover_change(move |s: &mut State, inside, _| {
                if inside {
                    s.ui.set_hover(Some(id));
                } else if s.ui.hover == Some(id) {
                    s.ui.set_hover(None);
                }
            })
            .on_press(move |s: &mut State| s.pick(id))
            .help(c.blurb)
        }))
        .child(reclaim(s, ink))
}

/// What in the table can go: the folders an install or a build brings back.
fn reclaim(s: &State, ink: &Ink) -> El {
    let (count, bytes) = s.ui.reclaim_summary(&s.projects);
    let c = col().key("reclaim").w_full().mt(S2).px(S2).pt(S3).gap(S1).border_sides([1.0, 0.0, 0.0, 0.0], ink.line);
    if count == 0 {
        return c.child(label(
            if s.scanning { "Still measuring" } else { "Nothing listed here rebuilds from a command" },
            12.0,
            400.0,
            ink.faint,
        ));
    }
    c.child(
        row()
            .w_full()
            .items_center()
            .gap(S2)
            .child(label("Can be freed here", 12.0, 400.0, ink.muted).grow(1.0))
            .child(data(model::gb(bytes), 13.0, ink.green)),
    )
    .child(label(format!("{} an install or a build brings back", fmt::plural(count, "folder", "folders")), 11.0, 400.0, ink.faint))
}

fn side(s: &State, ink: &Ink, k: &Kit, sizes: &[u64; 4]) -> El {
    col()
        .key("categories")
        .w(if compact(s) { SIDE_W_COMPACT } else { SIDE_W })
        .h_full()
        .shrink()
        .child(
            col()
                .key("side-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(16.0)
                .child(
                    col()
                        .w_full()
                        .gap(S3)
                        .child(details(s, ink, k))
                        .child(categories_card(s, ink, sizes)),
                ),
        )
}

// ---- the table ------------------------------------------------------------------

fn crumb(label_: String, color: Color, ink: &Ink, active: bool, level: usize) -> El {
    let hint = label_.clone();
    let mut cell = row()
        .key(format!("crumb/{level}"))
        .items_center()
        .shrink()
        .h(26.0)
        .px(S2)
        .radius(6.0)
        .child(label(label_, 13.0, if active { 500.0 } else { 400.0 }, color).truncate());
    if active {
        cell = cell.fill(ink.raised);
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
    let depth = s.ui.stack.len();
    let accent = color_of(s.ui.filter.color());
    let mut bar = row().items_center().shrink().min_w(0.0).gap(2.0);
    bar = bar.child(crumb(s.ui.filter.name().to_string(), if depth == 0 { ink.fg } else { ink.muted }, ink, depth == 0, 0));
    for (i, level) in s.ui.stack.iter().enumerate() {
        let last = i + 1 == depth;
        bar = bar.child(icon(&icons::CHEVRON_RIGHT, 12.0).color(ink.faint));
        bar = bar.child(crumb(level.name.clone(), if last { ink.fg } else { ink.muted }, ink, last, i + 1));
    }
    row()
        .items_center()
        .gap(S2)
        .min_w(0.0)
        .shrink()
        .children(s.ui.filter.cat().map(|_| small_dot(accent)))
        .child(bar)
}

fn project_age(s: &State, item: &Item) -> Option<u64> {
    let name = if s.ui.stack.is_empty() { item.name.as_str() } else { s.ui.stack[0].name.as_str() };
    s.projects.iter().find(|p| p.name == name).and_then(|p| p.days)
}

fn header_cell(s: &State, ink: &Ink, sort: store::Sort, text_: &str, width: Option<f32>, end: bool) -> El {
    let active = sort != store::Sort::None && s.ui.sort == sort;
    let tone = if active { ink.fg } else { ink.muted };
    let mut cell = if end { row().justify_end() } else { row() };
    cell = cell
        .h_full()
        .gap(S1)
        .items_center()
        // The arrow sits on the column's open side, so the word stays flush with the figures.
        .children((active && end).then(|| icon(if s.ui.sort_down { &icons::DOWN } else { &icons::UP }, 12.0).color(ink.muted)))
        .child(label(text_, 12.0, 500.0, tone))
        .children((active && !end).then(|| icon(if s.ui.sort_down { &icons::DOWN } else { &icons::UP }, 12.0).color(ink.muted)));
    cell = match width {
        Some(w) => cell.w(w),
        None => cell.grow(1.0).min_w(0.0),
    };
    if sort == store::Sort::None {
        return cell;
    }
    cell.cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.ui.set_sort(sort))
        .help(if active { "Reverse the order" } else { "Sort by this column" })
}

fn table_row(s: &State, ink: &Ink, item: &Item, index: usize) -> El {
    let selected = s.ui.row == index;
    let category = s.ui.filter.cat().unwrap_or(Cat::Source).category();
    let label_ = s.ui.label(item, &s.projects);
    let days = project_age(s, item);
    let stale = days.is_some_and(|d| d > 180);
    let scope = s.ui.scope_size(&s.projects).max(1);
    let share = s.ui.share(item, &s.projects);
    let disk = s.ui.disk_size(&s.projects).max(1);
    let disk_share = item.size as f32 / disk as f32;
    let tint_rgb = if label_.verdict == Verdict::Yours { category.color } else { label_.verdict.color() };
    row()
        .key(format!("row/{index}"))
        .w_full()
        .h(ROW_H)
        .px(S2)
        .radius(R_CTRL)
        .items_center()
        .gap(GAP)
        .cursor(Cursor::Pointer)
        .fill(if selected { ink.raised } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_press(move |s: &mut State| s.ui.set_row(index))
        .on_double_press(|s: &mut State| s.open())
        .child(
            row()
                .w(ARROW_W)
                .h_full()
                .items_center()
                .justify_center()
                .cursor(if item.dir { Cursor::Pointer } else { Cursor::Default })
                .on_press(move |s: &mut State| {
                    s.ui.set_row(index);
                    s.open();
                })
                .children(item.dir.then(|| icon(&icons::CHEVRON_RIGHT, 12.0).color(if selected { ink.fg } else { ink.faint }))),
        )
        .child(
            row()
                .w(14.0)
                .children(item.dir.then(|| icon(&icons::FOLDER, 14.0).color(if selected { ink.muted } else { ink.faint }))),
        )
        .child(
            label(item.name.clone(), 13.0, if selected { 500.0 } else { 400.0 }, if selected { ink.fg } else { ink.row })
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(
            row()
                .w(verdict_w(s))
                .items_center()
                .children((!label_.verdict.tag().is_empty()).then(|| {
                    if compact(s) { small_dot(color_of(label_.verdict.color())) } else { chip(label_.verdict.tag(), label_.verdict.color()) }
                })),
        )
        .child(data(model::gb(item.size), 12.0, if selected { ink.fg } else { ink.muted }).w(SIZE_W).align(Align::End))
        .child(bar_at(bar_w(s), disk_share, if selected { color_of(tint_rgb) } else { tint(tint_rgb, 0.7) }, ink))
        .children((!compact(s)).then(|| data(pct(share, scope), 11.0, ink.faint).w(PCT_W).align(Align::End)))
        .child(
            data(days.map(cats::short_age).unwrap_or_default(), 11.0, if stale { ink.amber } else { ink.faint })
                .w(AGE_W)
                .align(Align::End),
        )
        .help(format!(
            "{} · {}{}",
            item.name,
            model::gb(item.size),
            if label_.reason.is_empty() { String::new() } else { format!(" · {}", label_.reason) }
        ))
}

fn empty(s: &State, ink: &Ink, k: &Kit) -> El {
    let (line, busy) = if s.ui.listing {
        ("Measuring what is inside", true)
    } else if s.scanning && s.ui.stack.is_empty() {
        ("Still measuring the folder", true)
    } else if !s.ui.stack.is_empty() {
        ("Empty, or it couldn't be read", false)
    } else if s.projects.is_empty() {
        ("Nothing under this folder yet", false)
    } else {
        ("Nothing in this category", false)
    };
    col()
        .key("table-empty")
        .w_full()
        .pt(64.0)
        .gap(S3)
        .items_center()
        .children(busy.then(|| Spinner::new().build(k)))
        .child(label(line, 13.0, 400.0, ink.muted))
}

fn table(s: &State, ink: &Ink, k: &Kit) -> El {
    let entries = s.ui.entries(&s.projects);
    let listing = s.ui.listing_count(&s.projects);
    let total = s.ui.total(&s.projects);
    let scope = s.ui.scope_size(&s.projects);
    let category = s.ui.filter.cat().unwrap_or(Cat::Source).category();
    let blurb = match s.ui.stack.last() {
        Some(level) => level.path.display().to_string(),
        None => match s.ui.filter.cat() {
            Some(cat) => cat.category().blurb.to_string(),
            None => "Every project under this folder, biggest first".to_string(),
        },
    };
    let head = col()
        .w_full()
        .px(S4)
        .pt(S3)
        .gap(S1)
        .child(
            row()
                .w_full()
                .h(28.0)
                .items_center()
                .gap(S4)
                .child(breadcrumb(s, ink))
                .child(el().grow(1.0))
                .child(data(model::gb(scope), 13.0, ink.fg))
                .child(label(format!("{} of disk", pct(scope, total)), 12.0, 400.0, ink.muted))
                .children((category.tier != Tier::Keep && s.ui.filter.cat().is_some()).then(|| {
                    label(
                        cats::tier_note(category.tier),
                        12.0,
                        400.0,
                        match category.tier {
                            Tier::Safe => ink.green,
                            _ => ink.amber,
                        },
                    )
                })),
        )
        .child(
            row()
                .w_full()
                .pl(S2)
                .items_center()
                .child(if s.ui.stack.is_empty() { label(blurb, 12.0, 400.0, ink.faint) } else { data(blurb, 11.0, ink.faint) }.grow(1.0).min_w(0.0).truncate())
                .children(s.ui.listing.then(|| label("measuring…", 11.0, 400.0, ink.muted))),
        );
    // Laid out with the row's own widths, gaps and inset, so every heading sits over its column.
    let columns = row()
        .key("table-head")
        .w_full()
        .h(32.0)
        .px(S4 + S2)
        .gap(GAP)
        .items_center()
        .border_sides([0.0, 0.0, 1.0, 0.0], ink.line)
        .child(el().size(ARROW_W + 14.0 + GAP, 0.0))
        .child(header_cell(s, ink, store::Sort::Name, "Folder", None, false))
        .child(header_cell(s, ink, store::Sort::None, "", Some(verdict_w(s)), false))
        .child(header_cell(s, ink, store::Sort::Size, "Size", Some(SIZE_W), true))
        .child(el().size(bar_w(s), 0.0))
        .children((!compact(s)).then(|| el().size(PCT_W, 0.0)))
        .child(header_cell(s, ink, store::Sort::Age, "Last commit", Some(AGE_W), true));
    let body: El = if entries.is_empty() {
        empty(s, ink, k)
    } else {
        col()
            .key("scroll")
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(16.0)
            .child(
                col()
                    .key("rows")
                    .w_full()
                    .px(S4)
                    .py(S2)
                    .gap(2.0)
                    .children(entries.iter().enumerate().map(|(i, item)| table_row(s, ink, item, i)))
                    .children((listing > store::MAX_ROWS).then(|| {
                        label(format!("and {} smaller", listing - store::MAX_ROWS), 12.0, 400.0, ink.faint).pt(S2).pl(S2 + ARROW_W + 14.0 + 2.0 * GAP)
                    })),
            )
    };
    let footer = row()
        .key("bottom")
        .w_full()
        .h(36.0)
        .px(S4)
        .gap(S3)
        .items_center()
        .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
        .child(label(fmt::plural(listing, "item", "items"), 12.0, 400.0, ink.faint))
        .child(el().grow(1.0))
        .children(s.ui.note.clone().map(|note| label(note, 12.0, 400.0, ink.muted).truncate()))
        .children(s.ui.note.is_none().then(|| label("Double-click a folder to open it", 11.0, 400.0, ink.faint)));
    col()
        .key("inside")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .fill(ink.card)
        .radius(R_CARD)
        .border(1.0, ink.line)
        .child(head)
        .child(columns)
        .child(body)
        .child(footer)
}

// ---- the page -------------------------------------------------------------------

pub fn view(s: &State, ink: &Ink, k: &Kit, _cx: &mut Cx<State>) -> El {
    let sizes = s.ui.category_sizes(&s.projects);
    let meta = if s.demo {
        format!("{} · demo data", crate::ui::clip_path(&s.ui.root().display().to_string(), 40))
    } else {
        match &s.done_at {
            Some(at) => format!("{} · scanned {at}", crate::ui::clip_path(&s.ui.root().display().to_string(), 40)),
            None => crate::ui::clip_path(&s.ui.root().display().to_string(), 40),
        }
    };
    let header = shell::header(
        "Storage",
        Some(meta),
        vec![Button::new(if s.scanning { "Scanning" } else { "Rescan" })
            .variant(ButtonVariant::Neutral)
            .icon(&icons::REFRESH)
            .control_size(ControlSize::Large)
            .disabled(s.scanning)
            .on_press(|s: &mut State| s.rescan())
            .help("Rescan · Ctrl+R")
            .build(k)
            .key("storage-rescan")],
        ink,
    );
    col().key("storage").w_full().h_full().child(header).child(
        col()
            .key("storage-body")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .px(S6)
            .pb(S6)
            .gap(S4)
            .child(totals(s, ink, &sizes))
            .child(meter_card(s, ink, &sizes))
            .child(
                row()
                    .key("storage-split")
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .gap(S4)
                    .child(side(s, ink, k, &sizes))
                    .child(table(s, ink, k)),
            ),
    )
}

#[allow(dead_code)]
fn unused() -> f32 {
    S5
}
