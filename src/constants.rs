//! Twitch client identity, GQL persisted operations and websocket topics.

use serde_json::{Value, json};
use std::time::Duration;

pub const MAX_EXTRA_MINUTES: u32 = 15;
pub const MAX_WEBSOCKETS: usize = 8;
pub const WS_TOPICS_LIMIT: usize = 50;
const BASE_TOPICS: usize = 2;
const TOPICS_PER_CHANNEL: usize = 2;
pub const MAX_CHANNELS: usize = (MAX_WEBSOCKETS * WS_TOPICS_LIMIT - BASE_TOPICS) / TOPICS_PER_CHANNEL;
pub const PING_INTERVAL: Duration = Duration::from_secs(3 * 60);
pub const PING_TIMEOUT: Duration = Duration::from_secs(10);
pub const ONLINE_DELAY: Duration = Duration::from_secs(120);
pub const WATCH_INTERVAL: Duration = Duration::from_secs(59);
pub const SPECIAL_GAME_IDS: [u64; 2] = [509663, 509672];
pub const WS_URL: &str = "wss://pubsub-edge.twitch.tv/v1";
pub const GQL_URL: &str = "https://gql.twitch.tv/gql";

pub struct ClientInfo {
    pub url: &'static str,
    pub id: &'static str,
    pub user_agents: &'static [&'static str],
}

/// The Android app client; the only one upstream currently uses.
pub static ANDROID_APP: ClientInfo = ClientInfo {
    url: "https://www.twitch.tv",
    id: "kd1unb4b3q4t58fwlpcbzcbnm76a8fp",
    user_agents: &[
        "Dalvik/2.1.0 (Linux; U; Android 16; SM-S911B Build/TP1A.220624.014) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; U; Android 16; SM-S938B Build/BP2A.250605.031) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; Android 16; SM-X716N Build/UP1A.231005.007) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; U; Android 15; SM-G990B Build/AP3A.240905.015.A2) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; U; Android 15; SM-G970F Build/AP3A.241105.008) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; U; Android 15; SM-A566E Build/AP3A.240905.015.A2) tv.twitch.android.app/25.3.0/2503006",
        "Dalvik/2.1.0 (Linux; U; Android 14; SM-X306B Build/UP1A.231005.007) tv.twitch.android.app/25.3.0/2503006",
    ],
};

fn persisted(name: &str, sha256: &str, variables: Value) -> Value {
    json!({
        "operationName": name,
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": sha256}},
        "variables": variables,
    })
}

pub mod gql {
    use super::*;

    /// Stream information for a particular channel.
    pub fn get_stream_info(login: &str) -> Value {
        persisted(
            "VideoPlayerStreamInfoOverlayChannel",
            "198492e0857f6aedead9665c81c5a06d67b25b58034649687124083ff288597d",
            json!({"channel": login}),
        )
    }

    pub fn claim_drop(drop_instance_id: &str) -> Value {
        persisted(
            "DropsPage_ClaimDropRewards",
            "a455deea71bdc9015b78eb49f4acfbce8baa7ccbedd28e549bb025bd0f751930",
            json!({"input": {"dropInstanceID": drop_instance_id}}),
        )
    }

    /// All in-progress campaigns.
    pub fn inventory() -> Value {
        persisted(
            "Inventory",
            "8337eb8541b314040b0edde0c09c5c7a2783ba1960aa9edfbf3bac16d0fec404",
            json!({"fetchRewardCampaigns": false}),
        )
    }

    /// Current drop progress for the watched channel.
    pub fn current_drop(channel_id: u64) -> Value {
        persisted(
            "DropCurrentSessionContext",
            "4d06b702d25d652afb9ef835d2a550031f1cf762b193523a92166f40ea3d142b",
            json!({"channelID": channel_id.to_string(), "channelLogin": ""}),
        )
    }

    /// All available campaigns.
    pub fn campaigns() -> Value {
        persisted(
            "ViewerDropsDashboard",
            "c16bb890cc8ce7647a96ee69cd313d423a378a3dedadf630a1017cde18975feb",
            json!({"fetchRewardCampaigns": false}),
        )
    }

    pub fn campaign_details(user_id: u64, campaign_id: &str) -> Value {
        persisted(
            "DropCampaignDetails",
            "039277bf98f3130929262cc7c6efd9c141ca3749cb6dca442fc8ead9a53f77c1",
            json!({"channelLogin": user_id.to_string(), "dropID": campaign_id}),
        )
    }

    pub fn available_drops(channel_id: u64) -> Value {
        persisted(
            "DropsHighlightService_AvailableDrops",
            "782dad0f032942260171d2d80a654f88bdd0c5a9dddc392e9bc92218a0f42d20",
            json!({"channelID": channel_id.to_string()}),
        )
    }

    pub fn game_directory(slug: &str, limit: u32, drops_enabled: bool) -> Value {
        let filters: Vec<&str> = if drops_enabled { vec!["DROPS_ENABLED"] } else { vec![] };
        persisted(
            "DirectoryPage_Game",
            "86bcceb4e8b1a51256ff8eed8bd8aae4acacf80d737efe904f84f3aeadf8cafd",
            json!({
                "limit": limit,
                "slug": slug,
                "imageWidth": 50,
                "includeCostreaming": false,
                "options": {
                    "broadcasterLanguages": [],
                    "freeformTags": null,
                    "includeRestricted": ["SUB_ONLY_LIVE"],
                    "recommendationsContext": {"platform": "web"},
                    "sort": "RELEVANCE",
                    "systemFilters": filters,
                    "tags": [],
                    "requestID": "JIRA-VXP-2397",
                },
                "sortTypeIsRecency": false,
            }),
        )
    }

    pub fn notifications_delete(id: &str) -> Value {
        persisted(
            "OnsiteNotifications_DeleteNotification",
            "13d463c831f28ffe17dccf55b3148ed8b3edbbd0ebadd56352f1ff0160616816",
            json!({"input": {"id": id}}),
        )
    }
}

pub mod topic {
    pub const USER_DROPS: &str = "user-drop-events";
    pub const USER_NOTIFICATIONS: &str = "onsite-notifications";
    pub const STREAM_STATE: &str = "video-playback-by-id";
    pub const STREAM_UPDATE: &str = "broadcast-settings-update";

    pub fn user_drops(user_id: u64) -> String {
        format!("{USER_DROPS}.{user_id}")
    }
    pub fn user_notifications(user_id: u64) -> String {
        format!("{USER_NOTIFICATIONS}.{user_id}")
    }
    pub fn stream_state(channel_id: u64) -> String {
        format!("{STREAM_STATE}.{channel_id}")
    }
    pub fn stream_update(channel_id: u64) -> String {
        format!("{STREAM_UPDATE}.{channel_id}")
    }

    /// Split "name.12345" into (name, 12345).
    pub fn split(topic: &str) -> Option<(&str, u64)> {
        let (name, id) = topic.rsplit_once('.')?;
        Some((name, id.parse().ok()?))
    }
}
