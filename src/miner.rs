//! The mining state machine. One task owns all state; websocket, timers and frontends talk
//! to it through `Msg`. Ported from upstream `twitch.py`, minus the GUI coupling.

use crate::channel::{Channel, Game, Stream, WatchTarget, get_spade_url, send_watch};
use crate::constants::{MAX_CHANNELS, ONLINE_DELAY, WATCH_INTERVAL, gql, topic};
use crate::events::*;
use crate::http::Http;
use crate::inventory::Campaign;
use crate::settings::{PriorityMode, Settings, data_dir};
use crate::t;
use crate::util::{json_str, json_u64, timestamp};
use crate::websocket::WsPool;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Map, Value};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::sleep;

/// Everything that can wake the miner.
pub enum Msg {
    Cmd(Command),
    Ws { topic: String, message: Value },
    OnlineCheck(u64),
    WatchTick { channel_id: u64, sent_at: Instant },
    MntCleanup,
    MntReload,
    HoursCheck,
}

#[derive(Clone)]
pub struct MinerHandle {
    tx: mpsc::UnboundedSender<Msg>,
}

impl MinerHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(Msg::Cmd(cmd));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    InventoryFetch,
    GamesUpdate,
    ChannelsCleanup,
    ChannelsFetch,
    ChannelSwitch,
    Exit,
}

pub struct Miner {
    settings: Settings,
    http: Arc<Http>,
    events: broadcast::Sender<Event>,
    tx: mpsc::UnboundedSender<Msg>,
    rx: mpsc::UnboundedReceiver<Msg>,
    state: State,
    /// Set whenever the state machine should run another step before waiting for messages.
    dirty: bool,
    /// `--dump` mode: fetch the inventory, write dump.dat, exit. Ported from upstream.
    dump: bool,
    inventory: Vec<Campaign>,
    wanted_games: Vec<Game>,
    channels: Vec<Channel>,
    watching: Option<u64>,
    selected: Option<u64>,
    watch_tx: watch::Sender<Option<WatchTarget>>,
    watch_rx: Option<watch::Receiver<Option<WatchTarget>>>,
    ws: WsPool,
    tasks: Vec<AbortHandle>,
    mnt_task: Option<AbortHandle>,
    last_progress: Instant,
    full_cleanup: bool,
    /// Watch events are held back while paused.
    paused: bool,
    notify_url: watch::Sender<String>,
    /// What the last active-hours check decided, so a manual resume sticks until the next boundary.
    outside_hours: bool,
    /// Campaign IDs already announced via `Event::UnlinkedCampaign`.
    announced_unlinked: HashSet<String>,
}

impl Miner {
    pub fn new(settings: Settings, events: broadcast::Sender<Event>, dump: bool) -> Result<(Miner, MinerHandle)> {
        let http = Http::new(&settings, events.clone())?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (watch_tx, watch_rx) = watch::channel(None);
        let ws = WsPool::new(http.clone(), tx.clone(), events.clone());
        let notify_url = watch::Sender::new(settings.notify_url.clone());
        let miner = Miner {
            settings,
            http,
            events,
            tx: tx.clone(),
            rx,
            state: State::Idle,
            dirty: true,
            dump,
            inventory: Vec::new(),
            wanted_games: Vec::new(),
            channels: Vec::new(),
            watching: None,
            selected: None,
            watch_tx,
            watch_rx: Some(watch_rx),
            ws,
            tasks: Vec::new(),
            mnt_task: None,
            last_progress: Instant::now(),
            full_cleanup: false,
            paused: false,
            notify_url,
            outside_hours: false,
            announced_unlinked: HashSet::new(),
        };
        Ok((miner, MinerHandle { tx }))
    }

    /// Runs until `Command::Exit` or a fatal error. Always emits `Event::Exited` at the end.
    pub async fn run(mut self) -> Result<()> {
        let notifier = crate::notify::spawn(self.events.subscribe(), self.notify_url.subscribe());
        let status = crate::status::spawn(self.events.subscribe());
        let hours_tx = self.tx.clone();
        self.tasks.push(
            tokio::spawn(async move {
                let mut every = tokio::time::interval(Duration::from_secs(60));
                loop {
                    every.tick().await;
                    if hours_tx.send(Msg::HoursCheck).is_err() {
                        break;
                    }
                }
            })
            .abort_handle(),
        );
        let result = self.run_inner().await;
        if let Err(e) = &result {
            self.print(format!("Fatal error encountered: {e:#}"));
        }
        self.shutdown().await;
        let _ = self.events.send(Event::Exited);
        // let queued notifications (e.g. the login code before a headless exit) go out
        let _ = tokio::time::timeout(Duration::from_secs(10), notifier).await;
        let _ = status.await;
        result
    }

    async fn run_inner(&mut self) -> Result<()> {
        if self.dump {
            // replace the existing dump file with an empty one
            std::fs::File::create(data_dir().join("dump.dat")).context("creating dump.dat")?;
        }
        let http = self.http.clone();
        tokio::select! {
            r = http.login() => r?,
            _ = self.wait_exit_during_login() => return Ok(()),
        }
        let uid = self.http.user_id();
        let rx = self.watch_rx.take().expect("run called twice");
        self.tasks.push(tokio::spawn(watch_loop(self.http.clone(), rx, self.tx.clone())).abort_handle());
        self.ws.add_topics([topic::user_drops(uid), topic::user_notifications(uid)])?;
        self.change_state(State::InventoryFetch);
        loop {
            if self.dirty {
                self.dirty = false;
                self.step().await?;
            }
            if self.state == State::Exit {
                return Ok(());
            }
            if !self.dirty {
                match self.rx.recv().await {
                    Some(m) => self.handle(m).await?,
                    None => return Ok(()),
                }
            }
            while let Ok(m) = self.rx.try_recv() {
                self.handle(m).await?;
            }
            if self.state == State::Exit {
                return Ok(());
            }
        }
    }

    /// While logging in, only Exit (and settings saves) are honored; everything else waits.
    async fn wait_exit_during_login(&mut self) {
        loop {
            match self.rx.recv().await {
                Some(Msg::Cmd(Command::Exit)) | None => {
                    self.state = State::Exit;
                    return;
                }
                Some(Msg::Cmd(Command::Settings(s))) => self.set_settings(s),
                Some(Msg::Cmd(Command::Pause(p))) => self.set_paused(p),
                Some(Msg::HoursCheck) => self.check_active_hours(),
                Some(_) => {}
            }
        }
    }

