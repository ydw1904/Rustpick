//! Headless frontend: prints miner events to stdout, Ctrl-C exits cleanly.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rustpick::events::{CampaignStatus, ChannelStatus, Claim};
use rustpick::settings::{PriorityMode, data_dir};
use rustpick::{Command, Event, Miner, Settings};
use std::collections::BTreeSet;
use tokio::sync::broadcast;
#[derive(Parser)]
#[command(name = "rustpick", version, about = "Headless Twitch drops miner (a port of TwitchDropsMiner)")]
struct Cli {
    /// Increase log verbosity (-v info, -vv debug, -vvv trace). RUST_LOG overrides.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    /// Write logs to log.txt in the data directory instead of stderr
    #[arg(long, global = true)]
    log: bool,
    /// Use a named profile (its own settings, login and lock) under the user config dir.
    /// RUSTPICK_DIR still wins when set.
    #[arg(long, global = true)]
    profile: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mine drops (the default when no subcommand is given)
    Run {
        /// Fetch the inventory once, write it to dump.dat and exit
        #[arg(long)]
        dump: bool,
        /// If a login is needed, print the activation URL and code, then exit with code 4
        #[arg(long)]
        no_interactive_login: bool,
    },
    /// Print the settings file path and contents
    Show,
    /// Replace the game priority list (exact game names, highest priority first)
    Priority { games: Vec<String> },
    /// Replace the excluded games list
    Exclude { games: Vec<String> },
    /// Priority mode: priority-only | ending-soonest | low-avbl-first
    Mode { mode: String },
    /// Toggle a flag: badges-emotes | drops-check | dark-mode
    Set { key: String, value: bool },
    /// Set the webhook URL for notifications (claims, login needed, all done); "" turns it off
    NotifyUrl { url: String },
    /// Print the claimed drops history (claims.jsonl)
    Claims,
    /// Show what the running miner is doing (from status.json)
    Status {
        /// Print nothing; exit 1 unless a running miner updated status.json in the last 10 minutes
        #[arg(long)]
        check: bool,
    },
    /// List active campaigns that need a linked game account (from status.json)
    Unlinked,
    /// Only mine between these local hours, e.g. 22-6 (wraps past midnight); "off" to always mine
    ActiveHours { hours: String },
    /// Set the UI language (run `rustpick language` for the list)
    Language { name: Option<String> },
    /// Forget the saved Twitch login
    Logout,
    /// Show (no argument) or toggle login-time autostart: on | off
    Autostart { state: Option<String> },
    /// Import settings and login from a TwitchDropsMiner install (its settings.json or its folder)
    Import { path: std::path::PathBuf },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(p) = &cli.profile {
        rustpick::settings::set_profile(p)?;
    }
    rustpick::init_logging(cli.verbose, cli.log)?;
    let mut settings = Settings::load()?;
    rustpick::translate::init(&settings.language);
    match cli.cmd.unwrap_or(Cmd::Run { dump: false, no_interactive_login: false }) {
        Cmd::Run { dump, no_interactive_login } => return run(settings, dump, no_interactive_login).await,
        Cmd::Show => {
            println!("{}", Settings::path().display());
            println!("{}", serde_json::to_string_pretty(&settings)?);
            return Ok(());
        }
        Cmd::NotifyUrl { url } => settings.notify_url = url,
        Cmd::Claims => return print_claims(),
        Cmd::Status { check } => return print_status(check),
        Cmd::Unlinked => return print_unlinked(),
        Cmd::ActiveHours { hours } => {
            settings.active_hours = match hours.as_str() {
                "off" => None,
                _ => Some(
                    rustpick::settings::parse_active_hours(&hours)
                        .ok_or_else(|| anyhow!("expected START-END hours like 22-6, or off"))?,
                ),
            };
        }
        Cmd::Priority { games } => settings.priority = games,
        Cmd::Exclude { games } => settings.exclude = games.into_iter().collect(),
        Cmd::Mode { mode } => {
            settings.priority_mode =
                PriorityMode::parse(&mode).ok_or_else(|| anyhow!("unknown mode '{mode}'"))?;
        }
        Cmd::Set { key, value } => match key.as_str() {
            "badges-emotes" => settings.enable_badges_emotes = value,
            "drops-check" => settings.available_drops_check = value,
            "dark-mode" => settings.dark_mode = value,
            other => bail!("unknown key '{other}'"),
        },
        Cmd::Language { name } => match name {
            None => {
                println!("{}", rustpick::translate::LANGUAGES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "));
                return Ok(());
            }
            Some(name) => {
                settings.language = rustpick::translate::LANGUAGES
                    .iter()
                    .map(|(n, _)| *n)
                    .find(|n| n.eq_ignore_ascii_case(&name))
                    .ok_or_else(|| anyhow!("unknown language '{name}' (run `rustpick language` for the list)"))?
                    .to_owned();
            }
        },
        Cmd::Logout => {
            let _ = std::fs::remove_file(data_dir().join("auth.json"));
            println!("Logged out.");
            return Ok(());
        }
        Cmd::Autostart { state } => match state.as_deref() {
            None => println!("Autostart is {}", if rustpick::autostart::enabled() { "on" } else { "off" }),
            Some("on") => {
                rustpick::autostart::set(true, &["run"])?;
                println!("Autostart enabled: {} runs at login.", std::env::current_exe()?.display());
            }
            Some("off") => {
                rustpick::autostart::set(false, &[])?;
                println!("Autostart disabled.");
            }
            Some(other) => bail!("unknown autostart state '{other}' (use on or off)"),
        },
        Cmd::Import { path } => {
            settings = Settings::import_tdm(&path)?;
            rustpick::translate::init(&settings.language);
            import_tdm_login(&path)?;
        }
    }
    settings.save()?;
    println!("Saved {}", Settings::path().display());
    Ok(())
}

/// Look for a `cookies.jar` next to the TDM settings file and adopt its login
/// session, so a migrated install doesn't need to log in again.
fn import_tdm_login(path: &std::path::Path) -> Result<()> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(std::path::Path::to_path_buf).unwrap_or_default()
    };
    match rustpick::http::import_tdm_cookies(&dir) {
        Some((device_id, token)) => {
            let device_id = if device_id.len() == 32 {
                device_id
            } else {
                rustpick::util::nonce(rustpick::util::CHARS_HEX_LOWER, 32)
            };
            rustpick::http::save_auth_file(device_id, Some(token))?;
            println!("Imported the login session from {}", dir.join("cookies.jar").display());
        }
        None => println!("No usable cookies.jar found next to the settings (you'll need to log in)."),
    }
    Ok(())
}

