//! Notify-only update check against the latest GitHub release. Never downloads anything.

use crate::events::Event;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::broadcast;

const LATEST: &str = "https://api.github.com/repos/ydw1904/Rustpick/releases/latest";

/// `v1.2.3` / `1.2` into comparable numbers; anything unparsable is None.
fn version(tag: &str) -> Option<Vec<u64>> {
    let core = tag.trim().trim_start_matches('v').split(['-', '+']).next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(tag: &str, current: &str) -> bool {
    matches!((version(tag), version(current)), (Some(t), Some(c)) if t > c)
}

async fn latest() -> Result<(String, String)> {
    let v: serde_json::Value = reqwest::Client::builder()
        .user_agent(concat!("rustpick/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?
        .get(LATEST)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tag = v["tag_name"].as_str().context("release without tag_name")?.to_owned();
    let url = v["html_url"].as_str().unwrap_or("https://github.com/ydw1904/Rustpick/releases").to_owned();
    Ok((tag, url))
}

/// Check now and then once a day, emitting `Event::UpdateAvailable` whenever a newer release exists.
pub async fn run(events: broadcast::Sender<Event>) {
    let mut every = tokio::time::interval(Duration::from_secs(24 * 3600));
    loop {
        every.tick().await;
        match latest().await {
            Ok((tag, url)) if is_newer(&tag, env!("CARGO_PKG_VERSION")) => {
                tracing::info!("Rustpick {tag} is available (running {}): {url}", env!("CARGO_PKG_VERSION"));
                let _ = events.send(Event::UpdateAvailable { version: tag, url });
            }
            Ok(_) => tracing::debug!("Rustpick is up to date"),
            Err(e) => tracing::debug!("Update check failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0-rc1", "0.9.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }
}
