/**
 * Ventana de estado del agente.
 *
 * Antes mostraba cuatro filas fijas: estado, puerto, trabajos y versión. Con un
 * trabajo de seis horas en curso eso decía literalmente «Procesando · 1» — la
 * misma pantalla a los dos minutos que a las cinco horas. Cuando algo fallaba,
 * no aparecía nada en ninguna parte.
 *
 * Ahora la ventana cuenta lo que está pasando: en qué tramo va, a qué ritmo,
 * cuánto falta según lo medido en ESTE equipo, hace cuánto el motor dio señales,
 * qué error hubo y qué hacer al respecto. Y si un acta quedó sin subir, deja
 * reintentarlo con un botón.
 *
 * Sigue sin usar IPC con Rust: la fuente única de verdad del runtime es el
 * servidor HTTP local, igual que para el navegador.
 */

const PORT = Number(import.meta.env.VITE_AGENT_PORT ?? 17173)
const BASE = `http://localhost:${PORT}`

// ── Contrato de `GET /status` ───────────────────────────────────────────────

type JobStage =
  | "queued"
  | "downloadingModel"
  | "preprocess"
  | "transcribing"
  | "diarizing"
  | "uploading"
  | "uploadPending"
  | "completed"
  | "failed"

interface Job {
  jobId: string
  fileName: string
  model: string
  hardware: string | null
  stage: JobStage
  progress: number
  etaSeconds: number | null
  audioSeconds: number | null
  note: string | null
  startedAtMs: number
  engineSeenAtMs: number
  updatedAtMs: number
  error: string | null
  hint: string | null
}

interface PendingUpload {
  jobId: string
  /** Epoch en SEGUNDOS (así lo guarda el buzón en disco), no en milisegundos. */
  savedAt: number
  durationSeconds: number
  characters: number
  language: string
  model: string
}

interface LogLine {
  atMs: number
  level: string
  text: string
}

interface UpdateInfo {
  current: string
  latest: string
  available: boolean
  /** Enlace directo al instalador de ESTA plataforma; lo arma el agente. */
  downloadUrl: string
  notesUrl: string
}

interface Status {
  version: string
  port: number
  dependenciesOk: boolean
  dependenciesError: string | null
  activeJobs: number
  jobs: Job[]
  pendingUploads: PendingUpload[]
  logs: LogLine[]
  /** `null` mientras el agente no haya podido preguntar (sin red, por ejemplo). */
  update: UpdateInfo | null
  nowMs: number
}

/** Trabajos que la persona ya despachó; no vuelven a "vivos" solos. */
const TERMINAL: JobStage[] = ["completed", "failed"]

/**
 * Cuánto silencio del motor tomamos como normal antes de decirlo en pantalla.
 * Por debajo de esto no vale la pena mencionarlo; por encima, callarlo es lo
 * que hace que un trabajo sano parezca colgado.
 */
const QUIET_ENGINE_SECONDS = 90

let lastStatus: Status | null = null
/** Cuándo recibimos `lastStatus`, para que los relojes de la ventana sigan
 *  avanzando entre sondeos sin tener que sondear cada segundo. */
let lastStatusAt = 0
let retrying: string | null = null

/** "Ahora" según el reloj del agente, extrapolado desde el último sondeo. */
function agentNow(status: Status): number {
  return status.nowMs + (Date.now() - lastStatusAt)
}

// ── Utilidades de formato ───────────────────────────────────────────────────

function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—"
  const total = Math.floor(seconds)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) return `${h} h ${String(m).padStart(2, "0")} min`
  if (m > 0) return `${m} min ${String(s).padStart(2, "0")} s`
  return `${s} s`
}

