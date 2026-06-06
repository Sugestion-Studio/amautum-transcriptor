/**
 * Ventana de estado del agente. Es una webview Tauri minimal que:
 *   1. consulta `GET http://localhost:<PORT>/health` para refrescar estado.
 *   2. reabrir cierra al icono de bandeja en lugar de matar el proceso (eso lo
 *      gestiona Tauri directamente desde main.rs).
 *
 * No hacemos IPC con Rust para evitar acoplamiento: la fuente única de verdad
 * del runtime es el servidor HTTP local, igual que para el navegador.
 */

const PORT = Number(import.meta.env.VITE_AGENT_PORT ?? 17173)
const HEALTH_URL = `http://localhost:${PORT}/health`

interface HealthResponse {
  ok: boolean
  version: string
  busy: boolean
  jobsRunning: number
}

function setText(id: string, text: string, cls?: "ok" | "busy" | "error") {
  const el = document.getElementById(id)
  if (!el) return
  el.textContent = text
  if (cls) {
    el.classList.remove("ok", "busy", "error")
    el.classList.add(cls)
  }
}

async function refresh() {
  try {
    const res = await fetch(HEALTH_URL, { cache: "no-store" })
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    const data = (await res.json()) as HealthResponse
    setText("state", data.busy ? "Procesando" : "Listo", data.busy ? "busy" : "ok")
    setText("port", String(PORT))
    setText("jobs", String(data.jobsRunning))
    setText("version", data.version)
  } catch (err) {
    setText("state", "Servidor local no responde", "error")
    setText("port", String(PORT))
  }
}

refresh()
setInterval(refresh, 2000)
