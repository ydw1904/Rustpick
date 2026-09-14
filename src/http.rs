//! HTTP client, device-code login and GQL requests with upstream's retry rules.

use crate::constants::{ANDROID_APP, ClientInfo, GQL_URL};
use crate::events::Event;
use crate::settings::{Settings, data_dir};
use crate::t;
use crate::util::{Backoff, CHARS_HEX_LOWER, nonce};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{Duration as ChronoDuration, Utc};
use rand::seq::IndexedRandom;
use reqwest::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio::time::sleep;

/// GQL returned an error payload we don't retry.
#[derive(Debug, thiserror::Error)]
#[error("GQL error: {0}")]
pub struct GqlError(pub String);

#[derive(Serialize, Deserialize, Default)]
struct AuthFile {
    device_id: String,
    access_token: Option<String>,
}

/// Persist a login session. Also used by the TwitchDropsMiner cookie import.
pub fn save_auth_file(device_id: String, access_token: Option<String>) -> Result<()> {
    let path = data_dir().join("auth.json");
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(&path, serde_json::to_string_pretty(&AuthFile { device_id, access_token })?)?;
    Ok(())
}

/// Import a login session from a TwitchDropsMiner install's `cookies.jar`
/// (a pickled aiohttp cookie jar). Returns `(device_id, token)` if a token was found.
pub fn import_tdm_cookies(tdm_dir: &Path) -> Option<(String, String)> {
    let data = std::fs::read(tdm_dir.join("cookies.jar")).ok()?;
    let token = crate::pickle::cookie_value(&data, "auth-token")?;
    let device_id = crate::pickle::cookie_value(&data, "unique_id").unwrap_or_default();
    Some((device_id, token))
}

pub struct Http {
    client: Client,
    pub info: &'static ClientInfo,
    pub user_agent: &'static str,
    /// Proxy URL from settings, reused by the websocket pool.
    pub proxy: String,
    device_id: String,
    session_id: String,
    token: Mutex<Option<String>>,
    user_id: OnceLock<u64>,
    // ponytail: fixed 5 req/s window like upstream; do not loosen, GQL rate limits bite hard.
    limiter: Mutex<(Instant, u32)>,
    events: broadcast::Sender<Event>,
}

