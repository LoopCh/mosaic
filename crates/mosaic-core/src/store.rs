//! Persistence: one JSON file in the user's profile.

use std::path::{Path, PathBuf};

use crate::model::State;

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("MOSAIC_HOME") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        if let Ok(a) = std::env::var("APPDATA") {
            return PathBuf::from(a).join("Mosaic");
        }
    }
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(x).join("mosaic");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".config").join("mosaic");
    }
    PathBuf::from(".mosaic")
}

pub fn load_state(dir: &Path) -> State {
    let path = dir.join("state.json");
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
            Ok(s) => s,
            Err(_) => {
                // Keep the broken file for inspection, start clean.
                let _ = std::fs::rename(&path, dir.join("state.broken.json"));
                State::default()
            }
        },
        Err(_) => State::default(),
    }
}

pub fn save_state(dir: &Path, st: &State) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("state.json.tmp");
    let bytes = serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("state.json")).map_err(|e| e.to_string())
}
