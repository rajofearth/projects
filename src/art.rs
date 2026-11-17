//! Pictures from disk: each editor's own icon, and a project's logo. Decoding runs on Rune's
//! `ImageCache` worker, so nothing here blocks a frame. This module finds the files and keeps
//! the handles alive; a handle that nothing holds is dropped and decoded again.
//!
//! Views read through [`ready`], so a picture that is still decoding (or failed) gives way to
//! the glyph or initial the caller draws instead.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rune::{Image, ImageCache, Waker};

use crate::launch::App;

/// Editor icons are drawn at this many points square (the menu and the detail button).
const APP_PT: f32 = 16.0;
/// Project logos are rasterized at this many points square; smaller draws use the mips.
const LOGO_PT: f32 = 40.0;

/// Where a project keeps its logo, relative to the project folder, most specific first.
/// Expo's `icon` in app.json is checked before these.
const LOGO_FILES: &[&str] = &[
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "public/icon.svg",
    "public/icon.png",
    "public/logo.svg",
    "public/logo.png",
    "app/icon.svg",
    "app/icon.png",
    "app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "src/app/favicon.ico",
    "static/favicon.svg",
    "static/favicon.ico",
    "static/favicon.png",
    "src-tauri/icons/icon.png",
    "src-tauri/icons/128x128.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    "assets/favicon.ico",
    "assets/favicon.png",
    "icon.svg",
    "icon.png",
    "logo.svg",
    "logo.png",
    ".github/logo.svg",
    ".github/logo.png",
];

/// The first logo file a project has, or `None`. Costs a handful of `is_file` checks plus a
/// read of app.json when there is one.
pub fn find_logo(project: &Path) -> Option<PathBuf> {
    if let Some(expo) = expo_icon(project) {
        return Some(expo);
    }
    LOGO_FILES.iter().map(|f| project.join(f)).find(|p| p.is_file())
}

/// Expo's `expo.icon` (or a top-level `icon`) from app.json, when it names an existing file.
fn expo_icon(project: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(project.join("app.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let rel = json
        .pointer("/expo/icon")
        .or_else(|| json.get("icon"))?
        .as_str()?;
    let path = project.join(rel);
    path.is_file().then_some(path)
}

/// Decoded pictures, kept while they are on screen. Lives in the app state and is created
/// in setup, once the window's waker exists.
#[derive(Default)]
pub struct Art {
    cache: Option<ImageCache>,
    /// The window's device pixels per point, set by the view each frame.
    scale: Cell<f32>,
    /// Editor icons by app id and scale (scale × 100). `None`: the app has no icon file.
    apps: RefCell<HashMap<(&'static str, u32), Option<Image>>>,
    /// Logos by project folder and scale (scale × 100). `None`: no logo file.
    logos: RefCell<HashMap<(PathBuf, u32), Option<Image>>>,
}

impl Art {
    /// Start decoding. Call once from setup, with the app's waker.
    pub fn start(&mut self, waker: Waker) {
        self.cache = Some(ImageCache::new(waker));
    }

    /// The window's scale (`Cx::env().scale`), so pictures are rasterized at the size they draw at.
    pub fn set_scale(&self, scale: f32) {
        if scale.is_finite() && scale > 0.0 {
            self.scale.set(scale);
        }
    }

    /// The scale pictures are rasterized at: the one the view reported, 2 until it has.
    fn scale(&self) -> f32 {
        let s = self.scale.get();
        if s > 0.0 { s } else { 2.0 }
    }

    /// The app's own icon, requested on first use and kept after that. Pending until decoded.
    pub fn app(&self, app: &App) -> Option<Image> {
        let exe = app.exe.as_ref()?;
        let cache = self.cache.as_ref()?;
        let scale = self.scale();
        let key = (app.id, scale_key(scale));
        self.apps
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Some(cache.app_icon_scaled(exe, APP_PT, scale)))
            .clone()
    }

    /// The project's logo, found on first use and kept after that. Pending until decoded.
    pub fn logo(&self, project: &Path) -> Option<Image> {
        let cache = self.cache.as_ref()?;
        let scale = self.scale();
        let key = (project.to_path_buf(), scale_key(scale));
        self.logos
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| find_logo(project).map(|file| cache.file(file, LOGO_PT, scale)))
            .clone()
    }
}

/// The image if it has decoded and did not fail; `None` while pending or on error.
pub fn ready(image: Option<Image>) -> Option<Image> {
    image.filter(|i| i.is_ready() && i.error().is_none())
}

fn scale_key(scale: f32) -> u32 {
    (scale.max(0.0) * 100.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("art-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_the_first_known_logo_and_none_otherwise() {
        let dir = temp_dir("logo");
        assert_eq!(find_logo(&dir), None);
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public/logo.png"), b"x").unwrap();
        std::fs::write(dir.join("public/favicon.ico"), b"x").unwrap();
        assert_eq!(find_logo(&dir), Some(dir.join("public/favicon.ico")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expo_icon_wins_over_the_static_list() {
        let dir = temp_dir("expo");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/icon.png"), b"x").unwrap();
        std::fs::write(dir.join("app.json"), r#"{"expo":{"name":"a","icon":"./assets/icon.png"}}"#).unwrap();
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public/favicon.ico"), b"x").unwrap();
        assert_eq!(find_logo(&dir), Some(dir.join("./assets/icon.png")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
