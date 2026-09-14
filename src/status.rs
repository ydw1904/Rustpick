//! `status.json` in the data dir: a snapshot of what the running miner is doing, for
//! `rustpick status`, scripts and container health checks. `updated_at` is refreshed at
//! least every minute while the miner runs, so a stale file means a dead or stuck miner.

use crate::events::{CampaignStatus, Event, ProgressView};
use crate::settings::data_dir;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
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
    progress: Option<ProgressView>,
    unlinked: Vec<Unlinked>,
    exited: bool,
}

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
        let mut s = Status { pid: std::process::id(), profile: crate::settings::profile(), ..Default::default() };
        let mut channels: HashMap<u64, String> = HashMap::new();
        let mut watching: Option<u64> = None;
        let mut dirty = true;
        let mut last_write = tokio::time::Instant::now() - Duration::from_secs(60);
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if dirty || last_write.elapsed() >= Duration::from_secs(60) {
                        s.channel = watching.and_then(|id| channels.get(&id).cloned());
                        write(&mut s);
                        (dirty, last_write) = (false, tokio::time::Instant::now());
                    }
                }
                ev = events.recv() => {
                    let ev = match ev {
                        Ok(ev) => ev,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    };
                    dirty |= match ev {
                        Event::Status(t) => { s.status = t; true }
                        Event::LoginRequired | Event::LoginCode { .. } => { s.login_required = true; true }
                        Event::LoggedIn { user_id } => { (s.user_id, s.login_required) = (Some(user_id), false); true }
                        Event::Paused(p) => { s.paused = p; true }
                        Event::Watching(id) => { watching = id; true }
                        Event::Channels(list) => { channels = list.into_iter().map(|c| (c.id, c.name)).collect(); true }
                        Event::Progress(p) => { s.progress = p; true }
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
                }
            }
        }
    })
}