function fmtClock(atMs: number): string {
  const d = new Date(atMs)
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(
    2,
    "0",
  )}:${String(d.getSeconds()).padStart(2, "0")}`
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
}

function stageLabel(stage: JobStage): string {
  switch (stage) {
    case "queued":
      return "En cola"
    case "downloadingModel":
      return "Descargando el motor"
    case "preprocess":
      return "Preparando el audio"
    case "transcribing":
      return "Transcribiendo"
    case "diarizing":
      return "Identificando interlocutores"
    case "uploading":
      return "Subiendo el acta"
    case "uploadPending":
      return "Acta a salvo, pendiente de subir"
    case "completed":
      return "Terminado"
    case "failed":
      return "Falló"
  }
}

function hardwareLabel(hardware: string | null): string {
  switch (hardware) {
    case "metal":
      return "GPU (Metal)"
    case "cuda":
      return "GPU (CUDA)"
    case "cpu":
      return "CPU"
    default:
      return "—"
  }
}

// ── Render ──────────────────────────────────────────────────────────────────

function setHeadline(status: Status | null) {
  const chip = document.getElementById("chip")!
  const headline = document.getElementById("headline")!
  const pulse = document.getElementById("pulse")!

  if (!status) {
    chip.textContent = "Sin respuesta"
    chip.className = "chip chip-error"
    pulse.className = "pulse pulse-error"
    headline.textContent =
      "El motor local no responde. Cierra el agente desde la bandeja y vuelve a abrirlo."
    return
  }

  const live = status.jobs.filter((j) => !TERMINAL.includes(j.stage))
  const pending = status.pendingUploads.length

  if (!status.dependenciesOk) {
    chip.textContent = "Revisar"
    chip.className = "chip chip-error"
    pulse.className = "pulse pulse-error"
    headline.textContent = "Falta un componente: no se puede transcribir todavía."
    return
  }
  if (live.length > 0) {
    const job = live[0]
    chip.textContent = "Trabajando"
    chip.className = "chip chip-busy"
    pulse.className = "pulse pulse-busy"
    headline.textContent =
      live.length === 1
        ? `${stageLabel(job.stage)} · ${job.fileName}`
        : `${live.length} trabajos en curso`
    return
  }
  if (pending > 0) {
    chip.textContent = "Pendiente"
    chip.className = "chip chip-warn"
    pulse.className = "pulse pulse-warn"
    headline.textContent = `${pending} acta(s) esperando conexión para subirse.`
    return
  }
  chip.textContent = "Listo"
  chip.className = "chip chip-ok"
  pulse.className = "pulse pulse-ok"
  headline.textContent = "Todo en orden. Puedes lanzar una transcripción desde Amautum."
}

function renderJob(job: Job, nowMs: number): string {
  const elapsed = Math.max(0, (nowMs - job.startedAtMs) / 1000)
  const engineQuiet = Math.max(0, (nowMs - job.engineSeenAtMs) / 1000)
  const running = !TERMINAL.includes(job.stage) && job.stage !== "uploadPending"

  const rows: string[] = []
  rows.push(row("Modelo", `${job.model} · ${hardwareLabel(job.hardware)}`))
  if (job.audioSeconds) rows.push(row("Audio", fmtDuration(job.audioSeconds)))
  rows.push(row("Lleva", fmtDuration(elapsed)))
  if (running && job.etaSeconds != null) {
    rows.push(row("Falta (estimado)", fmtDuration(job.etaSeconds)))
  }

  // La línea clave del rediseño: distingue "lento" de "colgado". Es la
  // respuesta a la pregunta que la gente hacía por teléfono.
  if (job.stage === "transcribing") {
    rows.push(
      row(
        "El motor reportó",
        engineQuiet < QUIET_ENGINE_SECONDS
          ? `hace ${fmtDuration(engineQuiet)} — todo normal`
          : `hace ${fmtDuration(engineQuiet)}`,
        engineQuiet < QUIET_ENGINE_SECONDS ? "ok" : "warn",
      ),
    )
  }

  const bar =
    running || job.stage === "uploading"
      ? `<div class="bar"><div class="bar-fill" style="width:${Math.min(
          100,
          Math.max(0, job.progress),
        )}%"></div></div>`
      : ""

  const stageNote = job.note ? `<p class="detail">${escapeHtml(job.note)}</p>` : ""

  const patience =
    job.stage === "transcribing" && engineQuiet >= QUIET_ENGINE_SECONDS
      ? `<p class="detail">El motor informa su avance cada 5%. En equipos sin tarjeta gráfica
         dedicada, ese 5% puede tardar bastante: mientras el agente siga aquí, el trabajo
         sigue vivo. Si quieres que termine antes, cancélalo desde Amautum y relánzalo con el
         modelo <em>base</em> o <em>small</em>.</p>`
      : ""

  // Junto al error, no en un pie de página: el momento en que alguien necesita
  // escribir a soporte es justo cuando está leyendo qué le falló.
  const problem = job.error
    ? `<div class="alert ${job.stage === "failed" ? "alert-error" : "alert-warn"}">
         <p class="alert-title">${
           job.stage === "failed" ? "No se pudo terminar" : "La subida no pasó todavía"
         }</p>
         <p class="detail">${escapeHtml(job.error)}</p>
         ${job.hint ? `<p class="detail detail-hint">${escapeHtml(job.hint)}</p>` : ""}
         ${
           job.stage === "failed"
             ? `<p class="detail">¿Lo intentaste y sigue igual?
                  <button class="linkish" data-open="support" type="button">Escríbenos a soporte</button>
                  — copia antes el diagnóstico del pie de esta ventana y pégalo en el ticket.</p>`
             : ""
         }
       </div>`
    : ""

  const retry =
    job.stage === "uploadPending"
      ? `<button class="btn btn-primary" data-retry="${escapeHtml(job.jobId)}" ${
          retrying === job.jobId ? "disabled" : ""
        }>${retrying === job.jobId ? "Reintentando…" : "Reintentar subida ahora"}</button>`
      : ""

  return `
    <article class="panel job job-${job.stage}">
      <div class="job-head">
        <h2 title="${escapeHtml(job.fileName)}">${escapeHtml(job.fileName)}</h2>
        <span class="stage">${stageLabel(job.stage)}${
          running ? ` · ${job.progress}%` : ""
        }</span>
      </div>
      ${bar}
      ${stageNote}
      <dl class="rows">${rows.join("")}</dl>
      ${patience}
      ${problem}
      ${retry}
    </article>`
}

