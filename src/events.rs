//! Messages between the miner core and any frontend (CLI, GUI).

use crate::settings::Settings;
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Clone, Debug)]
pub enum Command {
    /// Refetch the inventory and start over.
    Reload,
    Exit,
    /// Manually pick a channel to watch (None clears the manual selection).
    Select(Option<u64>),
    /// Replace and persist settings. Takes effect on the next reload.
    Settings(Settings),
}

/// Serialized to the webview as `{ "kind": <variant>, "data": <payload> }`.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "data")]
pub enum Event {
    /// A line for the console/output window.
    Print(String),
    /// Short status bar text.
    Status(String),
    /// Device-code login: the user must open `url` and enter `code`.
    LoginCode { url: String, code: String },
    LoggedIn { user_id: u64 },
    /// A drop was claimed; the payload is the translated "Claimed drop: ..." line.
    DropClaimed(String),
    Websocket { idx: usize, status: String, topics: usize },
    Channels(Vec<ChannelView>),
    Watching(Option<u64>),
    Inventory(Vec<CampaignView>),
    Progress(Option<ProgressView>),
    /// The core loop has ended; nothing else will be sent.
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ChannelStatus {
    Online,
    Pending,
    Offline,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelView {
    pub id: u64,
    pub name: String,
    pub url: String,
    pub acl_based: bool,
    pub status: ChannelStatus,
    pub game: Option<String>,
    pub drops_enabled: bool,
    pub viewers: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum CampaignStatus {
    Active,
    Upcoming,
    Expired,
}

#[derive(Clone, Debug, Serialize)]
pub struct DropView {
    pub id: String,
    pub name: String,
    pub rewards: String,
    pub current_minutes: u32,
    pub required_minutes: u32,
    pub is_claimed: bool,
    pub progress: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CampaignView {
    pub id: String,
    pub name: String,
    pub game: String,
    pub image_url: String,
    pub link_url: String,
    pub linked: bool,
    pub eligible: bool,
    pub status: CampaignStatus,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub progress: f32,
    pub claimed: usize,
    pub total: usize,
    pub drops: Vec<DropView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgressView {
    pub drop_rewards: String,
    pub drop_progress: f32,
    pub drop_current: u32,
    pub drop_required: u32,
    pub campaign_name: String,
    pub game: String,
    pub campaign_progress: f32,
    pub campaign_claimed: usize,
    pub campaign_total: usize,
    pub campaign_remaining_minutes: u32,
    /// True when a fresh minute of progress was just recorded, so a 60s countdown makes sense.
    pub countdown: bool,
}
