//! Standalone XDG JSON UI preferences persistence (zero GSettings schemas).
//!
//! `./bin/fedora-updater` is distributed as a portable single binary, so UI
//! window geometry and sidebar state are stored in
//! `$XDG_CONFIG_HOME/fedora-updater/ui.json` (falling back to
//! `$HOME/.config/fedora-updater/ui.json`).

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_WINDOW_WIDTH: i32 = 960;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 676;
pub const MIN_WINDOW_WIDTH: i32 = 360;
pub const MIN_WINDOW_HEIGHT: i32 = 360;
const MAX_WINDOW_EXTENT: i32 = 16_384;

const APP_CONFIG_DIR: &str = "fedora-updater";
const PREFS_FILE_NAME: &str = "ui.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub sidebar_open: bool,
    pub window_width: i32,
    pub window_height: i32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            window_width: DEFAULT_WINDOW_WIDTH,
            window_height: DEFAULT_WINDOW_HEIGHT,
        }
    }
}

impl UiPrefs {
    /// Clamp out-of-range dimensions back to Observatory defaults while keeping
    /// the sidebar toggle intact.
    pub fn normalized(self) -> Self {
        let window_width =
            if (MIN_WINDOW_WIDTH..=MAX_WINDOW_EXTENT).contains(&self.window_width) {
                self.window_width
            } else {
                DEFAULT_WINDOW_WIDTH
            };
        let window_height =
            if (MIN_WINDOW_HEIGHT..=MAX_WINDOW_EXTENT).contains(&self.window_height) {
                self.window_height
            } else {
                DEFAULT_WINDOW_HEIGHT
            };
        Self {
            sidebar_open: self.sidebar_open,
            window_width,
            window_height,
        }
    }
}

/// Resolve the XDG UI preferences file path from explicit environment values.
pub fn config_path_from(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(dir) = xdg_config_home.filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(dir)
                .join(APP_CONFIG_DIR)
                .join(PREFS_FILE_NAME),
        );
    }
    if let Some(home_dir) = home.filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(home_dir)
                .join(".config")
                .join(APP_CONFIG_DIR)
                .join(PREFS_FILE_NAME),
        );
    }
    None
}

pub fn default_config_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    config_path_from(xdg.as_deref(), home.as_deref())
}

pub fn persistence_disabled_from(preview: Option<&OsStr>, screenshot: Option<&OsStr>) -> bool {
    preview.is_some() || screenshot.is_some()
}

pub fn is_persistence_disabled() -> bool {
    let preview = std::env::var_os("FEDORA_UPDATER_PREVIEW");
    let screenshot = std::env::var_os("FEDORA_UPDATER_SCREENSHOT");
    persistence_disabled_from(preview.as_deref(), screenshot.as_deref())
}

/// Load preferences from `path`, silently falling back to defaults if missing
/// or corrupt.
pub fn load_from(path: &Path) -> UiPrefs {
    let Ok(raw) = fs::read_to_string(path) else {
        return UiPrefs::default();
    };
    serde_json::from_str::<UiPrefs>(&raw)
        .map(UiPrefs::normalized)
        .unwrap_or_default()
}

/// Persist preferences atomically via a sibling temp file and rename.
pub fn save_to(path: &Path, prefs: &UiPrefs) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let normalized = prefs.normalized();
    let mut payload = serde_json::to_string_pretty(&normalized)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    payload.push('\n');

    let tmp_name = format!("{PREFS_FILE_NAME}.tmp.{}", std::process::id());
    let tmp_path = path.with_file_name(tmp_name);
    if let Err(err) = fs::write(&tmp_path, payload.as_bytes()) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

pub fn load_if_enabled(path: Option<&Path>, disabled: bool) -> UiPrefs {
    if disabled {
        return UiPrefs::default();
    }
    path.map(load_from).unwrap_or_default()
}

pub fn save_if_enabled(path: Option<&Path>, prefs: &UiPrefs, disabled: bool) -> bool {
    if disabled {
        return false;
    }
    let Some(path) = path else {
        return false;
    };
    save_to(path, prefs).is_ok()
}

pub fn load() -> UiPrefs {
    let path = default_config_path();
    load_if_enabled(path.as_deref(), is_persistence_disabled())
}

