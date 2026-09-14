Object.defineProperty(globalThis, "location", { value: { search: "" }, configurable: true })
Object.defineProperty(globalThis, "window", { value: {}, configurable: true })

type MockEvent = { kind: string; data?: unknown }
type MockTauri = {
  __TAURI__: {
    core: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> }
    event: { listen: (name: string, cb: (e: { payload: MockEvent }) => void) => Promise<() => void> }
  }
}

const { install } = await import("./mock.ts")
install()
const tauri = (window as unknown as MockTauri).__TAURI__
const counts = new Map<string, number>()

let unlisten = await tauri.event.listen("miner-event", (e) => {
  if (e.payload.kind !== "Print") return
  const line = String(e.payload.data)
  counts.set(line, (counts.get(line) ?? 0) + 1)
})

// Simulate React Strict Mode: first mount invokes startup, cleans up, then remounts.
await tauri.core.invoke("frontend_ready")
await unlisten()
unlisten = await tauri.event.listen("miner-event", (e) => {
  if (e.payload.kind !== "Print") return
  const line = String(e.payload.data)
  counts.set(line, (counts.get(line) ?? 0) + 1)
})
await tauri.core.invoke("frontend_ready")
await new Promise((resolve) => setTimeout(resolve, 100))
await unlisten()

if (counts.size !== 13) {
  throw new Error(`expected 13 startup output lines, got ${counts.size}`)
}
if (counts.get("Rustpick starting") !== 1) {
  throw new Error(`startup count=${counts.get("Rustpick starting") ?? 0}`)
}
for (const [line, count] of counts) {
  if (line.includes("Channel") && line.includes("updated") && count !== 1) {
    throw new Error(`${line} count=${count}`)
  }
}

console.log(`mock startup checks passed (${counts.size} unique lines)`)
