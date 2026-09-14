//! Tauri desktop frontend over the same miner core the CLI uses.
//!
//! The webview is a static page (`dist/index.html`); this backend spawns the miner,
//! forwards its [`Event`]s to the page as a `miner-event`, and maps a handful of
//! `#[tauri::command]`s back to [`Command`]s.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use rustpick::events::{ChannelStatus, ChannelView, Event, ProgressView};
use rustpick::{Command, Miner, MinerHandle, Settings};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;

struct AppState {
    handle: MinerHandle,
    settings: Mutex<Settings>,
    /// The core, taken and started once the webview signals it is listening.
    miner: Mutex<Option<Miner>>,
    events: broadcast::Sender<Event>,
    /// The last `LOG_LINES` log-tab entries, so a reloaded page can refill its log.
    log: Mutex<VecDeque<Event>>,
    /// Mirrors the last `Event::Paused`, for the tray toggle.
    paused: AtomicBool,
}

/// Cached tray state; the menu is rebuilt when any of these values changes.
struct TrayState {
    tray: TrayIcon,
    profile: Option<String>,
    status: String,
    paused: bool,
    channels: Vec<ChannelView>,
    watching: Option<u64>,
    pinned: Option<u64>,
    progress: Option<ProgressView>,
    login: Option<(String, String)>,
    unlinked: Vec<(String, String, String)>,
    update: Option<(String, String)>,
    notify: bool,
    autostart: bool,
}

/// A manual channel pick sticks until the miner either confirms it or chooses somebody else.
fn next_pin(pinned: Option<u64>, watching: Option<u64>) -> Option<u64> {
    if pinned.is_some() && pinned != watching {
        None
    } else {
        pinned
    }
}

fn tray_channels(channels: &[ChannelView]) -> Vec<&ChannelView> {
    let mut list: Vec<_> = channels
        .iter()
        .filter(|c| c.status == ChannelStatus::Online && c.drops_enabled)
        .collect();
    list.sort_by_key(|c| std::cmp::Reverse(c.viewers.unwrap_or(0)));
    list.truncate(10);
    list
}

fn minute_parts(minutes: u32) -> (u32, u32, u32) {
    (minutes / 1440, (minutes % 1440) / 60, minutes % 60)
}

fn time_left(minutes: u32) -> String {
    let (days, hours, mins) = minute_parts(minutes);
    if days > 0 {
        rustpick::t!["gui", "tray", "time_days_hours"; "days" => days.to_string(), "hours" => hours.to_string()]
    } else if hours > 0 {
        rustpick::t!["gui", "tray", "time_hours_minutes"; "hours" => hours.to_string(), "minutes" => mins.to_string()]
    } else {
        rustpick::t!["gui", "tray", "time_minutes"; "minutes" => mins.to_string()]
    }
}

fn progress_lines(progress: &ProgressView) -> (String, String) {
    let percent = (progress.drop_progress.clamp(0.0, 1.0) * 100.0).round() as u32;
    let drop = rustpick::t!["gui", "tray", "drop_line";
        "name" => progress.drop_rewards.clone(),
        "current" => progress.drop_current.to_string(),
        "required" => progress.drop_required.to_string(),
        "percent" => percent.to_string(),
    ];
    let campaign = rustpick::t!["gui", "tray", "campaign_line";
        "claimed" => progress.campaign_claimed.to_string(),
        "total" => progress.campaign_total.to_string(),
        "left" => time_left(progress.campaign_remaining_minutes),
    ];
    (drop, campaign)
}

fn tray_status(state: &TrayState, settings: &Settings) -> String {
    if state.paused
        && let Some(until) = rustpick::settings::quiet_hours_end(
            rustpick::settings::local_hour(),
            settings.active_hours,
        )
    {
        rustpick::t!["gui", "tray", "status_quiet_hours"; "until" => format!("{until:02}:00")]
    } else if state.paused {
        rustpick::t!["gui", "tray", "status_paused"]
    } else if state.status.is_empty() {
        rustpick::t!["gui", "tray", "status_starting"]
    } else {
        state.status.clone()
    }
}