    async fn shutdown(&mut self) {
        self.stop_watching();
        for h in self.tasks.drain(..) {
            h.abort();
        }
        if let Some(h) = self.mnt_task.take() {
            h.abort();
        }
        for c in &mut self.channels {
            c.cancel_pending();
        }
        self.ws.stop_all().await;
        if let Err(e) = self.settings.save() {
            tracing::error!("Saving settings failed: {e}");
        }
        self.status("Exited");
    }

    // ---- small helpers -------------------------------------------------------------------

    fn set_settings(&mut self, s: Settings) {
        self.notify_url.send_replace(s.notify_url.clone());
        self.settings = s;
        if let Err(e) = self.settings.save() {
            self.print(format!("Failed to save settings: {e}"));
        }
    }

    /// Pause/resume at the active-hours boundaries only, so manual toggles in between stick.
    fn check_active_hours(&mut self) {
        use chrono::Timelike;
        let outside = self
            .settings
            .active_hours
            .is_some_and(|w| !crate::settings::in_active_hours(chrono::Local::now().hour() as u8, w));
        if outside != self.outside_hours {
            self.outside_hours = outside;
            self.print(if outside { "Outside active hours, pausing." } else { "Active hours started, resuming." });
            self.set_paused(outside);
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        let _ = self.events.send(Event::Paused(paused));
        if paused {
            // the watch loop idles on None; `watching` is kept so resume picks the same channel
            let _ = self.watch_tx.send(None);
            self.status("Paused");
        } else if let Some(id) = self.watching {
            self.watch(id, true);
        } else if self.state == State::Idle {
            // mid-chain (fetching inventory/channels) the chain ends in a switch anyway
            self.change_state(State::ChannelSwitch);
        }
    }

    fn change_state(&mut self, s: State) {
        if self.state != State::Exit {
            self.state = s;
        }
        self.dirty = true;
    }

    fn print(&self, s: impl Into<String>) {
        let _ = self.events.send(Event::Print(s.into()));
    }

    fn status(&self, s: impl Into<String>) {
        let _ = self.events.send(Event::Status(s.into()));
    }

    fn badges_ok(&self) -> bool {
        self.settings.enable_badges_emotes
    }

    fn channel(&self, id: u64) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    fn channel_mut(&mut self, id: u64) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    fn watching_channel(&self) -> Option<&Channel> {
        self.watching.and_then(|id| self.channel(id))
    }

    fn find_drop(&self, drop_id: &str) -> Option<(usize, usize)> {
        self.inventory
            .iter()
            .enumerate()
            .find_map(|(ci, c)| c.drop_index(drop_id).map(|di| (ci, di)))
    }

    // ---- views ---------------------------------------------------------------------------

    fn channel_view(&self, c: &Channel) -> ChannelView {
        ChannelView {
            id: c.id,
            name: c.name().to_owned(),
            url: c.url(self.http.info.url),
            acl_based: c.acl_based,
            status: if c.online() {
                ChannelStatus::Online
            } else if c.pending_online() {
                ChannelStatus::Pending
            } else {
                ChannelStatus::Offline
            },
            game: c.game().map(|g| g.name.clone()),
            drops_enabled: c.drops_enabled(),
            viewers: c.viewers(),
        }
    }

    fn emit_channels(&self) {
        let views = self.channels.iter().map(|c| self.channel_view(c)).collect();
        let _ = self.events.send(Event::Channels(views));
    }

    fn campaign_view(&self, c: &Campaign) -> CampaignView {
        CampaignView {
            id: c.id.clone(),
            name: c.name.clone(),
            game: c.game.name.clone(),
            image_url: c.image_url.clone(),
            link_url: c.link_url.clone(),
            linked: c.linked,
            needs_link: c.needs_link(),
            eligible: c.eligible(self.badges_ok()),
            status: if c.active() {
                CampaignStatus::Active
            } else if c.upcoming() {
                CampaignStatus::Upcoming
            } else {
                CampaignStatus::Expired
            },
            starts_at: c.starts_at,
            ends_at: c.ends_at,
            progress: c.progress() as f32,
            claimed: c.claimed_drops(),
            total: c.total_drops(),
            drops: c
                .drops
                .iter()
                .map(|d| DropView {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    rewards: d.rewards_text(),
                    current_minutes: d.current_minutes(),
                    required_minutes: d.required_minutes,
                    is_claimed: d.is_claimed,
                    progress: d.progress() as f32,
                })
                .collect(),
        }
    }

    fn emit_inventory(&self) {
        let views = self.inventory.iter().map(|c| self.campaign_view(c)).collect();
        let _ = self.events.send(Event::Inventory(views));
    }

    fn progress_view(&self, channel: Option<&Channel>, countdown: bool) -> Option<ProgressView> {
        let ci = self.get_active_campaign(channel)?;
        let c = &self.inventory[ci];
        let di = c.first_drop(self.badges_ok())?;
        let d = &c.drops[di];
        Some(ProgressView {
            drop_rewards: d.rewards_text(),
            drop_progress: d.progress() as f32,
            drop_current: d.current_minutes(),
            drop_required: d.required_minutes,
            campaign_name: c.name.clone(),
            game: c.game.name.clone(),
            campaign_progress: c.progress() as f32,
            campaign_claimed: c.claimed_drops(),
            campaign_total: c.total_drops(),
            campaign_remaining_minutes: c.remaining_minutes(),
            countdown,
        })
    }

    fn emit_progress(&self, countdown: bool) {
        let _ = self.events.send(Event::Progress(self.progress_view(None, countdown)));
    }

    // ---- selection rules -----------------------------------------------------------------

    /// 0 is the highest priority; usize::MAX means "not wanted".
    fn get_priority(&self, c: &Channel) -> usize {
        c.game()
            .and_then(|g| self.wanted_games.iter().position(|w| w == g))
            .unwrap_or(usize::MAX)
    }

    fn get_active_campaign(&self, channel: Option<&Channel>) -> Option<usize> {
        if self.wanted_games.is_empty() {
            return None;
        }
        let ch = self.watching_channel().or(channel)?;
        let badges = self.badges_ok();
        (0..self.inventory.len())
            .filter(|&i| self.inventory[i].can_earn(Some(ch), false, badges))
            .min_by_key(|&i| self.inventory[i].remaining_minutes())
    }

    fn can_watch(&self, c: &Channel) -> bool {
        if !c.online() {
            return false;
        }
        let badges = self.badges_ok();
        let wanted_game = c.game().is_some_and(|g| self.wanted_games.contains(g)) && c.drops_enabled();
        self.inventory
            .iter()
            .any(|camp| camp.can_earn(Some(c), false, badges) && (wanted_game || camp.game.is_special()))
    }

    fn should_switch(&self, c: &Channel) -> bool {
        if !self.can_watch(c) {
            return false;
        }
        let Some(w) = self.watching_channel() else { return true };
        if !self.can_watch(w) {
            return true;
        }
        let (co, wo) = (self.get_priority(c), self.get_priority(w));
        co < wo || (co == wo && c.acl_based && !w.acl_based)
    }

    fn check_drops_enabled(&self, ch: &Channel, available: &[Value]) -> bool {
        let badges = self.badges_ok();
        available
            .iter()
            .filter_map(|c| c.get("id").and_then(Value::as_str))
            .any(|cid| {
                self.inventory
                    .iter()
                    .find(|c| c.id == cid)
                    .is_some_and(|c| c.can_earn(Some(ch), true, badges))
            })
    }

    // ---- watching ------------------------------------------------------------------------

    fn watch(&mut self, id: u64, update_status: bool) {
        let Some(ch) = self.channel(id) else { return };
        let Some(stream) = &ch.stream else { return };
        let target = WatchTarget::new(ch, stream, self.http.info.url, self.http.user_id());
        let name = ch.name().to_owned();
        self.watching = Some(id);
        if !self.paused {
            let _ = self.watch_tx.send(Some(target));
        }
        let _ = self.events.send(Event::Watching(Some(id)));
        if update_status {
            let s = t!["status", "watching"; "channel" => name];
            self.print(s.clone());
            if !self.paused {
                self.status(s);
            }
        }
        self.emit_progress(false);
    }

    fn stop_watching(&mut self) {
        self.watching = None;
        let _ = self.watch_tx.send(None);
        let _ = self.events.send(Event::Watching(None));
        let _ = self.events.send(Event::Progress(None));
    }

    /// Re-send the watch payload right away (the watch task wakes on any send).
    fn restart_watching(&mut self) {
        let cur = self.watch_tx.borrow().clone();
        let _ = self.watch_tx.send(cur);
        self.emit_progress(false);
    }

    fn set_drop_minutes(&mut self, ci: usize, di: usize, minutes: u32) {
        self.inventory[ci].set_drop_minutes(di, minutes);
        self.last_progress = Instant::now();
        self.emit_progress(true);
        self.emit_inventory();
    }

    // ---- channel status ------------------------------------------------------------------

    /// "stream-up" arrives before stream info exists: check again after ONLINE_DELAY.
    fn check_online(&mut self, id: u64) {
        let tx = self.tx.clone();
        let Some(ch) = self.channel_mut(id) else { return };
        if ch.pending.is_none() {
            let h = tokio::spawn(async move {
                sleep(ONLINE_DELAY).await;
                let _ = tx.send(Msg::OnlineCheck(id));
            });
            ch.pending = Some(h.abort_handle());
            self.emit_channels();
        }
    }

    fn set_offline(&mut self, id: u64) {
        let Some(ch) = self.channel_mut(id) else { return };
        let needs_display = ch.cancel_pending();
        if ch.online() {
            let old = ch.stream.take();
            self.on_channel_update(id, old.as_ref(), None);
        } else if needs_display {
            self.emit_channels();
        }
    }

    async fn update_stream(&mut self, id: u64) -> Result<()> {
        let new = self.fetch_stream(id).await?;
        let Some(ch) = self.channel_mut(id) else { return Ok(()) };
        let old = std::mem::replace(&mut ch.stream, new.clone());
        self.on_channel_update(id, old.as_ref(), new.as_ref());
        Ok(())
    }

    async fn fetch_stream(&mut self, id: u64) -> Result<Option<Stream>> {
        let Some(ch) = self.channel(id) else { return Ok(None) };
        let login = ch.login.clone();
        let resp = self
            .http
            .gql(gql::get_stream_info(&login))
            .await
            .with_context(|| format!("Channel: {login}"))?;
        let user = &resp["data"]["user"];
        if user.is_null() {
            return Ok(None);
        }
        if let Some(dn) = user.get("displayName").and_then(json_str)
            && let Some(ch) = self.channel_mut(id)
            && ch.display_name.is_none()
        {
            ch.display_name = Some(dn);
        }
        if user["stream"].is_null() {
            return Ok(None);
        }
        let Some(mut stream) = Stream::from_get_stream(user, !self.settings.available_drops_check) else {
            return Ok(None);
        };
        if !stream.drops_enabled {
            match self.http.gql(gql::available_drops(id)).await {
                Ok(v) => {
                    let list = v
                        .pointer("/data/channel/viewerDropCampaigns")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(ch) = self.channel(id) {
                        stream.drops_enabled = self.check_drops_enabled(ch, &list);
                    }
                }
                Err(e) => tracing::info!("AvailableDrops GQL call failed for channel {login}: {e}"),
            }
        }
        Ok(Some(stream))
    }

    fn on_channel_update(&mut self, id: u64, before: Option<&Stream>, after: Option<&Stream>) {
        let Some(ch) = self.channel(id).cloned() else { return };
        let name = ch.name().to_owned();
        let flag = |s: &Stream| if s.drops_enabled { "yes" } else { "no" };
        match (before, after) {
            (None, Some(_)) => {
                if self.should_switch(&ch) {
                    self.print(t!["status", "goes_online"; "channel" => name]);
                    self.watch(id, true);
                } else {
                    tracing::info!("{name} goes ONLINE");
                }
            }
            (None, None) => tracing::debug!("{name} stays OFFLINE"),
            (Some(b), _) => {
                if self.watching == Some(id) {
                    if !self.can_watch(&ch) {
                        match after {
                            None => self.print(t!["status", "goes_offline"; "channel" => name]),
                            Some(a) => tracing::info!(
                                "{name} status has been updated, switching... (drops: {} -> {})",
                                flag(b),
                                flag(a)
                            ),
                        }
                        self.change_state(State::ChannelSwitch);
                    }
                } else if let Some(a) = after {
                    tracing::info!("{name} status has been updated (drops: {} -> {})", flag(b), flag(a));
                    if self.should_switch(&ch) {
                        self.watch(id, true);
                    }
                } else {
                    tracing::info!("{name} goes OFFLINE");
                }
            }
        }
        self.emit_channels();
    }

    // ---- message handling ----------------------------------------------------------------

    async fn handle(&mut self, msg: Msg) -> Result<()> {
        match msg {
            Msg::Cmd(Command::Exit) => {
                self.state = State::Exit;
                self.dirty = true;
            }
            Msg::Cmd(Command::Reload) => self.change_state(State::InventoryFetch),
            Msg::Cmd(Command::Select(id)) => {
                self.selected = id;
                self.change_state(State::ChannelSwitch);
            }
            Msg::Cmd(Command::Settings(s)) => self.set_settings(s),
            Msg::Cmd(Command::Pause(p)) => self.set_paused(p),
            // Event handlers are non-fatal, like upstream's task_wrapper (logged, not raised).
            Msg::Ws { topic, message } => {
                if let Err(e) = self.process_ws(topic, message).await {
                    tracing::error!("Error while handling a websocket message: {e:#}");
                }
            }
            Msg::OnlineCheck(id) => {
                if let Some(ch) = self.channel_mut(id) {
                    ch.pending = None;
                    if let Err(e) = self.update_stream(id).await {
                        tracing::error!("Online check failed: {e:#}");
                    }
                }
            }
            Msg::WatchTick { channel_id, sent_at } => {
                if let Err(e) = self.watch_tick(channel_id, sent_at).await {
                    tracing::error!("Watch tick failed: {e:#}");
                }
            }
            Msg::MntCleanup => {
                tracing::debug!("Maintenance task requests channels cleanup");
                self.change_state(State::ChannelsCleanup);
            }
            Msg::HoursCheck => self.check_active_hours(),
            Msg::MntReload => {
                tracing::debug!("Maintenance task requests a reload");
                self.change_state(State::InventoryFetch);
            }
        }
        Ok(())
    }

    async fn process_ws(&mut self, topic_str: String, message: Value) -> Result<()> {
        let Some((name, id)) = topic::split(&topic_str) else { return Ok(()) };
        match name {
            topic::STREAM_STATE => self.process_stream_state(id, &message),
            topic::STREAM_UPDATE => self.process_stream_update(id, &message),
            topic::USER_DROPS => self.process_drops(&message).await?,
            topic::USER_NOTIFICATIONS => self.process_notifications(&message).await?,
            _ => tracing::warn!("Message for an unknown topic: {topic_str}"),
        }
        Ok(())
    }

    fn process_stream_state(&mut self, id: u64, msg: &Value) {
        let Some(ch) = self.channel(id) else {
            tracing::error!("Stream state change for a non-existing channel: {id}");
            return;
        };
        match msg["type"].as_str().unwrap_or("") {
            "viewcount" => {
                if !ch.online() {
                    self.check_online(id);
                } else {
                    let viewers = json_u64(&msg["viewers"]).unwrap_or(0);
                    if let Some(s) = self.channel_mut(id).and_then(|c| c.stream.as_mut()) {
                        s.viewers = viewers;
                    }
                    self.emit_channels();
                }
            }
            "stream-down" => self.set_offline(id),
            "stream-up" => self.check_online(id),
            "commercial" => {}
            other => tracing::warn!("Unknown stream state: {other}"),
        }
    }

    fn process_stream_update(&mut self, id: u64, msg: &Value) {
        let Some(ch) = self.channel(id) else {
            tracing::error!("Broadcast settings update for a non-existing channel: {id}");
            return;
        };
        let (old_game, game) = (&msg["old_game"], &msg["game"]);
        if old_game != game {
            tracing::debug!("Channel update from websocket: {}, game changed: {old_game} -> {game}", ch.name());
        } else {
            tracing::debug!("Channel update from websocket: {}", ch.name());
        }
        // Tags/title changed; re-check the stream after a delay so several edits coalesce.
        self.check_online(id);
    }

    async fn process_drops(&mut self, msg: &Value) -> Result<()> {
        let msg_type = msg["type"].as_str().unwrap_or("");
        if !matches!(msg_type, "drop-progress" | "drop-claim") {
            return Ok(());
        }
        let data = &msg["data"];
        let drop_id = data["drop_id"].as_str().unwrap_or("").to_owned();
        let found = self.find_drop(&drop_id);
        let badges = self.badges_ok();
        if msg_type == "drop-claim" {
            let instance = data["drop_instance_id"].as_str().unwrap_or("").to_owned();
            let Some((ci, di)) = found else {
                tracing::error!("Received a drop claim ID for a non-existing drop: {drop_id} (claim ID: {instance})");
                return Ok(());
            };
            self.inventory[ci].drops[di].claim_id = Some(instance);
            self.claim_drop(ci, di).await;
            self.emit_progress(false);
            // ponytail: this blocks the actor for 4-20s like upstream's claim task would block
            // the watch loop; move to a spawned task if it ever matters.
            sleep(Duration::from_secs(4)).await;
            if let Some(wid) = self.watching {
                for _ in 0..8 {
                    let ctx = self.http.gql(gql::current_drop(wid)).await?;
                    let current = ctx
                        .pointer("/data/currentUser/dropCurrentSession/dropID")
                        .and_then(Value::as_str);
                    if current != Some(drop_id.as_str()) {
                        break;
                    }
                    sleep(Duration::from_secs(2)).await;
                }
            }
            let can_continue = self.inventory[ci].can_earn(self.watching_channel(), false, badges);
            if can_continue {
                self.restart_watching();
            } else {
                self.change_state(State::InventoryFetch);
            }
            return Ok(());
        }
        let current = json_u64(&data["current_progress_min"]).unwrap_or(0) as u32;
        let required = json_u64(&data["required_progress_min"]).unwrap_or(0);
        match found {
            Some((ci, di)) => {
                let c = &self.inventory[ci];
                tracing::debug!(
                    "Drop update from websocket: {} ({}, {current}/{required})",
                    c.drops[di].name,
                    c.game.name
                );
                if c.drop_can_earn(di, self.watching_channel(), false, badges) {
                    self.set_drop_minutes(ci, di, current);
                }
            }
            None => tracing::debug!("Drop update from websocket: <Unknown> ({current}/{required})"),
        }
        Ok(())
    }

    async fn process_notifications(&mut self, msg: &Value) -> Result<()> {
        if msg["type"] != "create-notification" {
            return Ok(());
        }
        let n = &msg["data"]["notification"];
        if matches!(
            n["type"].as_str(),
            Some("user_drop_reward_reminder_notification" | "quests_viewer_reward_campaign_earned_emote")
        ) {
            self.change_state(State::InventoryFetch);
            if let Some(id) = n["id"].as_str() {
                self.http.gql(gql::notifications_delete(id)).await?;
            }
        }
        Ok(())
    }

    /// 20s after each watch payload: if Twitch didn't report progress, ask GQL, else pretend.
    async fn watch_tick(&mut self, channel_id: u64, sent_at: Instant) -> Result<()> {
        if self.paused || self.watching != Some(channel_id) || self.last_progress > sent_at {
            return Ok(());
        }
        let Some(ch) = self.channel(channel_id).cloned() else { return Ok(()) };
        let badges = self.badges_ok();
        let mut handled = false;
        let session = match self.http.gql(gql::current_drop(channel_id)).await {
            Ok(v) => v.pointer("/data/currentUser/dropCurrentSession").filter(|d| !d.is_null()).cloned(),
            Err(e) => {
                tracing::debug!("CurrentDrop query failed: {e}");
                None
            }
        };
        if let Some(session) = session
            && let Some(cur_id) = session["dropID"].as_str()
            && let Some((ci, di)) = self.find_drop(cur_id)
            && self.inventory[ci].drop_can_earn(di, Some(&ch), false, badges)
        {
            let minutes = json_u64(&session["currentMinutesWatched"]).unwrap_or(0) as u32;
            self.set_drop_minutes(ci, di, minutes);
            let (c, d) = (&self.inventory[ci], &self.inventory[ci].drops[di]);
            tracing::debug!(
                "Drop progress from GQL: {} ({}, {}/{})",
                d.name,
                c.game.name,
                d.current_minutes(),
                d.required_minutes
            );
            handled = true;
        }
        if !handled {
            match self.get_active_campaign(Some(&ch)) {
                Some(ci) => {
                    let hit_max = self.inventory[ci].bump_minutes(&ch, badges);
                    let c = &self.inventory[ci];
                    let label = format!("{} ({})", c.name, c.game.name);
                    if hit_max {
                        tracing::warn!("At least one of the drops in campaign {label} has reached the maximum extra minutes limit!");
                        self.change_state(State::ChannelSwitch);
                    }
                    tracing::debug!("Drop progress from active search: {label}");
                    self.last_progress = Instant::now();
                    self.emit_progress(true);
                    self.emit_inventory();
                }
                None => tracing::debug!("No active drop could be determined"),
            }
        }
        Ok(())
    }

    async fn claim_drop(&mut self, ci: usize, di: usize) -> bool {
        let (claim_id, is_claimed, can_claim, drop_id) = {
            let c = &self.inventory[ci];
            let d = &c.drops[di];
            (d.claim_id.clone(), d.is_claimed, d.can_claim(c.ends_at), d.id.clone())
        };
        if is_claimed {
            return true;
        }
        let Some(claim_id) = claim_id.filter(|_| can_claim) else { return false };
        let ok = match self.http.gql(gql::claim_drop(&claim_id)).await {
            Ok(resp) => {
                let data = &resp["data"];
                let has_errors = data.get("errors").and_then(Value::as_array).is_some_and(|e| !e.is_empty());
                !has_errors
                    && matches!(
                        data.pointer("/claimDropRewards/status").and_then(Value::as_str),
                        Some("ELIGIBLE_FOR_ALL" | "DROP_INSTANCE_ALREADY_CLAIMED")
                    )
            }
            Err(e) => {
                tracing::debug!("Claim request error: {e}");
                false
            }
        };
        if ok {
            self.inventory[ci].mark_claimed(di);
            let c = &self.inventory[ci];
            let drop_text = format!(
                "{} {} ({}/{})",
                c.game.name,
                c.drops[di].rewards_text(),
                c.claimed_drops(),
                c.total_drops()
            );
            self.print(t!["status", "claimed_drop"; "drop" => drop_text]);
            let d = &c.drops[di];
            let claim = Claim {
                ts: Utc::now(),
                game: c.game.name.clone(),
                campaign: c.name.clone(),
                drop: d.name.clone(),
                rewards: d.rewards_text(),
                benefit_ids: d.benefits.iter().map(|b| b.id.clone()).collect(),
            };
            if let Err(e) = append_claim(&claim) {
                tracing::error!("Writing claims.jsonl failed: {e}");
            }
            let _ = self.events.send(Event::DropClaimed(claim));
            self.emit_inventory();
        } else {
            tracing::error!("Drop claim has potentially failed! Drop ID: {drop_id}");
        }
        ok
    }

    // ---- state machine -------------------------------------------------------------------

    async fn step(&mut self) -> Result<()> {
        match self.state {
            State::Idle => {
                if self.dump {
                    self.state = State::Exit; // dump mode: we're done once idle
                    return Ok(());
                }
                self.status(t!["gui", "status", "idle"]);
                self.stop_watching();
            }
            State::InventoryFetch => {
                self.fetch_inventory().await?;
                if let Err(e) = self.settings.save() {
                    tracing::error!("Saving settings failed: {e}");
                }
                self.change_state(State::GamesUpdate);
            }
            State::GamesUpdate => self.games_update().await,
            State::ChannelsCleanup => self.channels_cleanup(),
            State::ChannelsFetch => self.channels_fetch().await?,
            State::ChannelSwitch => {
                if self.dump {
                    self.state = State::Exit; // dump mode: we're done once the channels are in
                    return Ok(());
                }
                self.channel_switch();
            }
            State::Exit => {}
        }
        Ok(())
    }

    async fn games_update(&mut self) {
        // claim whatever is claimable from active and expired campaigns
        let claimable: Vec<(usize, usize)> = self
            .inventory
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.upcoming())
            .flat_map(|(ci, c)| {
                c.drops
                    .iter()
                    .enumerate()
                    .filter(move |(_, d)| d.can_claim(c.ends_at))
                    .map(move |(di, _)| (ci, di))
            })
            .collect();
        for (ci, di) in claimable {
            self.claim_drop(ci, di).await;
        }
        // figure out which games we want, in priority order
        let badges = self.badges_ok();
        let next_hour = Utc::now() + ChronoDuration::hours(1);
        let s = &self.settings;
        let priority_only = s.priority_mode == PriorityMode::PriorityOnly;
        let mut order: Vec<usize> = (0..self.inventory.len()).collect();
        match s.priority_mode {
            PriorityMode::PriorityOnly => {}
            PriorityMode::EndingSoonest => order.sort_by_key(|&i| self.inventory[i].ends_at),
            PriorityMode::LowAvblFirst => {
                order.sort_by(|&a, &b| self.inventory[a].availability().total_cmp(&self.inventory[b].availability()))
            }
        }
        order.sort_by_key(|&i| {
            s.priority
                .iter()
                .position(|p| *p == self.inventory[i].game.name)
                .unwrap_or(usize::MAX)
        });
        let mut wanted: Vec<Game> = Vec::new();
        for i in order {
            let c = &self.inventory[i];
            let g = &c.game;
            if !wanted.contains(g)
                && !s.exclude.contains(&g.name)
                && (!priority_only || s.priority.contains(&g.name))
                && c.can_earn_within(next_hour, badges)
            {
                wanted.push(g.clone());
            }
        }
        tracing::info!("Wanted games: {:?}", wanted.iter().map(|g| g.name.as_str()).collect::<Vec<_>>());
        if wanted.is_empty() && !self.wanted_games.is_empty() {
            let _ = self.events.send(Event::AllDone);
        }
        self.wanted_games = wanted;
        self.full_cleanup = true;
        self.restart_watching();
        self.change_state(State::ChannelsCleanup);
    }

