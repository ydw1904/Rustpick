//! Games, streams, channels and the "minute watched" payload.

use crate::constants::SPECIAL_GAME_IDS;
use crate::http::Http;
use crate::util::{isonow, json_str, json_u64, slug};
use anyhow::{Result, anyhow};
use base64::Engine;
use regex::Regex;
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use tokio::task::AbortHandle;

#[derive(Clone, Debug)]
pub struct Game {
    pub id: u64,
    pub name: String,
    slug: Option<String>,
}

impl PartialEq for Game {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Game {}
impl Hash for Game {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl Game {
    pub fn from_json(v: &Value) -> Option<Game> {
        let id = json_u64(v.get("id")?)?;
        let name = v
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| v.get("name").and_then(Value::as_str))?
            .to_owned();
        Some(Game { id, name, slug: v.get("slug").and_then(json_str) })
    }

    pub fn slug(&self) -> String {
        self.slug.clone().unwrap_or_else(|| slug(&self.name))
    }

    pub fn is_special(&self) -> bool {
        SPECIAL_GAME_IDS.contains(&self.id)
    }
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub broadcast_id: u64,
    pub viewers: u64,
    pub drops_enabled: bool,
    pub game: Option<Game>,
    pub title: String,
}

impl Stream {
    /// From a GetStreamInfo "user" object whose "stream" is non-null.
    pub fn from_get_stream(user: &Value, drops_enabled: bool) -> Option<Stream> {
        let stream = user.get("stream")?;
        let settings = user.get("broadcastSettings").unwrap_or(&Value::Null);
        Some(Stream {
            broadcast_id: json_u64(stream.get("id")?)?,
            viewers: stream.get("viewersCount").and_then(json_u64).unwrap_or(0),
            drops_enabled,
            game: settings.get("game").and_then(Game::from_json),
            title: settings.get("title").and_then(json_str).unwrap_or_default(),
        })
    }

    /// From a GameDirectory stream edge node.
    pub fn from_directory(node: &Value, drops_enabled: bool) -> Option<Stream> {
        Some(Stream {
            broadcast_id: json_u64(node.get("id")?)?,
            viewers: node.get("viewersCount").and_then(json_u64).unwrap_or(0),
            drops_enabled,
            game: node.get("game").and_then(Game::from_json),
            title: node.get("title").and_then(json_str).unwrap_or_default(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: u64,
    pub login: String,
    pub display_name: Option<String>,
    /// From a campaign's allowed-channels list: preferred when switching, never cleaned up
    /// unless streaming an unwanted game.
    pub acl_based: bool,
    pub stream: Option<Stream>,
    /// Delayed online check in flight ("stream-up" arrives before stream info is available).
    pub pending: Option<AbortHandle>,
}

impl Channel {
    pub fn from_acl(v: &Value) -> Option<Channel> {
        Some(Channel {
            id: json_u64(v.get("id")?)?,
            login: json_str(v.get("name")?)?,
            display_name: v.get("displayName").and_then(json_str),
            acl_based: true,
            stream: None,
            pending: None,
        })
    }

    pub fn from_directory(node: &Value, drops_enabled: bool) -> Option<Channel> {
        let b = node.get("broadcaster")?;
        if b.is_null() {
            return None;
        }
        Some(Channel {
            id: json_u64(b.get("id")?)?,
            login: json_str(b.get("login")?)?,
            display_name: b.get("displayName").and_then(json_str),
            acl_based: false,
            stream: Stream::from_directory(node, drops_enabled),
            pending: None,
        })
    }

    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.login)
    }

    pub fn url(&self, base: &str) -> String {
        format!("{base}/{}", self.login)
    }

    pub fn online(&self) -> bool {
        self.stream.is_some()
    }

    pub fn offline(&self) -> bool {
        self.stream.is_none() && self.pending.is_none()
    }

    pub fn pending_online(&self) -> bool {
        self.stream.is_none() && self.pending.is_some()
    }

    pub fn game(&self) -> Option<&Game> {
        self.stream.as_ref().and_then(|s| s.game.as_ref())
    }

    pub fn viewers(&self) -> Option<u64> {
        self.stream.as_ref().map(|s| s.viewers)
    }

    pub fn drops_enabled(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| s.drops_enabled)
    }

    pub fn cancel_pending(&mut self) -> bool {
        match self.pending.take() {
            Some(h) => {
                h.abort();
                true
            }
            None => false,
        }
    }
}

/// What the watch loop needs to keep "watching" a stream without the miner's state.
#[derive(Clone, Debug)]
pub struct WatchTarget {
    pub channel_id: u64,
    pub login: String,
    pub page_url: String,
    pub broadcast_id: u64,
    pub game: Option<Game>,
    pub user_id: u64,
}

impl WatchTarget {
    pub fn new(channel: &Channel, stream: &Stream, base_url: &str, user_id: u64) -> Self {
        Self {
            channel_id: channel.id,
            login: channel.login.clone(),
            page_url: channel.url(base_url),
            broadcast_id: stream.broadcast_id,
            game: stream.game.clone(),
            user_id,
        }
    }

    /// Base64 "minute-watched" event, as posted to the spade endpoint.
    pub fn spade_payload(&self) -> String {
        let payload = json!([{
            "event": "minute-watched",
            "properties": {
                "broadcast_id": self.broadcast_id.to_string(),
                "channel_id": self.channel_id.to_string(),
                "channel": self.login,
                "client_time": isonow(),
                "game": self.game.as_ref().map(|g| g.name.as_str()).unwrap_or(""),
                "game_id": self.game.as_ref().map(|g| g.id.to_string()).unwrap_or_default(),
                "hidden": false,
                "is_live": true,
                "live": true,
                "logged_in": true,
                "minutes_logged": 1,
                "muted": false,
                "user_id": self.user_id,
            }
        }]);
        base64::engine::general_purpose::STANDARD.encode(payload.to_string())
    }
}

/// Streamer page (HTML) -> settings JS -> spade URL. Mobile pages embed it directly.
pub async fn get_spade_url(http: &Http, page_url: &str) -> Result<String> {
    static SETTINGS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)src="(https://[\w.]+/config/settings\.[0-9a-f]{32}\.js)""#).unwrap()
    });
    static SPADE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)"spade_?url": ?"(https://[.\w\-/]+)""#).unwrap());
    let html = http.text(page_url).await?;
    if let Some(m) = SPADE.captures(&html) {
        return Ok(m[1].to_owned());
    }
    let settings_url = SETTINGS
        .captures(&html)
        .map(|m| m[1].to_owned())
        .ok_or_else(|| anyhow!("spade_url extraction failed: step #1"))?;
    let js = http.text(&settings_url).await?;
    SPADE
        .captures(&js)
        .map(|m| m[1].to_owned())
        .ok_or_else(|| anyhow!("spade_url extraction failed: step #2"))
}

/// POST one minute-watched event. True on the expected 204.
pub async fn send_watch(http: &Http, spade_url: &str, target: &WatchTarget) -> bool {
    match http.send(http.post(spade_url).form(&[("data", target.spade_payload())])).await {
        Ok(resp) => resp.status().as_u16() == 204,
        Err(e) => {
            tracing::debug!("watch request failed: {e}");
            false
        }
    }
}