pub fn save(prefs: &UiPrefs) -> bool {
    let path = default_config_path();
    save_if_enabled(path.as_deref(), prefs, is_persistence_disabled())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_observatory_desktop_geometry() {
        let prefs = UiPrefs::default();
        assert!(prefs.sidebar_open);
        assert_eq!(prefs.window_width, 960);
        assert_eq!(prefs.window_height, 676);
    }

    #[test]
    fn config_path_prefers_xdg_then_falls_back_to_home() {
        assert_eq!(
            config_path_from(Some(OsStr::new("/tmp/xdg-cfg")), Some(OsStr::new("/home/alice"))),
            Some(PathBuf::from("/tmp/xdg-cfg/fedora-updater/ui.json"))
        );
        assert_eq!(
            config_path_from(None, Some(OsStr::new("/home/alice"))),
            Some(PathBuf::from("/home/alice/.config/fedora-updater/ui.json"))
        );
        assert_eq!(
            config_path_from(Some(OsStr::new("")), Some(OsStr::new("/home/alice"))),
            Some(PathBuf::from("/home/alice/.config/fedora-updater/ui.json"))
        );
        assert_eq!(
            config_path_from(Some(OsStr::new("")), Some(OsStr::new(""))),
            None
        );
        assert_eq!(config_path_from(None, None), None);
    }

    #[test]
    fn save_and_load_round_trip_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/fedora-updater/ui.json");
        let prefs = UiPrefs {
            sidebar_open: false,
            window_width: 1120,
            window_height: 740,
        };

        save_to(&path, &prefs).expect("save ui prefs");
        assert!(path.is_file());
        assert_eq!(load_from(&path), prefs);
    }

    #[test]
    fn load_recovers_cleanly_from_missing_empty_corrupt_or_invalid_dimensions() {
        let dir = tempfile::tempdir().expect("tempdir");

        let missing = dir.path().join("does-not-exist.json");
        assert_eq!(load_from(&missing), UiPrefs::default());

        let empty = dir.path().join("empty.json");
        fs::write(&empty, "").expect("write empty");
        assert_eq!(load_from(&empty), UiPrefs::default());

        let corrupt = dir.path().join("corrupt.json");
        fs::write(&corrupt, "{not valid json!!!").expect("write corrupt");
        assert_eq!(load_from(&corrupt), UiPrefs::default());

        let partial = dir.path().join("partial.json");
        fs::write(&partial, r#"{"sidebar_open": false}"#).expect("write partial");
        assert_eq!(
            load_from(&partial),
            UiPrefs {
                sidebar_open: false,
                window_width: DEFAULT_WINDOW_WIDTH,
                window_height: DEFAULT_WINDOW_HEIGHT,
            }
        );

        let undersized = dir.path().join("undersized.json");
        fs::write(
            &undersized,
            r#"{"sidebar_open": false, "window_width": 0, "window_height": -20}"#,
        )
        .expect("write undersized");
        assert_eq!(
            load_from(&undersized),
            UiPrefs {
                sidebar_open: false,
                window_width: DEFAULT_WINDOW_WIDTH,
                window_height: DEFAULT_WINDOW_HEIGHT,
            }
        );
    }

    #[test]
    fn preview_or_screenshot_flags_bypass_disk_reads_and_writes() {
        assert!(!persistence_disabled_from(None, None));
        assert!(persistence_disabled_from(Some(OsStr::new("ready")), None));
        assert!(persistence_disabled_from(
            None,
            Some(OsStr::new("/tmp/out.png"))
        ));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fedora-updater/ui.json");
        let custom = UiPrefs {
            sidebar_open: false,
            window_width: 1024,
            window_height: 700,
        };

        assert!(!save_if_enabled(Some(&path), &custom, true));
        assert!(
            !path.exists(),
            "disabled save must never create or write ui.json"
        );

        assert!(save_if_enabled(Some(&path), &custom, false));
        assert_eq!(load_if_enabled(Some(&path), false), custom);
        assert_eq!(
            load_if_enabled(Some(&path), true),
            UiPrefs::default(),
            "disabled load must return defaults rather than reading disk"
        );
        assert_eq!(load_if_enabled(None, false), UiPrefs::default());
        assert!(!save_if_enabled(None, &custom, false));
    }
}
