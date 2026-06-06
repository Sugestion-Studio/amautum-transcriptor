import { defineConfig } from "vite"

// Tauri 2.0: el devServer corre aquí y Tauri abre la webview contra esta URL.
// El puerto debe coincidir con `build.devUrl` en tauri.conf.json.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 17172,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: true,
  },
})
