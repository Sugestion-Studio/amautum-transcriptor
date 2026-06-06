# Iconos del agente

Iconos generados a partir de `icon-source.png` (en la raíz del repo) con:

```bash
npx @tauri-apps/cli icon icon-source.png --output src-tauri/icons
cp src-tauri/icons/32x32.png src-tauri/icons/tray.png
```

Para reemplazarlos por arte nuevo: pega un PNG cuadrado de 1024×1024 como
`icon-source.png` y vuelve a correr esos dos comandos.