    fn channels_cleanup(&mut self) {
        self.status(t!["gui", "status", "cleanup"]);
        let to_remove: Vec<u64> = if self.wanted_games.is_empty() || self.full_cleanup {
            self.channels.iter().map(|c| c.id).collect()
        } else {
            self.channels
                .iter()
                .filter(|c| {
                    !c.acl_based && (c.offline() || c.game().is_none_or(|g| !self.wanted_games.contains(g)))
                })
                .map(|c| c.id)
                .collect()
        };
        self.full_cleanup = false;
        if !to_remove.is_empty() {
            let topics: Vec<String> = to_remove
                .iter()
                .flat_map(|&id| [topic::stream_state(id), topic::stream_update(id)])
                .collect();
            self.ws.remove_topics(topics);
            for id in &to_remove {
                if let Some(pos) = self.channels.iter().position(|c| c.id == *id) {
                    let mut ch = self.channels.remove(pos);
                    ch.cancel_pending();
                }
            }
            self.emit_channels();
        }
        if self.wanted_games.is_empty() {
            self.print(t!["status", "no_campaign"]);
            self.change_state(State::Idle);
        } else {
            self.change_state(State::ChannelsFetch);
        }
    }

    async fn channels_fetch(&mut self) -> Result<()> {
        self.status(t!["gui", "status", "gathering"]);
        let badges = self.badges_ok();
        let next_hour = Utc::now() + ChronoDuration::hours(1);
        let mut new_channels: Vec<Channel> = std::mem::take(&mut self.channels);
        self.emit_channels();
        // ACL channels from campaigns we can progress, plus games needing a directory search
        let mut no_acl: Vec<Game> = Vec::new();
        let mut acl_channels: Vec<Channel> = Vec::new();
        for c in &self.inventory {
            if !(self.wanted_games.contains(&c.game) && c.can_earn_within(next_hour, badges)) {
                continue;
            }
            if c.allowed_channels.is_empty() {
                if !no_acl.contains(&c.game) {
                    no_acl.push(c.game.clone());
                }
            } else {
                for a in &c.allowed_channels {
                    if !acl_channels.iter().any(|x| x.id == a.id) {
                        acl_channels.push(Channel {
                            id: a.id,
                            login: a.login.clone(),
                            display_name: a.display_name.clone(),
                            acl_based: true,
                            stream: None,
                            pending: None,
                        });
                    }
                }
            }
        }
        acl_channels.retain(|a| !new_channels.iter().any(|c| c.id == a.id));
        self.bulk_check_online(&mut acl_channels).await?;
        new_channels.extend(acl_channels);
        for game in &no_acl {
            for ch in self.get_live_streams(game, true).await? {
                if !new_channels.iter().any(|c| c.id == ch.id) {
                    new_channels.push(ch);
                }
            }
        }
        // viewers desc (online first), ACL first, then by game priority; trim from the end
        new_channels.sort_by_key(|c| Reverse(c.viewers().map(|v| v as i64).unwrap_or(-1)));
        new_channels.sort_by_key(|c| Reverse(c.acl_based));
        new_channels.sort_by_key(|c| self.get_priority(c));
        if new_channels.len() > MAX_CHANNELS {
            let removed = new_channels.split_off(MAX_CHANNELS);
            let topics: Vec<String> = removed
                .iter()
                .flat_map(|c| [topic::stream_state(c.id), topic::stream_update(c.id)])
                .collect();
            self.ws.remove_topics(topics);
            for mut c in removed {
                c.cancel_pending();
            }
        }
        self.channels = new_channels;
        let topics: Vec<String> = self
            .channels
            .iter()
            .flat_map(|c| [topic::stream_state(c.id), topic::stream_update(c.id)])
            .collect();
        self.ws.add_topics(topics)?;
        // relink the watched channel, or drop it if it no longer qualifies
        if let Some(wid) = self.watching {
            if self.channel(wid).is_some_and(|c| self.can_watch(c)) {
                self.watch(wid, false);
            } else {
                self.stop_watching();
            }
        }
        // pre-display the drop we're about to mine
        if let Some(ch) = self.channels.iter().find(|c| self.can_watch(c)) {
            let view = self.progress_view(Some(ch), false);
            let _ = self.events.send(Event::Progress(view));
        }
        self.emit_channels();
        self.change_state(State::ChannelSwitch);
        Ok(())
    }

