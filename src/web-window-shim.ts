/**
 * 网页版窗口 API shim。
 *
 * 桌面壳（DesktopChrome 标题栏、Windows 无边框缩放）只用到 `getCurrentWindow()` 返回对象上的
 * 少量方法；浏览器里没有原生窗口可操作，所以全部是空操作 —— 按钮点了不会崩，
 * 只是不会真的最小化 / 最大化 / 关闭标签页。
 *
 * 只在 `VITE_WEB=1` 构建时由 vite.config.ts 的 alias 顶替 `@tauri-apps/api/window`。
 * 注意 `onResized` 等订阅方法必须 resolve 成 unlisten 函数（现有代码会存起来在卸载时调用）。
 */

const noop = async (): Promise<void> => {};

const unlisten = async (): Promise<() => void> => () => {};

const currentWindow = {
  label: "main",
  isMaximized: async () => false,
  isFullscreen: async () => false,
  isFocused: async () => true,
  isVisible: async () => true,
  isMinimized: async () => false,
  isDecorated: async () => false,
  isResizable: async () => false,
  title: async () => "HGhub",
  theme: async () => "dark" as const,
  scaleFactor: async () => 1,
  innerPosition: async () => ({ x: 0, y: 0 }),
  outerPosition: async () => ({ x: 0, y: 0 }),
  innerSize: async () => ({ width: 0, height: 0 }),
  outerSize: async () => ({ width: 0, height: 0 }),
  onResized: unlisten,
  onMoved: unlisten,
  onFocusChanged: unlisten,
  onCloseRequested: unlisten,
  onScaleChanged: unlisten,
  onThemeChanged: unlisten,
  minimize: noop,
  maximize: noop,
  unmaximize: noop,
  toggleMaximize: noop,
  hide: noop,
  show: noop,
  close: noop,
  destroy: noop,
  setFocus: noop,
  setFullscreen: noop,
  setResizable: noop,
  setTitle: noop,
  setTheme: noop,
  setAlwaysOnTop: noop,
  setDecorations: noop,
  startDragging: noop,
  startResizeDragging: noop,
};

/** 与 `@tauri-apps/api/window` 的 `getCurrentWindow()` 兼容。 */
export function getCurrentWindow() {
  return currentWindow;
}

export function getAllWindows() {
  return [currentWindow];
}

/** 旧版 API 的别名（`appWindow`）。 */
export const appWindow = currentWindow;

export default { getCurrentWindow, getAllWindows, appWindow };
