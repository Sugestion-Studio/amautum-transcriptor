#!/usr/bin/env bash
#
# Publica una versión nueva del agente Amautum Transcriptor.
#
# Este repo es la ÚNICA casa del agente. Antes existía una copia dentro de
# Amautum y se sincronizaban a mano; entre v0.1.6 y v0.1.10 dejaron de estar
# sincronizadas y la copia interna describía un agente que ningún cliente
# corría. La mudanza acabó con esa clase de problema: aquí no hay nada que
# sincronizar.
#
# ── El mismo comando, dos fases ───────────────────────────────────────────
#
# El script mira en qué punto del flujo `issue-*` → `develop` → `main` estás y
# hace lo que toca:
#
#   bash scripts/release.sh 0.1.13
#     → La versión no está en `origin/main`, así que PREPARA: la escribe en los
#       tres archivos donde vive, compila, corre los tests y commitea el bump en
#       tu rama. Después abres tus PRs como siempre.
#
#   bash scripts/release.sh 0.1.13 --push
#     → La versión ya está en `origin/main`, así que PUBLICA: taggea y empuja.
#       El tag dispara el build de las cuatro plataformas.
#
# Lo que se publica es el commit de `origin/main`, no tu árbol de trabajo: da
# igual en qué rama estés y qué tengas a medias, y es imposible publicar código
# que no haya pasado por los PRs.
#
# Opciones:
#   --push          autoriza la publicación (acción hacia fuera)
#   --skip-checks   se salta compilar y testear (bajo tu responsabilidad)
#   --no-commit     prepara los archivos pero no commitea
#
# Tras publicar, en Amautum: `AGENT_LATEST_VERSION` de
# `lib/transcriptor/agent-downloads.ts` es solo el respaldo por si la API de
# GitHub no responde —la web lee la última versión en vivo—, así que se actualiza
# cuando toque, no urgentemente.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRUNK="${RELEASE_TRUNK:-main}"

VERSION="${1:-}"
DO_PUSH=false
SKIP_CHECKS=false
DO_COMMIT=true
for arg in "${@:2}"; do
  case "$arg" in
    --push) DO_PUSH=true ;;
    --skip-checks) SKIP_CHECKS=true ;;
    --no-commit) DO_COMMIT=false ;;
    *) echo "ERROR: opción desconocida: $arg" >&2; exit 1 ;;
  esac
done

die() { echo "" >&2; echo "ERROR: $*" >&2; exit 1; }
step() { echo ""; echo "→ $*"; }

[[ -n "$VERSION" ]] || die "falta la versión. Uso: $0 X.Y.Z [--push]"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "'$VERSION' no es un semver limpio (X.Y.Z, sin 'v' ni sufijos)."

cd "$REPO_DIR"

# ── La clave del actualizador tiene que ser válida ────────────────────────
#
# Copiar la clave pública desde una terminal zsh arrastra un `%` al final —la
# marca de "esto no terminaba en salto de línea"— y con ese carácter deja de ser
# base64 válido. El release sale igual y NINGÚN agente puede verificar la
# actualización; se descubriría versiones después, con todos los clientes ya
# incapaces de actualizarse solos.
#
# Vacía es válido: significa "sin actualizador", y todo sigue como antes.

step "Comprobando la clave pública del actualizador"
read_pubkey() {
  sed -n 's/.*"pubkey"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p' "$1" | head -1
}
check_pubkey() {
  local key="$1" origen="$2"
  if [[ -z "$key" ]]; then
    echo "  ($origen: vacía → sin actualización automática)"
    return 0
  fi
  if printf '%s' "$key" | base64 -d 2>/dev/null | grep -q "minisign public key"; then
    echo "  ✓ $origen: $(printf '%s' "$key" | base64 -d 2>/dev/null | head -1)"
    return 0
  fi
  die "la clave pública del actualizador ($origen) no es una clave minisign válida.

       La causa habitual: al copiarla desde la terminal se cuela un carácter de
       más. Vuelve a ponerla limpia con:

         tr -d '\\n%' < ~/.tauri/<tu-llave>.key.pub | pbcopy"
}
LOCAL_PUBKEY="$(read_pubkey src-tauri/tauri.conf.json)"
check_pubkey "$LOCAL_PUBKEY" "copia local"

