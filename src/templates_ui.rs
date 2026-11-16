//! The templates page: the recipes the new project sheet offers, each a named list of commands
//! run in order. Edits stay a draft until saved to `templates.json`.

use rune::{col, row, Color, Cursor, El, ScrollSpec, TextEdit, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Switch, TextInput};

use crate::icons;
use crate::shell;
use crate::templates::{self, Step, Template};
use crate::ui::{data, label, tag, Ink, R_CARD, R_CTRL, S1, S2, S3, S4, S5, S6};
use crate::State;

pub struct TemplatesPage {
    /// What is saved, and what the new project sheet offers.
    pub list: Vec<Template>,
    /// What is being edited.
    pub draft: Vec<Template>,
    pub selected: usize,
    pub name: TextEdit,
    pub kind: TextEdit,
    pub steps: Vec<TextEdit>,
}

impl TemplatesPage {
    pub fn new() -> Self {
        let list = if cfg!(test) { templates::defaults() } else { templates::load() };
        let mut p = TemplatesPage {
            draft: list.clone(),
            list,
            selected: 0,
            name: TextEdit::new(""),
            kind: TextEdit::new(""),
            steps: Vec::new(),
        };
        p.load_edits();
        p
    }

    pub fn dirty(&self) -> bool {
        self.draft != self.list
    }

    /// Fills the fields from the selected template.
    fn load_edits(&mut self) {
        self.selected = self.selected.min(self.draft.len().saturating_sub(1));
        match self.draft.get(self.selected) {
            Some(t) => {
                self.name = TextEdit::new(&t.name);
                self.kind = TextEdit::new(&t.kind);
                self.steps = t.steps.iter().map(|st| TextEdit::new(&st.run)).collect();
            }
            None => {
                self.name = TextEdit::new("");
                self.kind = TextEdit::new("");
                self.steps.clear();
            }
        }
    }

    /// Writes the fields back into the draft after a keystroke.
    fn store_edits(&mut self) {
        let Some(t) = self.draft.get_mut(self.selected) else { return };
        t.name = self.name.text.clone();
        t.kind = self.kind.text.clone();
        for (st, e) in t.steps.iter_mut().zip(&self.steps) {
            st.run = e.text.clone();
        }
    }

    pub fn select(&mut self, i: usize) {
        self.store_edits();
        self.selected = i;
        self.load_edits();
    }

    fn unique_id(&self, base: &str) -> String {
        let slug: String = base
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let slug = if slug.is_empty() { "template".to_string() } else { slug };
        let mut id = slug.clone();
        let mut n = 2;
        while self.draft.iter().any(|t| t.id == id) {
            id = format!("{slug}-{n}");
            n += 1;
        }
        id
    }

    pub fn add(&mut self) {
        self.store_edits();
        let id = self.unique_id("custom");
        self.draft.push(Template {
            id,
            name: "Untitled template".into(),
            kind: "folder".into(),
            steps: vec![Step { run: String::new(), in_parent: false }],
        });
        self.selected = self.draft.len() - 1;
        self.load_edits();
        self.name.focused = true;
    }

    pub fn duplicate(&mut self) {
        self.store_edits();
        let Some(t) = self.draft.get(self.selected).cloned() else { return };
        let id = self.unique_id(&t.id);
        self.draft.insert(self.selected + 1, Template { id, name: format!("{} copy", t.name), ..t });
        self.selected += 1;
        self.load_edits();
    }

    pub fn delete(&mut self) {
        if self.selected < self.draft.len() {
            self.draft.remove(self.selected);
            self.load_edits();
        }
    }

    pub fn shift(&mut self, i: usize, up: bool) {
        self.store_edits();
        let j = if up { i.checked_sub(1) } else { (i + 1 < self.draft.len()).then_some(i + 1) };
        if let Some(j) = j {
            self.draft.swap(i, j);
            if self.selected == i {
                self.selected = j;
            } else if self.selected == j {
                self.selected = i;
            }
            self.load_edits();
        }
    }

    pub fn add_step(&mut self) {
        self.store_edits();
        if let Some(t) = self.draft.get_mut(self.selected) {
            let in_parent = false;
            t.steps.push(Step { run: String::new(), in_parent });
            self.load_edits();
            if let Some(e) = self.steps.last_mut() {
                e.focused = true;
            }
        }
    }

    pub fn remove_step(&mut self, i: usize) {
        self.store_edits();
        if let Some(t) = self.draft.get_mut(self.selected) {
            if i < t.steps.len() {
                t.steps.remove(i);
            }
            self.load_edits();
        }
    }

