# Amautum Transcriptor

> Aplicación de escritorio que transcribe audiencias y declaraciones en la **propia máquina del estudio jurídico**. El audio nunca sale del equipo del cliente; solo el texto sincronizado se sube a Amautum cuando termina.

[![Latest release](https://img.shields.io/github/v/release/Sugestion-Studio/amautum-transcriptor?label=descarga)](https://github.com/Sugestion-Studio/amautum-transcriptor/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

## ¿Qué es esto?

Este repositorio aloja el **código fuente y los instaladores oficiales** del agente de escritorio que acompaña a [Amautum Transcriptor](https://www.amautum.com/transcriptor) — la línea de Amautum para estudios jurídicos.

El agente se distribuye públicamente para que los estudios puedan **auditar el código** (verificar con sus propios ojos que el audio nunca sale de su máquina) y para que los administradores de TI puedan **firmar y empaquetar** sus propias compilaciones internas si lo requieren.

## Descargar

Ve a la [última release](https://github.com/Sugestion-Studio/amautum-transcriptor/releases/latest) y elige tu sistema operativo:

| Plataforma | Archivo | Para quién |
| --- | --- | --- |
| macOS — chip Apple | `AmautumTranscriptor-<version>-macos-arm64.dmg` | Mac M1/M2/M3/M4 o superior (mayoría desde 2021) |
| macOS — chip Intel | `AmautumTranscriptor-<version>-macos-x64.dmg` | Mac de antes de 2021 con procesador Intel |
| Windows 10 / 11 | `AmautumTranscriptor-<version>-windows-x64.msi` | PC 64 bits con Windows 10 o 11 |
| Linux — AppImage | `AmautumTranscriptor-<version>-linux-x64.AppImage` | Cualquier distribución moderna |
| Linux — .deb | `AmautumTranscriptor-<version>-linux-x64.deb` | Ubuntu, Debian, Mint y derivados |

> Para una guía paso a paso de instalación (con los avisos típicos de cada sistema operativo), visita [amautum.com/downloads/transcriptor](https://www.amautum.com/downloads/transcriptor).

### macOS: aviso "está dañada y no se puede abrir"

Mientras no tengamos firma con Developer ID de Apple, macOS rechazará la app con ese mensaje **falsa alarma**. Para autorizarla:

1. Mueve `Amautum Transcriptor.app` a la carpeta Aplicaciones.
2. Abre Terminal y pega:

   ```bash
   xattr -cr "/Applications/Amautum Transcriptor.app"
   ```

3. Abre la app con doble clic desde Aplicaciones. A partir de ahora se comporta como cualquier otra.

Si te pide contraseña, usa: `sudo xattr -dr com.apple.quarantine "/Applications/Amautum Transcriptor.app"`.

### Windows: SmartScreen "Windows protegió tu PC"

Es el equivalente: como el `.msi` no está firmado, SmartScreen pregunta. Haz clic en **Más información** → **Ejecutar de todos modos**. La firma con certificado EV está en la lista de cosas por hacer (ver `.github/workflows/release.yml`).

## ¿Cómo funciona?

```
┌────────────────┐    ┌───────────────────┐    ┌─────────────────┐
│   amautum.com  │ ──►│  Agente local     │ ──►│   amautum.com   │
│   (navegador)  │    │  (esta app)       │    │   (API)         │
│                │    │                   │    │                 │
│ "Transcribe    │    │ 1. Pide ruta del  │    │ Recibe el       │
│  esta audien-  │    │    audio          │    │ texto +         │
│  cia"          │    │ 2. Convierte el   │    │ marcas de       │
│                │    │    audio en texto │    │ tiempo y lo     │
│ Muestra        │    │    EN TU EQUIPO   │    │ guarda en el    │
│ progreso en    │    │ 3. Sube el texto  │    │ expediente.     │
│ tiempo real    │    │                   │    │                 │
└────────────────┘    └───────────────────┘    └─────────────────┘
```

El audio **nunca abandona la computadora** donde corre el agente. Lo único que viaja por internet es el JSON con el texto y las marcas de tiempo de cada frase.

## Compilar desde el código fuente

Si quieres auditar el binario o producir tu propia compilación firmada:

### Requisitos

- Rust 1.77+ (`rustup default stable`)
- Node 20+
- En macOS: Xcode Command Line Tools (`xcode-select --install`)
- En Linux: ver [prerrequisitos de Tauri](https://tauri.app/v1/guides/getting-started/prerequisites)

### Pasos

```bash
git clone https://github.com/Sugestion-Studio/amautum-transcriptor.git
cd amautum-transcriptor
npm install

# Necesitas dos binarios "sidecar" antes de compilar:
#   - whisper-cli (de whisper.cpp)
#   - ffmpeg
# Mira src-tauri/binaries/README.md para las instrucciones detalladas.

npm run tauri:build
```

Los instaladores quedan en `src-tauri/target/release/bundle/`.

## Privacidad y seguridad

- ✅ El audio nunca sale del equipo donde corre la app.
- ✅ La comunicación entre el navegador y la app local usa CORS estricto: solo `amautum.com` puede hablar con ella.
- ✅ Cada trabajo recibe un token de un solo uso firmado por Amautum; sin sesión activa el agente no puede subir nada.
- ✅ Código abierto bajo licencia MIT — auditá lo que quieras.

## Releases

Los releases se publican automáticamente en este repositorio cuando se empuja un tag `v*` (ver [`.github/workflows/release.yml`](.github/workflows/release.yml)).

Para usuarios finales: [usa siempre la última versión publicada](https://github.com/Sugestion-Studio/amautum-transcriptor/releases/latest).

Para administradores de TI: cada release viene con sus checksums SHA-256 y, cuando aplica, con la firma del código de Sugestion S.A.S

## Soporte

- **Usuarios finales**: contacta a tu administrador del estudio o escribe a [soporte@amautum.com](mailto:soporte@amautum.com).
- **Administradores de TI**: abre un [issue](https://github.com/Sugestion-Studio/amautum-transcriptor/issues) en este repo.
- **Reporte de seguridad**: escribe en privado a [security@amautum.com](mailto:security@amautum.com). No abras un issue público si encuentras una vulnerabilidad.

## Licencia

[MIT](./LICENSE) © Sugestion S.A.S
