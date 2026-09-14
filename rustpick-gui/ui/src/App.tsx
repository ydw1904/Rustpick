import { useEffect, useRef, useState } from "react"
import { HugeiconsIcon } from "@hugeicons/react"
import {
  Add01Icon,
  ArrowDown02Icon,
  ArrowDownDoubleIcon,
  ArrowUp02Icon,
  ArrowUpDoubleIcon,
  Copy01Icon,
  Delete02Icon,
  GemIcon,
  GiftIcon,
  Image01Icon,
  LinkSquare02Icon,
  LockIcon,
  RefreshIcon,
  Tick02Icon,
} from "@hugeicons/core-free-icons"
import logo from "../../icons/128x128.png"
import { invoke } from "@/lib/tauri"
import { countdownParts } from "@/lib/countdown"
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Checkbox } from "@/components/ui/checkbox"
import { Input } from "@/components/ui/input"
import { Progress } from "@/components/ui/progress"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Slider } from "@/components/ui/slider"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

/* ---------- types mirroring the Rust core's serde output ---------- */

type ChannelStatus = "Online" | "Pending" | "Offline"
type CampaignStatus = "Active" | "Upcoming" | "Expired"
type PriorityMode = "priority_only" | "ending_soonest" | "low_avbl_first"

interface Channel {
  id: number
  login: string
  name: string
  url: string
  acl_based: boolean
  status: ChannelStatus
  game: string | null
  drops_enabled: boolean
  viewers: number | null
}

interface Drop {
  id: string
  name: string
  rewards: string
  current_minutes: number
  required_minutes: number
  is_claimed: boolean
  progress: number
}

interface Campaign {
  id: string
  name: string
  game: string
  image_url: string
  link_url: string
  linked: boolean
  needs_link: boolean
  eligible: boolean
  status: CampaignStatus
  starts_at: string
  ends_at: string
  progress: number
  claimed: number
  total: number
  drops: Drop[]
}

interface ProgressView {
  drop_rewards: string
  drop_progress: number
  drop_current: number
  drop_required: number
  campaign_name: string
  game: string
  campaign_progress: number
  campaign_claimed: number
  campaign_total: number
  campaign_remaining_minutes: number
  countdown: boolean
}

interface Settings {
  proxy: string
  priority: string[]
  exclude: string[]
  priority_mode: PriorityMode
  connection_quality: number
  enable_badges_emotes: boolean
  available_drops_check: boolean
  theme: "system" | "light" | "dark"
  language: string
  tray_notifications: boolean
  autostart_tray: boolean
  notify_url: string
  check_updates: boolean
  active_hours: [number, number] | null
}

type MinerEvent =
  | { kind: "Print"; data: string }
  | { kind: "Log"; data: LogLine }
  | { kind: "UpdateAvailable"; data: { version: string; url: string } }
  | { kind: "Status"; data: string }
  | { kind: "LoginCode"; data: { url: string; code: string } }
  | { kind: "LoggedIn"; data: { user_id: number } }
  | { kind: "LoginRequired" }
  | { kind: "DropClaimed"; data: { ts: string; game: string; campaign: string; drop: string; rewards: string; benefit_ids: string[] } }
  | { kind: "AllDone" }
  | { kind: "UnlinkedCampaign"; data: { game: string; campaign: string; link_url: string } }
  | { kind: "Paused"; data: boolean }
  | { kind: "Websocket"; data: { idx: number; status: string; topics: number } }
  | { kind: "Channels"; data: Channel[] }
  | { kind: "Watching"; data: number | null }
  | { kind: "Inventory"; data: Campaign[] }
  | { kind: "Progress"; data: ProgressView | null }
  | { kind: "Exited" }

/* ---------- constants / helpers ---------- */

const HOURS = Array.from({ length: 24 }, (_, h) => `${String(h).padStart(2, "0")}:00`)

function HourSelect({ value, onChange }: { value: number; onChange: (h: number) => void }) {
  return (
    <Select items={Object.fromEntries(HOURS.map((l, h) => [String(h), l]))} value={String(value)} onValueChange={(v) => onChange(Number(v))}>
      <SelectTrigger className="w-24"><SelectValue /></SelectTrigger>
      <SelectContent>
        {HOURS.map((l, h) => <SelectItem key={h} value={String(h)}>{l}</SelectItem>)}
      </SelectContent>
    </Select>
  )
}

const WS_MAX_TOPICS = 50
const WS_SLOTS = 8
const MODES: [PriorityMode, string][] = [
  ["priority_only", "Priority list only"],
  ["ending_soonest", "Ending soonest first"],
  ["low_avbl_first", "Low availability first"],
]
const FILTERS: [keyof Filters, string][] = [
  ["notlinked", "Not linked"],
  ["upcoming", "Upcoming"],
  ["expired", "Expired"],
  ["excluded", "Excluded"],
  ["finished", "Finished"],
]
const OK = "text-emerald-600 dark:text-emerald-400"
const WARN = "text-amber-600 dark:text-amber-400"
const BAD = "text-red-600 dark:text-red-400"
const DIM = "text-muted-foreground"
const STATUS_CLS: Record<ChannelStatus, string> = { Online: OK, Pending: WARN, Offline: DIM }
const CAMP_STATUS: Record<string, [string, string]> = {
  Active_true: ["Active", OK],
  Active_false: ["Active, not linked", BAD],
  Upcoming: ["Upcoming", WARN],
  Expired: ["Expired", DIM],
}

interface LogLine { ts: string; level: string; msg: string }

const LOG_LEVELS = ["ERROR", "WARN", "INFO"] as const
const LOG_MAX = 2000
type Sort = "ending" | "game" | "progress"
const SORTS: [Sort, string][] = [["ending", "Ending soonest"], ["game", "Game name"], ["progress", "Progress"]]