# ── ¿En qué fase estamos? ─────────────────────────────────────────────────

step "Mirando si la v$VERSION ya llegó a origin/$TRUNK"
git fetch --quiet origin "$TRUNK" 2>/dev/null \
  || echo "  (no pude contactar con origin; sigo con lo que tengo en local)"

TRUNK_VERSION="$(git show "origin/$TRUNK:src-tauri/Cargo.toml" 2>/dev/null \
  | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)"
[[ -n "$TRUNK_VERSION" ]] || die "no pude leer src-tauri/Cargo.toml en origin/$TRUNK."

if [[ "$TRUNK_VERSION" == "$VERSION" ]]; then
  PHASE="publish"
  echo "  ✓ origin/$TRUNK ya está en la v$VERSION → toca PUBLICAR"
else
  PHASE="prepare"
  echo "  origin/$TRUNK está en la v$TRUNK_VERSION → toca PREPARAR la v$VERSION"
fi

# =========================================================================
# FASE 1 — PREPARAR
# =========================================================================

if [[ "$PHASE" == "prepare" ]]; then
  if [[ "$DO_PUSH" == true ]]; then
    cat >&2 <<EOF

No puedo publicar todavía: la v$VERSION no está en origin/$TRUNK
(ahí está la v$TRUNK_VERSION).

Publicar desde otro sitio dejaría instaladores en manos de clientes construidos
desde código que no pasó por tus PRs. Sigo con la fase de preparación.
EOF
    DO_PUSH=false
  fi

  BRANCH="$(git symbolic-ref --quiet --short HEAD || echo "")"
  if [[ "$BRANCH" == "$TRUNK" || "$BRANCH" == "develop" ]]; then
    die "estás en '$BRANCH'. El bump de versión es un cambio como cualquier otro:
       va en una rama issue-* y llega por PR."
  fi

  # Las notas de la versión las lee quien descarga el instalador, no quien
  # revisa el código. Se escriben a mano en CHANGELOG.md: volcar ahí los mensajes
  # de commit publica cosas como "arreglada la advertencia del compilador" en la
  # página donde un abogado decide si le merece la pena actualizar.
  #
  # Aviso, no bloqueo: a veces se prepara la versión antes de redactar las notas.
  # El CI vuelve a mirar y, si siguen faltando, cae a los commits.
  if ! grep -q "^## $VERSION\b" CHANGELOG.md 2>/dev/null; then
    cat <<EOF

⚠  CHANGELOG.md no tiene sección para la $VERSION.

   Añade al principio, bajo el encabezado:

     ## $VERSION

     ### Arreglado
     - …

     ### Nuevo
     - …

     ### Al actualizar
     - …

   Escrito para quien USA el programa: qué le cambia, no qué se tocó por dentro.
   Sin esta sección el CI publica los mensajes de commit, que son notas internas.

EOF
  else
    echo ""
    echo "  ✓ CHANGELOG.md tiene sección para la $VERSION"
  fi

  step "Fijando la versión $VERSION en los tres archivos donde vive"
  python3 - "$VERSION" <<'PY'
import json, pathlib, re, sys
version = sys.argv[1]

for rel in ("package.json", "src-tauri/tauri.conf.json"):
    p = pathlib.Path(rel)
    data = json.loads(p.read_text())
    data["version"] = version
    p.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    print(f"  {rel}")

cargo = pathlib.Path("src-tauri/Cargo.toml")
text = cargo.read_text()
new, n = re.subn(r'(?m)^version = "[^"]+"', f'version = "{version}"', text, count=1)
if n != 1:
    sys.exit("no encontré la línea `version = ...` en Cargo.toml")
