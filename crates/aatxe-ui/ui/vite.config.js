import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// The Rust crate serves the built bundle from `/assets/` and embeds it with
// `include_str!("../assets/{index.html,app.js,app.css}")`. So we build with
// fixed, unhashed filenames straight into ../assets (sibling of this dir),
// and prefix asset URLs with /assets/ to match the server routes.
export default defineConfig({
  plugins: [svelte()],
  base: "/assets/",
  build: {
    outDir: "../assets",
    emptyOutDir: true,
    assetsDir: "",
    cssCodeSplit: false,
    target: "es2022",
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        chunkFileNames: "app.js",
        assetFileNames: "app.[ext]",
      },
    },
  },
});
