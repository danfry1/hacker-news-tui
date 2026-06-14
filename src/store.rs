//! Best-effort persistence of settings, read-state, and bookmarks to a small
//! JSON file in the platform's data directory. All operations fail silently —
//! losing local UI state is never worth interrupting the user.
//!
//! Read-state and bookmarks are only written when the user has enabled them in
//! the in-app settings pane; the settings themselves are always persisted so the
//! choice sticks across runs.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::Item;

/// Persistence is opt-in: both default to `false` so a fresh install writes
/// nothing to disk until the user enables it in the settings pane.
#[derive(Default, Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    #[serde(default)]
    pub remember_read: bool,
    #[serde(default)]
    pub remember_bookmarks: bool,
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
    if !settings.remember_read && !settings.remember_bookmarks {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let store = Store {
        settings: settings.clone(),
        read: if settings.remember_read {
            read.iter().copied().collect()
        } else {
            Vec::new()
        },
        saved: if settings.remember_bookmarks {
            saved.to_vec()
        } else {
            Vec::new()
        },
    };
    if let Ok(json) = serde_json::to_vec_pretty(&store) {
        let _ = std::fs::write(path, json);
    }
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

    #[test]
    fn store_round_trips_through_json() {
        let store = Store {
            settings: Settings {
                remember_read: false,
                remember_bookmarks: true,
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
