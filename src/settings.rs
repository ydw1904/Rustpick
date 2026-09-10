//! settings.json: plain JSON, not compatible with upstream's typed format.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

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

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let tmp = path.with_extension("json.new");
        std::fs::create_dir_all(data_dir())?;
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
