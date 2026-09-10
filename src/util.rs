//! Small helpers shared across the crate.

use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngExt;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

pub const CHARS_ASCII: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
pub const CHARS_HEX_LOWER: &[u8] = b"0123456789abcdef";

pub fn nonce(chars: &[u8], len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| chars[rng.random_range(0..chars.len())] as char)
        .collect()
}

/// Parse a Twitch ISO-8601 timestamp (with or without fractional seconds).
pub fn timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub fn isonow() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Read a JSON field that Twitch returns either as a number or a numeric string.
pub fn json_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn json_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

/// Game name -> directory slug, mirroring upstream's regex chain.
pub fn slug(name: &str) -> String {
    static NON_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\W+").unwrap());
    static DASHES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-{2,}").unwrap());
    let s = name.to_lowercase().replace('\'', "");
    let s = NON_WORD.replace_all(&s, "-");
    let s = s.trim_matches('-');
    DASHES.replace_all(s, "-").into_owned()
}

/// Strip the "-285x380" dimension suffix from a box art URL.
pub fn remove_dimensions(url: &str) -> String {
    static DIMS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)-\d+x\d+(\.(?:jpg|png|gif))$").unwrap());
    DIMS.replace(url, "$1").into_owned()
}

pub struct Backoff {
    steps: u32,
    maximum: f64,
}

impl Backoff {
    pub fn new(maximum_secs: f64) -> Self {
        Self { steps: 0, maximum: maximum_secs }
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn delay(&mut self) -> Duration {
        let value = 2f64.powi(self.steps as i32) * rand::rng().random_range(0.9..1.1);
        if value > self.maximum {
            return Duration::from_secs_f64(self.maximum);
        }
        self.steps += 1;
        Duration::from_secs_f64(value)
    }

    pub fn reset(&mut self) {
        self.steps = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_upstream() {
        assert_eq!(slug("Tom Clancy's Rainbow Six Siege"), "tom-clancys-rainbow-six-siege");
        assert_eq!(slug("  Fall Guys: Ultimate Knockout "), "fall-guys-ultimate-knockout");
    }

    #[test]
    fn dimensions_removed() {
        assert_eq!(
            remove_dimensions("https://x/ttv-boxart/123-285x380.jpg"),
            "https://x/ttv-boxart/123.jpg"
        );
        assert_eq!(remove_dimensions("https://x/a.png"), "https://x/a.png");
    }

    #[test]
    fn timestamps_parse() {
        assert!(timestamp("2024-01-02T03:04:05.678Z").is_some());
        assert!(timestamp("2024-01-02T03:04:05Z").is_some());
        assert!(timestamp("nope").is_none());
    }
}
