//! Webhook notifications: one JSON POST per interesting event to `Settings::notify_url`.
//!
//! The body carries the text under `content` (Discord), `text` (Slack) and `message`
//! (Gotify) plus the raw event, so one generic URL covers most services.

use crate::events::Event;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// The notification text for an event, or None if it isn't worth a notification.
fn text(ev: &Event) -> Option<String> {
    match ev {
        Event::DropClaimed(c) => Some(format!("Claimed drop: {} {} ({})", c.game, c.rewards, c.campaign)),
        Event::LoginCode { url, code } => Some(format!("Twitch login needed: open {url} and enter the code {code}")),
        Event::UnlinkedCampaign { game, campaign, link_url } => {
            Some(format!("{campaign} ({game}) needs a linked account before it can earn: {link_url}"))
        }
        Event::AllDone => Some("All campaigns are done, nothing left to mine.".to_owned()),
        _ => None,
    }
}

fn body(ev: &Event, text: &str) -> Value {
    json!({ "title": "Rustpick", "content": text, "text": text, "message": text, "event": ev })
}

/// Forward events to the URL in `url` (re-read per event, so settings saves apply). Runs in
/// its own task so a slow webhook never holds up the miner; ends after `Event::Exited`.
pub fn spawn(mut events: broadcast::Receiver<Event>, url: watch::Receiver<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
        loop {
            let ev = match events.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if matches!(ev, Event::Exited) {
                return;
            }
            let url = url.borrow().clone();
            let Some(text) = text(&ev).filter(|_| !url.is_empty()) else { continue };
            let body = body(&ev, &text);
            // one retry, then give up
            for attempt in 0..2 {
                match client.post(&url).json(&body).send().await.and_then(|r| r.error_for_status()) {
                    Ok(_) => break,
                    Err(e) if attempt == 0 => {
                        tracing::debug!("Notification failed, retrying: {e}");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => tracing::warn!("Notification failed: {e}"),
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_interesting_events_notify() {
        assert!(text(&Event::Status("x".into())).is_none());
        let ev = Event::LoginCode { url: "https://www.twitch.tv/activate".into(), code: "ABCD".into() };
        let t = text(&ev).unwrap();
        assert!(t.contains("ABCD"));
        let b = body(&ev, &t);
        assert_eq!(b["content"], t);
        assert_eq!(b["event"]["kind"], "LoginCode");
    }
}
