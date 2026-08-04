#!/usr/bin/env bash
#
# Prepara los binarios sidecar para poder correr `npm run tauri:dev`.
#
# Sin ellos el build ni siquiera arranca:
#
#     resource path `whisper-cli-aarch64-apple-darwin` doesn't exist
#
# Tauri los busca PLANOS, en `src-tauri/<nombre>-<triple>[.exe]`. Ojo con esto:
# hay documentación que habla de `src-tauri/binaries/`, y ahí NO los encuentra.
# Los binarios no van al repo (pesan decenas de MB y son específicos de cada
# plataforma), así que cada equipo los coloca una vez.
#
# De dónde los saca, por orden:
#   1. De una instalación existente de Amautum Transcriptor — es lo más fiable,
#      porque son exactamente los que usa la app publicada.
#   2. De tu sistema (`which ffmpeg`, `which whisper-cli`).
#
# `sherpa-diarize` (identificación de interlocutores) es OPCIONAL: si no está, el
# agente arranca igual y solo falla si alguien pide diarización.
#
# Uso:
#   bash scripts/setup-dev-sidecars.sh

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v rustc >/dev/null 2>&1 || { echo "ERROR: falta rustc (instala Rust)." >&2; exit 1; }
TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
EXT=""; [[ "$TRIPLE" == *windows* ]] && EXT=".exe"
echo "Plataforma: $TRIPLE"
echo ""

APP_MACOS="/Applications/Amautum Transcriptor.app/Contents/MacOS"

# Busca un binario y lo deja en su hueco. Devuelve 1 si no lo encuentra.
install_sidecar() {
  local name="$1" optional="${2:-no}"
  local dest="src-tauri/${name}-${TRIPLE}${EXT}"

  if [[ -f "$dest" ]]; then
    echo "  ✓ $name — ya estaba"
    return 0
  fi

  local src=""
  if [[ -f "$APP_MACOS/$name" ]]; then
    src="$APP_MACOS/$name"
  elif command -v "$name" >/dev/null 2>&1; then
    src="$(command -v "$name")"
  fi

  if [[ -z "$src" ]]; then
    if [[ "$optional" == "opcional" ]]; then
      echo "  – $name — no encontrado (es opcional: solo hace falta para diarización)"
      return 0
    fi
    echo "  ✗ $name — NO ENCONTRADO" >&2
    return 1
  fi

  cp "$src" "$dest"
  chmod +x "$dest"
  echo "  ✓ $name — copiado desde $src"
}

echo "Colocando binarios en src-tauri/:"
missing=0
install_sidecar whisper-cli || missing=1
install_sidecar ffmpeg || missing=1
install_sidecar sherpa-diarize opcional || true

if [[ "$missing" -gt 0 ]]; then
  cat >&2 <<EOF

Falta algún binario obligatorio. Dos formas de conseguirlos:

  a) Instala Amautum Transcriptor desde
     https://github.com/Sugestion-Studio/amautum-transcriptor/releases/latest
     y vuelve a lanzar este script: los toma de ahí.

  b) Consíguelos por tu cuenta:
       ffmpeg       brew install ffmpeg   (o el gestor de tu sistema)
       whisper-cli  compilando whisper.cpp — ver src-tauri/binaries/README.md

EOF
  exit 1
fi

echo ""
echo "Listo. Ya puedes:  npm run tauri:dev"
echo ""
echo "Un aviso que ahorra confusión: CIERRA la app instalada desde la bandeja"
echo "antes de arrancar. Si las dos corren, la instalada se queda el puerto 17173"
echo "y verás la versión vieja creyendo que ves la tuya."