/** localStorage-backed state; falls back to `init` when storage is unavailable or holds junk. */
function useStored<T extends object>(key: string, init: T) {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(key)
      return raw ? { ...init, ...JSON.parse(raw) } : init
    } catch {
      return init
    }
  })
  useEffect(() => {
    try { localStorage.setItem(key, JSON.stringify(value)) } catch { /* private mode */ }
  }, [key, value])
  return [value, setValue] as const
}

/** "3d 4h", "2h 5m", "12m" until `iso`, or "ended". */
const until = (iso: string) => {
  const m = Math.floor((new Date(iso).getTime() - Date.now()) / 60000)
  if (m <= 0) return "ended"
  const d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60)
  return d ? `${d}d ${h}h` : h ? `${h}h ${m % 60}m` : `${m}m`
}

interface Filters {
  notlinked: boolean
  upcoming: boolean
  expired: boolean
  excluded: boolean
  finished: boolean
}

const esc = (s: unknown) => String(s ?? "")
const fmt = (m: number, s: number) =>
  `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${String(s).padStart(2, "0")}`
const dt = (s: string) => new Date(s).toLocaleString()
const pct = (v: number) => `${(Math.max(0, Math.min(1, v)) * 100).toFixed(1)}%`

function Tick({ className }: { className?: string }) {
  return <HugeiconsIcon icon={Tick02Icon} strokeWidth={2} className={cn("inline size-3.5 align-[-2px]", className)} />
}

/** Colored dot + text, used for every status indicator so they all read the same. */
function Dot({ className }: { className?: string }) {
  return <span className={cn("inline-block size-1.5 shrink-0 rounded-full bg-current", className)} />
}

function ProgressBlock({ label, title, value, detail, remaining }: {
  label: string; title: string; value: number; detail: string; remaining: string
}) {
  return (
    <div>
      <div className="flex items-baseline justify-between gap-3">
        <div className="min-w-0">
          <div className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{label}</div>
          <div className="truncate font-medium" title={title}>{title}</div>
        </div>
        <span className="shrink-0 text-sm font-medium tabular-nums">{pct(value)}</span>
      </div>
      <Progress value={value * 100} className="mt-2" />
      <div className="mt-1.5 flex justify-between text-xs text-muted-foreground tabular-nums">
        <span>{detail}</span>
        <span>{remaining} remaining</span>
      </div>
    </div>
  )
}

/** Selectable list used by the Priority and Exclude settings. */
function GameList({ items, selected, onSelect, numbered }: {
  items: string[]; selected: number | null; onSelect: (i: number) => void; numbered?: boolean
}) {
  return (
    <div className="flex h-56 flex-col gap-0.5 overflow-y-auto rounded-lg border bg-muted/20 p-1">
      {items.map((g, i) => (
        <button
          key={g}
          type="button"
          className={cn(
            "flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm whitespace-nowrap outline-none hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring/50",
            selected === i && "bg-accent hover:bg-accent",
          )}
          onClick={() => onSelect(i)}
        >
          {numbered && <span className="w-4 text-xs text-muted-foreground tabular-nums">{i + 1}</span>}
          <span className="truncate">{g}</span>
        </button>
      ))}
      {items.length === 0 && (
        <div className="flex flex-1 items-center justify-center text-xs text-muted-foreground">No games yet</div>
      )}
    </div>
  )
}

/* ---------- app ---------- */

