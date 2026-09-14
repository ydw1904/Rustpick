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
pub mod status;
pub mod translate;
pub mod update;
pub mod util;
pub mod websocket;

pub use events::{Command, Event};
pub use miner::{Miner, MinerHandle};
pub use settings::Settings;

/// stderr (or log.txt) tracing at a level picked by `-v` count. `RUST_LOG` overrides.
/// With `events`, our own info-and-above records are also sent as `Event::Log`.
pub fn init_logging(verbosity: u8, to_file: bool, events: Option<tokio::sync::broadcast::Sender<Event>>) -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, filter::LevelFilter, fmt};
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("rustpick={level}")));
    let output = if to_file {
        let file = std::fs::File::create(settings::data_dir().join("log.txt"))?;
        fmt::layer().with_target(false).with_ansi(false).with_writer(std::sync::Mutex::new(file)).boxed()
    } else {
        fmt::layer().with_target(false).with_writer(std::io::stderr).boxed()
    };
    let events = events.map(|tx| EventLayer(tx).with_filter(LevelFilter::INFO));
    tracing_subscriber::registry().with(output.with_filter(filter)).with(events).init();
    Ok(())
}

/// Forwards rustpick's own tracing records to the event channel.
struct EventLayer(tokio::sync::broadcast::Sender<Event>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let meta = event.metadata();
        if !meta.target().starts_with("rustpick") {
            return;
        }
        let mut msg = Message(String::new());
        event.record(&mut msg);
        let _ = self.0.send(Event::log(meta.level().to_string(), msg.0));
    }
}
