//! Sharded PubSub connections: up to 8 sockets x 50 topics, each a tokio task.

use crate::constants::{MAX_WEBSOCKETS, PING_INTERVAL, PING_TIMEOUT, WS_TOPICS_LIMIT, WS_URL};
use crate::events::Event;
use crate::http::Http;
use crate::miner::Msg;
use crate::util::{Backoff, CHARS_ASCII, nonce};
use anyhow::{Result, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async_tls, connect_async};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum WsCmd {
    Topics(HashSet<String>),
    Stop,
}

struct Conn {
    topics: HashSet<String>,
    tx: mpsc::UnboundedSender<WsCmd>,
    handle: JoinHandle<()>,
}

pub struct WsPool {
    http: Arc<Http>,
    out: mpsc::UnboundedSender<Msg>,
    events: broadcast::Sender<Event>,
    conns: Vec<Conn>,
}

impl WsPool {
    pub fn new(http: Arc<Http>, out: mpsc::UnboundedSender<Msg>, events: broadcast::Sender<Event>) -> Self {
        Self { http, out, events, conns: Vec::new() }
    }

    fn spawn_conn(&mut self, idx: usize) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_ws(idx, self.http.clone(), rx, self.out.clone(), self.events.clone()));
        self.conns.push(Conn { topics: HashSet::new(), tx, handle });
    }

    pub fn add_topics(&mut self, topics: impl IntoIterator<Item = String>) -> Result<()> {
        let mut set: HashSet<String> = topics.into_iter().collect();
        for c in &self.conns {
            set.retain(|t| !c.topics.contains(t));
        }
        if set.is_empty() {
            return Ok(());
        }
        for idx in 0..MAX_WEBSOCKETS {
            if idx >= self.conns.len() {
                self.spawn_conn(idx);
            }
            let conn = &mut self.conns[idx];
            let mut changed = false;
            while conn.topics.len() < WS_TOPICS_LIMIT {
                let Some(t) = set.iter().next().cloned() else { break };
                set.remove(&t);
                conn.topics.insert(t);
                changed = true;
            }
            if changed {
                let _ = conn.tx.send(WsCmd::Topics(conn.topics.clone()));
            }
            if set.is_empty() {
                return Ok(());
            }
        }
        bail!("Maximum topics limit has been reached")
    }

    pub fn remove_topics(&mut self, topics: impl IntoIterator<Item = String>) {
        let set: HashSet<String> = topics.into_iter().collect();
        if set.is_empty() {
            return;
        }
        for conn in &mut self.conns {
            let before = conn.topics.len();
            conn.topics.retain(|t| !set.contains(t));
            if conn.topics.len() != before {
                let _ = conn.tx.send(WsCmd::Topics(conn.topics.clone()));
            }
        }
        // Drop trailing sockets we no longer need and re-home their topics.
        let mut recycled = Vec::new();
        while let Some(last) = self.conns.last() {
            let count: usize = self.conns.iter().map(|c| c.topics.len()).sum();
            if count > (self.conns.len() - 1) * WS_TOPICS_LIMIT {
                break;
            }
            let _ = last.tx.send(WsCmd::Stop);
            let conn = self.conns.pop().unwrap();
            recycled.extend(conn.topics);
        }
        if !recycled.is_empty() {
            let _ = self.add_topics(recycled);
        }
    }

    pub async fn stop_all(&mut self) {
        let handles: Vec<JoinHandle<()>> = self
            .conns
            .drain(..)
            .map(|c| {
                let _ = c.tx.send(WsCmd::Stop);
                c.handle
            })
            .collect();
        let _ = timeout(Duration::from_secs(2), futures_util::future::join_all(handles)).await;
    }
}

/// Open the PubSub socket, through the HTTP proxy when one is configured.
async fn connect(proxy: &str) -> Result<Socket> {
    if proxy.is_empty() {
        return Ok(connect_async(WS_URL).await?.0);
    }
    let target = reqwest::Url::parse(WS_URL)?;
    let host = target.host_str().unwrap_or_default();
    let tcp = tunnel(proxy, host, target.port_or_known_default().unwrap_or(443)).await?;
    Ok(client_async_tls(WS_URL, tcp).await?.0)
}

/// HTTP CONNECT to `host:port` via `proxy` (http:// only, like upstream's aiohttp).
async fn tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream> {
    let url = reqwest::Url::parse(proxy)?;
    if url.scheme() != "http" {
        bail!("websocket proxy must be http://, not {}://", url.scheme());
    }
    let mut tcp =
        TcpStream::connect((url.host_str().unwrap_or_default(), url.port_or_known_default().unwrap_or(80))).await?;
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if !url.username().is_empty() {
        let decode = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
        let creds = format!("{}:{}", decode(url.username()), decode(url.password().unwrap_or("")));
        req += &format!("Proxy-Authorization: Basic {}\r\n", base64::engine::general_purpose::STANDARD.encode(creds));
    }
    req += "\r\n";
    tcp.write_all(req.as_bytes()).await?;
    // byte at a time so nothing past the header (the TLS handshake) gets swallowed
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            bail!("proxy CONNECT response too long");
        }
        head.push(tcp.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head);
    if head.split_whitespace().nth(1) != Some("200") {
        bail!("proxy CONNECT refused: {}", head.lines().next().unwrap_or_default());
    }
    Ok(tcp)
}

async fn send_json(ws: &mut Socket, mut msg: Value) -> Result<()> {
    if msg["type"] != "PING" {
        msg["nonce"] = Value::String(nonce(CHARS_ASCII, 30));
    }
    tracing::trace!("ws send: {msg}");
    ws.send(Message::text(msg.to_string())).await?;
    Ok(())
}

