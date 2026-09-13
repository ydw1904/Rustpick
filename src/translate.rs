//! Message translations, ported from upstream `translate.py`.
//!
//! English is embedded as the base; every other bundled language is deep-merged
//! over it, and missing keys fall back to English. The files are upstream's own
//! `lang/*.json`, so a translation stays in sync with TwitchDropsMiner.

use serde_json::Value;
use std::sync::OnceLock;

/// (name, embedded JSON) for every bundled translation.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("English", include_str!("../lang/English.json")),
    ("العربية", include_str!("../lang/العربية.json")),
    ("Čeština", include_str!("../lang/Čeština.json")),
    ("Dansk", include_str!("../lang/Dansk.json")),
    ("Deutsch", include_str!("../lang/Deutsch.json")),
    ("Español", include_str!("../lang/Español.json")),
    ("Français", include_str!("../lang/Français.json")),
    ("Indonesian", include_str!("../lang/Indonesian.json")),
    ("Italiano", include_str!("../lang/Italiano.json")),
    ("Magyar", include_str!("../lang/Magyar.json")),
    ("Nederlandse", include_str!("../lang/Nederlandse.json")),
    ("Norsk", include_str!("../lang/Norsk.json")),
    ("Polski", include_str!("../lang/Polski.json")),
    ("Português", include_str!("../lang/Português.json")),
    ("Română", include_str!("../lang/Română.json")),
    ("Türkçe", include_str!("../lang/Türkçe.json")),
    ("Русский", include_str!("../lang/Русский.json")),
    ("Українська", include_str!("../lang/Українська.json")),
    ("日本語", include_str!("../lang/日本語.json")),
    ("简体中文", include_str!("../lang/简体中文.json")),
    ("繁體中文", include_str!("../lang/繁體中文.json")),
];

static BASE: OnceLock<Value> = OnceLock::new();
static CURRENT: OnceLock<std::sync::RwLock<Value>> = OnceLock::new();

/// Load `language` into the global translator. Unknown names keep English.
pub fn init(language: &str) {
    let (name, english) = LANGUAGES[0];
    let base: Value = serde_json::from_str(english).expect("bundled English translation");
    let _ = BASE.set(base.clone());
    let current = CURRENT.get_or_init(|| std::sync::RwLock::new(base.clone()));
    let chosen = LANGUAGES
        .iter()
        .find(|(n, _)| *n == language)
        .map(|(_, s)| *s)
        .unwrap_or(english);
    *current.write().unwrap() = if language == name {
        base
    } else {
        match serde_json::from_str::<Value>(chosen) {
            Ok(file) => merge(base, file),
            Err(e) => {
                tracing::warn!("Failed to parse the '{language}' translation: {e}");
                base
            }
        }
    };
}

fn merge(mut base: Value, over: Value) -> Value {
    match (&mut base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => merge(bv, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            base
        }
        (_, over) => over,
    }
}

/// Translate a key path like `_("gui", "status", "idle")`, substituting `{var}`
/// placeholders. Falls back to English, then to the joined key path.
pub fn t(path: &[&str], vars: &[(&str, &str)]) -> String {
    let lookup = |root: Option<&Value>| -> Option<String> {
        let mut v = root?;
        for key in path {
            v = v.get(key)?;
        }
        Some(vars.iter().fold(v.as_str()?.to_owned(), |s, (k, val)| {
            s.replace(&format!("{{{k}}}"), val)
        }))
    };
    let current = CURRENT.get().map(|l| l.read().unwrap());
    lookup(current.as_deref())
        .or_else(|| lookup(BASE.get()))
        .unwrap_or_else(|| {
            tracing::warn!("Missing translation key: {}", path.join(" -> "));
            path.join(" -> ")
        })
}

/// `t!["gui", "status", "idle"]` or `t!["status", "watching"; "channel" => name]`.
/// Placeholder values may be `&str` or `String`.
#[macro_export]
macro_rules! t {
    ($($key:expr),+ $(,)?) => {
        $crate::translate::t(&[$($key),+], &[])
    };
    ($($key:expr),+ ; $($var:expr => $val:expr),+ $(,)?) => {
        $crate::translate::t(&[$($key),+], &[$(($var, std::convert::AsRef::<str>::as_ref(&$val))),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_merge_and_fallback() {
        init("English");
        assert_eq!(t!["gui", "status", "idle"], "Idle");
        assert_eq!(t!["status", "watching"; "channel" => "foo"], "Watching: foo");

        init("Deutsch");
        assert_eq!(t!["gui", "status", "idle"], "Im Leerlauf");
        // key the German file lacks (device login is Rustpick-only) falls back to English
        assert_eq!(
            t!["login", "device_code"; "url" => "u", "code" => "c"],
            "Login required: open u and enter the code c"
        );

        init("Nonexistent");
        assert_eq!(t!["gui", "status", "idle"], "Idle");
    }

    #[test]
    fn every_bundled_file_parses() {
        for (name, json) in LANGUAGES {
            let v: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(v.get("gui").is_some(), "{name} lacks gui keys");
        }
    }
}
