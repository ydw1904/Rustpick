//! Tauri desktop frontend over the same miner core the CLI uses.
//!
//! The webview is a static page (`dist/index.html`); this backend spawns the miner,
//! forwards its [`Event`]s to the page as a `miner-event`, and maps a handful of
//! `#[tauri::command]`s back to [`Command`]s.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use rustpick::events::Event;
use rustpick::{Command, Miner, MinerHandle, Settings};
use std::sync::Mutex;
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
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(state: tauri::State<AppState>, settings: Settings) {
    *state.settings.lock().unwrap() = settings.clone();
    state.handle.send(Command::Settings(settings));
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
    let settings = Settings::load().unwrap_or_else(|e| {
        eprintln!("settings: {e}");
        Settings::default()
    });
    rustpick::init_logging(0, false).ok();
    let (events, _) = broadcast::channel::<Event>(1024);
    let (miner, handle) = Miner::new(settings.clone(), events.clone()).expect("miner");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            handle,
            settings: Mutex::new(settings),
            miner: Mutex::new(Some(miner)),
            events,
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            reload,
            select,
            shutdown,
            open_url,
            copy_text,
            frontend_ready,
        ])
        .setup(|app| {
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .icon(tray_icon())
                .tooltip("Rustpick")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.state::<AppState>().handle.send(Command::Exit),
                    "show" => show_main(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
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
