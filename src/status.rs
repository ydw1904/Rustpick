//! `status.json` in the data dir: a snapshot of what the running miner is doing, for
//! `rustpick status`, scripts and container health checks. `updated_at` is refreshed at
//! least every minute while the miner runs, so a stale file means a dead or stuck miner.
//! The same snapshot backs the optional Prometheus endpoint (`serve_metrics`).

use crate::events::{CampaignStatus, Event, ProgressView};
use crate::settings::data_dir;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

pub const FILE: &str = "status.json";

#[derive(Default, Serialize)]
struct Status {
    updated_at: DateTime<Utc>,
    pid: u32,
    profile: Option<&'static str>,
    user_id: Option<u64>,
    status: String,
    paused: bool,
    login_required: bool,
    channel: Option<String>,
    /// The most recently watched channel, kept after exit so a restart can resume it.
    last_channel: Option<LastChannel>,
    progress: Option<ProgressView>,
    unlinked: Vec<Unlinked>,
    exited: bool,
    minutes_watched: u64,
    drops_claimed: u64,
    websockets_connected: usize,
    update_available: Option<String>,
}

#[derive(Serialize, Clone)]
struct LastChannel {
    id: u64,
    login: String,
    name: String,
    acl_based: bool,
    game: Option<String>,
}

/// The live snapshot, shared by the file writer and the metrics endpoint.
static CURRENT: LazyLock<Mutex<Status>> = LazyLock::new(Default::default);

#[derive(Serialize)]
struct Unlinked {
    game: String,
    campaign: String,
    link_url: String,
}

fn write(status: &mut Status) {
    status.updated_at = Utc::now();
    let path = data_dir().join(FILE);
    let tmp = path.with_extension("json.new");
    let result = serde_json::to_vec_pretty(status)
        .map_err(std::io::Error::other)
        .and_then(|json| std::fs::write(&tmp, json))
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = result {
        tracing::warn!("Writing {} failed: {e}", path.display());
    }
}

/// Track events and rewrite the file when something changed (at most every 5s) or a minute
/// has passed. Ends after `Event::Exited`, writing a final snapshot.
pub fn spawn(mut events: broadcast::Receiver<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        {
            let mut s = CURRENT.lock().unwrap();
            (s.pid, s.profile) = (std::process::id(), crate::settings::profile());
        }
        let mut channels: HashMap<u64, crate::events::ChannelView> = HashMap::new();
        let mut watching: Option<u64> = None;
        let mut websockets: HashMap<usize, bool> = HashMap::new();
        let mut dirty = true;
        let mut last_write = tokio::time::Instant::now() - Duration::from_secs(60);
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if dirty || last_write.elapsed() >= Duration::from_secs(60) {
                        write(&mut CURRENT.lock().unwrap());
                        (dirty, last_write) = (false, tokio::time::Instant::now());
                    }
                }
                ev = events.recv() => {
                    let ev = match ev {
                        Ok(ev) => ev,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    };
                    let mut s = CURRENT.lock().unwrap();
                    let changed = match ev {
                        Event::Status(t) => { s.status = t; true }
                        Event::LoginRequired | Event::LoginCode { .. } => { s.login_required = true; true }
                        Event::LoggedIn { user_id } => { (s.user_id, s.login_required) = (Some(user_id), false); true }
                        Event::Paused(p) => { s.paused = p; true }
                        Event::Watching(id) => { watching = id; true }
                        Event::Channels(list) => { channels = list.into_iter().map(|c| (c.id, c)).collect(); true }
                        Event::Progress(p) => {
                            s.minutes_watched += p.as_ref().is_some_and(|p| p.countdown) as u64;
                            s.progress = p;
                            true
                        }
                        Event::DropClaimed(_) => { s.drops_claimed += 1; true }
                        Event::Websocket { idx, status, .. } => {
                            websockets.insert(idx, status == "Connected");
                            s.websockets_connected = websockets.values().filter(|c| **c).count();
                            true
                        }
                        Event::UpdateAvailable { version, .. } => { s.update_available = Some(version); true }
                        Event::Inventory(campaigns) => {
                            s.unlinked = campaigns
                                .into_iter()
                                .filter(|c| c.status == CampaignStatus::Active && c.needs_link)
                                .map(|c| Unlinked { game: c.game, campaign: c.name, link_url: c.link_url })
                                .collect();
                            true
                        }
                        Event::Exited => {
                            s.exited = true;
                            s.channel = None;
                            write(&mut s);
                            return;
                        }
                        _ => false,
                    };
                    if changed {
                        // Watching can arrive before the channel list that names it; resolve on every change
                        let current = watching.and_then(|id| channels.get(&id));
                        s.channel = current.map(|c| c.name.clone());
                        if let Some(c) = current {
                            s.last_channel = Some(LastChannel {
                                id: c.id,
                                login: c.login.clone(),
                                name: c.name.clone(),
                                acl_based: c.acl_based,
                                game: c.game.clone(),
                            });
                        }
                    }
                    dirty |= changed;
                }
            }
        }
    })
}

