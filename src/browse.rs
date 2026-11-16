//! What is inside a folder, and where its space actually is.
//!
//! The on-demand half of the scanner: opening a project lists its children measured
//! directory by directory, and a culprit search follows the biggest child down while it holds
//! most of its parent. Both run on a thread and stream back so the window never blocks; a
//! newer request cancels the older one.
//!
//! Ported from the upstream interface's `browse.ts` (MIT), with `du` replaced by a directory
//! walk that the scanner already knows how to do.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::cats::is_whole;

/// A child of a folder, as the detail pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

impl Item {
    pub fn file(path: PathBuf, name: String, size: u64) -> Self {
        Self {
            path,
            name,
            size,
            dir: false,
        }
    }
    pub fn folder(path: PathBuf, name: String, size: u64) -> Self {
        Self {
            path,
            name,
            size,
            dir: true,
        }
    }
}

/// Where a folder's space actually is: folders (and maybe one file) each holding most of the
/// one before, plus the biggest things directly inside for when no single one dominates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Culprit {
    pub chain: Vec<Item>,
    pub top: Vec<Item>,
    /// Size of the folder the search started in, the denominator the chain's share uses.
    pub root_size: u64,
}

pub enum BrowseEvent {
    /// The list so far for `path`, largest first; `done` once every folder is measured.
    Listing {
        path: PathBuf,
        items: Vec<Item>,
        done: bool,
    },
    Culprit {
        path: PathBuf,
        culprit: Culprit,
    },
}

pub type Cancel = Arc<AtomicBool>;

pub fn cancel_flag() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

/// Bytes on disk under `path`: every file's length, junctions and symlinks left alone.
/// Returns `None` when `cancel` was raised mid-walk, so a navigated-away listing stops
/// instead of measuring a tree nobody is looking at any more.
pub fn folder_size(path: &Path, cancel: &AtomicBool) -> Option<u64> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    let mut entries_seen = 0u32;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // The flag is read once every 4096 entries: often enough to abandon a big
            // node_modules within a few milliseconds, rare enough to stay off the per-file
            // path of a size walk.
            entries_seen += 1;
            if entries_seen % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

fn child(path: &Path, name: &str, size: u64, dir: bool) -> Item {
    Item {
        path: path.join(name),
        name: name.to_string(),
        size,
        dir,
    }
}

/// Every child of `path`, largest first, without measuring subfolders (their sizes are 0).
pub fn list_immediate(path: &Path) -> (Vec<Item>, Vec<Item>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return (files, dirs);
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if kind.is_dir() {
            dirs.push(child(path, &name, 0, true));
        } else if let Ok(meta) = entry.metadata() {
            files.push(child(path, &name, meta.len(), false));
        }
    }
    files.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    (files, dirs)
}

/// The whole list, measured, largest first. Blocking, and used by the tests only: the
/// window's own listing streams through `list_folder`.
/// `cancel` stops the walk between folders; `on_progress` sees each interim list.
#[cfg(test)]
pub fn list_folder_blocking(
    path: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&[Item]),
) -> Vec<Item> {
    let (files, dirs) = list_immediate(path);
    let mut items: Vec<Item> = files;
    items.sort_by(|a, b| b.size.cmp(&a.size));
    on_progress(&items);
    for mut dir in dirs {
        if cancel.load(Ordering::Relaxed) {
            return items;
        }
        let Some(size) = folder_size(&dir.path, cancel) else {
            // Cancelled mid-folder: the list stays as it is rather than showing a wrong size.
            return items;
        };
        dir.size = size;
        items.push(dir);
        items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        on_progress(&items);
    }
    items
}

/// Lists `path` on a thread, streaming the list as folders come back measured.
pub fn list_folder(path: PathBuf, tx: Sender<BrowseEvent>, cancel: Cancel) {
    std::thread::spawn(move || {
        let (files, dirs) = list_immediate(&path);
        let mut items = files;
        let send = |items: &[Item], done: bool| {
            let _ = tx.send(BrowseEvent::Listing {
                path: path.clone(),
                items: items.to_vec(),
                done,
            });
        };
        send(&items, dirs.is_empty());
        for mut dir in dirs {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(size) = folder_size(&dir.path, &cancel) else {
                return;
            };
            dir.size = size;
            items.push(dir);
            items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
            send(&items, false);
        }
        send(&items, true);
    });
}

/// A child holding at least this share of its parent counts as "where the space is".
const DOMINANT: f64 = 0.5;
const DEPTH: usize = 12;