fn build_tray_menu(
    app: &AppHandle,
    state: &TrayState,
    settings: &Settings,
) -> tauri::Result<Menu<tauri::Wry>> {
    let status = tray_status(state, settings);
    let drop_line = state.progress.as_ref().map(|p| progress_lines(p).0);
    let campaign_line = state.progress.as_ref().map(|p| progress_lines(p).1);

    let status_item = MenuItem::with_id(app, "status", status.clone(), false, None::<&str>)?;
    let drop_item = drop_line
        .as_ref()
        .map(|text| MenuItem::with_id(app, "drop", text, false, None::<&str>))
        .transpose()?;
    let campaign_item = campaign_line
        .as_ref()
        .map(|text| MenuItem::with_id(app, "campaign", text, false, None::<&str>))
        .transpose()?;
    let info_sep = PredefinedMenuItem::separator(app)?;

    let pause = MenuItem::with_id(
        app,
        "pause",
        rustpick::t!["gui", "tray", if state.paused { "resume" } else { "pause" }],
        true,
        None::<&str>,
    )?;
    let automatic = CheckMenuItem::with_id(
        app,
        "chan:auto",
        rustpick::t!["gui", "tray", "automatic"],
        true,
        state.pinned.is_none(),
        None::<&str>,
    )?;
    let channel_sep = PredefinedMenuItem::separator(app)?;
    let candidates = tray_channels(&state.channels);
    let mut channel_entry_items = Vec::new();
    for channel in &candidates {
        let checked = state.pinned == Some(channel.id) || state.watching == Some(channel.id);
        channel_entry_items.push(CheckMenuItem::with_id(
            app,
            format!("chan:{}", channel.id),
            format!(
                "{} ({}, {})",
                channel.name,
                channel.game.as_deref().unwrap_or("-"),
                channel.viewers.unwrap_or(0)
            ),
            true,
            checked,
            None::<&str>,
        )?);
    }
    let mut channel_items: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&automatic, &channel_sep];
    channel_items.extend(
        channel_entry_items
            .iter()
            .map(|i| i as &dyn IsMenuItem<tauri::Wry>),
    );
    let switch = Submenu::with_items(
        app,
        rustpick::t!["gui", "tray", "switch_channel"],
        true,
        &channel_items,
    )?;
    let reload = MenuItem::with_id(
        app,
        "reload",
        rustpick::t!["gui", "tray", "reload"],
        true,
        None::<&str>,
    )?;
    let controls_sep = PredefinedMenuItem::separator(app)?;

    let can_open_stream = state.watching.is_some_and(|id| {
        state
            .channels
            .iter()
            .any(|c| c.id == id && !c.url.is_empty())
    });
    let open_stream = MenuItem::with_id(
        app,
        "open_stream",
        rustpick::t!["gui", "tray", "open_stream"],
        can_open_stream,
        None::<&str>,
    )?;
    let login = state
        .login
        .as_ref()
        .map(|(url, code)| {
            MenuItem::with_id(
                app,
                "signin",
                rustpick::t!["gui", "tray", "sign_in"; "code" => code.clone()],
                true,
                None::<&str>,
            )
            .map(|item| (item, url.clone(), code.clone()))
        })
        .transpose()?;
    let mut link_entries = Vec::new();
    for (idx, (game, campaign, _)) in state.unlinked.iter().enumerate() {
        link_entries.push(MenuItem::with_id(
            app,
            format!("link:{idx}"),
            rustpick::t!["gui", "tray", "link_account"; "campaign" => format!("{game} - {campaign}")],
            true,
            None::<&str>,
        )?);
    }
    let link_menu = if link_entries.len() > 1 {
        let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = link_entries
            .iter()
            .map(|i| i as &dyn IsMenuItem<tauri::Wry>)
            .collect();
        Some(Submenu::with_items(
            app,
            rustpick::t!["gui", "tray", "link_accounts"],
            true,
            &refs,
        )?)
    } else {
        None
    };
    let update = state
        .update
        .as_ref()
        .map(|(version, _)| {
            MenuItem::with_id(
                app,
                "update",
                rustpick::t!["gui", "tray", "update_available"; "version" => version.clone()],
                true,
                None::<&str>,
            )
            .map(|item| (item, version.clone()))
        })
        .transpose()?;
    let actions_sep = PredefinedMenuItem::separator(app)?;

    let show = MenuItem::with_id(
        app,
        "show",
        rustpick::t!["gui", "tray", "show_window"],
        true,
        None::<&str>,
    )?;
    let inventory = MenuItem::with_id(
        app,
        "inventory",
        rustpick::t!["gui", "tray", "inventory"],
        true,
        None::<&str>,
    )?;
    let settings_item = MenuItem::with_id(
        app,
        "settings",
        rustpick::t!["gui", "tray", "settings"],
        true,
        None::<&str>,
    )?;
    let nav_sep = PredefinedMenuItem::separator(app)?;

    let notify = CheckMenuItem::with_id(
        app,
        "notify",
        rustpick::t!["gui", "tray", "notify"],
        true,
        state.notify,
        None::<&str>,
    )?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        rustpick::t!["gui", "tray", "launch_at_login"],
        true,
        state.autostart,
        None::<&str>,
    )?;
    let toggles_sep = PredefinedMenuItem::separator(app)?;

    let quit_text = match &state.profile {
        Some(profile) => rustpick::t!["gui", "tray", "quit_profile"; "profile" => profile.clone()],
        None => rustpick::t!["gui", "tray", "quit_app"],
    };
    let quit = MenuItem::with_id(app, "quit", quit_text, true, None::<&str>)?;

    let mut items: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&status_item];
    if let Some(item) = &drop_item {
        items.push(item);
    }
    if let Some(item) = &campaign_item {
        items.push(item);
    }
    items.extend([
        &info_sep as &dyn IsMenuItem<tauri::Wry>,
        &pause,
        &switch,
        &reload,
        &controls_sep,
        &open_stream,
    ]);
    if let Some((item, _, _)) = &login {
        items.push(item);
    }
    if link_entries.len() == 1 {
        items.push(&link_entries[0]);
    } else if let Some(menu) = &link_menu {
        items.push(menu);
    }
    if let Some((item, _)) = &update {
        items.push(item);
    }
    items.extend([
        &actions_sep as &dyn IsMenuItem<tauri::Wry>,
        &show,
        &inventory,
        &settings_item,
        &nav_sep,
        &notify,
        &autostart,
        &toggles_sep,
        &quit,
    ]);
    Menu::with_items(app, &items)
}