impl Http {
    pub fn new(settings: &Settings, events: broadcast::Sender<Event>) -> Result<Arc<Self>> {
        let info = &ANDROID_APP;
        let user_agent = *info.user_agents.choose(&mut rand::rng()).unwrap();
        let quality = settings.connection_quality.clamp(1, 6) as u64;
        let mut builder = Client::builder()
            .user_agent(user_agent)
            .gzip(true)
            .connect_timeout(Duration::from_secs(5 * quality))
            .timeout(Duration::from_secs(10 * quality));
        if !settings.proxy.is_empty() {
            builder = builder.proxy(reqwest::Proxy::all(&settings.proxy).context("proxy URL")?);
        }
        let auth_path = data_dir().join("auth.json");
        let mut auth: AuthFile = std::fs::read_to_string(&auth_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if auth.device_id.len() != 32 {
            auth.device_id = nonce(CHARS_HEX_LOWER, 32);
        }
        Ok(Arc::new(Self {
            client: builder.build()?,
            info,
            user_agent,
            proxy: settings.proxy.clone(),
            device_id: auth.device_id,
            session_id: nonce(CHARS_HEX_LOWER, 16),
            token: Mutex::new(auth.access_token),
            user_id: OnceLock::new(),
            limiter: Mutex::new((Instant::now(), 0)),
            events,
        }))
    }

    fn print(&self, msg: impl Into<String>) {
        let _ = self.events.send(Event::Print(msg.into()));
    }

    pub fn user_id(&self) -> u64 {
        *self.user_id.get().expect("user_id read before login")
    }

    pub fn access_token(&self) -> String {
        self.token.lock().unwrap().clone().unwrap_or_default()
    }

    fn save_auth(&self) -> Result<()> {
        save_auth_file(self.device_id.clone(), self.token.lock().unwrap().clone())
    }

    fn with_headers(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("Accept", "*/*")
            .header("Accept-Language", "en-US")
            .header("Pragma", "no-cache")
            .header("Cache-Control", "no-cache")
            .header("Client-Id", self.info.id)
            .header("Client-Session-Id", &self.session_id)
            .header("X-Device-Id", &self.device_id)
    }

    pub fn get(&self, url: &str) -> RequestBuilder {
        self.with_headers(self.client.get(url))
    }

    pub fn post(&self, url: &str) -> RequestBuilder {
        self.with_headers(self.client.post(url))
    }

    /// Send with exponential backoff on connection problems and 5xx responses.
    pub async fn send(&self, rb: RequestBuilder) -> Result<Response> {
        let mut backoff = Backoff::new(180.0);
        loop {
            let delay = backoff.delay();
            let req = rb.try_clone().ok_or_else(|| anyhow!("request body is not clonable"))?;
            match req.send().await {
                Ok(resp) if resp.status().as_u16() < 500 => return Ok(resp),
                Ok(resp) => {
                    tracing::debug!("{} from {}", resp.status(), resp.url());
                    self.print(t!["error", "site_down"; "seconds" => delay.as_secs().to_string()]);
                }
                Err(e) if e.to_string().contains("certificate") => return Err(e.into()),
                Err(e) if e.is_connect() || e.is_timeout() || e.is_request() || e.is_body() => {
                    tracing::debug!("request error: {e}");
                    if backoff.steps() > 1 {
                        self.print(t!["error", "no_connection";
                            "seconds" => delay.as_secs().to_string(),
                            "url" => e.url().map(|u| u.as_str()).unwrap_or("?").to_owned(),
                        ]);
                    }
                }
                Err(e) => return Err(e.into()),
            }
            sleep(delay).await;
        }
    }

    pub async fn text(&self, url: &str) -> Result<String> {
        Ok(self.send(self.get(url)).await?.text().await?)
    }

    async fn acquire(&self) {
        loop {
            let wait = {
                let mut g = self.limiter.lock().unwrap();
                if g.0.elapsed() >= Duration::from_secs(1) {
                    *g = (Instant::now(), 0);
                }
                if g.1 < 5 {
                    g.1 += 1;
                    return;
                }
                Duration::from_secs(1).saturating_sub(g.0.elapsed())
            };
            sleep(wait).await;
        }
    }

    /// One GQL operation (object) or a batch (array); the response has the same shape.
    pub async fn gql(&self, ops: Value) -> Result<Value> {
        tracing::trace!("GQL request: {ops}");
        let mut backoff = Backoff::new(60.0);
        let mut single_retry = true;
        loop {
            let mut delay = backoff.delay();
            self.acquire().await;
            let rb = self
                .post(GQL_URL)
                .header("Origin", self.info.url)
                .header("Referer", self.info.url)
                .header("Authorization", format!("OAuth {}", self.access_token()))
                .json(&ops);
            let body: Value = self.send(rb).await?.json().await.context("GQL response body")?;
            tracing::trace!("GQL response: {body}");
            let is_list = body.is_array();
            let items = match body {
                Value::Array(a) => a,
                other => vec![other],
            };
            let mut out = Vec::with_capacity(items.len());
            let mut force_retry = false;
            for mut item in items {
                if let Some(errors) = item.get("errors").and_then(Value::as_array).cloned() {
                    let mut handled = false;
                    for err in &errors {
                        let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
                        if single_retry && matches!(msg, "service error" | "PersistedQueryNotFound") {
                            let op = item.pointer("/extensions/operationName").cloned().unwrap_or_default();
                            tracing::error!("Retrying a '{msg}' for {op}");
                            single_retry = false;
                            delay = delay.max(Duration::from_secs(5));
                            force_retry = true;
                            handled = true;
                            break;
                        } else if msg == "server error" {
                            // nullify whatever the error path points at
                            let path: Vec<String> = err
                                .get("path")
                                .and_then(Value::as_array)
                                .map(|p| p.iter().map(|x| x.to_string().trim_matches('"').to_owned()).collect())
                                .unwrap_or_default();
                            if !path.is_empty() {
                                let ptr = format!("/data/{}", path.join("/"));
                                if let Some(slot) = item.pointer_mut(&ptr) {
                                    *slot = Value::Null;
                                }
                            }
                            handled = true;
                            break;
                        } else if matches!(
                            msg,
                            "service timeout" | "request cancelled" | "service unavailable" | "context deadline exceeded"
                        ) {
                            force_retry = true;
                            handled = true;
                            break;
                        }
                    }
                    if !handled {
                        bail!(GqlError(Value::Array(errors).to_string()));
                    }
                } else if let Some(e) = item.get("error") {
                    bail!(GqlError(format!("{e}: {}", item.get("message").unwrap_or(&Value::Null))));
                }
                if force_retry {
                    break;
                }
                out.push(item);
            }
            if !force_retry {
                return Ok(if is_list { Value::Array(out) } else { out.pop().unwrap_or(Value::Null) });
            }
            sleep(delay).await;
        }
    }

    /// Restore the saved token or run the device-code flow, then validate it.
    pub async fn login(&self) -> Result<()> {
        let _ = self.events.send(Event::Status(t!["gui", "login", "logging_in"].into()));
        for _ in 0..3 {
            let saved = self.token.lock().unwrap().clone();
            let token = match saved {
                Some(t) => t,
                None => {
                    let t = self.device_login().await?;
                    *self.token.lock().unwrap() = Some(t.clone());
                    t
                }
            };
            let resp = self
                .send(
                    self.client
                        .get("https://id.twitch.tv/oauth2/validate")
                        .header("Authorization", format!("OAuth {token}")),
                )
                .await?;
            match resp.status().as_u16() {
                401 => {
                    tracing::info!("Stored token is invalid");
                    *self.token.lock().unwrap() = None;
                    let _ = self.events.send(Event::LoginRequired);
                }
                200 => {
                    let v: Value = resp.json().await?;
                    if v.get("client_id").and_then(Value::as_str) != Some(self.info.id) {
                        tracing::info!("Token client ID mismatch, logging in again");
                        *self.token.lock().unwrap() = None;
                        let _ = self.events.send(Event::LoginRequired);
                        continue;
                    }
                    let uid: u64 = v
                        .get("user_id")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| anyhow!("validate response lacks user_id"))?;
                    let _ = self.user_id.set(uid);
                    self.save_auth()?;
                    tracing::info!("Login successful, user ID: {uid}");
                    let _ = self.events.send(Event::LoggedIn { user_id: uid });
                    return Ok(());
                }
                s => bail!("Unexpected status {s} while validating the token"),
            }
        }
        bail!("Login verification failure")
    }

