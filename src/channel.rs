//! Games, streams, channels and the watch requests.

use crate::constants::{SPECIAL_GAME_IDS, gql};
use crate::http::Http;
use crate::util::{json_str, json_u64, slug};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::hash::{Hash, Hasher};
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
}

/// Playback access token -> usher master playlist -> URL of its last (lowest quality) variant.
pub async fn get_stream_url(http: &Http, login: &str) -> Result<String> {
    let v = http.gql(gql::playback_access_token(login)).await?;
    let token = &v["data"]["streamPlaybackAccessToken"];
    let (Some(value), Some(sig)) = (token["value"].as_str(), token["signature"].as_str()) else {
        bail!("no playback access token: {v}");
    };
    let url = reqwest::Url::parse_with_params(
        &format!("https://usher.ttvnw.net/api/channel/hls/{login}.m3u8"),
        &[("sig", sig), ("token", value)],
    )?;
    // errors (e.g. offline) come back as JSON instead of a playlist
    let text = http.text(url.as_str()).await?;
    text.lines()
        .rev()
        .find(|l| l.starts_with("http"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no stream playlist: {}", text.chars().take(300).collect::<String>()))
}

/// "Watch" like a player: fetch the live chunk list and HEAD every chunk (no video is
/// downloaded). Twitch dropped spade "minute-watched" credit in Oct 2026; upstream TDM
/// switched the same way. True if every request succeeded.
pub async fn send_watch(http: &Http, stream_url: &str) -> bool {
    let list = match http.send(http.get(stream_url).header("Connection", "close")).await {
        // 4xx: the stream went offline or the playlist URL expired
        Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
        Ok(resp) => {
            tracing::debug!("chunk list: {}", resp.status());
            return false;
        }
        Err(e) => {
            tracing::debug!("chunk list request failed: {e}");
            return false;
        }
    };
    let chunks: Vec<&str> = list.lines().filter(|l| l.starts_with("http")).collect();
    if chunks.is_empty() {
        tracing::debug!("chunk list has no chunks: {}", list.chars().take(300).collect::<String>());
        return false;
    }
    for chunk in chunks {
        match http.send(http.head(chunk)).await {
            Ok(resp) if resp.status().as_u16() == 200 => {}
            Ok(resp) => {
                tracing::debug!("chunk HEAD: {}", resp.status());
                return false;
            }
            Err(e) => {
                tracing::debug!("chunk HEAD failed: {e}");
                return false;
            }
        }
    }
    true
}
