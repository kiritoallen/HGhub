//! HGhub HTTP 网页版服务端。
//!
//! 复用桌面端（`src-tauri/`）的 Rust 核心，不引入 tauri：
//! * `net` / `network_policy` / `session` / `lines` / `logging` 用 `#[path]` 直接引入
//!   （这五个模块本身不依赖 tauri）；
//! * `media.rs` 复制一份，只把 `tauri::http` 换成 `http`，其余逻辑原样保留；
//! * `handlers.rs` 复刻 `src-tauri/src/api.rs` 的命令逻辑，对外暴露成 HTTP。
//!
//! 注意 `#[path]` 是相对**当前源文件所在目录**解析的：本文件在 `web-server/src/`，
//! 所以路径要写 `../../src-tauri/...`（`web-server/src/../../` == 仓库根）。
//!
//! 环境变量：
//! * `HG_WEB_PASSWORD`（**必填**）：HTTP Basic Auth 密码，用户名任意；未设置或为空则拒绝启动。
//! * `HG_WEB_BIND`（默认 `127.0.0.1:8900`）：监听地址。
//! * `HG_WEB_DIST`（默认 `./dist`）：前端 `dist/` 静态文件目录，SPA 回退到 `index.html`。
//! * `HG_WEB_DATA`（默认 `./data`）：线路 / 会话 / 安全模式 / 诊断日志的落盘目录。
//! * `HG_WEB_PLAYBACK_LOG`（可选，`1`/`true` 开启）：把播放诊断事件写到 `data/playback.log`。

#[path = "../../src-tauri/src/lines.rs"]
mod lines;
#[path = "../../src-tauri/src/logging.rs"]
mod logging;
#[path = "../../src-tauri/src/net.rs"]
mod net;
#[path = "../../src-tauri/src/network_policy.rs"]
mod network_policy;
#[path = "../../src-tauri/src/session.rs"]
mod session;

mod handlers;
mod media;
mod playback_log;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

/// 全进程共享状态：业务命令（含会话）+ 数据目录 + 网页版密码门。
pub struct AppState {
    pub api: handlers::Api,
    pub data_dir: PathBuf,
    password: String,
}

#[tokio::main]
async fn main() {
    // 密码门是唯一的前置认证：没配密码就绝不监听端口，避免把上游账号裸奔在公网。
    let password = match std::env::var("HG_WEB_PASSWORD") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            eprintln!(
                "启动失败：必须设置环境变量 HG_WEB_PASSWORD（HTTP Basic Auth 密码，用户名任意）"
            );
            std::process::exit(1);
        }
    };

    let bind = env_or("HG_WEB_BIND", "127.0.0.1:8900");
    let dist = PathBuf::from(env_or("HG_WEB_DIST", "./dist"));
    let data_dir = PathBuf::from(env_or("HG_WEB_DATA", "./data"));

    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("启动失败：无法创建数据目录 {}：{error}", data_dir.display());
        std::process::exit(1);
    }

    // 与桌面端 setup() 顺序一致：先恢复线路（含网络安全模式），再绑定诊断日志与会话。
    net::bind_base(&data_dir);
    playback_log::bind(&data_dir);
    let api = handlers::Api::new();
    api.session.bind(data_dir.clone());

    let index = dist.join("index.html");
    if !index.is_file() {
        eprintln!(
            "警告：前端入口 {} 不存在，静态页面会返回 404（请先 `VITE_WEB=1 pnpm build` 或用 HG_WEB_DIST 指定 dist 目录）",
            index.display()
        );
    }
    // SPA：命中文件就返回文件，未命中一律回退 index.html（前端自己路由）。
    // 用 fallback 而不是 not_found_service：后者会把回退响应的状态码改成 404，
    // 而单页应用直链（/search、/video/1）应该拿到 200 + index.html 才能正常渲染。
    let static_files = ServeDir::new(dist.clone()).fallback(ServeFile::new(index));

    let state = Arc::new(AppState {
        api,
        data_dir: data_dir.clone(),
        password,
    });

    let app = Router::new()
        .route("/api/cmd/{cmd}", post(handlers::cmd))
        .route("/proxy/hls", get(handlers::proxy_hls))
        .route("/proxy/seg", get(handlers::proxy_seg))
        .route("/proxy/img", get(handlers::proxy_img))
        .fallback_service(static_files)
        // 密码门覆盖全部路由：业务 API、/proxy/* 与静态资源都要先过 Basic Auth。
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("启动失败：无法监听 {bind}：{error}");
            std::process::exit(1);
        }
    };

    println!(
        "HGhub web server 已启动：http://{bind}（dist={}，data={}，线路={}）",
        dist.display(),
        data_dir.display(),
        net::base()
    );
    if let Err(error) = axum::serve(listener, app).await {
        eprintln!("服务异常退出：{error}");
        std::process::exit(1);
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// 所有路由的 Basic Auth 中间件：用户名任意，只比较密码。
async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(request.headers(), &state.password) {
        next.run(request).await
    } else {
        unauthorized()
    }
}

fn unauthorized() -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, "需要认证（HTTP Basic Auth）").into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"HGhub\""),
    );
    response
}

/// 解析 `Authorization: Basic base64(user:password)`；用户名不校验。
fn authorized(headers: &HeaderMap, password: &str) -> bool {
    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, encoded)) = raw.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let Ok(decoded) = B64.decode(encoded.trim()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((_user, candidate)) = text.split_once(':') else {
        return false;
    };
    constant_time_eq(candidate.as_bytes(), password.as_bytes())
}

/// 常量时间比较：长度不同也要把较长的那个走完，避免按字节提前返回。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: u8 = if a.len() == b.len() { 0 } else { 1 };
    let len = a.len().max(b.len());
    for index in 0..len {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_only_checks_the_password() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Basic {}", B64.encode("anyone:s3cret"))
                .parse::<HeaderValue>()
                .unwrap(),
        );
        assert!(authorized(&headers, "s3cret"));
        assert!(!authorized(&headers, "s3cret2"));
        assert!(!authorized(&headers, "s3cre"));
        assert!(!authorized(&headers, ""));
    }

    #[test]
    fn malformed_authorization_headers_are_rejected() {
        for raw in [
            "Basic",
            "Basic !!!not-base64!!!",
            "Bearer abcdef",
            "Basic dXNlcg==", // 没有冒号
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                HeaderValue::from_static(raw),
            );
            assert!(!authorized(&headers, "x"), "{raw}");
        }
        assert!(!authorized(&HeaderMap::new(), "x"));
    }
}