fn sync_tray(app: &AppHandle) {
    let Some(tray) = app.try_state::<Mutex<TrayState>>() else {
        return;
    };
    let settings = app.state::<AppState>().settings.lock().unwrap().clone();
    let state = tray.lock().unwrap();
    let status = tray_status(&state, &settings);
    let Ok(menu) = build_tray_menu(app, &state, &settings) else {
        return;
    };
    let tooltip = state
        .progress
        .as_ref()
        .map(|p| format!("{status}\n{}", progress_lines(p).0))
        .unwrap_or(status);
    let _ = state.tray.set_menu(Some(menu));
    let _ = state.tray.set_tooltip(Some(&tooltip));
}

fn tray_event(app: &AppHandle, ev: &Event) {
    let Some(tray) = app.try_state::<Mutex<TrayState>>() else {
        return;
    };
    let changed = {
        let mut state = tray.lock().unwrap();
        match ev {
            Event::Status(text) => {
                state.status = text.clone();
                true
            }
            Event::Channels(channels) => {
                state.channels = channels.clone();
                true
            }
            Event::Watching(id) => {
                state.watching = *id;
                state.pinned = next_pin(state.pinned, *id);
                true
            }
            Event::Progress(progress) => {
                state.progress = progress.clone();
                true
            }
            Event::Paused(paused) => {
                state.paused = *paused;
                true
            }
            Event::LoginCode { url, code } => {
                state.login = Some((url.clone(), code.clone()));
                true
            }
            Event::LoggedIn { .. } => {
                state.login = None;
                true
            }
            Event::LoginRequired => {
                state.status = rustpick::t!["gui", "tray", "status_sign_in"];
                true
            }
            Event::AllDone => {
                state.status = rustpick::t!["gui", "tray", "status_all_done"];
                state.progress = None;
                true
            }
            Event::UnlinkedCampaign {
                game,
                campaign,
                link_url,
            } => {
                let entry = (game.clone(), campaign.clone(), link_url.clone());
                if state.unlinked.contains(&entry) {
                    false
                } else {
                    state.unlinked.push(entry);
                    true
                }
            }
            Event::Inventory(_) => {
                if state.unlinked.is_empty() {
                    false
                } else {
                    state.unlinked.clear();
                    true
                }
            }
            Event::UpdateAvailable { version, url } => {
                state.update = Some((version.clone(), url.clone()));
                true
            }
            _ => false,
        }
    };
    if changed {
        sync_tray(app);
    }
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

fn apply_settings(app: &AppHandle, settings: Settings) {
    // keep an existing autostart entry in step with the "start in tray" choice
    if rustpick::autostart::enabled() {
        let _ = rustpick::autostart::set(true, autostart_args(&settings));
    }
    let state = app.state::<AppState>();
    state.handle.send(Command::Settings(settings.clone()));
    *state.settings.lock().unwrap() = settings.clone();
    if let Some(tray) = app.try_state::<Mutex<TrayState>>() {
        tray.lock().unwrap().notify = settings.tray_notifications;
    }
    sync_tray(app);
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) {
    apply_settings(&app, settings);
}

/// Parse a TwitchDropsMiner settings file/folder and hand the values back to the
/// page. The user reviews them in the form and clicks Save to commit.
#[tauri::command]
fn import_tdm(path: String) -> Result<Settings, String> {
    let path = std::path::Path::new(&path);
    let settings = Settings::import_tdm(path).map_err(|e| e.to_string())?;
    // adopt the TDM install's login session too, if its cookies.jar is there
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(std::path::Path::to_path_buf).unwrap_or_default()
    };
    if let Some((device_id, token)) = rustpick::http::import_tdm_cookies(&dir) {
        let device_id = if device_id.len() == 32 {
            device_id
        } else {
            rustpick::util::nonce(rustpick::util::CHARS_HEX_LOWER, 32)
        };
        let _ = rustpick::http::save_auth_file(device_id, Some(token));
    }
    Ok(settings)
}

