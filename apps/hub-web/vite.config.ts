import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 5174,
    proxy: { "/api": { target: "http://127.0.0.1:8787", ws: true } },
    fs: { allow: [".."] },
  },
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
});