/// Prometheus label value escaping.
fn label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// The snapshot in Prometheus text exposition format.
pub fn metrics_text() -> String {
    let s = CURRENT.lock().unwrap();
    let mut out = String::new();
    let mut metric = |name: &str, kind: &str, help: &str, labels: String, value: f64| {
        let _ = writeln!(out, "# HELP rustpick_{name} {help}\n# TYPE rustpick_{name} {kind}\nrustpick_{name}{labels} {value}");
    };
    metric("up", "gauge", "1 while the miner runs", String::new(), (!s.exited) as u8 as f64);
    metric("paused", "gauge", "1 while paused", String::new(), s.paused as u8 as f64);
    metric("login_required", "gauge", "1 while waiting for a Twitch login", String::new(), s.login_required as u8 as f64);
    metric("minutes_watched_total", "counter", "Minutes of drop progress recorded", String::new(), s.minutes_watched as f64);
    metric("drops_claimed_total", "counter", "Drops claimed since start", String::new(), s.drops_claimed as f64);
    metric("websockets_connected", "gauge", "Connected PubSub websockets", String::new(), s.websockets_connected as f64);
    metric("unlinked_campaigns", "gauge", "Active campaigns that need a linked account", String::new(), s.unlinked.len() as f64);
    if let Some(p) = &s.progress {
        let labels = format!("{{game=\"{}\",campaign=\"{}\"}}", label(&p.game), label(&p.campaign_name));
        let left = p.drop_required.saturating_sub(p.drop_current) as f64;
        metric("drop_minutes_remaining", "gauge", "Minutes left on the current drop", labels, left);
    }
    if let Some(c) = &s.channel {
        metric("watching", "gauge", "The channel being watched", format!("{{channel=\"{}\"}}", label(c)), 1.0);
    }
    out
}

/// Answer every HTTP request on `addr` with `metrics_text()`. Runs until the listener fails.
pub async fn serve_metrics(addr: std::net::SocketAddr) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Serving metrics on http://{addr}/metrics");
    loop {
        let (mut conn, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut buf)).await;
            let body = metrics_text();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(head.as_bytes()).await;
            let _ = conn.write_all(body.as_bytes()).await;
        });
    }
}

/// Where the previous run left off: (channel id, login, acl_based), if it exited within `max_age`.
pub fn last_channel(max_age: chrono::Duration) -> Option<(u64, String, bool)> {
    let text = std::fs::read_to_string(data_dir().join(FILE)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let updated: DateTime<Utc> = v["updated_at"].as_str()?.parse().ok()?;
    let c = &v["last_channel"];
    (Utc::now() - updated < max_age).then_some(())?;
    Some((c["id"].as_u64()?, c["login"].as_str()?.to_owned(), c["acl_based"].as_bool().unwrap_or(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_prometheus_text() {
        {
            let mut s = CURRENT.lock().unwrap();
            s.drops_claimed = 3;
            s.channel = Some("a\"b".into());
        }
        let text = metrics_text();
        assert!(text.contains("# TYPE rustpick_drops_claimed_total counter\nrustpick_drops_claimed_total 3\n"));
        assert!(text.contains("rustpick_watching{channel=\"a\\\"b\"} 1\n"));
    }
}