cargo.write_text(new)
print("  src-tauri/Cargo.toml")
PY

  if [[ "$SKIP_CHECKS" == true ]]; then
    echo ""; echo "  (saltando compilación y tests por --skip-checks)"
  else
    step "Compilando y corriendo los tests"
    command -v cargo >/dev/null 2>&1 \
      || die "cargo no está instalado; instálalo o usa --skip-checks."
    # `tauri-build` exige que existan los slots de sidecar aunque estén vacíos.
    # En CI los pone el workflow; en local los simulamos y los borramos al salir.
    TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
    EXT=""; [[ "$TRIPLE" == *windows* ]] && EXT=".exe"
    STUBS=()
    for name in whisper-cli ffmpeg sherpa-diarize; do
      slot="src-tauri/${name}-${TRIPLE}${EXT}"
      if [[ ! -e "$slot" ]]; then
        printf '#!/bin/sh\nexit 0\n' > "$slot"; chmod +x "$slot"; STUBS+=("$slot")
      fi
    done
    cleanup_stubs() { [[ ${#STUBS[@]} -gt 0 ]] && rm -f "${STUBS[@]}"; return 0; }
    trap cleanup_stubs EXIT
    npm run build >/dev/null || die "la ventana del agente no compila."
    ( cd src-tauri && cargo test --lib --quiet ) || die "no compila o falla algún test."
    cleanup_stubs; STUBS=(); trap - EXIT
    echo "  ✓ compila y pasa los tests"
  fi

  if [[ "$DO_COMMIT" == true ]]; then
    step "Commiteando el bump en '$BRANCH'"
    git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
    if git diff --cached --quiet; then
      echo "  (la versión ya estaba puesta; nada que commitear)"
    else
      git commit -q -m "release: v$VERSION"
      echo "  ✓ commit creado"
    fi
  else
    step "Archivos escritos SIN commitear (--no-commit)"
  fi

  cat <<EOF

──────────────────────────────────────────────────────────────────────────
Fase 1 lista: la v$VERSION está preparada en '$BRANCH'.

  git push -u origin $BRANCH

PR a develop, PR a $TRUNK. Cuando esté mergeado:

  bash scripts/release.sh $VERSION --push
──────────────────────────────────────────────────────────────────────────
EOF
  exit 0
fi

# =========================================================================
# FASE 2 — PUBLICAR
# =========================================================================

git fetch --tags --quiet origin 2>/dev/null || true
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  die "el tag v$VERSION YA existe. Sube el número de versión."
fi

# La clave que se publica es la de origin/$TRUNK, no la de tu copia: comprobar
# una y publicar la otra sería un chequeo que aprueba algo que no es lo que sale.
TRUNK_PUBKEY="$(git show "origin/$TRUNK:src-tauri/tauri.conf.json" 2>/dev/null \
  | sed -n 's/.*"pubkey"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p' | head -1)"
check_pubkey "$TRUNK_PUBKEY" "origin/$TRUNK"
if [[ -n "$LOCAL_PUBKEY" && -z "$TRUNK_PUBKEY" ]]; then
  die "tu copia tiene clave pública del actualizador, pero origin/$TRUNK NO.
       Publicar ahora sacaría la v$VERSION sin actualización automática."
fi

step "Taggeando v$VERSION sobre origin/$TRUNK"
git tag "v$VERSION" "origin/$TRUNK"
git push origin "v$VERSION"

cat <<EOF

──────────────────────────────────────────────────────────────────────────
✓ v$VERSION empujada.

  Build:    https://github.com/Sugestion-Studio/amautum-transcriptor/actions
  Release:  https://github.com/Sugestion-Studio/amautum-transcriptor/releases/tag/v$VERSION

Los agentes instalados se actualizarán solos cuando queden ociosos.
──────────────────────────────────────────────────────────────────────────
EOF