export default function App() {
  const [tab, setTab] = useState("main")
  const [status, setStatus] = useState("Idle")
  const [login, setLogin] = useState<{ url: string; code: string } | null>(null)
  const [userId, setUserId] = useState<number | null>(null)
  const [consoleLines, setConsoleLines] = useState<string[]>([])
  const [channels, setChannels] = useState<Channel[]>([])
  const [watching, setWatching] = useState<number | null>(null)
  const [selected, setSelected] = useState<number | null>(null)
  const [inventory, setInventory] = useState<Campaign[]>([])
  const [progress, setProgress] = useState<ProgressView | null>(null)
  const [progressAt, setProgressAt] = useState(0)
  const [websockets, setWebsockets] = useState<Record<number, { status: string; topics: number }>>({})
  const [settings, setSettings] = useState<Settings | null>(null)
  const [saved, setSaved] = useState("")
  const [prioSel, setPrioSel] = useState<number | null>(null)
  const [exclSel, setExclSel] = useState<string | null>(null)
  const [prioInput, setPrioInput] = useState("")
  const [exclInput, setExclInput] = useState("")
  const [tdmPath, setTdmPath] = useState("")
  const [tdmMsg, setTdmMsg] = useState("")
  const [autostart, setAutostart] = useState<boolean | null>(null)
  const [languages, setLanguages] = useState<string[]>([])
  const [paused, setPaused] = useState(false)
  const [filters, setFilters] = useStored<Filters>("inventory-filters", {
    notlinked: true, upcoming: true, expired: true, excluded: false, finished: true,
  })
  const [inv, setInv] = useStored<{ search: string; sort: Sort }>("inventory-view", { search: "", sort: "ending" })
  const [logs, setLogs] = useState<LogLine[]>([])
  const [logView, setLogView] = useStored<{ level: string; search: string }>("log-view", { level: "INFO", search: "" })
  const [release, setRelease] = useState<{ version: string; url: string } | null>(null)
  const [, setTick] = useState(0)
  const consoleRef = useRef<HTMLPreElement>(null)
  const stickRef = useRef(true)
  const selectedRef = useRef<number | null>(null)
  selectedRef.current = selected

  const addLog = (line: LogLine) =>
    setLogs((l) => (l.length >= LOG_MAX ? [...l.slice(l.length - LOG_MAX + 1), line] : [...l, line]))

  const handle = (ev: MinerEvent) => {
    switch (ev.kind) {
      case "Log": addLog(ev.data); break
      case "UpdateAvailable": setRelease(ev.data); break
      case "Print":
        addLog({ ts: new Date().toISOString(), level: "INFO", msg: ev.data })
        setConsoleLines((l) => {
          const next = [...l, `${new Date().toLocaleTimeString()} ${ev.data}`]
          return next.length > 1000 ? next.slice(next.length - 1000) : next
        })
        break
      case "Status": setStatus(ev.data); break
      case "LoginCode": setLogin(ev.data); break
      case "LoggedIn": setUserId(ev.data.user_id); setLogin(null); break
      case "Websocket":
        setWebsockets((w) => ({ ...w, [ev.data.idx]: { status: ev.data.status, topics: ev.data.topics } }))
        break
      case "Channels":
        setChannels(ev.data)
        setSelected((sel) => (sel != null && ev.data.some((c) => c.id === sel) ? sel : null))
        break
      case "Watching": setWatching(ev.data); break
      case "Inventory": setInventory(ev.data); break
      case "Progress": setProgress(ev.data); setProgressAt(Date.now()); break
      case "Paused": setPaused(ev.data); break
      case "Exited": setStatus("Exited"); break
    }
  }

  useEffect(() => {
    invoke<Settings>("get_settings").then((s) => {
      setSettings(s)
      setSaved(JSON.stringify(s))
    })
    invoke<boolean>("get_autostart").then(setAutostart).catch(() => setAutostart(null))
    invoke<string[]>("list_languages").then(setLanguages).catch(() => setLanguages([]))
    invoke<{ kind: "Log"; data: LogLine }[]>("get_log").then((l) => setLogs((cur) => [...(l ?? []).map((e) => e.data), ...cur])).catch(() => {})
    let un: (() => void) | undefined
    let unnav: (() => void) | undefined
    ;(async () => {
      un = await window.__TAURI__.event.listen("miner-event", (e) => handle(e.payload as MinerEvent))
      unnav = await window.__TAURI__.event.listen("navigate", (e) => {
        const next = e.payload as string
        if (["main", "inventory", "log", "settings", "help"].includes(next)) setTab(next)
      })
      await invoke("frontend_ready")
    })()
    return () => {
      un?.()
      unnav?.()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 1000)
    return () => clearInterval(t)
  }, [])

  useEffect(() => {
    const theme = settings?.theme ?? "system"
    const mq = window.matchMedia("(prefers-color-scheme: dark)")
    const apply = () =>
      document.documentElement.classList.toggle("dark", theme === "dark" || (theme === "system" && mq.matches))
    apply()
    if (theme !== "system") return
    mq.addEventListener("change", apply)
    return () => mq.removeEventListener("change", apply)
  }, [settings?.theme])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && selectedRef.current != null) {
        setSelected(null)
        invoke("select", { id: null })
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  useEffect(() => {
    const el = consoleRef.current
    if (el && stickRef.current) el.scrollTop = el.scrollHeight
  }, [consoleLines])

  const dirty = settings != null && JSON.stringify(settings) !== saved
  const update = (patch: Partial<Settings>) => setSettings((s) => (s ? { ...s, ...patch } : s))
  const saveSettings = () => {
    if (!settings) return
    invoke("save_settings", { settings })
    setSaved(JSON.stringify(settings))
  }
  const importTdm = () => {
    setTdmMsg("")
    invoke<Settings>("import_tdm", { path: tdmPath })
      .then((s) => { setSettings(s); setTdmMsg("Imported. Review, then Save.") })
      .catch((e) => setTdmMsg(String(e)))
  }
  const toggleAutostart = (on: boolean) => {
    setAutostart(on)
    invoke("set_autostart", { enabled: on })
      .then(() => setAutostart(on))
      .catch((e) => { setAutostart(!on); setTdmMsg(String(e)) })
  }
  const games = [...new Set(inventory.map((c) => c.game))].sort()

  /* progress countdown derivation */
  let dropMin = 0, dropSecs = 0, campMin = 0, campSecs = 0
  if (progress) {
    const elapsed = (Date.now() - progressAt) / 1000
    ;[dropMin, dropSecs] = countdownParts(progress.drop_required - progress.drop_current, elapsed, progress.countdown)
    ;[campMin, campSecs] = countdownParts(progress.campaign_remaining_minutes, elapsed, progress.countdown)
  }

  /* inventory filter */
  const visibleCampaigns = inventory.filter((c) => {
    const excluded = settings ? settings.exclude.includes(c.game) : false
    const finished = c.total > 0 && c.claimed >= c.total
    if (finished && !filters.finished) return false
    if (c.status === "Upcoming" && !filters.upcoming) return false
    if (c.status === "Expired" && !filters.expired) return false
    if (excluded && !filters.excluded) return false
    if (!c.eligible && !filters.notlinked) return false
    const q = inv.search.trim().toLowerCase()
    if (q && !c.game.toLowerCase().includes(q) && !c.name.toLowerCase().includes(q)) return false
    return true
  }).sort((a, b) =>
    inv.sort === "game" ? a.game.localeCompare(b.game)
    : inv.sort === "progress" ? b.progress - a.progress
    : new Date(a.ends_at).getTime() - new Date(b.ends_at).getTime(),
  )

  /* log filter: the selected level and everything more severe */
  const logRank = (lvl: string) => { const i = LOG_LEVELS.indexOf(lvl as typeof LOG_LEVELS[number]); return i < 0 ? LOG_LEVELS.length : i }
  const logQ = logView.search.trim().toLowerCase()
  const visibleLogs = logs.filter((l) => logRank(l.level) <= logRank(logView.level) && (!logQ || l.msg.toLowerCase().includes(logQ)))
  const logText = visibleLogs.map((l) => `${new Date(l.ts).toLocaleTimeString()} ${l.level.padEnd(5)} ${l.msg}`).join("\n")

  const statusCls = status.startsWith("Watching") ? OK : status === "Idle" || status === "Exited" ? DIM : WARN
  const wsSlots = Array.from({ length: WS_SLOTS }, (_, i) => i)
    .concat(Object.keys(websockets).map(Number).filter((i) => i >= WS_SLOTS).sort((a, b) => a - b))
  const wsLive = Object.values(websockets).filter((w) => w.status === "Connected")
  const wsTopics = wsLive.reduce((n, w) => n + w.topics, 0)

  return (
    <Tabs value={tab} onValueChange={(v) => setTab(v as string)} className="app-shell h-screen gap-0 bg-background">
      <header className="app-header flex h-14 shrink-0 items-center gap-4 border-b px-4">
        <div className="flex min-w-0 items-center gap-3">
          <img src={logo} alt="Rustpick" className="size-8 shrink-0" />
          <span className="h-4 w-px bg-border" />
          <span className={cn("flex min-w-0 items-center gap-2 rounded-full bg-card/70 px-2.5 py-1 text-xs font-medium ring-1 ring-foreground/10", statusCls)} title={esc(status)}>
            <Dot />
            <span className="max-w-40 truncate text-foreground/80">{esc(status)}</span>
          </span>
        </div>
        <TabsList className="ml-auto bg-card/70 shadow-sm ring-1 ring-foreground/10 backdrop-blur">
          <TabsTrigger value="main" className="px-3">Main</TabsTrigger>
          <TabsTrigger value="inventory" className="px-3">Inventory</TabsTrigger>
          <TabsTrigger value="log" className="px-3">Log</TabsTrigger>
          <TabsTrigger value="settings" className="px-3">Settings</TabsTrigger>
          <TabsTrigger value="help" className="px-3">Help</TabsTrigger>
        </TabsList>
      </header>
      {release && (
        <div className="update-banner flex shrink-0 items-center gap-3 border-b px-4 py-2 text-sm">
          <span className="flex size-6 items-center justify-center rounded-md bg-primary/15 text-primary" aria-hidden="true">
            <HugeiconsIcon icon={GemIcon} size={14} strokeWidth={2} />
          </span>
          <span><strong>Rustpick {release.version}</strong> is available.</span>
          <a className="cursor-pointer font-medium text-primary hover:underline" onClick={() => invoke("open_url", { url: release.url })}>
            Release notes
          </a>
          <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setRelease(null)}>Dismiss</Button>
        </div>
      )}
      <datalist id="games-dl">
        {games.map((g) => <option key={g} value={g} />)}
      </datalist>

      {/* ===== MAIN ===== */}
      <TabsContent value="main" className="flex min-h-0 flex-col gap-3 overflow-y-auto p-3">
        <div className="grid gap-3 md:grid-cols-2">
          <div className="flex flex-col gap-3">
            <Card size="sm">
              <CardHeader>
                <CardTitle>Account</CardTitle>
                <CardAction>
                  <Badge variant="outline" className={cn("gap-1.5", userId != null ? OK : login ? WARN : DIM)}>
                    <Dot />
                    {userId != null ? "Logged in" : login ? "Login required" : "Logged out"}
                  </Badge>
                </CardAction>
                <CardDescription>
                  {userId != null ? <>User ID <span className="font-mono text-foreground">{userId}</span></> : "Twitch device login"}
                </CardDescription>
              </CardHeader>
              {login && (
                <CardContent className="flex flex-col gap-2.5">
                  <p className="text-sm text-muted-foreground">
                    Open{" "}
                    <a className="cursor-pointer font-medium text-primary hover:underline" onClick={() => invoke("open_url", { url: login.url })}>
                      {login.url.replace(/^https?:\/\/(www\.)?/, "")}
                    </a>{" "}
                    and enter this code:
                  </p>
                  <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-muted/40 px-3 py-2">
                    <span className="font-mono text-2xl font-medium tracking-[0.2em]">{login.code}</span>
                    <div className="flex gap-2">
                      <Button variant="outline" size="sm" onClick={() => invoke("copy_text", { text: login.code })}>
                        <HugeiconsIcon icon={Copy01Icon} size={14} strokeWidth={2} data-icon="inline-start" />
                        Copy
                      </Button>
                      <Button size="sm" onClick={() => invoke("open_url", { url: login.url })}>
                        <HugeiconsIcon icon={LinkSquare02Icon} size={14} strokeWidth={2} data-icon="inline-start" />
                        Open Twitch
                      </Button>
                    </div>
                  </div>
                </CardContent>
              )}
            </Card>

            <Card size="sm" className="mining-card">
              <CardHeader>
                <CardTitle>Mining progress</CardTitle>
                <CardAction>
                  <Button size="sm" variant="outline" onClick={() => invoke("set_paused", { paused: !paused })}>
                    {paused ? "Resume" : "Pause"}
                  </Button>
                </CardAction>
                {paused && <CardDescription>Paused: no watch time is being sent.</CardDescription>}
              </CardHeader>
              <CardContent>
                {progress ? (
                  <div className="flex flex-col gap-4">
                    <ProgressBlock
                      label={progress.game}
                      title={progress.campaign_name}
                      value={progress.campaign_progress}
                      detail={`${progress.campaign_claimed}/${progress.campaign_total} drops claimed`}
                      remaining={fmt(campMin, campSecs)}
                    />
                    <ProgressBlock
                      label="Current drop"
                      title={progress.drop_rewards}
                      value={progress.drop_progress}
                      detail={`${progress.drop_current}/${progress.drop_required} min`}
                      remaining={fmt(dropMin, dropSecs)}
                    />
                  </div>
                ) : (
                  <p className="text-sm text-muted-foreground">Nothing is being mined yet.</p>
                )}
              </CardContent>
            </Card>

            <Card size="sm">
              <CardHeader>
                <CardTitle>Websockets</CardTitle>
                <CardDescription>
                  {wsLive.length} connected · {wsTopics} topics
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-wrap gap-1.5">
                {wsSlots.map((i) => {
                  const w = websockets[i]
                  return (
                    <span
                      key={i}
                      title={w ? `Websocket #${i + 1}: ${w.status}` : `Websocket #${i + 1}: unused`}
                      className={cn(
                        "inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 font-mono text-xs tabular-nums",
                        !w && "border-dashed text-muted-foreground/60",
                      )}
                    >
                      <Dot className={w ? (w.status === "Connected" ? "bg-emerald-500" : "bg-amber-500") : "bg-border"} />
                      {i + 1}
                      {w && <span className="text-muted-foreground">{w.topics}/{WS_MAX_TOPICS}</span>}
                    </span>
                  )
                })}
              </CardContent>
            </Card>
          </div>

          <Card size="sm" className="self-start">
            <CardHeader>
              <CardTitle>Channels</CardTitle>
              <CardAction>
                <Button size="sm" disabled={selected == null} onClick={() => selected != null && invoke("select", { id: selected })}>
                  Switch
                </Button>
              </CardAction>
              <CardDescription>Select a channel and press Switch. Esc clears the selection.</CardDescription>
            </CardHeader>
            <CardContent className="px-0">
              <div className="max-h-[420px] overflow-y-auto border-t">
                <Table>
                  <TableHeader className="sticky top-0 z-10 bg-card shadow-[inset_0_-1px_0_var(--border)]">
                    <TableRow className="hover:bg-transparent [&>th]:h-9 [&>th]:text-xs [&>th]:text-muted-foreground">
                      <TableHead className="pl-4">Channel</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead>Game</TableHead>
                      <TableHead className="text-center" title="Drops enabled"><HugeiconsIcon icon={GiftIcon} size={14} strokeWidth={2} className="inline" /></TableHead>
                      <TableHead className="text-right">Viewers</TableHead>
                      <TableHead className="pr-4 text-center" title="Allow-list based"><HugeiconsIcon icon={LockIcon} size={14} strokeWidth={2} className="inline" /></TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {channels.map((c) => (
                      <TableRow
                        key={c.id}
                        aria-selected={selected === c.id}
                        className={cn("cursor-pointer", selected === c.id && "bg-accent hover:bg-accent")}
                        onClick={() => setSelected((sel) => (sel === c.id ? null : c.id))}
                      >
                        <TableCell className={cn("pl-4 font-medium", watching === c.id && OK)} title={watching === c.id ? "Currently watching" : undefined}>
                          {c.name}
                        </TableCell>
                        <TableCell className={cn("text-xs", STATUS_CLS[c.status])}>
                          <span className="inline-flex items-center gap-1.5"><Dot />{c.status}</span>
                        </TableCell>
                        <TableCell className={cn(!c.game && "text-muted-foreground")}>{c.game ?? "–"}</TableCell>
                        <TableCell className="text-center">{c.drops_enabled && <Tick className="text-muted-foreground" />}</TableCell>
                        <TableCell className="text-right text-muted-foreground tabular-nums">{c.viewers?.toLocaleString() ?? ""}</TableCell>
                        <TableCell className="pr-4 text-center">{c.acl_based && <Tick className="text-muted-foreground" />}</TableCell>
                      </TableRow>
                    ))}
                    {channels.length === 0 && (
                      <TableRow className="hover:bg-transparent">
                        <TableCell colSpan={6} className="py-8 text-center text-muted-foreground">No channels yet.</TableCell>
                      </TableRow>
                    )}
                  </TableBody>
                </Table>
              </div>
            </CardContent>
          </Card>
        </div>

        <Card size="sm" className="flex min-h-48 flex-1 flex-col">
          <CardHeader><CardTitle>Output</CardTitle></CardHeader>
          <CardContent className="flex min-h-0 flex-1 flex-col">
            <pre
              ref={consoleRef}
              onScroll={(e) => {
                const el = e.currentTarget
                stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 30
              }}
              className="min-h-0 flex-1 overflow-y-auto rounded-lg border bg-muted/30 px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap"
            >
              {consoleLines.length ? consoleLines.join("\n") : <span className="text-muted-foreground">Waiting for output…</span>}
            </pre>
          </CardContent>
        </Card>
      </TabsContent>

      {/* ===== INVENTORY ===== */}
      <TabsContent value="inventory" className="flex min-h-0 flex-col gap-3 overflow-y-auto p-3">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-xl bg-card px-3 py-2 ring-1 ring-foreground/10">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="text-sm text-muted-foreground">Show</span>
            {FILTERS.map(([key, label]) => (
              <label key={key} className="flex cursor-pointer items-center gap-1.5 text-sm">
                <Checkbox
                  checked={filters[key]}
                  onCheckedChange={(checked) => setFilters((f) => ({ ...f, [key]: !!checked }))}
                />
                {label}
              </label>
            ))}
          </div>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Input
              className="h-8 w-44"
              placeholder="Search game or campaign"
              value={inv.search}
              onChange={(e) => setInv((v) => ({ ...v, search: e.target.value }))}
            />
            <Select items={Object.fromEntries(SORTS)} value={inv.sort} onValueChange={(v) => setInv((x) => ({ ...x, sort: v as Sort }))}>
              <SelectTrigger className="w-40"><SelectValue /></SelectTrigger>
              <SelectContent>
                {SORTS.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}
              </SelectContent>
            </Select>
            <Button size="sm" variant="outline" onClick={() => invoke("reload")}>
              <HugeiconsIcon icon={RefreshIcon} size={14} strokeWidth={2} data-icon="inline-start" />
              Refresh
            </Button>
          </div>
        </div>
        {visibleCampaigns.map((c) => {
          const [label, cls] = CAMP_STATUS[c.status === "Active" ? `Active_${c.eligible}` : c.status] ?? ["", DIM]
          return (
            <Card key={c.id} size="sm" className="shrink-0">
              <CardContent className="flex flex-col gap-4 md:flex-row">
                <div className="flex w-full shrink-0 gap-3 md:w-80">
                  <div className="flex h-24 w-[72px] shrink-0 items-center justify-center overflow-hidden rounded-md bg-muted text-muted-foreground/50">
                    {c.image_url ? (
                      <img src={c.image_url} alt="" className="size-full object-cover" onError={(e) => (e.currentTarget.style.display = "none")} />
                    ) : (
                      <HugeiconsIcon icon={Image01Icon} size={20} strokeWidth={1.5} />
                    )}
                  </div>
                  <div className="flex min-w-0 flex-1 flex-col">
                    <div className="truncate font-medium" title={c.name}>{c.name}</div>
                    <div className="truncate text-xs text-muted-foreground">{c.game}</div>
                    <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
                      <span className={cn("inline-flex items-center gap-1.5 font-medium", cls)}><Dot />{label}</span>
                      {(c.linked || c.link_url) && <span className="text-muted-foreground">·</span>}
                      {c.linked ? (
                        <span className="text-muted-foreground">Linked</span>
                      ) : c.link_url ? (
                        <a className="cursor-pointer font-medium text-primary hover:underline" onClick={() => invoke("open_url", { url: c.link_url })}>
                          Link account
                        </a>
                      ) : null}
                    </div>
                    <div className="mt-0.5 text-xs text-muted-foreground">
                      {c.status === "Upcoming"
                        ? `Starts ${dt(c.starts_at)} (in ${until(c.starts_at)})`
                        : c.status === "Expired" ? `Ended ${dt(c.ends_at)}` : `Ends in ${until(c.ends_at)} (${dt(c.ends_at)})`}
                    </div>
                    <div className="mt-2 flex items-center gap-2">
                      <Progress value={c.progress * 100} className="flex-1" />
                      <span className="text-xs text-muted-foreground tabular-nums">{c.claimed}/{c.total}</span>
                    </div>
                  </div>
                </div>
                <div className="flex flex-1 flex-wrap content-start gap-2">
                  {c.drops.map((d) => (
                    <div
                      key={d.id}
                      title={`${d.name}: ${d.rewards}`}
                      className={cn(
                        "flex h-24 w-[132px] flex-col justify-between gap-2 rounded-lg border bg-muted/30 p-2.5 text-xs",
                        d.is_claimed && "border-emerald-500/30 bg-emerald-500/5",
                      )}
                    >
                      <div className="line-clamp-2 leading-snug font-medium">{d.rewards || d.name}</div>
                      {d.is_claimed ? (
                        <span className={cn("inline-flex items-center gap-1 font-medium", OK)}><Tick /> Claimed</span>
                      ) : (
                        <div>
                          <Progress value={d.progress * 100} />
                          <div className="mt-1.5 text-muted-foreground tabular-nums">{d.current_minutes}/{d.required_minutes} min</div>
                        </div>
                      )}
                    </div>
                  ))}
                  {c.drops.length === 0 && <span className="self-center text-sm text-muted-foreground">No drops.</span>}
                </div>
              </CardContent>
            </Card>
          )
        })}
        {visibleCampaigns.length === 0 && (
          <div className="flex flex-1 items-center justify-center py-16 text-sm text-muted-foreground">
            {inventory.length === 0 ? "No campaigns loaded yet." : "No campaigns match the current filters."}
          </div>
        )}
      </TabsContent>

      {/* ===== SETTINGS ===== */}
      <TabsContent value="settings" className="flex min-h-0 flex-col overflow-y-auto">
        <div className="grid gap-3 p-3 md:grid-cols-2 xl:grid-cols-[1.2fr_1fr_1fr]">
          <div className="flex flex-col gap-3 md:col-span-2 xl:col-span-1">
            <Card size="sm">
              <CardHeader><CardTitle>General</CardTitle></CardHeader>
              <CardContent className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-3 text-sm">
                <label htmlFor="s-theme">Theme</label>
                <Select
                  value={settings?.theme ?? "system"}
                  onValueChange={(v) => update({ theme: v as Settings["theme"] })}
                >
                  <SelectTrigger id="s-theme" className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="system">System</SelectItem>
                    <SelectItem value="light">Light</SelectItem>
                    <SelectItem value="dark">Dark</SelectItem>
                  </SelectContent>
                </Select>
                <label htmlFor="s-autostart">Autostart</label>
                <Checkbox
                  id="s-autostart"
                  checked={!!autostart}
                  onCheckedChange={(checked) => toggleAutostart(!!checked)}
                  disabled={autostart == null}
                />
                <label htmlFor="s-autostart-tray">Autostart in tray</label>
                <Checkbox
                  id="s-autostart-tray"
                  checked={!!settings?.autostart_tray}
                  onCheckedChange={(checked) => update({ autostart_tray: !!checked })}
                />
                <label htmlFor="s-notify">Drop notifications</label>
                <Checkbox
                  id="s-notify"
                  checked={!!settings?.tray_notifications}
                  onCheckedChange={(checked) => update({ tray_notifications: !!checked })}
                />
                <label>Language</label>
                <Select
                  value={settings?.language ?? "English"}
                  onValueChange={(v) => update({ language: v as string })}
                >
                  <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {languages.map((l) => <SelectItem key={l} value={l}>{l}</SelectItem>)}
                  </SelectContent>
                </Select>
                <label>Priority mode</label>
                <Select
                  items={Object.fromEntries(MODES)}
                  value={settings?.priority_mode}
                  onValueChange={(v) => update({ priority_mode: v as PriorityMode })}
                >
                  <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {MODES.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}
                  </SelectContent>
                </Select>
                <label htmlFor="s-proxy">Proxy</label>
                <Input
                  id="s-proxy"
                  placeholder="http://host:port (needs restart)"
                  value={settings?.proxy ?? ""}
                  onChange={(e) => update({ proxy: e.target.value })}
                />
                <label htmlFor="s-updates">Check for updates</label>
                <Checkbox
                  id="s-updates"
                  checked={!!settings?.check_updates}
                  onCheckedChange={(checked) => update({ check_updates: !!checked })}
                />
                <label htmlFor="s-notify-url">Webhook URL</label>
                <Input
                  id="s-notify-url"
                  placeholder="Discord/Slack/Gotify webhook (claims, login, done)"
                  value={settings?.notify_url ?? ""}
                  onChange={(e) => update({ notify_url: e.target.value })}
                />
                <label htmlFor="s-hours">Active hours</label>
                <div className="flex items-center gap-2">
                  <Checkbox
                    id="s-hours"
                    checked={settings?.active_hours != null}
                    onCheckedChange={(checked) => update({ active_hours: checked ? [22, 6] : null })}
                  />
                  {settings?.active_hours != null && (
                    <>
                      <HourSelect value={settings.active_hours[0]} onChange={(h) => update({ active_hours: [h, settings.active_hours![1]] })} />
                      <span className="text-muted-foreground">to</span>
                      <HourSelect value={settings.active_hours[1]} onChange={(h) => update({ active_hours: [settings.active_hours![0], h] })} />
                    </>
                  )}
                </div>
                <label htmlFor="s-quality">Connection quality</label>
                <div className="flex items-center gap-3">
                  <Slider
                    id="s-quality"
                    min={1}
                    max={6}
                    step={1}
                    value={[settings?.connection_quality ?? 3]}
                    onValueChange={(v) => update({ connection_quality: (v as number[])[0] })}
                    className="flex-1"
                  />
                  <span className="w-6 text-right text-xs text-muted-foreground tabular-nums">
                    {settings?.connection_quality}×
                  </span>
                </div>
              </CardContent>
            </Card>
            <Card size="sm">
              <CardHeader>
                <CardTitle>Advanced</CardTitle>
                <CardDescription className={WARN}>
                  These options can make the miner misbehave. Turn them off if you hit issues.
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-2.5 text-sm">
                <label className="flex cursor-pointer items-center gap-2.5">
                  <Checkbox
                    checked={!!settings?.enable_badges_emotes}
                    onCheckedChange={(checked) => update({ enable_badges_emotes: !!checked })}
                  />
                  Partial support for badges and emotes
                </label>
                <label className="flex cursor-pointer items-center gap-2.5">
                  <Checkbox
                    checked={!!settings?.available_drops_check}
                    onCheckedChange={(checked) => update({ available_drops_check: !!checked })}
                  />
                  Extra available-drops check
                </label>
              </CardContent>
            </Card>
            <Card size="sm">
              <CardHeader>
                <CardTitle>Import from TwitchDropsMiner</CardTitle>
                <CardDescription>
                  Paste the path to TDM&apos;s <code>settings.json</code> (or its folder). Loads its
                  values into the form; Save to keep them.
                </CardDescription>
              </CardHeader>
              <CardContent className="flex flex-col gap-2 text-sm">
                <div className="flex gap-2">
                  <Input
                    placeholder="/path/to/TwitchDropsMiner"
                    value={tdmPath}
                    onChange={(e) => setTdmPath(e.target.value)}
                  />
                  <Button size="sm" variant="outline" disabled={!tdmPath} onClick={importTdm}>Import</Button>
                </div>
                {tdmMsg && <span className="text-xs text-muted-foreground">{tdmMsg}</span>}
              </CardContent>
            </Card>
          </div>

          <Card size="sm">
            <CardHeader>
              <CardTitle>Priority</CardTitle>
              <CardDescription>Games to mine first, in order.</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-col gap-2">
              <div className="flex gap-2">
                <Input
                  list="games-dl"
                  placeholder="Add a game…"
                  value={prioInput}
                  onChange={(e) => setPrioInput(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") addPrio() }}
                />
                <Button variant="outline" size="icon" title="Add" onClick={addPrio}>
                  <HugeiconsIcon icon={Add01Icon} size={16} strokeWidth={2} />
                </Button>
              </div>
              <div className="grid grid-cols-[1fr_auto] gap-2">
                <GameList numbered items={settings?.priority ?? []} selected={prioSel} onSelect={setPrioSel} />
                <div className="flex flex-col gap-1">
                  {([["top", ArrowUpDoubleIcon, "Move to top"], ["up", ArrowUp02Icon, "Move up"],
                    ["down", ArrowDown02Icon, "Move down"], ["bottom", ArrowDownDoubleIcon, "Move to bottom"]] as const).map(
                    ([op, icon, title]) => (
                      <Button key={op} variant="ghost" size="icon-sm" title={title} disabled={prioSel == null} onClick={() => movePrio(op)}>
                        <HugeiconsIcon icon={icon} size={14} strokeWidth={2} />
                      </Button>
                    ),
                  )}
                  <Button variant="ghost" size="icon-sm" title="Remove" disabled={prioSel == null} className="mt-auto text-destructive hover:text-destructive" onClick={delPrio}>
                    <HugeiconsIcon icon={Delete02Icon} size={14} strokeWidth={2} />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card size="sm">
            <CardHeader>
              <CardTitle>Exclude</CardTitle>
              <CardDescription>Games that are never mined.</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-col gap-2">
              <div className="flex gap-2">
                <Input
                  list="games-dl"
                  placeholder="Add a game…"
                  value={exclInput}
                  onChange={(e) => setExclInput(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") addExcl() }}
                />
                <Button variant="outline" size="icon" title="Add" onClick={addExcl}>
                  <HugeiconsIcon icon={Add01Icon} size={16} strokeWidth={2} />
                </Button>
              </div>
              <div className="grid grid-cols-[1fr_auto] gap-2">
                <GameList
                  items={settings?.exclude ?? []}
                  selected={exclSel == null ? null : (settings?.exclude.indexOf(exclSel) ?? -1)}
                  onSelect={(i) => setExclSel(settings?.exclude[i] ?? null)}
                />
                <div className="flex flex-col">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    title="Remove"
                    disabled={exclSel == null}
                    className="mt-auto text-destructive hover:text-destructive"
                    onClick={() => {
                      if (!settings || exclSel == null) return
                      update({ exclude: settings.exclude.filter((x) => x !== exclSel) })
                      setExclSel(null)
                    }}
                  >
                    <HugeiconsIcon icon={Delete02Icon} size={14} strokeWidth={2} />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
        <div className="sticky bottom-0 mt-auto flex items-center gap-2 border-t bg-background/95 px-3 py-2 backdrop-blur">
          <span className="mr-auto text-xs text-muted-foreground">Most changes take effect after a reload.</span>
          {dirty && (
            <Badge variant="outline" className={cn("border-amber-500/40", WARN)}>Unsaved changes</Badge>
          )}
          <Button variant="outline" size="sm" disabled={!dirty} onClick={saveSettings}>Save</Button>
          <Button size="sm" onClick={() => { saveSettings(); invoke("reload") }}>Save &amp; Reload</Button>
        </div>
      </TabsContent>

      {/* ===== LOG ===== */}
      <TabsContent value="log" className="flex min-h-0 flex-col gap-3 p-3">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl bg-card px-3 py-2 ring-1 ring-foreground/10">
          <span className="text-sm text-muted-foreground">Level</span>
          <Select
            items={{ ERROR: "Errors", WARN: "Warnings and errors", INFO: "Everything" }}
            value={logView.level}
            onValueChange={(v) => setLogView((x) => ({ ...x, level: v as string }))}
          >
            <SelectTrigger className="w-48"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="ERROR">Errors</SelectItem>
              <SelectItem value="WARN">Warnings and errors</SelectItem>
              <SelectItem value="INFO">Everything</SelectItem>
            </SelectContent>
          </Select>
          <Input
            className="h-8 w-56"
            placeholder="Search"
            value={logView.search}
            onChange={(e) => setLogView((x) => ({ ...x, search: e.target.value }))}
          />
          <span className="text-xs text-muted-foreground tabular-nums">{visibleLogs.length} lines</span>
          <Button size="sm" variant="outline" className="ml-auto" disabled={!visibleLogs.length} onClick={() => invoke("copy_text", { text: logText })}>
            <HugeiconsIcon icon={Copy01Icon} size={14} strokeWidth={2} data-icon="inline-start" />
            Copy
          </Button>
        </div>
        <pre className="min-h-0 flex-1 overflow-auto rounded-xl border bg-muted/30 px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap">
          {visibleLogs.length ? visibleLogs.map((l, i) => (
            <div key={i} className={l.level === "ERROR" ? BAD : l.level === "WARN" ? WARN : undefined}>
              <span className="text-muted-foreground">{new Date(l.ts).toLocaleTimeString()} </span>
              <span className="inline-block w-12">{l.level}</span>
              {l.msg}
            </div>
          )) : <span className="text-muted-foreground">No log lines match.</span>}
        </pre>
      </TabsContent>

      {/* ===== HELP ===== */}
      <TabsContent value="help" className="min-h-0 overflow-y-auto p-3">
        <Card className="mx-auto max-w-3xl">
          <CardContent className="space-y-3 text-sm leading-relaxed [&_b]:font-semibold [&_h4]:mt-5 [&_h4]:font-medium [&_li]:list-disc [&_ul]:space-y-1 [&_ul]:pl-5">
            <div className="font-heading text-base font-semibold">Rustpick</div>
            <p>
              A headless Twitch drops miner, ported from{" "}
              <a className="cursor-pointer text-primary hover:underline" onClick={() => invoke("open_url", { url: "https://github.com/DevilXD/TwitchDropsMiner" })}>
                TwitchDropsMiner
              </a>
              . It watches drop-enabled channels without video, switches when one goes offline or a
              higher-priority game appears, and claims drops as they complete.
            </p>
            <h4>Tabs</h4>
            <ul>
              <li><b>Main</b>: login, live status, websocket health, mining progress, output log, and the channel list. Click a channel and press <b>Switch</b> to pin it; press Esc to unpin.</li>
              <li><b>Inventory</b>: every campaign and its drops, with progress and account-link status. Use the filters to hide categories.</li>
              <li><b>Settings</b>: priority and exclude lists, priority mode, and connection options. Most changes apply on the next reload.</li>
            </ul>
            <h4>Login</h4>
            <p>Device-code only: open the shown link, enter the code on Twitch, and approve. No password is ever entered here.</p>
            <h4>Tray</h4>
            <p>Closing the window shuts the miner down cleanly. The tray icon offers <i>Show window</i> and <i>Quit</i>.</p>
            <h4>Priority modes</h4>
            <ul>
              <li><b>Priority list only</b>: mine strictly in the order of the priority list.</li>
              <li><b>Ending soonest first</b>: prefer campaigns closest to ending.</li>
              <li><b>Low availability first</b>: prefer campaigns offered by the fewest channels.</li>
            </ul>
            <p className="text-muted-foreground">
              Files (settings.json, auth.json, log.txt) live in the working directory, or in $RUSTPICK_DIR.
              auth.json holds your Twitch token, so treat it like a password.
            </p>
          </CardContent>
        </Card>
      </TabsContent>
    </Tabs>
  )

  function addPrio() {
    if (!settings) return
    const v = prioInput.trim()
    if (v && !settings.priority.includes(v)) {
      update({ priority: [...settings.priority, v] })
      setPrioSel(settings.priority.length)
    }
    setPrioInput("")
  }
  function addExcl() {
    if (!settings) return
    const v = exclInput.trim()
    if (v && !settings.exclude.includes(v)) {
      update({ exclude: [...settings.exclude, v].sort() })
    }
    setExclInput("")
  }
  function delPrio() {
    if (!settings || prioSel == null || prioSel < 0 || prioSel >= settings.priority.length) return
    update({ priority: settings.priority.filter((_, i) => i !== prioSel) })
    setPrioSel(null)
  }
  function movePrio(op: "top" | "up" | "down" | "bottom") {
    if (!settings || prioSel == null || prioSel < 0 || prioSel >= settings.priority.length) return
    const a = [...settings.priority]
    const i = prioSel
    const move = (j: number) => { const [x] = a.splice(i, 1); a.splice(j, 0, x); setPrioSel(j) }
    if (op === "up" && i > 0) move(i - 1)
    else if (op === "down" && i < a.length - 1) move(i + 1)
    else if (op === "top" && i > 0) move(0)
    else if (op === "bottom" && i < a.length - 1) move(a.length - 1)
    else return
    update({ priority: a })
  }
}