pub fn find_culprit_blocking(path: &Path, cancel: &AtomicBool) -> Culprit {
    let mut chain: Vec<Item> = Vec::new();
    let mut current = path.to_path_buf();
    let mut current_size = folder_size(path, cancel).unwrap_or(0);
    let root_size = current_size;
    let mut top: Vec<Item> = Vec::new();

    for depth in 0..DEPTH {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (files, dirs) = list_immediate(&current);
        let mut kids: Vec<Item> = files
            .into_iter()
            .chain(dirs.into_iter().filter_map(|mut d| {
                d.size = folder_size(&d.path, cancel)?;
                Some(d)
            }))
            .collect();
        kids.sort_by(|a, b| b.size.cmp(&a.size));
        if depth == 0 {
            top = kids.iter().take(3).cloned().collect();
        }
        let Some(best) = kids.into_iter().next() else {
            break;
        };
        if (best.size as f64) < current_size as f64 * DOMINANT {
            break;
        }
        let whole = is_whole(&best.name);
        current = best.path.clone();
        current_size = best.size;
        chain.push(best);
        if whole {
            break;
        }
    }
    Culprit {
        chain,
        top,
        root_size,
    }
}

/// Searches for the culprit on a thread. A cancel before the walk starts or after it
/// ends means nothing is sent, and the caller drops the receiver when the thread ends.
pub fn find_culprit(path: PathBuf, tx: Sender<BrowseEvent>, cancel: Cancel) {
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let culprit = find_culprit_blocking(&path, &cancel);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(BrowseEvent::Culprit { path, culprit });
    });
}

/// Shows the item in Explorer (Finder, or the desktop's file manager elsewhere), opening
/// its parent folder with the item highlighted. "Open folder" is the action that opens a
/// folder itself.
///
/// The argument goes to Explorer raw: `Command::arg` escapes embedded quotes as `\"`,
/// which Explorer can't parse, so it falls back to opening Documents. The quotes are still
/// needed because `/select,` splits on a comma.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ps-browse-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn listing_measures_folders_and_sorts_largest_first() {
        let root = scratch("listing");
        fs::create_dir_all(root.join("small")).unwrap();
        fs::write(root.join("small/a.txt"), "x".repeat(100)).unwrap();
        fs::create_dir_all(root.join("big")).unwrap();
        fs::write(root.join("big/a.bin"), "x".repeat(9000)).unwrap();
        fs::write(root.join("loose.txt"), "x".repeat(500)).unwrap();

        let mut progress = 0;
        let live = AtomicBool::new(false);
        let items = list_folder_blocking(&root, &live, |_| progress += 1);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, "big", "the folder with 9 kB leads");
        assert_eq!(items[0].size, 9000);
        assert!(items[0].dir);
        assert_eq!(items[1].name, "loose.txt");
        assert!(!items[1].dir);
        assert_eq!(items[2].size, 100);
        assert!(progress >= 2, "the list streamed as folders landed");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_culprit_follows_the_biggest_child_down() {
        let root = scratch("culprit");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "x".repeat(100)).unwrap();
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::write(root.join("node_modules/dep/index.js"), "x".repeat(20_000)).unwrap();

        let live = AtomicBool::new(false);
        let culprit = find_culprit_blocking(&root, &live);
        let names: Vec<&str> = culprit.chain.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["node_modules"], "the search stops at node_modules");
        assert_eq!(culprit.chain[0].size, 20_000);
        assert_eq!(culprit.top.len(), 2, "both children are listed as the spread");
        assert_eq!(culprit.top[0].name, "node_modules");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_spread_out_folder_has_no_chain() {
        let root = scratch("spread");
        for name in ["a", "b", "c"] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.bin"), "x".repeat(1000)).unwrap();
        }
        let live = AtomicBool::new(false);
        let culprit = find_culprit_blocking(&root, &live);
        assert!(culprit.chain.is_empty(), "no one child holds half of it");
        assert_eq!(culprit.top.len(), 3);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cancelled_walk_returns_no_size_and_stops() {
        let root = scratch("cancel");
        fs::create_dir_all(root.join("deep")).unwrap();
        fs::write(root.join("deep/a.txt"), "x".repeat(100)).unwrap();
        // Raised before the walk starts: the first check inside `folder_size` returns None
        // rather than measuring a tree the user navigated away from.
        let stop = AtomicBool::new(true);
        assert_eq!(folder_size(&root, &stop), None, "a raised flag yields no size");
        // And a live flag still measures, so the check is not just refusing everything.
        let live = AtomicBool::new(false);
        assert_eq!(folder_size(&root, &live), Some(100));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_cancelled_listing_keeps_the_list_it_had() {
        let root = scratch("listing-cancel");
        fs::create_dir_all(root.join("one")).unwrap();
        fs::write(root.join("one/a.txt"), "x".repeat(10)).unwrap();
        fs::write(root.join("loose.txt"), "x".repeat(50)).unwrap();
        let stop = AtomicBool::new(true);
        let items = list_folder_blocking(&root, &stop, |_| {});
        // Nothing beyond the immediate files is measured: the folders keep size 0.
        assert!(items.iter().any(|i| i.name == "loose.txt" && i.size == 50));
        assert!(
            items.iter().filter(|i| i.dir).all(|i| i.size == 0),
            "cancelled folders are never walked: {items:?}"
        );
        fs::remove_dir_all(&root).ok();
    }
}