function row(label: string, value: string, tone?: "ok" | "warn"): string {
  return `<div class="row"><dt>${label}</dt><dd class="${
    tone ? `tone-${tone}` : ""
  }">${escapeHtml(value)}</dd></div>`
}

/**
 * Un acta pendiente cuyo trabajo ya no está en memoria (el agente se reinició
 * desde entonces). El buzón guarda menos datos que el registro vivo — no tiene
 * el nombre del audio — así que la identificamos por duración y tamaño.
 */
function renderPendingWithoutJob(p: PendingUpload, nowMs: number): string {
  const waiting = Math.max(0, nowMs / 1000 - p.savedAt)
  return `
    <article class="panel job job-uploadPending">
      <div class="job-head">
        <h2>Acta de ${escapeHtml(fmtDuration(p.durationSeconds))}</h2>
        <span class="stage">A salvo, pendiente de subir</span>
      </div>
      <dl class="rows">
        ${row("Esperando desde hace", fmtDuration(waiting))}
        ${row("Modelo", `${p.model} · ${p.language}`)}
        ${row("Tamaño del acta", `${p.characters.toLocaleString("es-EC")} caracteres`)}
      </dl>
      <div class="alert alert-warn">
        <p class="alert-title">No perdiste el trabajo</p>
        <p class="detail">
          La transcripción terminó y está guardada en este equipo. El agente la reintenta solo
          cada cinco minutos; en cuanto haya conexión con Amautum, se sube.
        </p>
      </div>
      <button class="btn btn-primary" data-retry="${escapeHtml(p.jobId)}" ${
        retrying === p.jobId ? "disabled" : ""
      }>${retrying === p.jobId ? "Reintentando…" : "Reintentar subida ahora"}</button>
    </article>`
}

function render(status: Status | null) {
  setHeadline(status)
  const jobsEl = document.getElementById("jobs")!
  const emptyEl = document.getElementById("empty")!
  const depsEl = document.getElementById("deps")!

  if (!status) {
    jobsEl.innerHTML = ""
    emptyEl.hidden = true
    return
  }

  document.getElementById("version")!.textContent = status.version
  document.getElementById("port")!.textContent = String(status.port)

  depsEl.hidden = status.dependenciesOk
  document.getElementById("deps-detail")!.textContent =
    status.dependenciesError ?? "No pudimos verificar los componentes del agente."

  const updateEl = document.getElementById("update")!
  const update = status.update
  updateEl.hidden = !update?.available
  if (update?.available) {
    document.getElementById("update-latest")!.textContent = update.latest
    document.getElementById("update-current")!.textContent = update.current
    document.getElementById("update-notes")!.setAttribute("href", update.notesUrl)
  }

  // Un acta pendiente cuyo trabajo ya no está en memoria (el agente se
  // reinició) también tiene que verse: es trabajo real esperando.
  const jobIds = new Set(status.jobs.map((j) => j.jobId))
  const orphanPending = status.pendingUploads.filter((p) => !jobIds.has(p.jobId))

  const now = agentNow(status)
  const cards = [
    ...status.jobs.map((j) => renderJob(j, now)),
    ...orphanPending.map((p) => renderPendingWithoutJob(p, now)),
  ]
  jobsEl.innerHTML = cards.join("")
  emptyEl.hidden = cards.length > 0

  const logsEl = document.getElementById("logs")!
  logsEl.innerHTML = status.logs
    .slice(-60)
    .reverse()
    .map(
      (l) =>
        `<li class="log log-${l.level}"><span class="log-at">${fmtClock(
          l.atMs,
        )}</span> ${escapeHtml(l.text)}</li>`,
    )
    .join("")
}

