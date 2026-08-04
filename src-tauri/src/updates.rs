//! Aviso de versión nueva DENTRO del agente.
//!
//! Amautum ya avisa en el navegador: el asistente de `/dashboard/transcribir`
//! compara la versión que reporta `/health` contra la última publicada y pinta
//! un banner con el enlace de descarga. Pero eso solo lo ve quien abre esa
//! pestaña. El agente vive en la bandeja del sistema durante semanas, y quien no
//! entra al asistente —o quien entra con el trabajo ya lanzado— nunca se entera
//! de que hay una versión que arregla precisamente lo que le está pasando.
//!
//! Así que el agente lo pregunta por su cuenta. La consulta vive en Rust, no en
//! la ventana, por dos razones: la CSP de la webview solo permite hablar con
//! `localhost:17173`, y así una sola consulta cada seis horas sirve a la ventana
//! las veces que haga falta sin pegarle a la API de GitHub en cada repintado.

use std::time::Duration;

use once_cell::sync::Lazy;
use reqwest::Client;
use serde::Serialize;

use crate::config;

/// Release marcada como «latest» en el repo público de instaladores.
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Sugestion-Studio/amautum-transcriptor/releases/latest";

const RELEASES_BASE: &str =
    "https://github.com/Sugestion-Studio/amautum-transcriptor/releases";

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(config::user_agent())
        .timeout(Duration::from_secs(20))
        .build()
        .expect("Could not build reqwest client for update checks")
});

/// Lo que la ventana necesita para poder decir «hay una versión nueva» y llevar
/// a la persona a la descarga correcta sin que tenga que elegir nada.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    /// Enlace DIRECTO al instalador de ESTA plataforma. Que la persona no tenga
    /// que adivinar entre .dmg/.msi/.AppImage/.deb ni entre arm64 y x64: el
    /// agente ya sabe sobre qué está corriendo.
    pub download_url: String,
    pub notes_url: String,
}

/// Consulta la última versión publicada. `None` si no hay red o GitHub no
/// coopera — quedarse sin saber no es un error que valga la pena mostrar.
pub async fn check() -> Option<UpdateInfo> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: Option<String>,
    }

    let res = CLIENT
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let release: Release = res.json().await.ok()?;
    let latest = release.tag_name?.trim().trim_start_matches('v').to_string();
    // Validamos que sea un semver limpio antes de confiar en él: un tag raro no
    // debe producir una URL de descarga inventada.
    if !is_clean_semver(&latest) {
        return None;
    }

    let current = config::version().to_string();
    Some(UpdateInfo {
        available: is_newer(&latest, &current),
        download_url: format!("{RELEASES_BASE}/download/v{latest}/{}", asset_name(&latest)),
        notes_url: format!("{RELEASES_BASE}/tag/v{latest}"),
        latest,
        current,
    })
}

fn is_clean_semver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// ¿`candidate` es posterior a `current`? Comparación numérica por componente:
/// `0.1.10` es MAYOR que `0.1.9`, cosa que una comparación de cadenas invierte.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> [u32; 3] {
        let mut out = [0u32; 3];
        for (i, part) in v.split('.').take(3).enumerate() {
            out[i] = part.parse().unwrap_or(0);
        }
        out
    };
    parse(candidate) > parse(current)
}

/// Nombre del instalador de ESTA plataforma, con la convención que produce el
/// paso «Normalize asset names» del workflow de release y que consume el
/// catálogo de la web (`lib/transcriptor/agent-downloads.ts`).
///
/// Si algún día cambia la convención, cambia en los tres sitios o el enlace
/// directo apunta a un 404.
fn asset_name(version: &str) -> String {
    let (os, arch, ext) = platform_triplet();
    format!("AmautumTranscriptor-{version}-{os}-{arch}.{ext}")
}

fn platform_triplet() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_os = "macos")]
    {
        if cfg!(target_arch = "aarch64") {
            ("macos", "arm64", "dmg")
        } else {
            ("macos", "x64", "dmg")
        }
    }
    #[cfg(target_os = "windows")]
    {
        ("windows", "x64", "msi")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        ("linux", "x64", "AppImage")
    }
}

/// URL a la que mandamos a la persona cuando pulsa «Descargar». La ventana no
/// arma URLs ni recibe una desde fuera: pide *abrir la descarga* y el agente
/// decide cuál. Sin parámetro no hay nada que inyectar.
pub fn releases_page() -> String {
    format!("{RELEASES_BASE}/latest")
}

