//! Tauri desktop frontend over the same miner core the CLI uses.
//!
//! The webview is a static page (`dist/index.html`); this backend spawns the miner,
//! forwards its [`Event`]s to the page as a `miner-event`, and maps a handful of
//! `#[tauri::command]`s back to [`Command`]s.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use rustpick::events::Event;
use rustpick::{Command, Miner, MinerHandle, Settings};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;

struct AppState {
    handle: MinerHandle,
    settings: Mutex<Settings>,
    /// The core, taken and started once the webview signals it is listening.
    miner: Mutex<Option<Miner>>,
    events: broadcast::Sender<Event>,
    /// Mirrors the last `Event::Paused`, for the tray toggle.
    paused: AtomicBool,
}

/// The tray's Pause/Resume item, relabeled on `Event::Paused`.
struct PauseItem(MenuItem<tauri::Wry>);

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(state: tauri::State<AppState>, settings: Settings) {
    // keep an existing autostart entry in step with the "start in tray" choice
    if rustpick::autostart::enabled() {
        let _ = rustpick::autostart::set(true, autostart_args(&settings));
    }
    *state.settings.lock().unwrap() = settings.clone();
    state.handle.send(Command::Settings(settings));
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
fn set_autostart(state: tauri::State<AppState>, enabled: bool) -> Result<(), String> {
    let settings = state.settings.lock().unwrap().clone();
    rustpick::autostart::set(enabled, autostart_args(&settings)).map_err(|e| e.to_string())
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
                    if let Event::Paused(p) = ev {
                        app.state::<AppState>().paused.store(p, Ordering::Relaxed);
                        if let Some(item) = app.try_state::<PauseItem>() {
                            let _ = item.0.set_text(if p { "Resume" } else { "Pause" });
                        }
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
    rustpick::init_logging(0, false).ok();
    let lock = rustpick::lock::SingleInstance::acquire(&rustpick::settings::data_dir().join("lock.file"));
    if let Err(e) = lock {
        eprintln!("Another instance appears to be running or the lock file is unwritable ({e}). Exiting.");
        std::process::exit(3);
    }
    rustpick::translate::init(&settings.language);
    let (events, _) = broadcast::channel::<Event>(1024);
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
            shutdown,
            open_url,
            copy_text,
            frontend_ready,
        ])
        .setup(move |app| {
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "Pause", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &pause, &quit])?;
            app.manage(PauseItem(pause));
            TrayIconBuilder::new()
                .icon(tray_icon())
                .tooltip(&title)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.state::<AppState>().handle.send(Command::Exit),
                    "show" => show_main(app),
                    "pause" => {
                        let state = app.state::<AppState>();
                        state.handle.send(Command::Pause(!state.paused.load(Ordering::Relaxed)));
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
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
