import { useEffect, useRef, useState } from "react"
import { HugeiconsIcon } from "@hugeicons/react"
import {
  Add01Icon,
  ArrowDown02Icon,
  ArrowDownDoubleIcon,
  ArrowUp02Icon,
  ArrowUpDoubleIcon,
  Copy01Icon,
  GiftIcon,
  LockIcon,
  MultiplicationSignIcon,
  RefreshIcon,
  Tick02Icon,
} from "@hugeicons/core-free-icons"
import { invoke } from "@/lib/tauri"
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
  dark_mode: boolean
}

type MinerEvent =
  | { kind: "Print"; data: string }
  | { kind: "Status"; data: string }
  | { kind: "LoginCode"; data: { url: string; code: string } }
  | { kind: "LoggedIn"; data: { user_id: number } }
  | { kind: "Websocket"; data: { idx: number; status: string; topics: number } }
  | { kind: "Channels"; data: Channel[] }
  | { kind: "Watching"; data: number | null }
  | { kind: "Inventory"; data: Campaign[] }
  | { kind: "Progress"; data: ProgressView }
  | { kind: "Exited" }

/* ---------- constants / helpers ---------- */

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
const STATUS_CLS: Record<ChannelStatus, string> = {
  Online: "text-emerald-600 dark:text-emerald-400",
  Pending: "text-amber-600 dark:text-amber-400",
  Offline: "text-muted-foreground",
}
const CAMP_STATUS: Record<string, [string, string]> = {
  Active_true: ["Active", "text-emerald-600 dark:text-emerald-400"],
  Active_false: ["Active, not linked", "text-red-600 dark:text-red-400"],
  Upcoming: ["Upcoming", "text-amber-600 dark:text-amber-400"],
  Expired: ["Expired", "text-muted-foreground"],
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
  const [filters, setFilters] = useState<Filters>({
    notlinked: true, upcoming: true, expired: true, excluded: false, finished: true,
  })
  const [, setTick] = useState(0)
  const consoleRef = useRef<HTMLPreElement>(null)
  const stickRef = useRef(true)
  const selectedRef = useRef<number | null>(null)
  selectedRef.current = selected

  const handle = (ev: MinerEvent) => {
    switch (ev.kind) {
      case "Print":
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
      case "Exited": setStatus("Exited"); break
    }
  }

  useEffect(() => {
    invoke<Settings>("get_settings").then((s) => {
      setSettings(s)
      setSaved(JSON.stringify(s))
    })
    let un: (() => void) | undefined
    ;(async () => {
      un = await window.__TAURI__.event.listen("miner-event", (e) => handle(e.payload as MinerEvent))
      await invoke("frontend_ready")
    })()
    return () => un?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 1000)
    return () => clearInterval(t)
  }, [])

  useEffect(() => {
    document.documentElement.classList.toggle("dark", !!settings?.dark_mode)
  }, [settings?.dark_mode])

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
  const games = [...new Set(inventory.map((c) => c.game))].sort()

  /* progress countdown derivation */
  let dropMin = 0, secs = 0
  if (progress) {
    const elapsed = Math.min(60, Math.floor((Date.now() - progressAt) / 1000))
    if (progress.countdown && elapsed < 60 && progress.drop_required > progress.drop_current) {
      dropMin = progress.drop_required - progress.drop_current - 1
      secs = 60 - elapsed
    } else {
      dropMin = Math.max(0, progress.drop_required - progress.drop_current)
    }
  }
  const campMin = progress ? Math.max(0, progress.campaign_remaining_minutes - (secs > 0 ? 1 : 0)) : 0

  /* inventory filter */
  const visibleCampaigns = inventory.filter((c) => {
    const excluded = settings ? settings.exclude.includes(c.game) : false
    const finished = c.total > 0 && c.claimed >= c.total
    if (finished && !filters.finished) return false
    if (c.status === "Upcoming" && !filters.upcoming) return false
    if (c.status === "Expired" && !filters.expired) return false
    if (excluded && !filters.excluded) return false
    if (!c.eligible && !filters.notlinked) return false
    return true
  })

  return (
    <Tabs value={tab} onValueChange={(v) => setTab(v as string)} className="px-3 pb-6">
      <TabsList className="sticky top-0 z-10 mt-2 self-center bg-background/90 backdrop-blur">
        <TabsTrigger value="main">Main</TabsTrigger>
        <TabsTrigger value="inventory">Inventory</TabsTrigger>
        <TabsTrigger value="settings">Settings</TabsTrigger>
        <TabsTrigger value="help">Help</TabsTrigger>
      </TabsList>
      <datalist id="games-dl">
        {games.map((g) => <option key={g} value={g} />)}
      </datalist>

      {/* ===== MAIN ===== */}
      <TabsContent value="main" className="space-y-2">
        <Card size="sm">
          <CardContent className="text-sm">{esc(status)}</CardContent>
        </Card>
        <div className="grid grid-cols-[minmax(360px,1fr)_minmax(320px,1fr)] items-start gap-2">
          <div className="space-y-2">
            <Card size="sm">
              <CardHeader><CardTitle>Websocket status</CardTitle></CardHeader>
              <CardContent className="font-mono text-xs">
                {Array.from({ length: WS_SLOTS }, (_, i) => i)
                  .concat(Object.keys(websockets).map(Number).filter((i) => i >= WS_SLOTS).sort((a, b) => a - b))
                  .map((i) => {
                    const w = websockets[i]
                    return (
                      <div key={i} className="flex justify-between gap-3 py-px">
                        <span>Websocket #{i + 1}:</span>
                        <span className="text-muted-foreground">
                          {w ? `${w.status}  ${w.topics}/${WS_MAX_TOPICS}` : ""}
                        </span>
                      </div>
                    )
                  })}
              </CardContent>
            </Card>
            <Card size="sm">
              <CardHeader><CardTitle>Login</CardTitle></CardHeader>
              <CardContent>
                <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
                  <span className="text-muted-foreground">Status:</span>
                  <span className="font-medium">{userId != null ? "Logged in" : login ? "Login required" : "Logged out"}</span>
                  <span className="text-muted-foreground">User ID:</span>
                  <span className="text-muted-foreground">{userId ?? "—"}</span>
                </div>
                {login && (
                  <div className="mt-3">
                    <div className="text-xs text-muted-foreground">
                      Open{" "}
                      <a className="cursor-pointer text-primary underline" onClick={() => invoke("open_url", { url: login.url })}>
                        {login.url}
                      </a>{" "}
                      and enter:
                    </div>
                    <div className="mt-1 flex items-center justify-center gap-2">
                      <span className="font-mono text-2xl tracking-widest">{login.code}</span>
                      <Button variant="outline" size="sm" onClick={() => invoke("copy_text", { text: login.code })}>
                        <HugeiconsIcon icon={Copy01Icon} size={14} strokeWidth={2} data-icon="inline-start" />
                        Copy
                      </Button>
                    </div>
                  </div>
                )}
                <div className="mt-3 flex justify-center">
                  <Button disabled={!login} onClick={() => login && invoke("open_url", { url: login.url })}>Login</Button>
                </div>
              </CardContent>
            </Card>
            <Card size="sm">
              <CardHeader><CardTitle>Campaign progress</CardTitle></CardHeader>
              <CardContent className="text-sm">
                <div className="grid grid-cols-2 text-center">
                  <div className="text-muted-foreground">
                    Game:<br /><span className="font-medium text-foreground">{progress?.game ?? "…"}</span>
                  </div>
                  <div className="text-muted-foreground">
                    Campaign:<br /><span className="font-medium text-foreground">{progress?.campaign_name ?? "…"}</span>
                  </div>
                </div>
                <div className="mt-3 flex items-center gap-2">
                  <span className="text-muted-foreground">Progress:</span>
                  <Progress value={progress ? progress.campaign_progress * 100 : 0} className="flex-1" />
                  <span className="tabular-nums text-xs text-muted-foreground">
                    {progress ? `${pct(progress.campaign_progress)} (${progress.campaign_claimed}/${progress.campaign_total})` : "-%"}
                  </span>
                </div>
                <div className="text-center text-xs text-muted-foreground">{fmt(campMin, secs)} remaining</div>
                <div className="my-3 border-t" />
                <div className="text-center text-xs font-medium text-muted-foreground">Drop:</div>
                <div className="text-center">{progress?.drop_rewards ?? "…"}</div>
                <div className="mt-2 flex items-center gap-2">
                  <span className="text-muted-foreground">Progress:</span>
                  <Progress value={progress ? progress.drop_progress * 100 : 0} className="flex-1" />
                  <span className="tabular-nums text-xs text-muted-foreground">
                    {progress ? `${pct(progress.drop_progress)} (${progress.drop_current}/${progress.drop_required} min)` : "-%"}
                  </span>
                </div>
                <div className="text-center text-xs text-muted-foreground">{fmt(dropMin, secs)} remaining</div>
              </CardContent>
            </Card>
          </div>
          <Card size="sm">
            <CardHeader>
              <CardTitle>Channels</CardTitle>
              <CardAction className="flex items-center gap-2">
                <Button size="sm" disabled={selected == null} onClick={() => selected != null && invoke("select", { id: selected })}>
                  Switch
                </Button>
              </CardAction>
              <CardDescription>Click a row, then Switch · Esc clears</CardDescription>
            </CardHeader>
            <CardContent className="px-0">
              <div className="max-h-[400px] overflow-y-auto">
                <Table>
                  <TableHeader className="sticky top-0 z-10 bg-card">
                    <TableRow>
                      <TableHead>Channel</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead>Game</TableHead>
                      <TableHead title="Drops enabled"><HugeiconsIcon icon={GiftIcon} size={14} strokeWidth={2} /></TableHead>
                      <TableHead>Viewers</TableHead>
                      <TableHead title="Allow-list based"><HugeiconsIcon icon={LockIcon} size={14} strokeWidth={2} /></TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {channels.map((c) => (
                      <TableRow
                        key={c.id}
                        className={cn("cursor-pointer", selected === c.id && "bg-accent")}
                        onClick={() => setSelected((sel) => (sel === c.id ? null : c.id))}
                      >
                        <TableCell className={cn(watching === c.id && "font-medium text-emerald-600 dark:text-emerald-400")}>
                          {c.name}
                        </TableCell>
                        <TableCell className={cn("font-medium", STATUS_CLS[c.status])}>
                          {c.status.toUpperCase()}
                        </TableCell>
                        <TableCell>{c.game ?? ""}</TableCell>
                        <TableCell>{c.drops_enabled && <Tick className="text-muted-foreground" />}</TableCell>
                        <TableCell className="tabular-nums">{c.viewers ?? ""}</TableCell>
                        <TableCell>{c.acl_based && <Tick className="text-muted-foreground" />}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </CardContent>
          </Card>
        </div>
        <Card size="sm">
          <CardHeader><CardTitle>Output</CardTitle></CardHeader>
          <CardContent className="px-1">
            <pre
              ref={consoleRef}
              onScroll={(e) => {
                const el = e.currentTarget
                stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 30
              }}
              className="h-40 overflow-y-auto rounded-md bg-muted/50 px-2 py-1.5 font-mono text-xs whitespace-pre-wrap"
            >
              {consoleLines.join("\n")}
            </pre>
          </CardContent>
        </Card>
      </TabsContent>

      {/* ===== INVENTORY ===== */}
      <TabsContent value="inventory" className="space-y-2">
        <Card size="sm">
          <CardContent className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="text-sm text-muted-foreground">Show:</span>
            {FILTERS.map(([key, label]) => (
              <label key={key} className="flex cursor-pointer items-center gap-1.5 text-sm">
                <Checkbox
                  checked={filters[key]}
                  onCheckedChange={(checked) => setFilters((f) => ({ ...f, [key]: !!checked }))}
                />
                {label}
              </label>
            ))}
            <Button size="sm" variant="outline" onClick={() => invoke("reload")}>
              <HugeiconsIcon icon={RefreshIcon} size={14} strokeWidth={2} data-icon="inline-start" />
              Refresh
            </Button>
          </CardContent>
        </Card>
        {visibleCampaigns.map((c) => {
          const [label, cls] = CAMP_STATUS[c.status === "Active" ? `Active_${c.eligible}` : c.status] ?? ["", ""]
          return (
            <Card key={c.id}>
              <CardContent className="flex flex-wrap gap-4">
                <div className="flex min-w-[260px] flex-1 gap-3">
                  {c.image_url ? (
                    <img
                      src={c.image_url}
                      alt=""
                      className="h-[99px] w-[74px] shrink-0 rounded-md bg-muted object-cover"
                      onError={(e) => (e.currentTarget.style.visibility = "hidden")}
                    />
                  ) : (
                    <div className="h-[99px] w-[74px] shrink-0 rounded-md bg-muted" />
                  )}
                  <div className="min-w-0">
                    <div className="font-medium">{c.name}</div>
                    <div className={cn("text-sm", cls)}>{label}</div>
                    <div className="text-xs text-muted-foreground">Ends: {dt(c.ends_at)}</div>
                    {!c.linked && c.link_url ? (
                      <a className="cursor-pointer text-sm text-primary underline" onClick={() => invoke("open_url", { url: c.link_url })}>
                        Link account
                      </a>
                    ) : c.linked ? (
                      <div className="text-sm text-emerald-600 dark:text-emerald-400"><Tick /> Linked</div>
                    ) : null}
                    <div className="mt-2 flex items-center gap-2">
                      <Progress value={c.progress * 100} className="flex-1" />
                      <span className="text-xs tabular-nums text-muted-foreground">
                        {pct(c.progress)} ({c.claimed}/{c.total})
                      </span>
                    </div>
                  </div>
                </div>
                <div className="flex flex-wrap content-start gap-2">
                  {c.drops.map((d) => (
                    <div key={d.id} title={d.rewards} className="w-[118px] rounded-lg bg-muted/40 p-2 text-center text-xs ring-1 ring-foreground/10">
                      <div className="flex h-14 items-center justify-center text-muted-foreground">{d.name}</div>
                      <div className="truncate font-medium">{d.rewards || d.name}</div>
                      <div className="mt-1 text-muted-foreground">
                        {d.is_claimed ? (
                          <span className="font-medium text-emerald-600 dark:text-emerald-400"><Tick /> Claimed</span>
                        ) : (
                          `${d.current_minutes}/${d.required_minutes} min`
                        )}
                      </div>
                    </div>
                  )) || <span className="text-sm text-muted-foreground">No drops.</span>}
                </div>
              </CardContent>
            </Card>
          )
        })}
        {visibleCampaigns.length === 0 && (
          <div className="text-sm text-muted-foreground">No campaigns match the current filters.</div>
        )}
      </TabsContent>

      {/* ===== SETTINGS ===== */}
      <TabsContent value="settings">
        <div className="grid grid-cols-[minmax(320px,1.2fr)_1fr_1fr] items-start gap-2">
          <div className="space-y-2">
            <Card size="sm">
              <CardHeader><CardTitle>General</CardTitle></CardHeader>
              <CardContent className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 text-sm">
                <label htmlFor="s-dark">Dark mode:</label>
                <Checkbox
                  id="s-dark"
                  checked={!!settings?.dark_mode}
                  onCheckedChange={(checked) => update({ dark_mode: !!checked })}
                />
                <label>Priority mode:</label>
                <Select
                  value={settings?.priority_mode}
                  onValueChange={(v) => update({ priority_mode: v as PriorityMode })}
                >
                  <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {MODES.map(([v, l]) => <SelectItem key={v} value={v}>{l}</SelectItem>)}
                  </SelectContent>
                </Select>
                <label htmlFor="s-proxy">Proxy <span className="text-muted-foreground">(restart)</span>:</label>
                <Input
                  id="s-proxy"
                  placeholder="http://…"
                  value={settings?.proxy ?? ""}
                  onChange={(e) => update({ proxy: e.target.value })}
                />
                <label htmlFor="s-quality">Connection quality:</label>
                <div className="flex items-center gap-2">
                  <Slider
                    id="s-quality"
                    min={1}
                    max={6}
                    step={1}
                    value={[settings?.connection_quality ?? 3]}
                    onValueChange={(v) => update({ connection_quality: (v as number[])[0] })}
                    className="flex-1"
                  />
                  <span className="w-7 text-xs text-muted-foreground tabular-nums">
                    {settings?.connection_quality}×
                  </span>
                </div>
              </CardContent>
            </Card>
            <Card size="sm">
              <CardHeader>
                <CardTitle>Advanced</CardTitle>
                <CardDescription className="font-medium text-amber-600 dark:text-amber-400">
                  Warning! These options can cause the miner to misbehave. If you hit issues, make sure they are disabled.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-2 text-sm">
                <label className="flex cursor-pointer items-center gap-2">
                  <Checkbox
                    checked={!!settings?.enable_badges_emotes}
                    onCheckedChange={(checked) => update({ enable_badges_emotes: !!checked })}
                  />
                  Enable partial support for badges and emotes
                </label>
                <label className="flex cursor-pointer items-center gap-2">
                  <Checkbox
                    checked={!!settings?.available_drops_check}
                    onCheckedChange={(checked) => update({ available_drops_check: !!checked })}
                  />
                  Enable extra available drops check
                </label>
              </CardContent>
            </Card>
          </div>
          <Card size="sm">
            <CardHeader><CardTitle>Priority</CardTitle></CardHeader>
            <CardContent>
              <div className="mb-2 flex gap-2">
                <Input
                  list="games-dl"
                  placeholder="Game name"
                  value={prioInput}
                  onChange={(e) => setPrioInput(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") addPrio() }}
                  className="min-w-0"
                />
                <Button variant="outline" size="icon" onClick={addPrio}>
                  <HugeiconsIcon icon={Add01Icon} size={16} strokeWidth={2} />
                </Button>
              </div>
              <div className="grid grid-cols-[1fr_auto] gap-2">
                <div className="h-56 overflow-y-auto rounded-md border">
                  {(settings?.priority ?? []).map((g, i) => (
                    <div
                      key={i}
                      className={cn("cursor-pointer px-2 py-0.5 text-sm whitespace-nowrap", prioSel === i && "bg-accent")}
                      onClick={() => setPrioSel(i)}
                    >
                      {g}
                    </div>
                  ))}
                  {(settings?.priority.length ?? 0) === 0 && (
                    <div className="px-2 py-0.5 text-sm text-muted-foreground">empty</div>
                  )}
                </div>
                <div className="flex flex-col justify-center gap-1">
                  {([["top", ArrowUpDoubleIcon, "Move to top"], ["up", ArrowUp02Icon, "Move up"],
                    ["down", ArrowDown02Icon, "Move down"], ["bottom", ArrowDownDoubleIcon, "Move to bottom"]] as const).map(
                    ([op, icon, title]) => (
                      <Button key={op} variant="ghost" size="icon-sm" title={title} onClick={() => movePrio(op)}>
                        <HugeiconsIcon icon={icon} size={14} strokeWidth={2} />
                      </Button>
                    ),
                  )}
                  <Button variant="ghost" size="icon-sm" title="Remove" className="text-destructive hover:text-destructive" onClick={delPrio}>
                    <HugeiconsIcon icon={MultiplicationSignIcon} size={14} strokeWidth={2} />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
          <Card size="sm">
            <CardHeader><CardTitle>Exclude</CardTitle></CardHeader>
            <CardContent>
              <div className="mb-2 flex gap-2">
                <Input
                  list="games-dl"
                  placeholder="Game name"
                  value={exclInput}
                  onChange={(e) => setExclInput(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") addExcl() }}
                  className="min-w-0"
                />
                <Button variant="outline" size="icon" onClick={addExcl}>
                  <HugeiconsIcon icon={Add01Icon} size={16} strokeWidth={2} />
                </Button>
              </div>
              <div className="h-56 overflow-y-auto rounded-md border">
                {(settings?.exclude ?? []).map((g) => (
                  <div
                    key={g}
                    className={cn("cursor-pointer px-2 py-0.5 text-sm whitespace-nowrap", exclSel === g && "bg-accent")}
                    onClick={() => setExclSel(g)}
                  >
                    {g}
                  </div>
                ))}
                {(settings?.exclude.length ?? 0) === 0 && (
                  <div className="px-2 py-0.5 text-sm text-muted-foreground">empty</div>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
                className="mt-2 w-full text-destructive hover:text-destructive"
                onClick={() => {
                  if (!settings || exclSel == null) return
                  update({ exclude: settings.exclude.filter((x) => x !== exclSel) })
                  setExclSel(null)
                }}
              >
                <HugeiconsIcon icon={MultiplicationSignIcon} size={14} strokeWidth={2} data-icon="inline-start" />
                Remove
              </Button>
            </CardContent>
          </Card>
        </div>
        <div className="mt-3 flex items-center justify-center gap-3">
          <span className="text-sm text-muted-foreground">Most changes require a reload to take effect:</span>
          <Button disabled={!dirty} onClick={saveSettings}>Save</Button>
          <Button variant="outline" onClick={() => { saveSettings(); invoke("reload") }}>
            Save &amp; Reload
          </Button>
          {dirty && (
            <Badge variant="outline" className="border-amber-500/40 text-amber-600 dark:text-amber-400">unsaved</Badge>
          )}
        </div>
      </TabsContent>

      {/* ===== HELP ===== */}
      <TabsContent value="help">
        <Card className="mx-auto max-w-3xl">
          <CardContent className="space-y-3 text-sm leading-relaxed [&_b]:font-semibold [&_h4]:mt-4 [&_h4]:font-medium [&_li]:list-disc [&_ul]:pl-5">
            <div className="text-base font-medium">Rustpick</div>
            <p>
              A headless Twitch drops miner, ported from{" "}
              <a className="cursor-pointer text-primary underline" onClick={() => invoke("open_url", { url: "https://github.com/DevilXD/TwitchDropsMiner" })}>
                TwitchDropsMiner
              </a>
              . It watches drop-enabled channels without video, switches when one goes offline or a
              higher-priority game appears, and claims drops as they complete.
            </p>
            <h4>Tabs</h4>
            <ul>
              <li><b>Main</b> — login, live status, websocket health, mining progress, output log, and the channel list. Click a channel and press <b>Switch</b> to pin it; press Esc to unpin.</li>
              <li><b>Inventory</b> — every campaign and its drops, with progress and account-link status. Use the filters to hide categories.</li>
              <li><b>Settings</b> — priority and exclude lists, priority mode, and connection options. Most changes apply on the next reload.</li>
            </ul>
            <h4>Login</h4>
            <p>Device-code only: open the shown link, enter the code on Twitch, and approve. No password is ever entered here.</p>
            <h4>Tray</h4>
            <p>Closing the window shuts the miner down cleanly. The tray icon offers <i>Show window</i> and <i>Quit</i>.</p>
            <h4>Priority modes</h4>
            <ul>
              <li><b>Priority list only</b> — mine strictly in the order of the priority list.</li>
              <li><b>Ending soonest first</b> — prefer campaigns closest to ending.</li>
              <li><b>Low availability first</b> — prefer campaigns offered by the fewest channels.</li>
            </ul>
            <p className="text-muted-foreground">
              Files (settings.json, auth.json, log.txt) live in the working directory, or in $RUSTPICK_DIR.
              auth.json holds your Twitch token — treat it like a password.
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
