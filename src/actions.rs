//! What you do with a project, wherever the action is offered (a card, the context menu, the
//! detail panel, a key). Each one says what happened in a toast.

use std::path::PathBuf;

use rune::Tx;

use crate::launch;
use crate::shell::Page;
use crate::State;

pub fn path_of(s: &State, name: &str) -> PathBuf {
    PathBuf::from(&s.root).join(name)
}

/// The app Enter and double-click use: the one picked in Settings if it is still installed,
/// else the first editor found.
pub fn default_app(s: &State) -> Option<&launch::App> {
    s.saved
        .editor
        .as_deref()
        .and_then(|id| s.apps.iter().find(|a| a.id == id))
        .or_else(|| s.apps.first())
}

fn report(s: &mut State, ok: String, r: std::io::Result<()>) {
    match r {
        Ok(()) => s.toast(ok),
        Err(e) => s.toast_error(format!("{ok} failed: {e}")),
    }
}

pub fn open_in(s: &mut State, name: &str, app_id: &str) {
    let path = path_of(s, name);
    let label = s.apps.iter().find(|a| a.id == app_id).map(|a| a.name).unwrap_or(app_id).to_string();
    if s.demo {
        s.toast(format!("Opened {name} in {label}"));
        return;
    }
    s.saved.mark_opened(name);
    report(s, format!("Opened {name} in {label}"), launch::open_in(app_id, &path));
}

pub fn open_default(s: &mut State, name: &str) {
    match default_app(s).map(|a| a.id) {
        Some(id) => open_in(s, name, id),
        None => reveal(s, name),
    }
}

pub fn terminal(s: &mut State, name: &str) {
    let path = path_of(s, name);
    if s.demo {
        s.toast(format!("Terminal at {name}"));
        return;
    }
    s.saved.mark_opened(name);
    report(s, format!("Terminal at {name}"), launch::open_terminal(&path));
}

pub fn reveal(s: &mut State, name: &str) {
    let path = path_of(s, name);
    if s.demo {
        s.toast(format!("Revealed {name}"));
        return;
    }
    report(s, format!("Revealed {name}"), crate::browse::reveal(&path));
}

pub fn copy_path(s: &mut State, name: &str, tx: &mut Tx) {
    let path = path_of(s, name).display().to_string();
    tx.copy(path.clone());
    s.toast(format!("Copied {path}"));
}

pub fn toggle_pin(s: &mut State, name: &str) {
    let on = s.saved.toggle_pin(name);
    s.toast(if on { format!("Pinned {name}") } else { format!("Unpinned {name}") });
}

pub fn toggle_archived(s: &mut State, name: &str) {
    let on = s.saved.toggle_archived(name);
    if on && !s.home.show_archived && s.home.selected.as_deref() == Some(name) {
        s.home.detail = false;
    }
    s.toast(if on {
        format!("Archived {name}, hidden from Home")
    } else {
        format!("Restored {name}")
    });
}

/// The command line a script runs with: `npm run dev` for a package.json script, the
/// command itself for the cargo ones.
pub fn script_line(pm: Option<&str>, name: &str, command: &str) -> String {
    match pm {
        Some(pm @ ("npm" | "pnpm" | "yarn" | "bun")) => format!("{pm} run {name}"),
        _ => command.to_string(),
    }
}

pub fn run_script(s: &mut State, name: &str, script: &str) {
    let Some(info) = s.inspect.get(name) else { return };
    let Some((_, command)) = info.scripts.iter().find(|(n, _)| n == script) else { return };
    let line = script_line(info.package_manager, script, command);
    let path = path_of(s, name);
    if s.demo {
        s.toast(format!("Running {line} in {name}"));
        return;
    }
    s.saved.mark_opened(name);
    report(s, format!("Running {line} in {name}"), launch::run_in_terminal(&path, &line));
}

pub fn open_url(s: &mut State, url: &str) {
    if s.demo {
        s.toast(format!("Opened {url}"));
        return;
    }
    report(s, format!("Opened {url}"), launch::open_url(url));
}

/// The storage page, on this project's row.
pub fn show_in_storage(s: &mut State, name: &str) {
    s.page = Page::Storage;
    s.filter_to(crate::store::Filter::All);
    if let Some(i) = s.ui.entries(&s.projects).iter().position(|it| it.name == name) {
        s.ui.set_row(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_scripts_run_through_their_package_manager() {
        assert_eq!(script_line(Some("pnpm"), "dev", "next dev"), "pnpm run dev");
        assert_eq!(script_line(Some("cargo"), "test", "cargo test"), "cargo test");
        assert_eq!(script_line(None, "x", "make x"), "make x");
    }
}