fn read_status() -> Result<serde_json::Value> {
    let path = data_dir().join(rustpick::status::FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {} (is a miner running for this data dir?)", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Minutes since the miner last wrote status.json, None if it exited or the file is unusable.
fn status_age_minutes(v: &serde_json::Value) -> Option<i64> {
    let updated = v["updated_at"].as_str()?.parse::<chrono::DateTime<chrono::Utc>>().ok()?;
    (!v["exited"].as_bool().unwrap_or(false)).then(|| (chrono::Utc::now() - updated).num_minutes())
}

fn print_status(check: bool) -> Result<()> {
    let v = read_status();
    if check {
        let healthy = v.ok().and_then(|v| status_age_minutes(&v)).is_some_and(|m| m < 10);
        std::process::exit(if healthy { 0 } else { 1 });
    }
    let v = v?;
    let updated = v["updated_at"]
        .as_str()
        .and_then(|t| t.parse::<chrono::DateTime<chrono::Utc>>().ok())
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let state = match status_age_minutes(&v) {
        None => "exited",
        Some(m) if m >= 10 => "stale (miner not updating)",
        _ if v["login_required"].as_bool() == Some(true) => "waiting for login",
        _ if v["paused"].as_bool() == Some(true) => "paused",
        _ => "running",
    };
    let s = |x: &serde_json::Value| x.as_str().unwrap_or("-").to_owned();
    let n = |x: &serde_json::Value| x.as_u64().unwrap_or(0);
    println!("State:    {state} (pid {}, updated {updated})", n(&v["pid"]));
    println!("Status:   {}", s(&v["status"]));
    println!("Channel:  {}", s(&v["channel"]));
    let p = &v["progress"];
    if p.is_object() {
        println!("Game:     {} | {}", s(&p["game"]), s(&p["campaign_name"]));
        println!(
            "Drop:     {} | {}/{} min, {} min left",
            s(&p["drop_rewards"]),
            n(&p["drop_current"]),
            n(&p["drop_required"]),
            n(&p["drop_required"]).saturating_sub(n(&p["drop_current"]))
        );
        println!(
            "Campaign: {}/{} drops, {} min left",
            n(&p["campaign_claimed"]),
            n(&p["campaign_total"]),
            n(&p["campaign_remaining_minutes"])
        );
    }
    let unlinked = v["unlinked"].as_array().map(Vec::len).unwrap_or(0);
    if unlinked > 0 {
        println!("Unlinked: {unlinked} active campaign(s) need a linked account (rustpick unlinked)");
    }
    Ok(())
}

fn print_unlinked() -> Result<()> {
    let v = read_status()?;
    let list = v["unlinked"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("No active campaigns need a linked account.");
    }
    for c in list {
        let s = |k: &str| c[k].as_str().unwrap_or("").to_owned();
        println!("{} | {} | {}", s("game"), s("campaign"), s("link_url"));
    }
    Ok(())
}

fn print_claims() -> Result<()> {
    let path = data_dir().join("claims.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No drops claimed yet.");
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Claim>(line) {
            Ok(c) => println!(
                "{} | {} | {} | {}",
                c.ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
                c.game,
                c.campaign,
                c.rewards
            ),
            Err(e) => eprintln!("skipping a malformed line ({e}): {line}"),
        }
    }
    Ok(())
}

/// Exit code for "a login is needed but --no-interactive-login was given".
const EXIT_LOGIN_REQUIRED: i32 = 4;

async fn run(settings: Settings, dump: bool, no_interactive_login: bool) -> Result<()> {
    let lock = rustpick::lock::SingleInstance::acquire(&data_dir().join("lock.file"));
    let lock = match lock {
        Ok(l) => l,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            eprintln!("Another instance appears to be running ({e}). Exiting.");
            std::process::exit(3);
        }
        Err(e) => return Err(e).context("Could not create the lock file"),
    };
    let (events_tx, mut events) = broadcast::channel(1024);
    let (miner, handle) = Miner::new(settings, events_tx, dump)?;
    let mut task = tokio::spawn(miner.run());
    let ctrlc_handle = handle.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("Exiting... (press Ctrl-C again to force)");
        ctrlc_handle.send(Command::Exit);
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(130);
    });
    let mut printer = Printer::default();
    let mut login_needed = false;
    let result = loop {
        tokio::select! {
            r = &mut task => break r?,
            ev = events.recv() => match ev {
                Ok(e) => {
                    if no_interactive_login && matches!(e, Event::LoginCode { .. }) && !login_needed {
                        login_needed = true;
                        handle.send(Command::Exit);
                    }
                    printer.print(e)
                }
                Err(broadcast::error::RecvError::Lagged(n)) => eprintln!("(dropped {n} events)"),
                Err(broadcast::error::RecvError::Closed) => break task.await?,
            }
        }
    };
    drop(lock);
    if login_needed {
        eprintln!("Login required and --no-interactive-login is set. Exiting with code {EXIT_LOGIN_REQUIRED}.");
        std::process::exit(EXIT_LOGIN_REQUIRED);
    }
    result
}

