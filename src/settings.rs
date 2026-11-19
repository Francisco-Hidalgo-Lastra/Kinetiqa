//! App settings, stored as JSON in the user's config folder (e.g. `~/.config/kinetiqa/settings.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where every board is saved to and opened from.
    pub boards_dir: PathBuf,
    /// Keep the top toolbar visible instead of auto-hiding it.
    pub always_show_toolbar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { boards_dir: default_boards_dir(), always_show_toolbar: false }
    }
}

impl Settings {
    /// Saved settings, or the defaults if there are none (or they can't be read).
    pub fn load() -> Self {
        config_file()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let file = config_file().ok_or("no config folder found")?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(file, json).map_err(|e| e.to_string())
    }
}

/// `~/Kinetiqa` (or `Kinetiqa` in the working directory if there's no home folder).
pub fn default_boards_dir() -> PathBuf {
    home_dir().unwrap_or_default().join("Kinetiqa")
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) { env_dir("USERPROFILE") } else { env_dir("HOME") }
}

fn config_file() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        env_dir("APPDATA")?
    } else if cfg!(target_os = "macos") {
        home_dir()?.join("Library/Application Support")
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| Some(home_dir()?.join(".config")))?
    };
    Some(dir.join("kinetiqa").join("settings.json"))
}
