//! Headless frontend: prints miner events to stdout, Ctrl-C exits cleanly.

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rustpick::events::{CampaignStatus, ChannelStatus};
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
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mine drops (the default when no subcommand is given)
    Run,
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
    /// Forget the saved Twitch login
    Logout,
    /// Import settings from a TwitchDropsMiner install (its settings.json or its folder)
    Import { path: std::path::PathBuf },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    rustpick::init_logging(cli.verbose, cli.log)?;
    let mut settings = Settings::load()?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => return run(settings).await,
        Cmd::Show => {
            println!("{}", Settings::path().display());
            println!("{}", serde_json::to_string_pretty(&settings)?);
            return Ok(());
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
        Cmd::Logout => {
            let _ = std::fs::remove_file(data_dir().join("auth.json"));
            println!("Logged out.");
            return Ok(());
        }
        Cmd::Import { path } => settings = Settings::import_tdm(&path)?,
    }
    settings.save()?;
    println!("Saved {}", Settings::path().display());
    Ok(())
}

async fn run(settings: Settings) -> Result<()> {
    let (events_tx, mut events) = broadcast::channel(1024);
    let (miner, handle) = Miner::new(settings, events_tx)?;
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
    loop {
        tokio::select! {
            r = &mut task => return r?,
            ev = events.recv() => match ev {
                Ok(e) => printer.print(e),
                Err(broadcast::error::RecvError::Lagged(n)) => eprintln!("(dropped {n} events)"),
                Err(broadcast::error::RecvError::Closed) => return task.await?,
            }
        }
    }
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
            Event::Watching(_) | Event::Websocket { .. } => {}
            Event::Exited => self.line("Exited."),
        }
    }
}