/// Is the login-time autostart entry present?
#[tauri::command]
fn get_autostart() -> bool {
    rustpick::autostart::enabled()
}

/// Create or remove the login-time autostart entry.
#[tauri::command]
fn set_autostart(
    app: AppHandle,
    state: tauri::State<AppState>,
    enabled: bool,
) -> Result<(), String> {
    let settings = state.settings.lock().unwrap().clone();
    rustpick::autostart::set(enabled, autostart_args(&settings)).map_err(|e| e.to_string())?;
    if let Some(tray) = app.try_state::<Mutex<TrayState>>() {
        tray.lock().unwrap().autostart = enabled;
    }
    sync_tray(&app);
    Ok(())
}

fn autostart_args(settings: &Settings) -> &'static [&'static str] {
    if settings.autostart_tray { &["--tray"] } else { &[] }
}

/// Names of the bundled UI languages.
#[tauri::command]
fn list_languages() -> Vec<&'static str> {
    rustpick::translate::LANGUAGES.iter().map(|(name, _)| *name).collect()
}

#[tauri::command]
fn reload(state: tauri::State<AppState>) {
    state.handle.send(Command::Reload);
}

#[tauri::command]
fn select(state: tauri::State<AppState>, id: Option<u64>) {
    state.handle.send(Command::Select(id));
}

const LOG_LINES: usize = 2000;

#[tauri::command]
fn get_log(state: tauri::State<AppState>) -> Vec<Event> {
    state.log.lock().unwrap().iter().cloned().collect()
}

#[tauri::command]
fn set_paused(state: tauri::State<AppState>, paused: bool) {
    state.handle.send(Command::Pause(paused));
}

#[tauri::command]
fn shutdown(state: tauri::State<AppState>) {
    state.handle.send(Command::Exit);
}

#[tauri::command]
fn open_url(app: AppHandle, url: String) {
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(url, None::<&str>);
}

#[tauri::command]
fn copy_text(app: AppHandle, text: String) {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let _ = app.clipboard().write_text(text);
}

