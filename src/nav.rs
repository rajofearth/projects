//! The app's pages and the tab strip in the top line that switches between them.

use rune::{row, text, Color, Cursor, El, Key, KeyEvent, Tx};

use crate::ui::{ch, mono, Ink, FS};
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Projects,
    Storage,
    Usage,
}

impl Page {
    pub const ALL: [Page; 3] = [Page::Projects, Page::Storage, Page::Usage];

    pub fn label(self) -> &'static str {
        match self {
            Page::Projects => "projects",
            Page::Storage => "storage",
            Page::Usage => "usage",
        }
    }
}

/// The page tabs: plain words, the current one bold on the picked surface.
pub fn tabs(s: &State, ink: &Ink) -> El {
    row()
        .key("tabs")
        .items_center()
        .gap(ch(0.3))
        .children(Page::ALL.into_iter().map(|page| {
            let active = s.page == page;
            row()
                .key(format!("tab/{}", page.label()))
                .h(FS * 1.7)
                .px(ch(0.8))
                .radius(4.0)
                .items_center()
                .fill(if active { ink.picked } else { Color::TRANSPARENT })
                .hover(ink.hover)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| s.page = page)
                .child(
                    text(page.label())
                        .font(mono(FS, if active { 700.0 } else { 400.0 }))
                        .color(if active { ink.fg } else { ink.muted }),
                )
        }))
}

/// Window keys: `/` or Ctrl+F jumps to the project search, Ctrl+1..3 switch pages.
pub fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let in_field = tx.focused().is_some_and(|k| k.ends_with("search") || k.ends_with("new-name"));
    let slash = matches!(&ev.key, Key::Text(t) if t == "/") && !in_field;
    if slash || ev.is_command('f') {
        s.page = Page::Projects;
        tx.focus("search", false);
        return true;
    }
    for (i, page) in Page::ALL.into_iter().enumerate() {
        if ev.is_command(char::from(b'1' + i as u8)) {
            s.page = page;
            return true;
        }
    }
    if ev.key == Key::Escape && s.page == Page::Projects {
        s.home.new_open = false;
    }
    false
}