// ── Acciones ────────────────────────────────────────────────────────────────

function toast(text: string, tone: "ok" | "error" = "ok") {
  const el = document.getElementById("toast")!
  el.textContent = text
  el.className = `toast toast-${tone}`
  el.hidden = false
  window.setTimeout(() => {
    el.hidden = true
  }, 6000)
}

async function retryUpload(jobId: string) {
  retrying = jobId
  render(lastStatus)
  try {
    const res = await fetch(`${BASE}/jobs/${encodeURIComponent(jobId)}/retry`, {
      method: "POST",
    })
    const body = (await res.json().catch(() => null)) as { error?: string } | null
    if (res.ok) {
      // El agente acepta el reintento y lo corre en segundo plano: no promete
      // que ya se subió. Si funciona, la tarjeta desaparece del listado sola en
      // el siguiente sondeo.
      toast("Reintentando la subida. Si funciona, el acta desaparecerá de esta lista.")
    } else {
      toast(body?.error ?? `No se pudo subir (HTTP ${res.status}).`, "error")
    }
  } catch {
    toast("No pudimos hablar con el motor local. ¿El agente sigue abierto?", "error")
  } finally {
    retrying = null
    await refresh()
  }
}

/**
 * Vuelca todo lo que soporte necesita. `navigator.clipboard` puede no estar
 * disponible según cómo sirva la webview la página, así que dejamos un camino
 * alterno en vez de fallar en silencio.
 */
async function copyDiagnostics() {
  const s = lastStatus
  const lines: string[] = [
    `Amautum Transcriptor — diagnóstico`,
    `Fecha: ${new Date().toISOString()}`,
    `Agente: v${s?.version ?? "?"} · puerto ${s?.port ?? PORT}`,
    `Sistema: ${navigator.userAgent}`,
    `Componentes: ${s?.dependenciesOk ? "ok" : `PROBLEMA — ${s?.dependenciesError ?? "?"}`}`,
    ``,
    `Trabajos:`,
    ...(s?.jobs.length
      ? s.jobs.map(
          (j) =>
            `  - ${j.fileName} · ${j.stage} · ${j.progress}% · modelo ${j.model} · ${
              j.hardware ?? "?"
            }${j.error ? ` · error: ${j.error}` : ""}`,
        )
      : ["  (ninguno)"]),
    ``,
    `Actas pendientes de subir:`,
    ...(s?.pendingUploads.length
      ? s.pendingUploads.map(
          (p) =>
            `  - ${p.jobId} · ${fmtDuration(p.durationSeconds)} de audio · ${
              p.characters
            } caracteres · guardada ${new Date(p.savedAt * 1000).toISOString()}`,
        )
      : ["  (ninguna)"]),
    ``,
    `Bitácora:`,
    ...(s?.logs ?? []).map((l) => `  ${fmtClock(l.atMs)} [${l.level}] ${l.text}`),
  ]
  const text = lines.join("\n")

  try {
    await navigator.clipboard.writeText(text)
    toast("Diagnóstico copiado. Pégalo en tu mensaje a soporte.")
    return
  } catch {
    // Camino alterno para webviews sin Clipboard API.
  }
  const area = document.createElement("textarea")
  area.value = text
  area.setAttribute("readonly", "")
  area.style.position = "fixed"
  area.style.opacity = "0"
  document.body.appendChild(area)
  area.select()
  const ok = document.execCommand("copy")
  document.body.removeChild(area)
  toast(
    ok
      ? "Diagnóstico copiado. Pégalo en tu mensaje a soporte."
      : "No pudimos copiar automáticamente. Selecciona el texto de la bitácora y cópialo a mano.",
    ok ? "ok" : "error",
  )
}