/// Called once by the page after it has registered its `miner-event` listener, so no
/// startup events (login code, initial status) are emitted into the void.
#[tauri::command]
fn frontend_ready(app: AppHandle) {
    let state = app.state::<AppState>();
    let Some(miner) = state.miner.lock().unwrap().take() else {
        return; // already started (e.g. a dev reload)
    };
    let mut rx = state.events.subscribe();
    tauri::async_runtime::spawn(miner.run());
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Event::DropClaimed(claim) = &ev {
                        if app.state::<AppState>().settings.lock().unwrap().tray_notifications {
                            use tauri_plugin_notification::NotificationExt;
                            let title = rustpick::t!["gui", "tray", "notification_title"];
                            let body = format!("{} {}", claim.game, claim.rewards);
                            let _ = app.notification().builder().title(title).body(body).show();
                        }
                    }
                    let entry = match &ev {
                        Event::Print(msg) => Some(Event::log("INFO", msg.clone())),
                        Event::Log { .. } => Some(ev.clone()),
                        _ => None,
                    };
                    if let Some(entry) = entry {
                        let state = app.state::<AppState>();
                        let mut log = state.log.lock().unwrap();
                        if log.len() == LOG_LINES {
                            log.pop_front();
                        }
                        log.push_back(entry);
                    }
                    tray_event(&app, &ev);
                    if let Event::Paused(p) = ev {
                        app.state::<AppState>().paused.store(p, Ordering::Relaxed);
                    }
                    let exited = matches!(ev, Event::Exited);
                    let _ = app.emit("miner-event", ev);
                    if exited {
                        app.exit(0);
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// A plain solid-colour tray glyph, so we ship no icon assets.
// ponytail: placeholder tray icon; swap for a real bitmap when there's artwork.
fn tray_icon() -> tauri::image::Image<'static> {
    let (w, h) = (32u32, 32u32);
    let rgba: Vec<u8> = std::iter::repeat([0x91, 0x47, 0xff, 0xff]).take((w * h) as usize).flatten().collect();
    tauri::image::Image::new_owned(rgba, w, h)
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    // `--profile <name>`: a separate settings/login/lock directory, one process per profile
    let args: Vec<String> = std::env::args().collect();
    let profile = args.iter().position(|a| a == "--profile").and_then(|i| args.get(i + 1)).cloned();
    if let Some(p) = &profile
        && let Err(e) = rustpick::settings::set_profile(p)
    {
        eprintln!("--profile: {e:#}");
        std::process::exit(2);
    }
    let title = match &profile {
        Some(p) => format!("Rustpick ({p})"),
        None => "Rustpick".to_owned(),
    };
    let settings = Settings::load().unwrap_or_else(|e| {
        eprintln!("settings: {e}");
        Settings::default()
    });

    let lock = rustpick::lock::SingleInstance::acquire(&rustpick::settings::data_dir().join("lock.file"));
    if let Err(e) = lock {
        eprintln!("Another instance appears to be running or the lock file is unwritable ({e}). Exiting.");
        std::process::exit(3);
    }
    rustpick::translate::init(&settings.language);
    let (events, _) = broadcast::channel::<Event>(1024);
    rustpick::init_logging(0, false, Some(events.clone())).ok();
    let (miner, handle) = Miner::new(settings.clone(), events.clone(), false).expect("miner");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            handle,
            settings: Mutex::new(settings),
            miner: Mutex::new(Some(miner)),
            events,
            log: Mutex::new(VecDeque::new()),
            paused: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            import_tdm,
            get_autostart,
            set_autostart,
            list_languages,
            reload,
            select,
            set_paused,
            get_log,
            shutdown,
            open_url,
            copy_text,
            frontend_ready,
        ])
        .setup(move |app| {
            let tray = TrayIconBuilder::new()
                .icon(tray_icon())
                .tooltip(&title)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.state::<AppState>().handle.send(Command::Exit),
                    "show" => show_main(app),
                    "pause" => {
                        let state = app.state::<AppState>();
                        state.handle.send(Command::Pause(!state.paused.load(Ordering::Relaxed)));
                    }
                    "reload" => app.state::<AppState>().handle.send(Command::Reload),
                    "open_stream" => {
                        let url = app.try_state::<Mutex<TrayState>>().and_then(|tray| {
                            let state = tray.lock().unwrap();
                            state.watching.and_then(|id| {
                                state
                                    .channels
                                    .iter()
                                    .find(|c| c.id == id)
                                    .map(|c| c.url.clone())
                            })
                        });
                        if let Some(url) = url {
                            open_url(app.clone(), url);
                        }
                    }
                    "signin" => {
                        let login = app
                            .try_state::<Mutex<TrayState>>()
                            .and_then(|tray| tray.lock().unwrap().login.clone());
                        if let Some((url, code)) = login {
                            copy_text(app.clone(), code);
                            open_url(app.clone(), url);
                        }
                    }
                    "update" => {
                        let update = app
                            .try_state::<Mutex<TrayState>>()
                            .and_then(|tray| tray.lock().unwrap().update.clone());
                        if let Some((_, url)) = update {
                            open_url(app.clone(), url);
                        }
                    }
                    "inventory" | "settings" => {
                        show_main(app);
                        let tab = if event.id.as_ref() == "inventory" {
                            "inventory"
                        } else {
                            "settings"
                        };
                        let _ = app.emit("navigate", tab);
                    }
                    "notify" => {
                        let settings = {
                            let state = app.state::<AppState>();
                            let mut settings = state.settings.lock().unwrap().clone();
                            settings.tray_notifications = !settings.tray_notifications;
                            settings
                        };
                        apply_settings(app, settings);
                    }
                    "autostart" => {
                        let (enabled, settings) = {
                            let state = app.state::<AppState>();
                            let settings = state.settings.lock().unwrap().clone();
                            (!rustpick::autostart::enabled(), settings)
                        };
                        if rustpick::autostart::set(enabled, autostart_args(&settings)).is_ok()
                            && let Some(tray) = app.try_state::<Mutex<TrayState>>()
                        {
                            tray.lock().unwrap().autostart = enabled;
                            sync_tray(app);
                        }
                    }
                    id if id == "chan:auto" || id.starts_with("chan:") => {
                        let id = id.strip_prefix("chan:").unwrap().parse::<u64>().ok();
                        if let Some(tray) = app.try_state::<Mutex<TrayState>>() {
                            tray.lock().unwrap().pinned = id;
                        }
                        app.state::<AppState>().handle.send(Command::Select(id));
                        sync_tray(app);
                    }
                    id if id.starts_with("link:") => {
                        let link = id
                            .strip_prefix("link:")
                            .and_then(|i| i.parse::<usize>().ok())
                            .and_then(|i| {
                                app.try_state::<Mutex<TrayState>>().and_then(|tray| {
                                    tray.lock()
                                        .unwrap()
                                        .unlinked
                                        .get(i)
                                        .map(|(_, _, url)| url.clone())
                                })
                            });
                        if let Some(url) = link {
                            open_url(app.clone(), url);
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            let (notify, autostart) = {
                let state = app.state::<AppState>();
                let settings = state.settings.lock().unwrap();
                (settings.tray_notifications, rustpick::autostart::enabled())
            };
            app.manage(Mutex::new(TrayState {
                tray,
                profile: profile.clone(),
                status: rustpick::t!["gui", "tray", "status_starting"],
                paused: false,
                channels: Vec::new(),
                watching: None,
                pinned: None,
                progress: None,
                login: None,
                unlinked: Vec::new(),
                update: None,
                notify,
                autostart,
            }));
            sync_tray(app.handle());
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&title);
            }
            // the window starts hidden (tauri.conf.json); `--tray` keeps it that way
            if !std::env::args().any(|a| a == "--tray") {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close = graceful shutdown: ask the miner to exit; the forwarder calls
            // app.exit(0) once it acknowledges with Event::Exited.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app = window.app_handle();
                app.state::<AppState>().handle.send(Command::Exit);
                let app = app.clone();
                // ponytail: 3s hard-exit fallback if the core never acknowledges.
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    app.exit(0);
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: u64, status: ChannelStatus, drops_enabled: bool, viewers: u64) -> ChannelView {
        ChannelView {
            id,
            login: format!("channel-{id}"),
            name: format!("Channel {id}"),
            url: format!("https://twitch.tv/channel-{id}"),
            acl_based: false,
            status,
            game: None,
            drops_enabled,
            viewers: Some(viewers),
        }
    }

    #[test]
    fn tray_channel_list_filters_sorts_and_caps() {
        let mut channels = vec![channel(1, ChannelStatus::Online, true, 10)];
        channels
            .extend((2..=11).map(|id| channel(id, ChannelStatus::Online, true, 20 + id as u64)));
        channels.push(channel(12, ChannelStatus::Pending, true, 1000));
        channels.push(channel(13, ChannelStatus::Online, false, 2000));

        let selected = tray_channels(&channels);
        assert_eq!(selected.len(), 10);
        assert!(
            selected
                .iter()
                .all(|c| c.status == ChannelStatus::Online && c.drops_enabled)
        );
        assert_eq!(selected[0].id, 11);
        assert!(!selected.iter().any(|c| c.id == 12 || c.id == 13));
    }

    #[test]
    fn manual_channel_pin_clears_when_core_picks_elsewhere() {
        assert_eq!(next_pin(Some(3), Some(3)), Some(3));
        assert_eq!(next_pin(Some(3), Some(4)), None);
        assert_eq!(next_pin(None, Some(4)), None);
    }

    #[test]
    fn minute_parts_formats_remaining_time_units() {
        assert_eq!(minute_parts(59), (0, 0, 59));
        assert_eq!(minute_parts(190), (0, 3, 10));
        assert_eq!(minute_parts(1500), (1, 1, 0));
    }
}
