//! Best-effort persistence of settings, read-state, and bookmarks to a small
//! JSON file in the platform's data directory. All operations fail silently —
//! losing local UI state is never worth interrupting the user.
//!
//! Read-state and bookmarks are only written when the user has enabled them in
//! the in-app settings pane, and the settings are saved alongside them. With
//! every setting at its default nothing is written, and any existing file is
//! removed.
//!
//! Writes are atomic (a temporary file renamed over the original), so an
//! interrupted save can never leave a truncated file that would load as empty
//! and then be saved over, losing bookmarks.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::Item;

/// Most read-story ids kept on disk. Hacker News ids increase over time, so
/// keeping the largest drops the oldest stories first, and the file stays
/// small (tens of KB) however long the app is used.
const MAX_READ: usize = 5_000;

/// Persistence is opt-in: both default to `false` so a fresh install writes
/// nothing to disk until the user enables it in the settings pane.
#[derive(Default, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    #[serde(default)]
    pub remember_read: bool,
    #[serde(default)]
    pub remember_bookmarks: bool,
    /// Capture the mouse for wheel scrolling and click-to-select. Off by
    /// default: capturing stops the terminal's own click-and-drag selection.
    #[serde(default)]
    pub mouse: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub read: Vec<u64>,
    #[serde(default)]
    pub saved: Vec<Item>,
}

/// Load persisted state, returning defaults on any error (missing file, bad JSON).
pub fn load() -> Store {
    let Some(path) = state_path() else {
        return Store::default();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Store::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Persist settings plus whichever data their toggles permit. Silent on failure.
///
/// When persistence is fully disabled, nothing is written — and any existing
/// state file is removed — so opting out leaves no trace on disk.
pub fn save(settings: &Settings, read: &HashSet<u64>, saved: &[Item]) {
    let Some(path) = state_path() else {
        return;
    };
    let Some(store) = snapshot(settings, read, saved) else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    if let Ok(json) = serde_json::to_vec_pretty(&store) {
        let _ = write_atomic(&path, &json);
    }
}

/// What to persist for the given state, or `None` when every setting is at
/// its default (nothing to remember, not even a preference).
fn snapshot(settings: &Settings, read: &HashSet<u64>, saved: &[Item]) -> Option<Store> {
    if *settings == Settings::default() {
        return None;
    }
    let read = if settings.remember_read {
        let mut ids: Vec<u64> = read.iter().copied().collect();
        ids.sort_unstable_by(|a, b| b.cmp(a)); // newest first
        ids.truncate(MAX_READ);
        ids
    } else {
        Vec::new()
    };
    let saved = if settings.remember_bookmarks {
        saved.to_vec()
    } else {
        Vec::new()
    };
    Some(Store {
        settings: settings.clone(),
        read,
        saved,
    })
}

/// Replace `path` with `bytes` all-or-nothing: write a sibling temp file, then
/// rename it into place (atomic on the same filesystem).
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn state_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("state.json"))
}

/// Platform data directory, computed from environment without extra crates.
fn data_dir() -> Option<PathBuf> {
    use std::env::var_os;
    let app = "hacker-news-tui";

    if cfg!(target_os = "macos") {
        var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library/Application Support")
                .join(app)
        })
    } else if cfg!(target_os = "windows") {
        var_os("APPDATA").map(|a| PathBuf::from(a).join(app))
    } else {
        var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|p| p.join(app))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_is_opt_in_by_default() {
        let s = Settings::default();
        assert!(!s.remember_read);
        assert!(!s.remember_bookmarks);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A partial file with no settings block loads as opt-out (disabled).
        let store: Store = serde_json::from_str(r#"{"read":[1,2,3]}"#).unwrap();
        assert_eq!(store.read, vec![1, 2, 3]);
        assert!(!store.settings.remember_read);
        assert!(store.saved.is_empty());
    }

    fn on(read: bool, bookmarks: bool) -> Settings {
        Settings {
            remember_read: read,
            remember_bookmarks: bookmarks,
            ..Default::default()
        }
    }

    #[test]
    fn a_changed_preference_is_saved_without_any_data() {
        let settings = Settings {
            mouse: true,
            ..Default::default()
        };
        let s = snapshot(&settings, &HashSet::from([1]), &[Item::default()]).unwrap();
        assert!(s.settings.mouse);
        assert!(s.read.is_empty());
        assert!(s.saved.is_empty());
    }

    #[test]
    fn nothing_is_persisted_when_both_toggles_are_off() {
        assert!(snapshot(&on(false, false), &HashSet::from([1]), &[]).is_none());
    }

    #[test]
    fn only_enabled_data_is_persisted() {
        let read = HashSet::from([1, 2]);
        let saved = [Item::default()];
        let s = snapshot(&on(true, false), &read, &saved).unwrap();
        assert_eq!(s.read.len(), 2);
        assert!(s.saved.is_empty());
        let s = snapshot(&on(false, true), &read, &saved).unwrap();
        assert!(s.read.is_empty());
        assert_eq!(s.saved.len(), 1);
    }

    #[test]
    fn read_history_is_capped_to_the_newest_ids() {
        let read: HashSet<u64> = (1..=(MAX_READ as u64 + 10)).collect();
        let s = snapshot(&on(true, false), &read, &[]).unwrap();
        assert_eq!(s.read.len(), MAX_READ);
        assert_eq!(s.read[0], MAX_READ as u64 + 10); // newest kept
        assert!(!s.read.contains(&10)); // oldest dropped
    }

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("hn-tui-store-{}", std::process::id()));
        let path = dir.join("nested").join("state.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_round_trips_through_json() {
        let store = Store {
            settings: Settings {
                remember_read: false,
                remember_bookmarks: true,
                mouse: true,
            },
            read: vec![10, 20],
            saved: vec![Item {
                id: 99,
                title: "kept".into(),
                ..Default::default()
            }],
        };
        let json = serde_json::to_vec(&store).unwrap();
        let back: Store = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.settings, store.settings);
        assert_eq!(back.read, store.read);
        assert_eq!(back.saved.len(), 1);
        assert_eq!(back.saved[0].id, 99);
    }
}
