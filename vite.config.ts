import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Port 1430 to avoid clashing with DevDock's vite on 1420.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1430,
    strictPort: true,
    host: "0.0.0.0",
    watch: {
      // Don't watch the Rust build output; its files lock mid-build and
      // crash Vite's watcher on Windows (EBUSY).
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "es2021",
    outDir: "dist",
  },
});
