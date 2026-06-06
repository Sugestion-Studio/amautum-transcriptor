# Sidecar binaries

Esta carpeta NO se versiona (ver `agent/.gitignore`). El agente espera dos binarios sidecar resueltos por Tauri, más una carpeta `models/` con el modelo GGML que se quiera usar.

## Convención de nombres (Tauri sidecar)

Tauri busca los binarios con el sufijo de la triple de Rust. Por ejemplo, en macOS arm64 el binario debe llamarse:

```
binaries/whisper-cli-aarch64-apple-darwin
binaries/ffmpeg-aarch64-apple-darwin
```

En Linux x86_64:

```
binaries/whisper-cli-x86_64-unknown-linux-gnu
binaries/ffmpeg-x86_64-unknown-linux-gnu
```

En Windows x86_64:

```
binaries/whisper-cli-x86_64-pc-windows-msvc.exe
binaries/ffmpeg-x86_64-pc-windows-msvc.exe
```

Averigua tu triple con:

```bash
rustc -vV | grep host
```

## whisper.cpp

1. Clona y compila whisper.cpp con soporte de Metal (macOS), CUDA (NVIDIA) o CPU.

   ```bash
   git clone https://github.com/ggerganov/whisper.cpp
   cd whisper.cpp
   # macOS con Metal (default):
   cmake -B build && cmake --build build --config Release
   # CUDA:
   cmake -B build -DGGML_CUDA=1 && cmake --build build --config Release
   ```

2. Copia el binario `main` (o `whisper-cli` en versiones recientes) a esta carpeta con el sufijo del target.

## ffmpeg

Descarga un build estático desde:

- macOS / Linux: <https://www.ffmpeg.org/download.html> (o `brew install ffmpeg` y copia el binario).
- Windows: <https://www.gyan.dev/ffmpeg/builds/>.

Copia el binario aquí con el sufijo del target.

## Modelos GGML

Coloca el modelo elegido en `binaries/models/`. El agente busca por nombre:

| Modelo solicitado | Archivo esperado          | Tamaño aprox. |
| ----------------- | ------------------------- | ------------- |
| `tiny`            | `ggml-tiny.bin`           | 75 MB         |
| `base`            | `ggml-base.bin`           | 142 MB        |
| `small`           | `ggml-small.bin`          | 466 MB        |
| `medium`          | `ggml-medium.bin`         | 1.5 GB        |
| `large-v3`        | `ggml-large-v3.bin`       | 3.1 GB        |

Descarga oficial:

```bash
mkdir -p binaries/models
bash whisper.cpp/models/download-ggml-model.sh medium ./binaries/models
```

Los modelos NO se incluyen en el bundle por defecto (son demasiado grandes). El instalador del agente puede:

- Bajarlos al primer arranque (recomendado para `large-v3`).
- O incluir `tiny` / `base` directamente como `resources` en `tauri.conf.json`.
