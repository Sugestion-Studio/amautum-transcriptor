//! Diarización de interlocutores con el sidecar `sherpa-diarize`
//! (`sherpa-onnx-offline-speaker-diarization` de k2-fsa).
//!
//! whisper.cpp transcribe QUÉ se dijo y CUÁNDO, pero no QUIÉN lo dijo: sobre
//! audio mono no distingue interlocutores. Este módulo cubre ese hueco como un
//! paso aparte que corre DESPUÉS de whisper:
//!
//!   1. sherpa-onnx segmenta el WAV por hablante (modelo pyannote) y calcula un
//!      embedding de voz por trozo (modelo 3D-Speaker), luego clusteriza los
//!      embeddings → tramos `[start, end] -> speaker_NN`.
//!   2. Cruzamos esos tramos con los segmentos de whisper por solape temporal:
//!      a cada segmento de texto le asignamos el hablante con el que más se
//!      solapa en el tiempo.
//!
//! El resultado se etiqueta como "Hablante 1", "Hablante 2"… (1-indexado, en
//! orden de aparición) — nombres neutros que el abogado renombra luego en la
//! web ("Juez", "Defensa"…) editando el campo de oradores.

use std::path::Path;

use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::config::DIARIZE_SIDECAR;
use crate::types::Segment;

#[derive(thiserror::Error, Debug)]
pub enum DiarizeError {
    #[error("sherpa-diarize falló: {0}")]
    Process(String),
    #[error("sherpa-diarize no devolvió tramos de hablante parseables")]
    NoOutput,
    #[error("Error de Tauri: {0}")]
    Tauri(String),
}

/// Un tramo de audio atribuido a un hablante por el diarizador.
#[derive(Debug, Clone)]
struct SpeakerSpan {
    start: f64,
    end: f64,
    /// Etiqueta cruda del diarizador, p. ej. `speaker_00`.
    raw_speaker: String,
}

/// Resultado de diarizar: la lista de oradores distintos (en orden de
/// aparición, ya con nombre legible) más el conteo de segmentos que sí
/// recibieron etiqueta. El llamador usa lo primero para `TranscriptUpload.speakers`.
pub struct DiarizationResult {
    pub speakers: Vec<String>,
    pub labeled_segments: usize,
}

/// Corre el diarizador sobre `wav_path` y ESCRIBE la etiqueta de hablante
/// directamente en cada `Segment` de `segments` (mutación in-place, por solape
/// temporal). Devuelve la lista de oradores distintos en orden de aparición.
///
/// `num_speakers`: si el operador conoce cuántos interlocutores hay, lo pasamos
/// como `--clustering.num-clusters` (clustering mucho más fiable). Si es `None`,
/// el diarizador estima el número por umbral de distancia.
pub async fn diarize_segments(
    app: &AppHandle,
    wav_path: &Path,
    segmentation_model: &Path,
    embedding_model: &Path,
    num_speakers: Option<u8>,
    segments: &mut [Segment],
) -> Result<DiarizationResult, DiarizeError> {
    let spans = run_sidecar(
        app,
        wav_path,
        segmentation_model,
        embedding_model,
        num_speakers,
    )
    .await?;
    if spans.is_empty() {
        return Err(DiarizeError::NoOutput);
    }

    // Mapa etiqueta-cruda → nombre legible, asignado en orden de aparición a lo
    // largo de los segmentos de TEXTO (no de los tramos del diarizador) para que
    // "Hablante 1" sea quien habla primero en el acta.
    let mut label_order: Vec<String> = Vec::new();
    let mut labeled = 0usize;

    for seg in segments.iter_mut() {
        if let Some(raw) = dominant_speaker(seg.start, seg.end, &spans) {
            if !label_order.iter().any(|r| r == &raw) {
                label_order.push(raw.clone());
            }
            let idx = label_order.iter().position(|r| r == &raw).unwrap();
            seg.speaker = Some(human_label(idx));
            labeled += 1;
        }
    }

    let speakers = (0..label_order.len()).map(human_label).collect();
    Ok(DiarizationResult {
        speakers,
        labeled_segments: labeled,
    })
}

fn human_label(index: usize) -> String {
    format!("Hablante {}", index + 1)
}

/// Devuelve la etiqueta cruda del hablante cuyo tramo solapa MÁS con
/// `[seg_start, seg_end]`. `None` si ningún tramo solapa (silencio/música que
/// el diarizador no atribuyó a nadie).
fn dominant_speaker(seg_start: f64, seg_end: f64, spans: &[SpeakerSpan]) -> Option<String> {
    let mut best: Option<(&str, f64)> = None;
    for span in spans {
        let overlap = (seg_end.min(span.end) - seg_start.max(span.start)).max(0.0);
        if overlap <= 0.0 {
            continue;
        }
        match best {
            Some((_, best_ov)) if overlap <= best_ov => {}
            _ => best = Some((span.raw_speaker.as_str(), overlap)),
        }
    }
    best.map(|(s, _)| s.to_string())
}