    async fn device_login(&self) -> Result<String> {
        let auth_headers = |rb: RequestBuilder| {
            rb.header("Accept", "application/json")
                .header("Accept-Language", "en-US")
                .header("Cache-Control", "no-cache")
                .header("Pragma", "no-cache")
                .header("Client-Id", self.info.id)
                .header("Origin", self.info.url)
                .header("Referer", self.info.url)
                .header("X-Device-Id", &self.device_id)
        };
        loop {
            let started = Utc::now();
            let resp = self
                .send(
                    auth_headers(self.client.post("https://id.twitch.tv/oauth2/device"))
                        .form(&[("client_id", self.info.id), ("scopes", "")]),
                )
                .await?;
            let v: Value = resp.json().await.context("device code response")?;
            let device_code = v["device_code"].as_str().ok_or_else(|| anyhow!("no device_code: {v}"))?.to_owned();
            let user_code = v["user_code"].as_str().unwrap_or("").to_owned();
            let url = v["verification_uri"].as_str().unwrap_or("https://www.twitch.tv/activate").to_owned();
            let interval = v["interval"].as_u64().unwrap_or(5);
            let expires_at = started + ChronoDuration::seconds(v["expires_in"].as_i64().unwrap_or(1800));
            self.print(t!["login", "device_code"; "url" => url.clone(), "code" => user_code.clone()]);
            let _ = self.events.send(Event::LoginCode { url, code: user_code });
            loop {
                sleep(Duration::from_secs(interval)).await;
                if Utc::now() >= expires_at - ChronoDuration::seconds(15) {
                    tracing::info!("Device code expired, requesting a new one");
                    break;
                }
                let resp = self
                    .send(auth_headers(self.client.post("https://id.twitch.tv/oauth2/token")).form(&[
                        ("client_id", self.info.id),
                        ("device_code", device_code.as_str()),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ]))
                    .await?;
                if resp.status().as_u16() != 200 {
                    continue; // 400: the user hasn't entered the code yet
                }
                let v: Value = resp.json().await?;
                if let Some(t) = v["access_token"].as_str() {
                    return Ok(t.to_owned());
                }
            }
        }
    }
}
