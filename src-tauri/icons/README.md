# Iconos del agente

Coloca aquí los iconos referenciados en `tauri.conf.json`:

- `tray.png` (32×32, template-friendly: blanco con alpha para macOS)
- `32x32.png`
- `128x128.png`
- `128x128@2x.png` (256×256)
- `icon.icns` (macOS bundle)
- `icon.ico` (Windows installer)

Se pueden generar con:

```bash
npx @tauri-apps/cli icon path/to/source-1024.png
```

El comando crea todas las variantes a partir de un PNG cuadrado de 1024×1024.