    pub fn shift_step(&mut self, i: usize, up: bool) {
        self.store_edits();
        if let Some(t) = self.draft.get_mut(self.selected) {
            let j = if up { i.checked_sub(1) } else { (i + 1 < t.steps.len()).then_some(i + 1) };
            if let Some(j) = j {
                t.steps.swap(i, j);
            }
            self.load_edits();
        }
    }

    pub fn toggle_parent(&mut self, i: usize) {
        self.store_edits();
        if let Some(st) = self.draft.get_mut(self.selected).and_then(|t| t.steps.get_mut(i)) {
            st.in_parent = !st.in_parent;
        }
    }

    pub fn revert(&mut self) {
        self.draft = self.list.clone();
        self.load_edits();
    }

    pub fn reset_defaults(&mut self) {
        self.draft = templates::defaults();
        self.selected = 0;
        self.load_edits();
    }

    /// Saves the draft; blank steps are dropped and names default.
    pub fn save(&mut self) -> std::io::Result<()> {
        self.store_edits();
        for t in &mut self.draft {
            t.steps.retain(|st| !st.run.trim().is_empty());
            if t.name.trim().is_empty() {
                t.name = "Untitled template".into();
            }
            if t.kind.trim().is_empty() {
                t.kind = "folder".into();
            }
        }
        if !cfg!(test) {
            templates::save(&self.draft)?;
        }
        self.list = self.draft.clone();
        self.load_edits();
        Ok(())
    }
}

fn save(s: &mut State) {
    match s.tmpl.save() {
        Ok(()) => s.toast("Saved templates".to_string()),
        Err(e) => s.toast_error(format!("Couldn't save templates: {e}")),
    }
}

fn template_row(s: &State, ink: &Ink, k: &Kit, i: usize, t: &Template) -> El {
    let on = s.tmpl.selected == i;
    let n = s.tmpl.draft.len();
    let steps = t.steps.iter().filter(|st| !st.run.trim().is_empty()).count();
    let mover = |glyph: &'static rune::Icon, tip: &str, up: bool, enabled: bool| {
        IconButton::new(glyph)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label(tip.to_string())
            .help(tip.to_string())
            .disabled(!enabled)
            .on_press(move |s: &mut State| s.tmpl.shift(i, up))
            .build(k)
    };
    row()
        .key(format!("tmpl/{i}"))
        .w_full()
        .h(52.0)
        .pl(S3)
        .pr(S1)
        .gap(S2)
        .radius(R_CTRL)
        .items_center()
        .cursor(Cursor::Pointer)
        .fill(if on { ink.raised } else { Color::TRANSPARENT })
        .hover(ink.hover)
        .on_press(move |s: &mut State| s.tmpl.select(i))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .child(label(if t.name.is_empty() { "Untitled".to_string() } else { t.name.clone() }, 13.0, 500.0, if on { ink.fg } else { ink.row }).truncate())
                .child(data(
                    match steps {
                        0 => "folder only".to_string(),
                        1 => "1 step".to_string(),
                        n => format!("{n} steps"),
                    },
                    11.0,
                    ink.faint,
                )),
        )
        .child(tag(t.kind.clone(), ink))
        .child(col().gap(0.0).child(mover(&icons::UP, "Move up", true, i > 0)).child(mover(&icons::DOWN, "Move down", false, i + 1 < n)))
}

fn step_row(s: &State, ink: &Ink, k: &Kit, i: usize, step: &Step, edit: &TextEdit, count: usize) -> El {
    let icon_btn = |glyph: &'static rune::Icon, tip: &str, enabled: bool, f: Box<dyn Fn(&mut State)>| {
        IconButton::new(glyph)
            .variant(IconButtonVariant::Ghost)
            .label(tip.to_string())
            .help(tip.to_string())
            .disabled(!enabled)
            .on_press(move |s: &mut State| f(s))
            .build(k)
    };
    let _ = s;
    row()
        .key(format!("step/{i}"))
        .w_full()
        .gap(S2)
        .items_center()
        .child(
            row()
                .size(22.0, 22.0)
                .radius(11.0)
                .items_center()
                .justify_center()
                .border(1.0, ink.line_strong)
                .child(data((i + 1).to_string(), 11.0, ink.muted)),
        )
        .child(
            TextInput::new(edit)
                .placeholder("command, e.g. npm install")
                .text_style(crate::ui::mono(12.0, 400.0))
                .on_edit(move |s: &mut State| &mut s.tmpl.steps[i])
                .on_change(|s: &mut State, _: &mut Tx| s.tmpl.store_edits())
                .build(k)
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            Switch::new(step.in_parent)
                .label("In parent")
                .on_toggle(move |s: &mut State| s.tmpl.toggle_parent(i))
                .build(k)
                .help("Run in the parent folder: for generators that make the project folder themselves"),
        )
        .child(icon_btn(&icons::UP, "Move step up", i > 0, Box::new(move |s| s.tmpl.shift_step(i, true))))
        .child(icon_btn(&icons::DOWN, "Move step down", i + 1 < count, Box::new(move |s| s.tmpl.shift_step(i, false))))
        .child(icon_btn(&icons::TRASH, "Remove step", true, Box::new(move |s| s.tmpl.remove_step(i))))
}

