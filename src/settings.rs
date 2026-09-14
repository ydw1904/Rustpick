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
    /// HTTP proxy URL; empty for none. Also tunnels the PubSub websocket (CONNECT).
    pub proxy: String,
    pub priority: Vec<String>,
    pub exclude: BTreeSet<String>,
    pub priority_mode: PriorityMode,
    /// 1..=6, multiplies request timeouts.
    pub connection_quality: u32,
    pub enable_badges_emotes: bool,
    pub available_drops_check: bool,
    pub dark_mode: bool,
    /// UI language for the core's messages; one of `translate::LANGUAGES`.
    pub language: String,
    /// GUI: desktop notification when a drop is claimed.
    pub tray_notifications: bool,
    /// GUI: the autostart entry launches minimized to the tray.
    pub autostart_tray: bool,
    /// Webhook URL that gets a JSON POST on claims, login needed and all done; empty for off.
    pub notify_url: String,
    /// Local-time `[start, end)` hours to mine in, wrapping past midnight (`[22, 6]`); None = always.
    pub active_hours: Option<(u8, u8)>,
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
            language: "English".to_owned(),
            tray_notifications: true,
            autostart_tray: false,
            notify_url: String::new(),
            active_hours: None,
        }
    }
}

/// Read TDM field `key`, transparently unwrapping its `{"__type":..,"data":..}`
/// wrapper. Plain (unwrapped) values pass through unchanged.
fn tdm_data<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let field = v.get(key)?;
    Some(field.get("data").unwrap_or(field))
}

static PROFILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Select a named profile for this process. Call once, before anything reads `data_dir()`.
pub fn set_profile(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("profile names may only use letters, digits, '-' and '_'");
    }
    PROFILE.set(name.to_owned()).map_err(|_| anyhow::anyhow!("profile already set"))?;
    let dir = data_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))
}

pub fn profile() -> Option<&'static str> {
    PROFILE.get().map(String::as_str)
}

/// The platform's per-user config directory.
fn config_dir() -> PathBuf {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let home = || env("HOME").unwrap_or_default();
    if cfg!(windows) {
        env("APPDATA").unwrap_or_default()
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else {
        env("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
    }
}

/// Directory holding settings.json and auth.json: $RUSTPICK_DIR, else the `--profile`
/// directory under the platform config dir, else the current directory.
pub fn data_dir() -> PathBuf {
    std::env::var_os("RUSTPICK_DIR")
        .map(PathBuf::from)
        .or_else(|| profile().map(|p| config_dir().join("rustpick").join(p)))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Is `hour` inside the `[start, end)` window? Wraps past midnight; start == end is all day.
pub fn in_active_hours(hour: u8, (start, end): (u8, u8)) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Equal => true,
        std::cmp::Ordering::Less => (start..end).contains(&hour),
        std::cmp::Ordering::Greater => hour >= start || hour < end,
    }
}

/// Parse `22-6` into `(22, 6)`.
pub fn parse_active_hours(s: &str) -> Option<(u8, u8)> {
    let (a, b) = s.split_once('-')?;
    let (a, b) = (a.trim().parse::<u8>().ok()?, b.trim().parse::<u8>().ok()?);
    (a < 24 && b < 24).then_some((a, b))
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
        s.apply_env()?;
        s.connection_quality = s.connection_quality.clamp(1, 6);
        Ok(s)
    }

    /// `RUSTPICK_<FIELD>` environment overrides (for containers); lists are comma-separated.
    // ponytail: overrides get persisted by the next save(); split file and env layers if that bites.
    fn apply_env(&mut self) -> Result<()> {
        let var = |k: &str| std::env::var(format!("RUSTPICK_{k}")).ok();
        fn list<T: FromIterator<String>>(v: String) -> T {
            v.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
        }
        if let Some(v) = var("PROXY") {
            self.proxy = v;
        }
        if let Some(v) = var("NOTIFY_URL") {
            self.notify_url = v;
        }
        if let Some(v) = var("LANGUAGE") {
            self.language = v;
        }
        if let Some(v) = var("PRIORITY") {
            self.priority = list(v);
        }
        if let Some(v) = var("EXCLUDE") {
            self.exclude = list(v);
        }
        if let Some(v) = var("PRIORITY_MODE") {
            self.priority_mode = PriorityMode::parse(&v).with_context(|| format!("RUSTPICK_PRIORITY_MODE={v}"))?;
        }
        if let Some(v) = var("ACTIVE_HOURS") {
            self.active_hours = match v.as_str() {
                "" | "off" => None,
                _ => Some(parse_active_hours(&v).with_context(|| format!("RUSTPICK_ACTIVE_HOURS={v}"))?),
            };
        }
        if let Some(v) = var("CONNECTION_QUALITY") {
            self.connection_quality = v.parse().with_context(|| format!("RUSTPICK_CONNECTION_QUALITY={v}"))?;
        }
        Ok(())
    }

    /// Build settings from a TwitchDropsMiner install. `path` is either its
    /// `settings.json` or the folder containing it. TDM wraps `set`/`Enum`/`URL`
    /// values as `{"__type":..,"data":..}`; [`tdm_data`] unwraps those. Fields TDM
    /// Fields missing from the file keep our defaults.
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
        if let Some(b) = tdm_data(&v, "tray_notifications").and_then(|x| x.as_bool()) {
            s.tray_notifications = b;
        }
        if let Some(b) = tdm_data(&v, "autostart_tray").and_then(|x| x.as_bool()) {
            s.autostart_tray = b;
        }
        if let Some(lang) = tdm_data(&v, "language").and_then(|x| x.as_str()) {
            s.language = crate::translate::LANGUAGES
                .iter()
                .map(|(name, _)| *name)
                .find(|n| n.eq_ignore_ascii_case(lang))
                .unwrap_or("English")
                .to_owned();
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
        assert!(s.autostart_tray);
        assert!(s.tray_notifications); // absent from the file, keeps the default
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn active_hours_window() {
        assert_eq!(parse_active_hours("22-06"), Some((22, 6)));
        assert_eq!(parse_active_hours("9-24"), None);
        assert!(in_active_hours(23, (22, 6)) && in_active_hours(5, (22, 6)));
        assert!(!in_active_hours(6, (22, 6)) && !in_active_hours(12, (22, 6)));
        assert!(in_active_hours(9, (9, 17)) && !in_active_hours(17, (9, 17)));
        assert!(in_active_hours(3, (8, 8)));
    }

    #[test]
    fn import_tdm_language() {
        let dir = std::env::temp_dir().join(format!("rustpick-tdm-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"language": "DEUTSCH"}"#).unwrap();
        let s = Settings::import_tdm(&dir).unwrap();
        assert_eq!(s.language, "Deutsch");
        std::fs::write(dir.join("settings.json"), r#"{"language": "Klingon"}"#).unwrap();
        let s = Settings::import_tdm(&dir).unwrap();
        assert_eq!(s.language, "English");
        std::fs::remove_dir_all(&dir).ok();
    }
}
