# Rustpick

A Rust port of [TwitchDropsMiner](https://github.com/DevilXD/TwitchDropsMiner), with one
deliberate change: the mining core is a library with no UI in it, and it ships two frontends,
a headless CLI and a desktop GUI.

Watches Twitch channels headlessly (GraphQL + PubSub websocket, no video) to farm drop
campaign progress, switches channels when one goes offline or a higher priority game comes
up, and claims drops as they complete.

## Status

Early port. What works:

- Device-code login (no password handling; you enter a code on twitch.tv/activate)
- Inventory and campaign fetch, eligibility rules, claiming
- Channel discovery (campaign allow-lists and game directory), sharded PubSub for
  stream up/down/viewcount events, 120s online-delay check
- Watching via the spade "minute-watched" event, GQL progress fallback, pretend-mining
  with the same 15 minute ceiling as upstream
- Priority / exclude lists, three priority modes, hourly reload and cleanup triggers

Not ported (yet or on purpose): autostart, translations, campaign images,
`--dump` mode, the single-instance lock, cookie-jar compatibility with upstream.

Untested against a live account so far: the code paths after login compile and the
rules have unit tests, but the first real run will likely shake out field-name mistakes.

## Build

```sh
cargo build --release            # rustpick (CLI) and rustpick-gui (Tauri desktop UI)
cargo build --release -p rustpick             # CLI only, skips the Tauri dependency tree
```

The GUI (`rustpick-gui/`) is a separate workspace crate: a [Tauri](https://tauri.app)
shell over the same miner core, with a static vanilla-JS frontend in `rustpick-gui/dist/`
(no Node toolchain). It mirrors the original TDM window: Main / Inventory / Settings / Help
tabs plus a tray icon.

On macOS with Command Line Tools that ship a newer SDK than your `ld` understands
(`tapi error: unknown architecture arm64e.x1`), point the build at an older SDK:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo build
```

## Usage

```sh
rustpick                         # mine (same as `rustpick run`)
rustpick -v run                  # more logging (-vv, -vvv); RUST_LOG overrides
rustpick priority "Game A" "Game B"
rustpick exclude "Game C"
rustpick mode ending-soonest     # priority-only | ending-soonest | low-avbl-first
rustpick set badges-emotes true
rustpick show                    # print settings.json
rustpick logout                  # forget the saved token
rustpick-gui                     # Tauri desktop UI: Main / Inventory / Settings / Help tabs
```

Files (`settings.json`, `auth.json`, `log.txt`) live in the current directory, or in
`$RUSTPICK_DIR` if set. `auth.json` holds your Twitch access token; treat it like a password.

## Architecture

- `src/miner.rs` is the state machine from upstream `twitch.py`, run as a single task that
  owns all state. Websocket tasks, timers and frontends send it `Msg`s.
- Frontends receive `Event`s over a broadcast channel and send `Command`s through a
  `MinerHandle`. The CLI (`src/bin/cli.rs`) and the Tauri GUI (`rustpick-gui/`) are both thin.
- `src/http.rs` is the request/GQL layer with upstream's retry and rate-limit rules,
  `src/inventory.rs` the campaign and drop rules, `src/websocket.rs` the PubSub pool.

## License

GPL-3.0, matching upstream TwitchDropsMiner.
