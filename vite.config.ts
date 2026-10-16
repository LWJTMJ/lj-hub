import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// LJ Hub frontend build. The dev server port is fixed so that the Tauri
// window can always find it during development.
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "safari15",
    sourcemap: false,
  },
});
