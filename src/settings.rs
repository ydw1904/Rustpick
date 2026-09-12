//! settings.json: plain JSON, not compatible with upstream's typed format.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PriorityMode {
    #[default]
    PriorityOnly,
    EndingSoonest,
    LowAvblFirst,
}

impl PriorityMode {
    pub const ALL: [PriorityMode; 3] =
        [Self::PriorityOnly, Self::EndingSoonest, Self::LowAvblFirst];

    pub fn label(self) -> &'static str {
        match self {
            Self::PriorityOnly => "Priority list only",
            Self::EndingSoonest => "Ending soonest first",
            Self::LowAvblFirst => "Low availability first",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.replace('-', "_").as_str() {
            "priority_only" => Some(Self::PriorityOnly),
            "ending_soonest" => Some(Self::EndingSoonest),
            "low_avbl_first" => Some(Self::LowAvblFirst),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// HTTP(S) proxy URL; empty for none. Not applied to the websocket.
    pub proxy: String,
    pub priority: Vec<String>,
    pub exclude: BTreeSet<String>,
    pub priority_mode: PriorityMode,
    /// 1..=6, multiplies request timeouts.
    pub connection_quality: u32,
    pub enable_badges_emotes: bool,
    pub available_drops_check: bool,
    pub dark_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            proxy: String::new(),
            priority: Vec::new(),
            exclude: BTreeSet::new(),
            priority_mode: PriorityMode::PriorityOnly,
            connection_quality: 1,
            enable_badges_emotes: false,
            available_drops_check: false,
            dark_mode: false,
        }
    }
}

/// Read TDM field `key`, transparently unwrapping its `{"__type":..,"data":..}`
/// wrapper. Plain (unwrapped) values pass through unchanged.
fn tdm_data<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let field = v.get(key)?;
    Some(field.get("data").unwrap_or(field))
}

/// Directory holding settings.json and auth.json: $RUSTPICK_DIR, else the current directory.
pub fn data_dir() -> PathBuf {
    std::env::var_os("RUSTPICK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut s: Settings =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        s.connection_quality = s.connection_quality.clamp(1, 6);
        Ok(s)
    }

    /// Build settings from a TwitchDropsMiner install. `path` is either its
    /// `settings.json` or the folder containing it. TDM wraps `set`/`Enum`/`URL`
    /// values as `{"__type":..,"data":..}`; [`tdm_data`] unwraps those. Fields TDM
    /// has but Rustpick lacks (language, autostart_tray, tray_notifications) are
    /// ignored. Fields missing from the file keep our defaults.
    pub fn import_tdm(path: &Path) -> Result<Self> {
        let file = if path.is_dir() { path.join("settings.json") } else { path.to_path_buf() };
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;

        let mut s = Self::default();
        if let Some(p) = tdm_data(&v, "proxy").and_then(|x| x.as_str()) {
            s.proxy = p.to_string();
        }
        if let Some(arr) = tdm_data(&v, "priority").and_then(|x| x.as_array()) {
            s.priority = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        }
        if let Some(arr) = tdm_data(&v, "exclude").and_then(|x| x.as_array()) {
            s.exclude = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        }
        if let Some(n) = tdm_data(&v, "connection_quality").and_then(|x| x.as_u64()) {
            s.connection_quality = (n as u32).clamp(1, 6);
        }
        if let Some(m) = tdm_data(&v, "priority_mode").and_then(|x| x.as_u64()) {
            s.priority_mode = match m {
                1 => PriorityMode::EndingSoonest,
                2 => PriorityMode::LowAvblFirst,
                _ => PriorityMode::PriorityOnly,
            };
        }
        if let Some(b) = tdm_data(&v, "dark_mode").and_then(|x| x.as_bool()) {
            s.dark_mode = b;
        }
        if let Some(b) = tdm_data(&v, "enable_badges_emotes").and_then(|x| x.as_bool()) {
            s.enable_badges_emotes = b;
        }
        if let Some(b) = tdm_data(&v, "available_drops_check").and_then(|x| x.as_bool()) {
            s.available_drops_check = b;
        }
        Ok(s)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let tmp = path.with_extension("json.new");
        std::fs::create_dir_all(data_dir())?;
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_tdm_typed_format() {
        let dir = std::env::temp_dir().join(format!("rustpick-tdm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "proxy": {"__type": "URL", "data": "http://user:pass@host:8080"},
            "priority": ["Game A", "Game B"],
            "exclude": {"__type": "set", "data": ["Game C"]},
            "priority_mode": {"__type": "PriorityMode", "data": 2},
            "connection_quality": 9,
            "dark_mode": true,
            "enable_badges_emotes": true,
            "available_drops_check": false,
            "language": "English",
            "autostart_tray": true
        }"#;
        std::fs::write(dir.join("settings.json"), json).unwrap();

        let s = Settings::import_tdm(&dir).unwrap();
        assert_eq!(s.proxy, "http://user:pass@host:8080");
        assert_eq!(s.priority, vec!["Game A", "Game B"]);
        assert!(s.exclude.contains("Game C"));
        assert_eq!(s.priority_mode, PriorityMode::LowAvblFirst);
        assert_eq!(s.connection_quality, 6); // clamped from 9
        assert!(s.dark_mode);
        assert!(s.enable_badges_emotes);
        std::fs::remove_dir_all(&dir).ok();
    }
}
