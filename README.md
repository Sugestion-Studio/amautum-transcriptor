# Amautum Transcriptor

> Aplicación de escritorio que transcribe audiencias y declaraciones en la **propia máquina del estudio jurídico**. El audio nunca sale del equipo del cliente; solo el texto sincronizado se sube a Amautum cuando termina.

[![Última versión](https://img.shields.io/github/v/release/Sugestion-Studio/amautum-transcriptor?label=descarga&color=brightgreen)](https://github.com/Sugestion-Studio/amautum-transcriptor/releases/latest)
[![Licencia MIT](https://img.shields.io/badge/licencia-MIT-blue.svg)](./LICENSE)

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

Usa siempre la última versión publicada. No hace falta que lo vigiles: la app
consulta si hay una nueva y te ofrece el instalador de tu sistema en cuanto
aparece.

### macOS: aviso "could not verify… is free of malware"

Como esta versión del agente todavía no está firmada con Developer ID de Apple, macOS Sequoia (15+) la bloquea por defecto. Es procedimiento estándar para apps de desarrolladores independientes — **no significa que la app sea dañina**. Para autorizarla (~4 clics, solo una vez por versión):

1. Mueve `Amautum Transcriptor.app` a la carpeta Aplicaciones.
2. Quita la etiqueta de cuarentena desde Terminal:

   ```bash
   xattr -cr "/Applications/Amautum Transcriptor.app"
   ```

3. Doble clic sobre la app. Aparece el aviso "could not verify…". Haz clic en **Done** (no en "Move to Trash").
4. Abre System Settings → Privacy & Security (atajo desde Terminal):

   ```bash
   open "x-apple.systempreferences:com.apple.preference.security?General"
   ```

5. Baja hasta la sección **Security**. Verás el mensaje "Amautum Transcriptor was blocked…" y un botón **Open Anyway**. Clic, autentica con Touch ID o contraseña.
6. Doble clic la app de nuevo. Un diálogo más suave aparece: "macOS cannot verify the developer… Are you sure you want to open?". Clic **Open**.

A partir de ahora abre con doble clic limpio — esta versión queda whitelisteada en tu Mac para siempre. Solo tendrás que repetir el ritual cuando salga una versión nueva.

> La firma con Developer ID de Apple está en nuestra hoja de ruta — se añadirá en una próxima versión y este aviso desaparecerá. Mientras tanto, el código fuente de este repositorio te da garantías técnicas de privacidad que ninguna firma de Apple aporta.

### Windows: SmartScreen "Windows protegió tu PC"

Es el equivalente: como el `.msi` todavía no está firmado, SmartScreen pregunta. Haz clic en **Más información** → **Ejecutar de todos modos**. La firma con certificado EV está en nuestra hoja de ruta.

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

## Desarrollo local

### Requisitos

- Rust 1.77+ (`rustup default stable`)
- Node 20+
- En macOS: Xcode Command Line Tools (`xcode-select --install`)
- En Linux: [prerrequisitos de Tauri](https://tauri.app/start/prerequisites/)

### Puesta en marcha

```bash
git clone https://github.com/Sugestion-Studio/amautum-transcriptor.git
cd amautum-transcriptor
npm install

bash scripts/setup-dev-sidecars.sh   # coloca whisper-cli y ffmpeg
npm run tauri:dev
```

#### Los binarios sidecar

El agente no transcribe por su cuenta: llama a `whisper-cli` (de whisper.cpp) y a
`ffmpeg`. Esos binarios **no están en el repo** —pesan decenas de MB y son
distintos por plataforma—, así que hay que colocarlos una vez. Sin ellos el build
ni siquiera arranca:

```
resource path `whisper-cli-aarch64-apple-darwin` doesn't exist
```

`setup-dev-sidecars.sh` los toma de tu instalación de Amautum Transcriptor si la
tienes —son exactamente los que usa la app publicada— o de tu sistema.

> **Dónde van.** Tauri los busca **planos**, en
> `src-tauri/<nombre>-<triple>[.exe]`, no dentro de `src-tauri/binaries/`.
> Ponerlos en la subcarpeta es un error fácil y el mensaje no lo aclara.
> El triple sale de `rustc -vV | grep host`.

`sherpa-diarize` (identificación de interlocutores) es **opcional**: sin él el
agente arranca igual y solo falla si un trabajo pide diarización.

### ⚠️ Cierra la app instalada antes de arrancar

Si tienes Amautum Transcriptor instalado, **sal desde el icono de la bandeja**
antes de `npm run tauri:dev`.

Las dos versiones escuchan en el puerto 17173. Si la instalada arranca primero se
queda el puerto, y **verás la versión publicada creyendo que ves la tuya**: tus
cambios parecerán no aplicarse por mucho que recompiles. Comprobar quién tiene el
puerto:

```bash
lsof -nP -iTCP:17173 -sTCP:LISTEN
```

En macOS hay un agravante: `open -b com.amautum.transcriptor.agent` (o cualquier
cosa que active por *bundle id*) **lanza la app instalada**, no la de desarrollo.
Para traer al frente la tuya, muéstrala desde su propio icono de bandeja.

### Cómo se ve que va bien

```bash
curl -s localhost:17173/health
```

`dependenciesOk: true` significa que los dos sidecars respondieron. Si sale
`false`, `dependenciesError` dice cuál falló y por qué.

Para el ciclo completo contra la web (crear un trabajo, ver el progreso), hace
falta Amautum corriendo en `localhost:3000`: el agente solo acepta peticiones
desde los orígenes de `ALLOWED_ORIGINS` en `src-tauri/src/config.rs`.

### Compilar los instaladores

```bash
npm run tauri:build
```

Quedan en `src-tauri/target/release/bundle/`. Para **publicar** una versión, no
compiles a mano: usa `bash scripts/release.sh X.Y.Z`, que versiona, prueba,
taggea y deja que el CI compile las cuatro plataformas.

## Privacidad y seguridad

- ✅ El audio nunca sale del equipo donde corre la app.
- ✅ La comunicación entre el navegador y la app local usa CORS estricto: solo `amautum.com` puede hablar con ella.
- ✅ Cada trabajo recibe un token de un solo uso firmado por Amautum; sin sesión activa el agente no puede subir nada.
- ✅ Código abierto bajo licencia MIT — audita lo que quieras.

## Soporte

- **Usuarios finales**: abre un ticket desde [Amautum → Soporte](https://www.amautum.com/dashboard/support). Queda con historial y te respondemos al correo de tu cuenta; un correo suelto se pierde. La propia app te lleva ahí desde el botón «Soporte», y si un trabajo falla el enlace aparece junto al error.

  Antes de escribir, pulsa **«Copiar diagnóstico»** en la ventana de la app y pega el resultado en el ticket: trae versión, sistema, estado de los componentes y bitácora. Con eso casi siempre basta para responder a la primera.

- **Sin cuenta de Amautum** (por ejemplo, un técnico que instala la app): escribe a [support@amautum.com](mailto:support@amautum.com).
- **Administradores de TI**: abre un [issue](https://github.com/Sugestion-Studio/amautum-transcriptor/issues) en este repo para dudas técnicas del código o del empaquetado.
- **Reporte de seguridad**: escribe en privado a [support@amautum.com](mailto:support@amautum.com) poniendo **[SEGURIDAD]** al inicio del asunto. No abras un issue público si encuentras una vulnerabilidad.

## Licencia

[MIT](./LICENSE) © Sugestion S.A.S
