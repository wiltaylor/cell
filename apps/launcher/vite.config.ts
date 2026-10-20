import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Tauri serves the built frontend; in dev it loads the Vite server.
export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: { port: 5173, strictPort: true, fs: { allow: [".."] } },
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
});