fn editor(s: &State, ink: &Ink, k: &Kit) -> El {
    let p = &s.tmpl;
    let Some(t) = p.draft.get(p.selected) else {
        return col()
            .key("tmpl-editor")
            .grow(1.0)
            .items_center()
            .pt(96.0)
            .gap(S3)
            .child(label("No templates", 15.0, 500.0, ink.fg))
            .child(label("Add one, or bring back the built-in set.", 13.0, 400.0, ink.muted))
            .child(
                Button::new("Restore defaults")
                    .variant(ButtonVariant::Neutral)
                    .on_press(|s: &mut State| s.tmpl.reset_defaults())
                    .build(k),
            );
    };
    let sample = "my-app";
    let root = s.root.clone();
    let preview = t
        .steps
        .iter()
        .filter(|st| !st.run.trim().is_empty())
        .map(|st| {
            let where_ = if st.in_parent { format!("{root}>") } else { "my-app>".to_string() };
            format!("{where_} {}", st.run.replace("{name}", sample).replace("{path}", &format!("{root}\\{sample}")))
        })
        .collect::<Vec<_>>();
    col()
        .key("tmpl-editor")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .scroll(ScrollSpec { stick_bottom: false })
        .child(
            col()
                .w_full()
                .padding(S5)
                .gap(S5)
                .child(
                    row()
                        .w_full()
                        .gap(S3)
                        .child(col().grow(1.0).min_w(0.0).gap(6.0).child(label("Name", 12.0, 400.0, ink.muted)).child(
                            TextInput::new(&p.name)
                                .placeholder("Next.js app")
                                .on_edit(|s: &mut State| &mut s.tmpl.name)
                                .on_change(|s: &mut State, _: &mut Tx| s.tmpl.store_edits())
                                .build(k)
                                .w_full()
                                .key("tmpl-name"),
                        ))
                        .child(col().w(180.0).gap(6.0).child(label("Kind", 12.0, 400.0, ink.muted)).child(
                            TextInput::new(&p.kind)
                                .placeholder("nextjs, rust, folder")
                                .text_style(crate::ui::mono(12.0, 400.0))
                                .width(Some(180.0))
                                .on_edit(|s: &mut State| &mut s.tmpl.kind)
                                .on_change(|s: &mut State, _: &mut Tx| s.tmpl.store_edits())
                                .build(k)
                                .key("tmpl-kind"),
                        )),
                )
                .child(
                    col()
                        .w_full()
                        .gap(S2)
                        .child(label("Steps", 13.0, 500.0, ink.fg))
                        .child(label(
                            "Run in order with cmd. {name} is the project's name and {path} its folder. A step in the parent folder runs beside the new project, for generators that make the folder themselves.",
                            12.0,
                            400.0,
                            ink.muted,
                        ).wrap().w_full())
                        .child(col().w_full().gap(S2).pt(S2).children(t.steps.iter().enumerate().filter_map(|(i, st)| {
                            p.steps.get(i).map(|e| step_row(s, ink, k, i, st, e, t.steps.len()))
                        })))
                        .children(t.steps.is_empty().then(|| {
                            label("No steps: the template makes an empty folder.", 12.0, 400.0, ink.faint).pt(S2)
                        }))
                        .child(
                            Button::new("Add step")
                                .variant(ButtonVariant::GhostMuted)
                                .icon(&icons::PLUS)
                                .on_press(|s: &mut State| s.tmpl.add_step())
                                .build(k)
                                .key("add-step")
                                .self_start()
                                .ml(-6.0),
                        ),
                )
                .child(
                    col()
                        .w_full()
                        .gap(S2)
                        .child(label(format!("What runs for \u{201c}{sample}\u{201d}"), 13.0, 500.0, ink.fg))
                        .child(
                            col()
                                .key("tmpl-preview")
                                .w_full()
                                .padding(S3)
                                .gap(S1)
                                .radius(R_CTRL)
                                .fill(ink.side)
                                .border(1.0, ink.line)
                                .children(t.steps.first().is_none_or(|st| !st.in_parent).then(|| data(format!("{}> mkdir my-app", s.root), 11.0, ink.faint).truncate()))
                                .children(preview.into_iter().map(|l| data(l, 11.0, ink.muted).wrap_anywhere()))
                                .child(data("my-app> git init    (when the sheet's git switch is on)", 11.0, ink.faint)),
                        ),
                )
                .child(
                    row()
                        .w_full()
                        .gap(S2)
                        .pt(S2)
                        .child(
                            Button::new("Duplicate")
                                .variant(ButtonVariant::Neutral)
                                .icon(&icons::COPY)
                                .on_press(|s: &mut State| s.tmpl.duplicate())
                                .build(k),
                        )
                        .child(
                            Button::new("Delete template")
                                .variant(ButtonVariant::Danger)
                                .icon(&icons::TRASH)
                                .on_press(|s: &mut State| s.tmpl.delete())
                                .build(k)
                                .key("tmpl-delete"),
                        ),
                ),
        )
}

