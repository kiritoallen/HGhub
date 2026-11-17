# HGhub 网页版服务端（web-server）

把桌面端（`src-tauri/`）的 Rust 核心原样复用，对外提供一套 HTTP 接口 + 同源媒体代理，
前端仍是同一份 React 代码（构建时用 `VITE_WEB=1` 把 Tauri IPC 换成 fetch）。

```
浏览器 ──Basic Auth──> hghub-web ──reqwest──> 上游站点（huangguoai.com / .ai / 镜像）
                         │
                         ├── /api/cmd/<cmd>   复刻 src-tauri/src/api.rs 的命令
                         ├── /proxy/hls|seg|img  复用 src-tauri/src/media.rs
                         └── /               前端 dist/（SPA 回退 index.html）
```

## 复用了什么

| 文件 | 方式 |
| --- | --- |
| `src-tauri/src/net.rs` | `#[path]` 引入（无 tauri 依赖） |
| `src-tauri/src/network_policy.rs` | `#[path]` 引入 |
| `src-tauri/src/session.rs` | `#[path]` 引入 |
| `src-tauri/src/lines.rs` | `#[path]` 引入 |
| `src-tauri/src/logging.rs` | `#[path]` 引入 |
| `src-tauri/src/media.rs` | 复制到 `web-server/src/media.rs`，仅把 `tauri::http` 换成 `http` |
| `src-tauri/src/playback_log.rs` | 写成本地 stub（`enabled/resource_id/record/error_kind/bind` + 四个日志命令） |

`handlers.rs` 逐个复刻了桌面端命令：`app_boot`、`security_set`、`api_login`、`api_register`、
`api_logout`、`api_me`、`api_feed`、`api_category`、`api_rank`、`api_search`、`api_posts`、
`api_post`、`api_video`、`api_play`（含 `normalize_play_info` / `has_play_url`）、`api_comments`、
`api_comment_send`、`api_like`、`api_collect`、`api_follow`、`api_post_like`、
`api_post_collect`、`api_post_comments`、`cache_clear`、`line_state`、`line_set`、`line_mirrors`、
`line_probe`、`check_update`、`net_diag`、`debug_log`。

## 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `HG_WEB_PASSWORD` | **必填** | HTTP Basic Auth 密码，用户名任意。未设置或为空直接拒绝启动 |
| `HG_WEB_BIND` | `127.0.0.1:8900` | 监听地址；要对外提供服务时改成 `0.0.0.0:8900` |
| `HG_WEB_DIST` | `./dist` | 前端构建产物目录 |
| `HG_WEB_DATA` | `./data` | 线路 / 会话 / 安全模式 / 诊断日志落盘目录 |
| `HG_WEB_PLAYBACK_LOG` | 未开启 | 设为 `1` 时把播放诊断事件写进 `data/playback.log` |
| `HGHUB_PROXY` | 未设置 | 强制指定出口代理（排查用，例如 `http://127.0.0.1:7897`）；不设则用系统代理 |

## 构建

前端（仓库根目录）：

```bash
VITE_WEB=1 pnpm install --frozen-lockfile
VITE_WEB=1 pnpm build          # 产出 dist/
```

服务端：

```bash
cargo build --release --manifest-path web-server/Cargo.toml
# 产物：web-server/target/release/hghub-web（Windows 为 hghub-web.exe）
```

CI（`.github/workflows/web.yml`）在 push 到分支 `web` 时同时产出：

* `web-server-linux-x64`：`hghub-web` + `dist/`
* `web-server-windows-x64`：`hghub-web.exe` + `dist/`（本地验证用）

## 运行

```bash
HG_WEB_PASSWORD='change-me' \
HG_WEB_BIND='127.0.0.1:8900' \
HG_WEB_DIST='./dist' \
HG_WEB_DATA='./data' \
./web-server/target/release/hghub-web
```

Windows（PowerShell）：

```powershell
$env:HG_WEB_PASSWORD='change-me'
$env:HG_WEB_BIND='127.0.0.1:8900'
$env:HG_WEB_DIST='.\dist'
$env:HG_WEB_DATA='.\data'
.\hghub-web.exe
```

快速自检：

```bash
curl -i http://127.0.0.1:8900/                                  # 401 + WWW-Authenticate
curl -u 'any:change-me' -i http://127.0.0.1:8900/                # 200 HTML
curl -u 'any:change-me' -X POST -H 'Content-Type: application/json' \
     -d '{}' http://127.0.0.1:8900/api/cmd/app_boot              # 200 JSON，含 base
curl -u 'any:change-me' -X POST -H 'Content-Type: application/json' \
     -d '{"page":1,"size":20}' http://127.0.0.1:8900/api/cmd/api_feed
```

### systemd（示例）

```ini
[Unit]
Description=HGhub web
After=network-online.target

[Service]
User=hghub
WorkingDirectory=/opt/hghub
Environment=HG_WEB_BIND=127.0.0.1:8900
Environment=HG_WEB_DIST=/opt/hghub/dist
Environment=HG_WEB_DATA=/opt/hghub/data
EnvironmentFile=/etc/hghub/web.env   # 里面放 HG_WEB_PASSWORD=...
ExecStart=/opt/hghub/hghub-web
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

## 安全说明

* **密码门**：`/`、`/api/cmd/*`、`/proxy/*` 全部经过 Basic Auth；401 一定带
  `WWW-Authenticate: Basic realm="HGhub"`，浏览器才会弹登录框。密码比较是常量时间实现，
  用户名不校验。
* 请把服务放在 HTTPS 反向代理之后（Basic Auth 是明文口令），并**不要**把
  `HG_WEB_PASSWORD` 写进仓库 / 镜像 / 日志。
* 上游目标校验、DNS 固定、重定向逐跳检查、私网拒绝都沿用桌面端的 `net.rs` /
  `network_policy.rs`，网页版没有放宽任何一条。
* 镜像线路仍然只允许游客浏览：登录 / 注册 / 带 token 的请求在严格模式下会被拒绝
  （与桌面端一致）。
* 会话落盘在 `HG_WEB_DATA/session.json`，内容是上游 token；该目录不要公开。
