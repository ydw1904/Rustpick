// Browser-only stand-in for the Tauri bridge so `vite dev` renders with data.
// Installed by tauri.ts only when window.__TAURI__ is missing.

const themeParam = new URLSearchParams(location.search).get("theme")
const settings = {
  proxy: "",
  priority: ["Rust", "Warframe", "Path of Exile 2"],
  exclude: ["Fortnite"],
  whitelist: true,
  blacklist: true,
  linked_only: true,
  order: "ending_soonest",
  connection_quality: 3,
  enable_badges_emotes: false,
  available_drops_check: true,
  theme: themeParam === "light" || themeParam === "dark" ? themeParam : "system",
  language: "English",
  tray_notifications: true,
  notify_url: "",
  check_updates: true,
  active_hours: null as [number, number] | null,
  autostart_tray: false,
}

const channels = [
  ["shroud", "Online", "Rust", true, 18234, false],
  ["Rustafied", "Online", "Rust", true, 4120, true],
  ["hJune", "Online", "Rust", true, 2310, false],
  ["Blooprint", "Pending", "Rust", true, null, false],
  ["Stimpee", "Offline", null, false, null, false],
  ["Willjum", "Offline", null, false, null, true],
  ["Posty", "Online", "Warframe", true, 980, false],
  ["Frost", "Offline", null, false, null, false],
].map(([name, status, game, drops_enabled, viewers, acl_based], i) => ({
  id: i + 1, name, url: `https://twitch.tv/${name}`, acl_based, status, game, drops_enabled, viewers,
}))

const rewardImage = "https://static-cdn.jtvnw.net/twitch-quests-assets/REWARD/e0ede26e-b071-47f0-af5f-b80b26fa9fb4.png"
const drop = (id: string, name: string, rewards: string, cur: number, req: number, claimed = false) => ({
  id, name, rewards, benefits: [{ id: `${id}-reward`, name: rewards, image_url: rewardImage }],
  current_minutes: cur, required_minutes: req, is_claimed: claimed, progress: cur / req,
})
const now = Date.now()
const iso = (h: number) => new Date(now + h * 3600_000).toISOString()
const inventory = [
  {
    id: "c1", name: "Rust Twitch Drops Round 26", game: "Rust", image_url: "", link_url: "https://twitch.facepunch.com",
    linked: true, needs_link: !true, eligible: true, status: "Active", starts_at: iso(-48), ends_at: iso(96), progress: 0.4, claimed: 2, total: 5,
    drops: [
      drop("d1", "Drop 1", "Neon Hoodie", 120, 120, true),
      drop("d2", "Drop 2", "Glowing Sledgehammer", 240, 240, true),
      drop("d3", "Drop 3", "Cyber Pants with an unusually long item name", 95, 360),
      drop("d4", "Drop 4", "Holo Door", 0, 480),
      drop("d5", "Drop 5", "Chrome Rock", 0, 600),
    ],
  },
  {
    id: "c2", name: "Warframe: Techrot Encore", game: "Warframe", image_url: "", link_url: "https://warframe.com/twitch",
    linked: false, needs_link: !false, eligible: false, status: "Active", starts_at: iso(-10), ends_at: iso(30), progress: 0, claimed: 0, total: 2,
    drops: [drop("d6", "Drop 1", "Orokin Reactor", 0, 60), drop("d7", "Drop 2", "Forma Bundle", 0, 120)],
  },
  {
    id: "c3", name: "Path of Exile 2 Launch Week", game: "Path of Exile 2", image_url: "", link_url: "",
    linked: true, needs_link: !true, eligible: true, status: "Upcoming", starts_at: iso(12), ends_at: iso(200), progress: 0, claimed: 0, total: 1,
    drops: [drop("d8", "Drop 1", "Twitch Portal Effect", 0, 180)],
  },
  {
    id: "c4", name: "Fortnite OG Weekend", game: "Fortnite", image_url: "", link_url: "",
    linked: true, needs_link: !true, eligible: true, status: "Expired", starts_at: iso(-200), ends_at: iso(-40), progress: 1, claimed: 1, total: 1,
    drops: [drop("d9", "Drop 1", "OG Spray", 60, 60, true)],
  },
]

type Listener = (e: { payload: unknown }) => void
const listeners = new Set<Listener>()
let bootTimer: ReturnType<typeof setTimeout> | undefined
const emit = (kind: string, data?: unknown) => listeners.forEach((l) => l({ payload: { kind, data } }))
const log = (s: string) => emit("Print", s)

export function install() {
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        if (cmd === "get_settings") return settings
        if (cmd === "save_settings") Object.assign(settings, args?.settings as object)
        if (cmd === "get_autostart") return false
        if (cmd === "pick_tdm_settings") return "/Users/me/TwitchDropsMiner/settings.json"
        if (cmd === "set_autostart") log(`Autostart ${args?.enabled ? "enabled" : "disabled"}`)
        if (cmd === "list_languages") return ["English", "Deutsch", "Français", "Русский", "简体中文"]
        if (cmd === "frontend_ready") {
          clearTimeout(bootTimer)
          bootTimer = setTimeout(boot, 50)
        }
        if (cmd === "select") log(`Switching to channel ${args?.id ?? "(auto)"}`)
        if (cmd === "reload") log("Reloading…")
        if (cmd === "get_log") return [
          { kind: "Log", data: { ts: new Date().toISOString(), level: "WARN", msg: "Websocket[2] connection problem: timed out (sleep: 5s)" } },
          { kind: "Log", data: { ts: new Date().toISOString(), level: "ERROR", msg: "Drop claim has potentially failed! Drop ID: d1" } },
        ]
        if (cmd === "set_paused") emit("Paused", args?.paused)
        if (cmd === "open_url" || cmd === "copy_text") log(`${cmd}: ${JSON.stringify(args)}`)
        return null
      },
    },
    event: {
      listen: async (_name, cb) => { listeners.add(cb); return () => listeners.delete(cb) },
    },
  }
}

function boot() {
  const scenario = new URLSearchParams(location.search).get("scene") ?? "mining"
  log("Rustpick starting")
  if (scenario === "login") {
    emit("Status", "Waiting for login…")
    emit("LoginCode", { url: "https://www.twitch.tv/activate", code: "ABCD-EFGH" })
    return
  }
  emit("LoggedIn", { user_id: 12345678 })
  emit("UpdateAvailable", { version: "v0.2.0", url: "https://github.com/ydw1904/Rustpick/releases/tag/v0.2.0" })
  emit("Status", "Watching Rustafied")
  for (let i = 0; i < 3; i++) emit("Websocket", { idx: i, status: "Connected", topics: i === 2 ? 12 : 50 })
  emit("Channels", channels)
  emit("Watching", 2)
  emit("Inventory", inventory)
  emit("Progress", {
    drop_rewards: "Cyber Pants with an unusually long item name", drop_progress: 95 / 360, drop_current: 95, drop_required: 360,
    campaign_name: "Rust Twitch Drops Round 26", game: "Rust", campaign_progress: 0.4, campaign_claimed: 2, campaign_total: 5,
    campaign_remaining_minutes: 1345, countdown: true,
  })
  for (let i = 0; i < 12; i++) log(`Channel ${channels[i % channels.length].name} updated (viewers ${1000 + i * 37})`)
}
