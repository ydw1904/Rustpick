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
- `--dump` mode, the single-instance lock, autostart, translations (upstream's
  `lang/*.json`), importing a TwitchDropsMiner install's `cookies.jar` login session

Deliberately different from upstream: campaign images need no port (the webview GUI
already loads them straight from the CDN, so there's no image cache like upstream's
Tkinter one), and the GUI's own chrome (tabs, buttons) is English-only — the core's
messages are the ones that come translated.

Untested against a live account so far: the code paths after login compile and the
rules have unit tests, but the first real run will likely shake out field-name mistakes.

## Build

```sh
cargo build --release -p rustpick       # CLI only, skips the Tauri dependency tree
cd rustpick-gui && npm run tauri build  # GUI: runs the UI build, then cargo --release
```

The first GUI build needs `cd rustpick-gui/ui && npm ci` once (installs Vite, the shadcn
primitives and the `@tauri-apps/cli` wrapper the `npm run tauri` script calls).

The GUI (`rustpick-gui/`) is a separate workspace crate: a [Tauri](https://tauri.app)
shell over the same miner core. Its frontend lives in `rustpick-gui/ui/` — React +
[shadcn/ui](https://ui.shadcn.com) primitives (Tailwind v4, Hugeicons) built with Vite.
It mirrors the original TDM window: Main / Inventory / Settings / Help tabs plus a tray
icon.

Develop the UI with hot reload:

```sh
cd rustpick-gui && npm run tauri dev    # starts Vite on :5173 + the app with HMR
```

Tauri commands must run from `rustpick-gui/` (not `ui/`). Plain `cargo build`/`cargo run`
on the GUI crate produces a *dev-mode* binary that expects `http://localhost:5173` to be
serving; add `--features tauri/custom-protocol` to embed `ui/dist` instead and get a
standalone binary.

On macOS with Command Line Tools that ship a newer SDK than your `ld` understands
(`tapi error: unknown architecture arm64e.x1`), point the build at an older SDK:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo build
```

(The same `SDKROOT=` prefix works for `npm run tauri dev` / `npm run tauri build`.)

## Usage

```sh
rustpick                         # mine (same as `rustpick run`)
rustpick -v run                  # more logging (-vv, -vvv); RUST_LOG overrides
rustpick run --dump              # fetch the inventory once, write dump.dat, exit
rustpick run --no-interactive-login  # headless: if a login is needed, print the code and exit 4
rustpick priority "Game A" "Game B"
rustpick exclude "Game C"
rustpick mode ending-soonest     # priority-only | ending-soonest | low-avbl-first
rustpick set badges-emotes true
rustpick language Deutsch        # no argument lists the bundled languages
rustpick autostart on            # off | bare to show the current state
rustpick show                    # print settings.json
rustpick notify-url https://discord.com/api/webhooks/...  # JSON POST on claims, login needed, all done ("" = off)
rustpick claims                  # list every claimed drop (claims.jsonl)
rustpick logout                  # forget the saved token
rustpick import ~/TwitchDropsMiner   # adopt a TDM install's settings AND cookies.jar login
rustpick-gui                     # Tauri desktop UI: Main / Inventory / Settings / Help tabs
rustpick-gui --tray              # start hidden in the tray (what "Autostart in tray" uses)
```

Files (`settings.json`, `auth.json`, `claims.jsonl`, `log.txt`, `lock.file`) live in the current directory, or in
`$RUSTPICK_DIR` if set. `auth.json` holds your Twitch access token; treat it like a password.
Only one miner can run per data directory at a time.

The webhook body carries the message as `content` (Discord), `text` (Slack) and `message` (Gotify),
plus `title` and the raw `event`. Pause/Resume is in the GUI (Main tab and tray) for now.

Exit codes: `0` normal exit, `1` fatal error, `3` another instance holds the lock,
`4` a login is needed and `--no-interactive-login` was given.

## Architecture

- `src/miner.rs` is the state machine from upstream `twitch.py`, run as a single task that
  owns all state. Websocket tasks, timers and frontends send it `Msg`s.
- Frontends receive `Event`s over a broadcast channel and send `Command`s through a
  `MinerHandle`. The CLI (`src/bin/cli.rs`) and the Tauri GUI (`rustpick-gui/`) are both thin.
- `src/http.rs` is the request/GQL layer with upstream's retry and rate-limit rules,
  `src/inventory.rs` the campaign and drop rules, `src/websocket.rs` the PubSub pool.
- `src/translate.rs` loads upstream's `lang/*.json` over an embedded English base,
  `src/lock.rs` is the single-instance lock, `src/autostart.rs` the per-OS login entry,
  `src/pickle.rs` a minimal Python-pickle reader for upstream's `cookies.jar`.

## License

GPL-3.0, matching upstream TwitchDropsMiner.