    fn channel_switch(&mut self) {
        self.status(t!["gui", "status", "switching"]);
        let mut new_watching: Option<u64> = None;
        if let Some(sel) = self.selected.and_then(|id| self.channel(id))
            && self.can_watch(sel)
        {
            new_watching = Some(sel.id);
        }
        if new_watching.is_none() {
            let mut order: Vec<usize> = (0..self.channels.len()).collect();
            order.sort_by_key(|&i| self.get_priority(&self.channels[i]));
            new_watching = order
                .into_iter()
                .map(|i| &self.channels[i])
                .find(|c| self.should_switch(c))
                .map(|c| c.id);
        }
        if let Some(id) = new_watching {
            self.watch(id, true);
        } else if let Some(w) = self.watching_channel()
            && self.can_watch(w)
        {
            self.status(t!["status", "watching"; "channel" => w.name()]);
        } else {
            self.print(t!["status", "no_channel"]);
            self.change_state(State::Idle);
        }
    }

    // ---- fetching ------------------------------------------------------------------------

    async fn fetch_inventory(&mut self) -> Result<()> {
        self.status(t!["gui", "status", "fetching_inventory"]);
        let resp = self.http.gql(gql::inventory()).await?;
        let inv = resp.pointer("/data/currentUser/inventory").cloned().unwrap_or(Value::Null);
        let claimed_benefits: HashMap<String, DateTime<Utc>> = inv
            .get("gameEventDrops")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|b| Some((json_str(b.get("id")?)?, timestamp(b.get("lastAwardedAt")?.as_str()?)?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut inventory_data: Map<String, Value> = inv
            .get("dropCampaignsInProgress")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| Some((json_str(c.get("id")?)?, c)))
            .collect();
        let resp = self.http.gql(gql::campaigns()).await?;
        let available: Vec<(String, Value)> = resp
            .pointer("/data/currentUser/dropCampaigns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| matches!(c["status"].as_str(), Some("ACTIVE" | "UPCOMING")))
            .filter_map(|c| Some((json_str(c.get("id")?)?, c)))
            .collect();
        self.status(t!["gui", "status", "adding_campaigns"; "counter" => available.len().to_string()]);
        let uid = self.http.user_id();
        let mut set = JoinSet::new();
        for chunk in available.chunks(20) {
            let http = self.http.clone();
            let chunk = chunk.to_vec();
            set.spawn(async move { fetch_campaigns(http, uid, chunk).await });
        }
        while let Some(r) = set.join_next().await {
            let data = r.context("campaign fetch task")??;
            inventory_data = merge_maps(inventory_data, data);
        }
        inventory_data.retain(|_, c| !c["game"].is_null());
        if self.dump {
            self.write_dump(&inventory_data, inv.get("gameEventDrops").cloned().unwrap_or(Value::Null))?;
        }
        let badges = self.badges_ok();
        let mut campaigns: Vec<Campaign> = inventory_data
            .values()
            .filter_map(|c| {
                let parsed = Campaign::from_json(c, &claimed_benefits);
                if parsed.is_none() {
                    tracing::warn!("Skipping malformed campaign: {}", c["id"]);
                }
                parsed
            })
            .collect();
        campaigns.sort_by_key(|c| Reverse(c.active()));
        campaigns.sort_by_key(|c| if c.upcoming() { c.starts_at } else { c.ends_at });
        campaigns.sort_by_key(|c| Reverse(c.eligible(badges)));
        // campaign/drop start and end times within the hour trigger a channel cleanup
        let next_hour = Utc::now() + ChronoDuration::hours(1);
        let now = Utc::now();
        let mut triggers: Vec<DateTime<Utc>> = campaigns
            .iter()
            .filter(|c| c.can_earn_within(next_hour, badges))
            .flat_map(Campaign::time_triggers)
            .filter(|t| *t > now)
            .collect();
        triggers.sort();
        triggers.dedup();
        tracing::info!("Inventory: {} campaigns", campaigns.len());
        for c in campaigns.iter().filter(|c| c.active() && c.needs_link()) {
            if self.announced_unlinked.insert(c.id.clone()) {
                self.print(format!("{} ({}) needs a linked account: {}", c.name, c.game.name, c.link_url));
                let _ = self.events.send(Event::UnlinkedCampaign {
                    game: c.game.name.clone(),
                    campaign: c.name.clone(),
                    link_url: c.link_url.clone(),
                });
            }
        }
        self.inventory = campaigns;
        self.emit_inventory();
        if let Some(h) = self.mnt_task.take() {
            h.abort();
        }
        self.mnt_task = Some(tokio::spawn(maintenance(triggers, self.tx.clone())).abort_handle());
        Ok(())
    }

    /// Append the merged campaign data to dump.dat, ACL lists reduced to counts and
    /// drop instance IDs masked. Ported from upstream's dump mode.
    fn write_dump(&self, inventory_data: &Map<String, Value>, game_event_drops: Value) -> Result<()> {
        let mut dump_data = inventory_data.clone();
        for campaign in dump_data.values_mut() {
            let enabled = campaign.pointer("/allow/isEnabled").and_then(Value::as_bool).unwrap_or(true);
            if let Some(channels) = campaign.pointer_mut("/allow/channels") {
                let count = channels.as_array().map(Vec::len).unwrap_or(0);
                if enabled && count > 0 {
                    *channels = Value::String(format!("{count} channels"));
                }
            }
            if let Some(drops) = campaign.get_mut("timeBasedDrops").and_then(Value::as_array_mut) {
                for drop in drops {
                    let has_instance =
                        !drop.pointer("/self/dropInstanceID").unwrap_or(&Value::Null).is_null();
                    if has_instance
                        && let Some(self_obj) = drop.get_mut("self").and_then(Value::as_object_mut)
                    {
                        self_obj.insert("dropInstanceID".to_owned(), "...".into());
                    }
                }
            }
        }
        let text = format!(
            "{}\n\n{}\n",
            serde_json::to_string_pretty(&Value::Object(dump_data))?,
            serde_json::to_string_pretty(&game_event_drops)?
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(data_dir().join("dump.dat"))?
            .write_all(text.as_bytes())?;
        self.print(format!("Dumped inventory data to {}", data_dir().join("dump.dat").display()));
        Ok(())
    }

    async fn get_live_streams(&self, game: &Game, drops_enabled: bool) -> Result<Vec<Channel>> {
        let slug = game.slug();
        let resp = self
            .http
            .gql(gql::game_directory(&slug, 20, drops_enabled))
            .await
            .with_context(|| format!("Game: {slug}"))?;
        Ok(resp
            .pointer("/data/game/streams/edges")
            .and_then(Value::as_array)
            .map(|edges| edges.iter().filter_map(|e| Channel::from_directory(&e["node"], drops_enabled)).collect())
            .unwrap_or_default())
    }

    /// Batched GQL to set ONLINE status (and drops_enabled) on many channels at once.
    async fn bulk_check_online(&self, channels: &mut [Channel]) -> Result<()> {
        if channels.is_empty() {
            return Ok(());
        }
        let mut streams: HashMap<u64, Value> = HashMap::new();
        let mut set = JoinSet::new();
        for chunk in channels.chunks(20) {
            let ops = Value::Array(chunk.iter().map(|c| gql::get_stream_info(&c.login)).collect());
            let http = self.http.clone();
            set.spawn(async move { http.gql(ops).await });
        }
        while let Some(r) = set.join_next().await {
            for item in r.context("stream info task")??.as_array().cloned().unwrap_or_default() {
                let user = &item["data"]["user"];
                if let Some(id) = user.get("id").and_then(json_u64) {
                    streams.insert(id, user.clone());
                }
            }
        }
        let mut available: HashMap<u64, Vec<Value>> = HashMap::new();
        if self.settings.available_drops_check {
            let online: Vec<u64> = streams.iter().filter(|(_, u)| !u["stream"].is_null()).map(|(id, _)| *id).collect();
            let mut set = JoinSet::new();
            for chunk in online.chunks(20) {
                let ops = Value::Array(chunk.iter().map(|id| gql::available_drops(*id)).collect());
                let http = self.http.clone();
                set.spawn(async move { http.gql(ops).await });
            }
            while let Some(r) = set.join_next().await {
                for item in r.context("available drops task")??.as_array().cloned().unwrap_or_default() {
                    let info = &item["data"]["channel"];
                    if let Some(id) = info.get("id").and_then(json_u64) {
                        available.insert(id, info["viewerDropCampaigns"].as_array().cloned().unwrap_or_default());
                    }
                }
            }
        }
        for ch in channels.iter_mut() {
            let Some(user) = streams.get(&ch.id) else { continue };
            if user["stream"].is_null() {
                continue;
            }
            let Some(mut stream) = Stream::from_get_stream(user, !self.settings.available_drops_check) else {
                continue;
            };
            if !stream.drops_enabled {
                stream.drops_enabled =
                    self.check_drops_enabled(ch, available.get(&ch.id).map(Vec::as_slice).unwrap_or(&[]));
            }
            ch.stream = Some(stream);
        }
        Ok(())
    }
}

/// Claim history: one JSON line per claimed drop in `claims.jsonl`.
pub fn append_claim(claim: &Claim) -> Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_string(claim)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(data_dir().join("claims.jsonl"))?
        .write_all(line.as_bytes())?;
    Ok(())
}

async fn fetch_campaigns(http: Arc<Http>, uid: u64, chunk: Vec<(String, Value)>) -> Result<Map<String, Value>> {
    let ops = Value::Array(chunk.iter().map(|(id, _)| gql::campaign_details(uid, id)).collect());
    let list = http.gql(ops).await?;
    let mut fetched = Map::new();
    for item in list.as_array().cloned().unwrap_or_default() {
        if let Some(cd) = item.pointer("/data/user/dropCampaign").filter(|c| !c.is_null())
            && let Some(id) = cd.get("id").and_then(json_str)
        {
            fetched.insert(id, cd.clone());
        }
    }
    Ok(merge_maps(chunk.into_iter().collect(), fetched))
}

/// Deep-merge two campaign JSON objects; primary wins on conflicts, nulls defer.
fn merge_value(primary: Value, secondary: Value) -> Value {
    match (primary, secondary) {
        (Value::Object(p), Value::Object(s)) => Value::Object(merge_maps(p, s)),
        (Value::Null, s) => s,
        (p, _) => p,
    }
}

fn merge_maps(mut primary: Map<String, Value>, secondary: Map<String, Value>) -> Map<String, Value> {
    for (k, sv) in secondary {
        let merged = match primary.remove(&k) {
            Some(pv) => merge_value(pv, sv),
            None => sv,
        };
        primary.insert(k, merged);
    }
    primary
}

/// Sleep to each campaign/drop boundary (cleanup), then request a full reload after an hour.
async fn maintenance(triggers: Vec<DateTime<Utc>>, tx: mpsc::UnboundedSender<Msg>) {
    let next_period = Utc::now() + ChronoDuration::hours(1);
    let mut triggers = triggers.into_iter().peekable();
    loop {
        let now = Utc::now();
        if now >= next_period {
            break;
        }
        let mut next_trigger = next_period;
        if let Some(&t) = triggers.peek()
            && t <= next_period
        {
            next_trigger = t;
            triggers.next();
        }
        let kind = if next_trigger == next_period { "Reload" } else { "Cleanup" };
        tracing::debug!("Maintenance task waiting until {} ({kind})", next_trigger.with_timezone(&chrono::Local).format("%X"));
        if let Ok(d) = (next_trigger - now).to_std() {
            sleep(d).await;
        }
        if Utc::now() >= next_period {
            break;
        }
        if next_trigger != next_period {
            let _ = tx.send(Msg::MntCleanup);
        }
    }
    let _ = tx.send(Msg::MntReload);
}

/// Posts a "minute watched" every WATCH_INTERVAL for whatever the miner says to watch.
async fn watch_loop(http: Arc<Http>, mut rx: watch::Receiver<Option<WatchTarget>>, msgs: mpsc::UnboundedSender<Msg>) {
    let mut spade_cache: HashMap<u64, String> = HashMap::new();
    loop {
        let target = loop {
            if let Some(t) = (*rx.borrow_and_update()).clone() {
                break t;
            }
            if rx.changed().await.is_err() {
                return;
            }
        };
        let spade_url = match spade_cache.get(&target.channel_id) {
            Some(u) => u.clone(),
            None => match get_spade_url(&http, &target.page_url).await {
                Ok(u) => {
                    spade_cache.insert(target.channel_id, u.clone());
                    u
                }
                Err(e) => {
                    tracing::error!("Spade URL lookup failed for {}: {e}", target.login);
                    tokio::select! {
                        _ = sleep(WATCH_INTERVAL) => {}
                        _ = rx.changed() => {}
                    }
                    continue;
                }
            },
        };
        if !send_watch(&http, &spade_url, &target).await {
            tracing::debug!("Watch request failed for channel: {}", target.login);
        }
        let sent_at = Instant::now();
        sleep(Duration::from_secs(20)).await;
        let _ = msgs.send(Msg::WatchTick { channel_id: target.channel_id, sent_at });
        let remaining = WATCH_INTERVAL.saturating_sub(sent_at.elapsed());
        tokio::select! {
            _ = sleep(remaining) => {}
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_prefers_primary_and_fills_gaps() {
        let p = json!({"a": 1, "self": {"x": 1}, "n": null}).as_object().unwrap().clone();
        let s = json!({"a": 2, "self": {"y": 2}, "n": 5, "extra": true}).as_object().unwrap().clone();
        let m = Value::Object(merge_maps(p, s));
        assert_eq!(m, json!({"a": 1, "self": {"x": 1, "y": 2}, "n": 5, "extra": true}));
    }
}