async fn run_sidecar(
    app: &AppHandle,
    wav_path: &Path,
    segmentation_model: &Path,
    embedding_model: &Path,
    num_speakers: Option<u8>,
) -> Result<Vec<SpeakerSpan>, DiarizeError> {
    let shell = app.shell();

    let mut args: Vec<String> = vec![
        format!(
            "--segmentation.pyannote-model={}",
            segmentation_model.to_string_lossy()
        ),
        format!("--embedding.model={}", embedding_model.to_string_lossy()),
        "--num-threads=2".into(),
    ];
    // Si conocemos el número de interlocutores lo fijamos; si no, dejamos que el
    // diarizador lo estime por umbral de distancia entre embeddings.
    match num_speakers {
        Some(n) if n >= 1 => args.push(format!("--clustering.num-clusters={n}")),
        _ => args.push("--clustering.cluster-threshold=0.5".into()),
    }
    args.push(wav_path.to_string_lossy().into_owned());

    let cmd = shell
        .sidecar(DIARIZE_SIDECAR)
        .map_err(|e| DiarizeError::Tauri(e.to_string()))?
        .args(args);

    let (mut rx, _child) = cmd
        .spawn()
        .map_err(|e| DiarizeError::Tauri(e.to_string()))?;

    // El binario imprime los tramos por stdout, pero según la versión algunos
    // van a stderr. Parseamos AMBOS para no perderlos.
    let mut spans: Vec<SpeakerSpan> = Vec::new();
    let mut last_err = String::new();
    let mut exit_code: Option<i32> = None;

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                if let Some(span) = parse_span_line(&String::from_utf8_lossy(&line)) {
                    spans.push(span);
                }
            }
            CommandEvent::Stderr(line) => {
                let text = String::from_utf8_lossy(&line).into_owned();
                if let Some(span) = parse_span_line(&text) {
                    spans.push(span);
                }
                last_err.push_str(&text);
                last_err.push('\n');
            }
            CommandEvent::Terminated(payload) => exit_code = payload.code,
            CommandEvent::Error(err) => return Err(DiarizeError::Process(err)),
            _ => {}
        }
    }

    if exit_code != Some(0) {
        return Err(DiarizeError::Process(tail(&last_err, 4)));
    }
    Ok(spans)
}

/// Parsea una línea del diarizador con la forma `0.038 -- 2.910 speaker_00`.
/// Tolerante: ignora cualquier línea que no calce el patrón (banners, logs).
fn parse_span_line(line: &str) -> Option<SpeakerSpan> {
    let line = line.trim();
    let sep = line.find(" -- ")?;
    let start: f64 = line[..sep].trim().parse().ok()?;
    let rest = &line[sep + 4..];
    // `rest` = "2.910 speaker_00" — el primer token es el fin, el resto la etiqueta.
    let mut it = rest.split_whitespace();
    let end: f64 = it.next()?.parse().ok()?;
    let raw_speaker = it.next()?.to_string();
    if !raw_speaker.to_ascii_lowercase().contains("speaker") {
        return None;
    }
    Some(SpeakerSpan {
        start,
        end,
        raw_speaker,
    })
}

fn tail(s: &str, n: usize) -> String {
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_span_line() {
        let span = parse_span_line("0.038 -- 2.910 speaker_00").unwrap();
        assert_eq!(span.start, 0.038);
        assert_eq!(span.end, 2.910);
        assert_eq!(span.raw_speaker, "speaker_00");
    }

    #[test]
    fn ignores_non_span_lines() {
        assert!(parse_span_line("Loading model from foo.onnx").is_none());
        assert!(parse_span_line("").is_none());
    }

    #[test]
    fn dominant_speaker_picks_max_overlap() {
        let spans = vec![
            SpeakerSpan { start: 0.0, end: 5.0, raw_speaker: "speaker_00".into() },
            SpeakerSpan { start: 5.0, end: 10.0, raw_speaker: "speaker_01".into() },
        ];
        // Segmento 4..9 solapa 1s con sp0 y 4s con sp1 → gana sp1.
        assert_eq!(dominant_speaker(4.0, 9.0, &spans).as_deref(), Some("speaker_01"));
        // Segmento fuera de todo tramo → None.
        assert_eq!(dominant_speaker(20.0, 21.0, &spans), None);
    }

    #[test]
    fn assigns_labels_in_appearance_order() {
        // Etiquetado por orden de aparición en el TEXTO: el primero que habla
        // es "Hablante 1" aunque su id crudo sea speaker_01.
        assert_eq!(human_label(0), "Hablante 1");
        assert_eq!(human_label(2), "Hablante 3");
    }
}