// ── Actualización silenciosa ────────────────────────────────────────────────
//
// POR QUÉ SILENCIOSA
//
// Hasta ahora actualizar era un trámite: bajar el instalador del navegador y
// repetir el ritual del sistema operativo —`xattr -cr` en macOS, «Ejecutar de
// todos modos» en SmartScreen— **en cada versión**, porque los instaladores no
// están firmados. Firmar de verdad cuesta dinero (Developer ID de Apple,
// certificado EV de Windows); el actualizador de Tauri no cuesta nada y elimina
// el ritual por otra vía: el archivo lo descarga la propia app, no el navegador,
// así que no recibe la marca de cuarentena ni el *mark-of-the-web* que disparan
// esos avisos. La firma es nuestra, con una llave minisign propia.
//
// CUÁNDO **NO** SE ACTUALIZA — y esto es lo importante
//
// Instalar implica reiniciar el agente. Reiniciar a mitad de una transcripción
// destruiría horas de CPU, que es justo el desastre que este agente existe para
// evitar. Así que la actualización silenciosa exige que el agente esté OCIOSO:
//
//   · Sin trabajos en curso.
//   · Sin actas pendientes de subir (un reinicio cortaría el reintento).
//
// Si hay algo en marcha no se toca nada y se vuelve a mirar en el siguiente
// ciclo. Un agente ocupado nunca se reinicia solo; uno ocioso se pone al día sin
// que nadie tenga que enterarse.

/// ¿Se puede reiniciar el agente ahora mismo sin destruir trabajo?
fn is_idle(state: &crate::pipeline::AppState) -> bool {
    state.active_jobs() == 0 && state.running.load(std::sync::atomic::Ordering::SeqCst) == 0
}

/// Descarga e instala la actualización, y reinicia. **No vuelve** si lo logra.
///
/// `silent` distingue las dos entradas: el ciclo automático (que se calla si algo
/// no está en su sitio) y el botón de la ventana (que sí devuelve el error para
/// poder explicárselo a la persona).
pub async fn install_update(
    app: &tauri::AppHandle,
    state: &crate::pipeline::AppState,
    silent: bool,
) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;

    if !is_idle(state) {
        let msg = "Hay trabajo en curso: la actualización espera a que el agente quede libre.";
        if !silent {
            state.log("info", msg);
        }
        return Err(msg.to_string());
    }

    // Si no hay llave pública configurada, o la red falla, esto devuelve error y
    // la ventana cae al camino de siempre (descargar desde el navegador).
    let updater = app
        .updater()
        .map_err(|e| format!("El actualizador no está disponible: {e}"))?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("No pudimos comprobar si hay versión nueva: {e}"))?;
    let Some(update) = update else {
        return Err("Ya tienes la última versión.".to_string());
    };

    state.log(
        "info",
        format!("Instalando la actualización a la v{}…", update.version),
    );
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("No se pudo instalar la actualización: {e}"))?;

    // Última comprobación antes de reiniciar: la descarga pudo tardar y en ese
    // rato la persona puede haber lanzado una transcripción.
    if !is_idle(state) {
        state.log(
            "warn",
            "La actualización quedó lista, pero entró trabajo mientras se descargaba. \
             Se aplicará al reiniciar el agente.",
        );
        return Ok(());
    }

    state.log("info", "Actualización instalada. Reiniciando el agente.");
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La trampa clásica de comparar versiones como texto: "0.1.9" > "0.1.10".
    #[test]
    fn compares_numerically_not_lexicographically() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.9", "0.1.10"));
        assert!(!is_newer("0.1.11", "0.1.11"));
    }

    #[test]
    fn rejects_tags_that_are_not_clean_semver() {
        assert!(is_clean_semver("0.1.11"));
        assert!(!is_clean_semver("0.1"));
        assert!(!is_clean_semver("0.1.11-rc1"));
        assert!(!is_clean_semver("latest"));
        assert!(!is_clean_semver(""));
    }

    /// El enlace directo tiene que coincidir con lo que el workflow publica.
    #[test]
    fn asset_name_follows_release_convention() {
        let name = asset_name("0.1.11");
        assert!(name.starts_with("AmautumTranscriptor-0.1.11-"), "{name}");
        assert!(
            name.ends_with(".dmg") || name.ends_with(".msi") || name.ends_with(".AppImage"),
            "{name}"
        );
    }
}

/// Notas de la versión que está corriendo ahora mismo. Lo usa el menú «Ayuda →
/// Novedades de esta versión»: quien acaba de actualizarse quiere saber qué
/// cambió en LA SUYA, no en la última publicada.
pub fn release_notes_url() -> String {
    format!("{RELEASES_BASE}/tag/v{}", config::version())
}