#[derive(Default)]
struct Printer {
    last_progress: Option<String>,
    last_channels: Option<String>,
}

impl Printer {
    fn line(&self, s: impl AsRef<str>) {
        println!("{} {}", chrono::Local::now().format("%H:%M:%S"), s.as_ref());
    }

    fn print(&mut self, e: Event) {
        match e {
            Event::Print(s) => self.line(s),
            Event::Status(s) => tracing::info!("Status: {s}"),
            Event::LoginRequired => self.line("The saved Twitch login has expired."),
            Event::AllDone => self.line("All campaigns are done."),
            Event::Paused(p) => self.line(if p { "Paused." } else { "Resumed." }),
            Event::LoginCode { url, code } => self.line(format!(">>> Open {url} and enter the code: {code}")),
            Event::LoggedIn { user_id } => self.line(format!("Logged in (user ID {user_id})")),
            Event::Inventory(campaigns) => {
                let active = campaigns.iter().filter(|c| c.status == CampaignStatus::Active).count();
                let games: BTreeSet<&str> = campaigns
                    .iter()
                    .filter(|c| c.eligible && c.status != CampaignStatus::Expired)
                    .map(|c| c.game.as_str())
                    .collect();
                self.line(format!(
                    "Inventory: {} campaigns, {active} active. Eligible games: {}",
                    campaigns.len(),
                    games.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
            Event::Channels(channels) => {
                let online = channels.iter().filter(|c| c.status == ChannelStatus::Online).count();
                let summary = format!("Tracking {} channels, {online} online", channels.len());
                if self.last_channels.as_deref() != Some(&summary) {
                    self.line(&summary);
                    self.last_channels = Some(summary);
                }
            }
            Event::Progress(Some(p)) => {
                let s = format!(
                    "{} | {} | {}/{} min ({:.0}%) | campaign {}/{} drops, {} min left",
                    p.game,
                    p.drop_rewards,
                    p.drop_current,
                    p.drop_required,
                    p.drop_progress * 100.0,
                    p.campaign_claimed,
                    p.campaign_total,
                    p.campaign_remaining_minutes
                );
                if self.last_progress.as_deref() != Some(&s) {
                    self.line(&s);
                    self.last_progress = Some(s);
                }
            }
            Event::Progress(None) => self.last_progress = None,
            // the miner already prints a line for these
            Event::Watching(_) | Event::Websocket { .. } | Event::DropClaimed(_) | Event::UnlinkedCampaign { .. } => {}
            Event::Exited => self.line("Exited."),
        }
    }
}
