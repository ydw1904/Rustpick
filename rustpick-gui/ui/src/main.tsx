import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import App from "./App"
import "./index.css"

// only the macOS window is transparent over a vibrancy material (tauri.macos.conf.json)
if ("__TAURI_INTERNALS__" in window && navigator.userAgent.includes("Mac")) {
  document.documentElement.classList.add("glass")
}

// no web "Reload / Inspect Element" menu, except where there is text to copy or edit
if (!import.meta.env.DEV) {
  document.addEventListener("contextmenu", (e) => {
    if (!(e.target as Element).closest("input, textarea, pre")) e.preventDefault()
  })
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
