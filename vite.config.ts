import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const host = process.env.TAURI_DEV_HOST;
/**
 * 网页版构建开关：`VITE_WEB=1 pnpm build` 时把 Tauri IPC / 窗口 API
 * 换成走 HTTP 的 shim（见 src/web-shim.ts、src/web-window-shim.ts）。
 * 桌面（Tauri）构建不设这个变量，alias 为 undefined，行为完全不变。
 */
const web = process.env.VITE_WEB === "1";
const { version } = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8"),
) as { version: string };

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // 版本号只维护 package.json（CI 会按 tag 改写它），界面里的版本由这里注入，避免写死后漂移
  define: {
    __APP_VERSION__: JSON.stringify(version),
  },
  // 只在网页版构建时生效；用 fileURLToPath 得到绝对路径，别写相对路径（会按引用方解析而失效）
  resolve: web
    ? {
        alias: {
          "@tauri-apps/api/core": fileURLToPath(new URL("./src/web-shim.ts", import.meta.url)),
          "@tauri-apps/api/window": fileURLToPath(new URL("./src/web-window-shim.ts", import.meta.url)),
        },
      }
    : undefined,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