async fn maybe_sleep(deadline: Option<Instant>) {
    match deadline {
        Some(t) => sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending().await,
    }
}

async fn run_ws(
    idx: usize,
    http: Arc<Http>,
    mut cmds: mpsc::UnboundedReceiver<WsCmd>,
    out: mpsc::UnboundedSender<Msg>,
    events: broadcast::Sender<Event>,
) {
    let status = |s: &str, n: usize| {
        let _ = events.send(Event::Websocket { idx, status: s.to_owned(), topics: n });
    };
    let mut wanted: HashSet<String> = HashSet::new();
    let mut backoff = Backoff::new(180.0);
    status("Connecting", 0);
    loop {
        let mut ws = match connect(&http.proxy).await {
            Ok(ws) => ws,
            Err(e) => {
                let delay = backoff.delay();
                tracing::info!("Websocket[{idx}] connection problem: {e} (sleep: {}s)", delay.as_secs());
                tokio::select! {
                    _ = sleep(delay) => {}
                    cmd = cmds.recv() => match cmd {
                        Some(WsCmd::Topics(t)) => wanted = t,
                        Some(WsCmd::Stop) | None => { status("Disconnected", 0); return; }
                    }
                }
                continue;
            }
        };
        backoff.reset();
        tracing::info!("Websocket[{idx}] connected.");
        status("Connected", wanted.len());
        let mut submitted: HashSet<String> = HashSet::new();
        let mut next_ping = Instant::now();
        let mut max_pong: Option<Instant> = None;
        let mut stop = false;
        let reason: &str = loop {
            if submitted != wanted {
                let token = http.access_token();
                let removed: Vec<&String> = submitted.difference(&wanted).collect();
                let added: Vec<&String> = wanted.difference(&submitted).collect();
                let mut failed = false;
                for (kind, list) in [("UNLISTEN", &removed), ("LISTEN", &added)] {
                    for chunk in list.chunks(20) {
                        tracing::debug!("Websocket[{idx}] {kind}: {chunk:?}");
                        let msg = json!({"type": kind, "data": {"topics": chunk, "auth_token": token}});
                        if send_json(&mut ws, msg).await.is_err() {
                            failed = true;
                        }
                    }
                }
                if failed {
                    break "send failed";
                }
                submitted = wanted.clone();
                status("Connected", wanted.len());
            }
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(WsCmd::Topics(t)) => wanted = t,
                    Some(WsCmd::Stop) | None => { stop = true; break "stopped"; }
                },
                msg = ws.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        tracing::trace!("Websocket[{idx}] received: {text}");
                        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                        match v["type"].as_str().unwrap_or("") {
                            "MESSAGE" => {
                                let topic = v["data"]["topic"].as_str().unwrap_or("").to_owned();
                                let inner = v["data"]["message"].as_str().unwrap_or("");
                                match serde_json::from_str::<Value>(inner) {
                                    Ok(message) => { let _ = out.send(Msg::Ws { topic, message }); }
                                    Err(e) => tracing::warn!("Websocket[{idx}] bad message payload: {e}"),
                                }
                            }
                            "PONG" => max_pong = None,
                            "RESPONSE" => {
                                if let Some(err) = v["error"].as_str().filter(|e| !e.is_empty()) {
                                    tracing::warn!("Websocket[{idx}] response error: {err}");
                                }
                            }
                            "RECONNECT" => break "server requested reconnect",
                            other => tracing::warn!("Websocket[{idx}] unknown payload type: {other}"),
                        }
                    }
                    Some(Ok(Message::Ping(p))) => { let _ = ws.send(Message::Pong(p)).await; }
                    Some(Ok(Message::Close(_))) | None => break "closed by server",
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { tracing::warn!("Websocket[{idx}] error: {e}"); break "error"; }
                },
                _ = sleep_until(tokio::time::Instant::from_std(next_ping)) => {
                    if send_json(&mut ws, json!({"type": "PING"})).await.is_err() {
                        break "send failed";
                    }
                    next_ping = Instant::now() + PING_INTERVAL;
                    max_pong = Some(Instant::now() + PING_TIMEOUT);
                }
                _ = maybe_sleep(max_pong) => break "no PONG",
            }
        };
        let _ = ws.close(None).await;
        if stop {
            tracing::info!("Websocket[{idx}] stopped.");
            status("Disconnected", 0);
            return;
        }
        tracing::warn!("Websocket[{idx}] reconnecting ({reason})...");
        status("Reconnecting", wanted.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn fake_proxy(reply: &'static str) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 1024];
            let n = sock.read(&mut buf).await.unwrap();
            sock.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (format!("http://us%40er:p%3Ass@{addr}"), task)
    }

    #[tokio::test]
    async fn tunnel_sends_connect_with_decoded_auth() {
        let (proxy, task) = fake_proxy("HTTP/1.1 200 Connection established\r\n\r\n").await;
        tunnel(&proxy, "pubsub-edge.twitch.tv", 443).await.unwrap();
        let req = task.await.unwrap();
        assert!(req.starts_with("CONNECT pubsub-edge.twitch.tv:443 HTTP/1.1\r\n"));
        let auth = base64::engine::general_purpose::STANDARD.encode("us@er:p:ss");
        assert!(req.contains(&format!("Proxy-Authorization: Basic {auth}\r\n")));
    }

    #[tokio::test]
    async fn tunnel_rejects_non_200() {
        let (proxy, _task) = fake_proxy("HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        let err = tunnel(&proxy, "pubsub-edge.twitch.tv", 443).await.unwrap_err();
        assert!(err.to_string().contains("407"));
        assert!(tunnel("socks5://127.0.0.1:1", "x", 443).await.is_err());
    }
}
