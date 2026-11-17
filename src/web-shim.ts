/**
 * 网页版 IPC shim。
 *
 * 桌面端前端用 `invoke(cmd, args)` 走 Tauri IPC；网页版没有 IPC，这里把它映射到
 *   POST /api/cmd/<cmd>   body = JSON 参数对象
 * 服务端（web-server/src/handlers.rs）复刻了桌面端同名命令的逻辑，返回 JSON；
 * 非 2xx 时把响应体文本当作错误信息抛出 —— 与 Tauri `invoke` reject 一个字符串一致，
 * 所以 src/api.ts 里 `catch (e) { typeof e === "string" ... }` 的分支不用改。
 *
 * 只在 `VITE_WEB=1` 构建时由 vite.config.ts 的 alias 顶替 `@tauri-apps/api/core`；
 * 桌面 / 移动端构建仍然使用官方实现。
 */

type InvokeArgs = Record<string, unknown> | undefined;

/** 宿主插件命令（`plugin:xxx|yyy`）在网页版没有对应实现，按空操作处理。 */
const isHostCommand = (cmd: string) => cmd.startsWith("plugin:");

async function request(cmd: string, args?: InvokeArgs): Promise<unknown> {
  const response = await fetch(`/api/cmd/${encodeURIComponent(cmd)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(args ?? {}),
    credentials: "same-origin",
  });
  const text = await response.text();
  if (!response.ok) {
    const message = text.trim();
    throw message || `HTTP ${response.status}`.trim();
  }
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

export async function invoke<T = unknown>(cmd: string, args?: InvokeArgs): Promise<T> {
  if (isHostCommand(cmd)) return null as T;
  return (await request(cmd, args)) as T;
}

/** 网页版没有原生事件通道：返回一个可以 unregister 的假句柄，避免调用方崩溃。 */
export async function addPluginListener(
  _plugin: string,
  _event: string,
  _handler: (...args: unknown[]) => void,
): Promise<{ unregister: () => Promise<void> }> {
  return { unregister: async () => {} };
}

/** 与 `@tauri-apps/api/core` 同名导出的空实现（现有代码未引用，保留以防宿主差异）。 */
export const isTauri = false;

export const convertFileSrc = (filePath: string) => filePath;

export const transformCallback = (_callback?: unknown, _once?: boolean) => 0;

export default { invoke, addPluginListener, isTauri, convertFileSrc, transformCallback };