// ── Ciclo ───────────────────────────────────────────────────────────────────

async function refresh() {
  try {
    const res = await fetch(`${BASE}/status`, { cache: "no-store" })
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    lastStatus = (await res.json()) as Status
    lastStatusAt = Date.now()
  } catch {
    lastStatus = null
  }
  render(lastStatus)
}

document.addEventListener("click", (ev) => {
  const target = ev.target
  if (!(target instanceof HTMLElement)) return
  const retryId = target.closest("[data-retry]")?.getAttribute("data-retry")
  if (retryId) {
    void retryUpload(retryId)
    return
  }
  if (target.closest("#copy")) {
    void copyDiagnostics()
    return
  }
  if (target.closest("#update-download")) {
    void installUpdate()
    return
  }
  const openTarget = target.closest("[data-open]")?.getAttribute("data-open")
  if (openTarget) void openInBrowser(openTarget as OpenTarget)
})

/**
 * Instala la actualización sin pasar por el navegador.
 *
 * Normalmente este botón sobra: el agente se actualiza solo en cuanto queda
 * ocioso. Está para quien no quiere esperar al siguiente ciclo.
 *
 * Si el agente está trabajando, el servidor responde 409 y NO reinicia — no se
 * tira una transcripción en curso por instalar antes. Y si el actualizador no
 * está disponible (compilación sin llave de firma, o sin red), caemos al camino
 * de siempre: descargar el instalador del navegador.
 */
async function installUpdate() {
  toast("Instalando la actualización…")
  try {
    const res = await fetch(`${BASE}/update/install`, { method: "POST" })
    if (res.ok) {
      // Si el reinicio ocurre no llegamos aquí; la ventana se va con el proceso.
      toast("Actualización instalada. El agente se está reiniciando.")
      return
    }
    const body = (await res.json().catch(() => null)) as { error?: string } | null
    toast(body?.error ?? "No se pudo instalar la actualización.", "error")
    // Sin actualizador utilizable, el navegador sigue siendo una salida válida.
    if (body?.error && !body.error.includes("trabajo en curso")) {
      void openInBrowser("release")
    }
  } catch {
    toast("No pudimos hablar con el motor local. ¿El agente sigue abierto?", "error")
  }
}

type OpenTarget = "support" | "downloads" | "release"

const OPEN_CONFIRMATION: Record<OpenTarget, string> = {
  support: "Abrimos soporte en tu navegador. Si ya iniciaste sesión, puedes escribir el ticket.",
  downloads: "Abrimos la guía de descarga en tu navegador.",
  release: "Abrimos la descarga en tu navegador. Instálala encima de la app actual.",
}

/**
 * Abre uno de los destinos que el agente conoce en el navegador del sistema.
 *
 * La ventana NO manda una URL: manda un NOMBRE de una lista cerrada y el agente
 * decide a dónde lleva (para la descarga, además, según la plataforma en la que
 * está corriendo). Así este endpoint no sirve para abrir un enlace cualquiera.
 *
 * Si el navegador no se puede abrir, mostramos la URL para que se pueda copiar
 * a mano — dejar a alguien sin camino justo cuando busca ayuda sería el peor
 * momento para hacerlo.
 */
async function openInBrowser(target: OpenTarget) {
  try {
    const res = await fetch(`${BASE}/open/${target}`, { method: "POST" })
    const body = (await res.json().catch(() => null)) as { url?: string } | null
    if (res.ok) {
      toast(OPEN_CONFIRMATION[target])
    } else {
      toast(
        body?.url
          ? `No pudimos abrir el navegador. Entra a ${body.url}`
          : "No pudimos abrir el navegador.",
        "error",
      )
    }
  } catch {
    toast("No pudimos hablar con el motor local. ¿El agente sigue abierto?", "error")
  }
}

void refresh()
// Sondeamos cada 2 s (el estado incluye una lectura del buzón en disco; hacerlo
// cada segundo es pedirle trabajo al antivirus por nada) y repintamos cada
// segundo con el reloj extrapolado, para que "lleva 1 h 14 min" avance suave.
window.setInterval(() => void refresh(), 2000)
window.setInterval(() => render(lastStatus), 1000)
