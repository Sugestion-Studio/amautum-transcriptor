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
