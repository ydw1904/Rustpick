//! Rustpick: a headless Twitch drops miner, ported from TwitchDropsMiner.
//!
//! The core ([`Miner`]) is frontend-agnostic: it emits [`Event`]s over a broadcast channel
//! and accepts [`Command`]s through a [`MinerHandle`]. `src/bin/cli.rs` and `src/bin/gui.rs`
//! are thin frontends over it.

pub mod autostart;
pub mod channel;
pub mod constants;
pub mod events;
pub mod http;
pub mod inventory;
pub mod lock;
pub mod miner;
pub mod notify;
pub mod pickle;
pub mod settings;
pub mod translate;
pub mod util;
pub mod websocket;

pub use events::{Command, Event};
pub use miner::{Miner, MinerHandle};
pub use settings::Settings;

/// stderr (or log.txt) tracing at a level picked by `-v` count. `RUST_LOG` overrides.
pub fn init_logging(verbosity: u8, to_file: bool) -> anyhow::Result<()> {
    use tracing_subscriber::EnvFilter;
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("rustpick={level}")));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    if to_file {
        let file = std::fs::File::create(settings::data_dir().join("log.txt"))?;
        builder.with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    } else {
        builder.with_writer(std::io::stderr).init();
    }
    Ok(())
}