pub fn view(s: &State, ink: &Ink, k: &Kit, _layer: &mut Vec<El>) -> El {
    let p = &s.tmpl;
    let dirty = p.dirty();
    let header = shell::header(
        "Templates",
        Some(crate::ui::clip_path(&templates::path().display().to_string(), 56)),
        vec![
            Button::new("Restore defaults")
                .variant(ButtonVariant::GhostMuted)
                .on_press(|s: &mut State| s.tmpl.reset_defaults())
                .build(k),
            Button::new("Discard changes")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Large)
                .disabled(!dirty)
                .on_press(|s: &mut State| s.tmpl.revert())
                .build(k),
            if dirty {
                shell::primary("Save", &icons::CHECK).on_press(save).help("Save to templates.json").build(k).key("tmpl-save")
            } else {
                Button::new("Saved")
                    .variant(ButtonVariant::GhostFaint)
                    .icon(&icons::CHECK)
                    .control_size(ControlSize::Large)
                    .disabled(true)
                    .build(k)
                    .key("tmpl-save")
            },
        ],
        ink,
    );
    let list = col()
        .key("tmpl-list")
        .w(300.0)
        .h_full()
        .shrink()
        .fill(ink.card)
        .radius(R_CARD)
        .border(1.0, ink.line)
        .child(
            col()
                .key("tmpl-list-scroll")
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .scroll(ScrollSpec { stick_bottom: false })
                .child(
                    col()
                        .w_full()
                        .padding(S2)
                        .gap(2.0)
                        .children(p.draft.iter().enumerate().map(|(i, t)| template_row(s, ink, k, i, t))),
                ),
        )
        .child(
            row()
                .w_full()
                .padding(S2)
                .border_sides([1.0, 0.0, 0.0, 0.0], ink.line)
                .child(
                    Button::new("New template")
                        .variant(ButtonVariant::GhostMuted)
                        .icon(&icons::PLUS)
                        .on_press(|s: &mut State| s.tmpl.add())
                        .build(k)
                        .key("tmpl-add"),
                ),
        );
    let body = row()
        .key("tmpl-body")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(S6)
        .pb(S6)
        .gap(S4)
        .child(list)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .fill(ink.card)
                .radius(R_CARD)
                .border(1.0, ink.line)
                .child(editor(s, ink, k)),
        );
    col().key("templates").w_full().h_full().child(header).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_reorder_and_save_round_trip() {
        let mut p = TemplatesPage::new();
        let first = p.draft[0].id.clone();
        p.shift(0, false);
        assert_eq!(p.draft[1].id, first);
        assert!(p.dirty());
        p.add();
        p.name.replace_text("Astro site");
        p.steps[0].replace_text("npm create astro@latest {name}");
        p.toggle_parent(0);
        p.save().unwrap();
        let t = p.list.last().unwrap();
        assert_eq!(t.name, "Astro site");
        assert!(t.steps[0].in_parent);
        assert!(!p.dirty());
    }

    #[test]
    fn blank_steps_are_dropped_on_save() {
        let mut p = TemplatesPage::new();
        p.add();
        p.add_step();
        p.save().unwrap();
        assert!(p.list.last().unwrap().steps.is_empty());
    }
}

#[allow(dead_code)]
fn unused() -> (f32, f32) {
    (S4, S6)
}
