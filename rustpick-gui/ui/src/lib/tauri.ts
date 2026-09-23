/// <reference types="vite/client" />
type Unlisten = () => void

declare global {
  interface Window {
    __TAURI__: {
      core: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> }
      event: { listen: (name: string, cb: (e: { payload: unknown }) => void) => Promise<Unlisten> }
      window?: { getCurrentWindow: () => { setTheme: (theme: "light" | "dark" | null) => Promise<void> } }
    }
  }
}

if (import.meta.env.DEV && !window.__TAURI__) {
  const { install } = await import("./mock")
  install()
}

export const invoke = <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  window.__TAURI__.core.invoke(cmd, args) as Promise<T>

export const listen = (name: string, cb: (payload: unknown) => void): Promise<Unlisten> =>
  window.__TAURI__.event.listen(name, (e) => cb(e.payload))
