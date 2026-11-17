//! 业务命令层：复刻 `src-tauri/src/api.rs` 的命令逻辑，但不依赖 tauri。
//!
//! 桌面端前端用 `invoke(cmd, args)` 走 Tauri IPC；网页版用
//! `POST /api/cmd/<cmd>`（body 为 JSON 参数对象）走 HTTP，语义保持一致：
//! 成功返回 JSON，失败返回 400/500 + 纯文本 —— 桌面端 `invoke` 失败时 reject 的
//! 值就是 `Err(String)`，所以这段文本正好是前端 `catch` 里看到的那条错误信息。

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderValue, Response, StatusCode};
use axum::response::Response as AxumResponse;
use serde_json::{json, Value};

use crate::net::{base, edge};
use crate::session::SessionStore;
use crate::AppState;

/// 与 `src-tauri/src/api.rs` 的 `Api` 一致：会话 + get/post/authed 三个入口。
pub struct Api {
    pub session: SessionStore,
}

impl Api {
    pub fn new() -> Self {
        Self {
            session: SessionStore::new(),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let origin = base();
        let token = self.session.token(&origin);
        edge(&origin, "GET", path, None, token.as_deref()).await
    }

    /// POST（游客也可调用，有登录态时带上 token）：新播放接口需要。
    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        let origin = base();
        let token = self.session.token(&origin);
        edge(&origin, "POST", path, body, token.as_deref()).await
    }

    async fn authed(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let origin = base();
        let token = self
            .session
            .token(&origin)
            .ok_or_else(|| "请先登录".to_string())?;
        edge(&origin, method, path, body, Some(&token)).await
    }
}

/* ───────────────────────── HTTP 入口 ───────────────────────── */

/// `POST /api/cmd/{cmd}`：body 是命令参数的 JSON 对象。
pub async fn cmd(
    State(state): State<Arc<AppState>>,
    Path(command): Path<String>,
    body: Bytes,
) -> AxumResponse {
    let args: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Null) => json!({}),
            Ok(value) => value,
            Err(error) => {
                return plain(
                    StatusCode::BAD_REQUEST,
                    format!("请求体不是合法 JSON：{error}"),
                )
            }
        }
    };
    match dispatch(&state, &command, &args).await {
        Ok(value) => json_response(&value),
        Err(message) => plain(status_for(&message), message),
    }
}

pub async fn proxy_hls(
    State(_state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> AxumResponse {
    proxy("/hls", query).await
}

pub async fn proxy_seg(
    State(_state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> AxumResponse {
    proxy("/seg", query).await
}

pub async fn proxy_img(
    State(_state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> AxumResponse {
    proxy("/img", query).await
}

/// 复用 `media::handle`：路径 `hls/seg/img` + 原始查询串 + 改写前缀 `/proxy`。
async fn proxy(path: &'static str, query: Option<String>) -> AxumResponse {
    let response = crate::media::handle(path, query.as_deref().unwrap_or(""), "/proxy").await;
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::from(body))
}

fn json_response(value: &Value) -> AxumResponse {
    let payload = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    let mut response = Response::new(Body::from(payload));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn plain(status: StatusCode, message: String) -> AxumResponse {
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// 参数缺失 / 未知命令 → 400；本地资源（数据目录、日志）故障 → 500。
fn status_for(message: &str) -> StatusCode {
    if message.starts_with("无法")
        || message.contains("锁失败")
        || message.contains("尚未初始化")
    {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::BAD_REQUEST
    }
}

/* ───────────────────────── 参数读取 ───────────────────────── */

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("缺少或无效的参数：{key}"))
}

fn arg_opt_str(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn arg_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .ok_or_else(|| format!("缺少或无效的参数：{key}"))
}

fn arg_opt_i64(args: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        args.get(*key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
    })
}

fn arg_u32(args: &Value, key: &str) -> Result<u32, String> {
    args.get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .filter(|value| *value <= u32::MAX as u64)
        .map(|value| value as u32)
        .ok_or_else(|| format!("缺少或无效的参数：{key}"))
}

fn arg_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("缺少或无效的参数：{key}"))
}

fn arg_str_vec(args: &Value, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .ok_or_else(|| format!("缺少或无效的参数：{key}"))
}

/* ───────────────────────── 命令分发 ───────────────────────── */

pub async fn dispatch(state: &AppState, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "app_boot" => app_boot(&state.api).await,
        "security_set" => security_set(&state.api, arg_bool(args, "strict")?).await,
        "api_login" => {
            api_login(
                &state.api,
                arg_str(args, "username")?,
                arg_str(args, "password")?,
            )
            .await
        }
        "api_register" => {
            api_register(
                &state.api,
                arg_str(args, "username")?,
                arg_str(args, "password")?,
                arg_opt_str(args, &["email"]).unwrap_or_default(),
            )
            .await
        }
        "api_logout" => api_logout(&state.api).await,
        "api_me" => api_me(&state.api).await,
        "api_feed" => {
            api_feed(
                &state.api,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "api_category" => {
            api_category(
                &state.api,
                arg_str(args, "cate")?,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "api_rank" => api_rank(&state.api, arg_str(args, "kind")?).await,
        "api_search" => {
            api_search(
                &state.api,
                arg_str(args, "q")?,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "api_posts" => {
            api_posts(
                &state.api,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "api_post" => api_post(&state.api, arg_i64(args, "id")?).await,
        "api_video" => api_video(&state.api, arg_i64(args, "id")?).await,
        "api_play" => {
            api_play(
                &state.api,
                arg_i64(args, "id")?,
                arg_u32(args, "ep")?,
            )
            .await
        }
        "api_comments" => {
            api_comments(
                &state.api,
                arg_i64(args, "id")?,
                arg_u32(args, "ep")?,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "api_comment_send" => {
            api_comment_send(
                &state.api,
                arg_i64(args, "id")?,
                arg_u32(args, "ep")?,
                arg_str(args, "text")?,
                // Tauri 会把 JS 的 replyId 映射到 Rust 的 reply_id；HTTP 层两种写法都收。
                arg_opt_i64(args, &["reply_id", "replyId"]),
            )
            .await
        }
        "api_like" => {
            api_like(
                &state.api,
                arg_i64(args, "id")?,
                arg_u32(args, "ep")?,
                arg_bool(args, "on")?,
            )
            .await
        }
        "api_collect" => {
            api_collect(
                &state.api,
                arg_i64(args, "id")?,
                arg_bool(args, "on")?,
            )
            .await
        }
        "api_follow" => {
            api_follow(
                &state.api,
                arg_i64(args, "uid")?,
                arg_bool(args, "on")?,
            )
            .await
        }
        "api_post_like" => {
            api_post_like(
                &state.api,
                arg_i64(args, "id")?,
                arg_bool(args, "on")?,
            )
            .await
        }
        "api_post_collect" => {
            api_post_collect(
                &state.api,
                arg_i64(args, "id")?,
                arg_bool(args, "on")?,
            )
            .await
        }
        "api_post_comments" => {
            api_post_comments(
                &state.api,
                arg_i64(args, "id")?,
                arg_u32(args, "page")?,
                arg_u32(args, "size")?,
            )
            .await
        }
        "cache_clear" => {
            crate::media::clear_caches();
            Ok(Value::Null)
        }
        "line_state" => line_state().await,
        "line_set" => line_set(&state.api, arg_str(args, "url")?).await,
        "line_mirrors" => line_mirrors().await,
        "line_probe" => line_probe(arg_str_vec(args, "urls")?).await,
        "check_update" => check_update().await,
        "net_diag" => net_diag().await,
        "debug_log" => debug_log(state, arg_str(args, "msg")?).await,
        "playback_log_status" => Ok(crate::playback_log::status()),
        "playback_log_set" => crate::playback_log::set(arg_bool(args, "enabled")?),
        "playback_log_write" => {
            crate::playback_log::write(args.get("entries").cloned().unwrap_or(Value::Null))?;
            Ok(Value::Null)
        }
        "playback_log_read" => {
            let text = crate::playback_log::read()?;
            Ok(json!(text))
        }
        _ => Err(format!("未知命令：{command}")),
    }
}

/* ───────────────────────── 会话 / 自检 ───────────────────────── */

pub async fn app_boot(api: &Api) -> Result<Value, String> {
    let origin = base();
    Ok(json!({
        "user": api.session.user(&origin),
        "logged_in": api.session.token(&origin).is_some(),
        "base": origin,
        "strict_security": crate::network_policy::strict_security(),
        "cli_relaxed": crate::network_policy::cli_strict_override() == Some(false),
    }))
}

/// 切换严格安全模式（关闭后回退到兼容清洗与网络守卫模式，立即生效并落盘）。
pub async fn security_set(_api: &Api, strict: bool) -> Result<Value, String> {
    crate::network_policy::set_strict_security(strict)?;
    crate::net::refresh_client();
    crate::playback_log::record(
        "security.mode_changed",
        json!({"strict": crate::network_policy::strict_security()}),
    );
    Ok(json!(crate::network_policy::strict_security()))
}

pub async fn debug_log(state: &AppState, msg: String) -> Result<Value, String> {
    crate::logging::append_debug(&state.data_dir, &msg)?;
    Ok(Value::Null)
}

/// 网络自检：系统代理 / DNS / 连通性快照，用于远程定位「打不开」。
pub async fn net_diag() -> Result<Value, String> {
    Ok(crate::net::diagnose().await)
}

/* ───────────────────────── 线路（数据源） ───────────────────────── */

pub async fn line_state() -> Result<Value, String> {
    Ok(json!({
        "base": base(),
        "default": crate::net::DEFAULT_BASE,
        "fixed": crate::lines::fixed(),
    }))
}

/// 手动切换线路：立即生效并落盘（下次启动沿用）。
pub async fn line_set(api: &Api, url: String) -> Result<Value, String> {
    let previous = base();
    let next = crate::net::set_base(&url)?;
    if previous != next {
        api.session.clear();
    }
    Ok(json!(next))
}

/// 从地址发布页抓镜像线路（只在用户点开「线路」时调用）。
pub async fn line_mirrors() -> Result<Value, String> {
    crate::net::refresh_client();
    crate::lines::mirrors().await
}

/// 可达性探测：只报结果，不做自动检查 / 自动切换。
pub async fn line_probe(urls: Vec<String>) -> Result<Value, String> {
    crate::net::refresh_client();
    Ok(crate::lines::probe(urls).await)
}

/* ───────────────────────── 更新检查 ───────────────────────── */

const REPO: &str = "Minireel/HGhub";

fn ver_parts(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .map(|s| s.trim().parse::<u64>().unwrap_or(0))
        .collect()
}

/// 只比较数字段：`v1.2.0` > `1.1.9`；解析不出的段按 0 处理。
fn is_newer(latest: &str, current: &str) -> bool {
    let (a, b) = (ver_parts(latest), ver_parts(current));
    for i in 0..a.len().max(b.len()) {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 对比 GitHub Release 的 tag 与当前版本（未认证 60 次/小时/IP，够用）。
pub async fn check_update() -> Result<Value, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = crate::net::get(
        &url,
        std::time::Duration::from_secs(12),
        Some("application/vnd.github+json"),
        None,
    )
    .await?;

    let status = resp.status();
    if status.as_u16() == 404 {
        // 还没发过 release
        return Ok(json!({
            "current": current, "latest": "", "has_update": false,
            "url": format!("https://github.com/{REPO}/releases"),
        }));
    }
    if !status.is_success() {
        return Err(format!("GitHub 返回 HTTP {status}"));
    }

    let v: Value = resp.json().await.map_err(|e| crate::net::err_text(&e))?;
    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let html = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases"));
    let notes = v.get("body").and_then(|x| x.as_str()).unwrap_or("");

    Ok(json!({
        "current": current,
        "latest": tag.trim_start_matches(['v', 'V']).to_string(),
        "tag": tag,
        "has_update": !tag.is_empty() && is_newer(&tag, &current),
        "url": html,
        "name": v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        "published_at": v.get("published_at").and_then(|x| x.as_str()).unwrap_or(""),
        "notes": notes.chars().take(800).collect::<String>(),
    }))
}

/* ───────────────────────── 账号 ───────────────────────── */

pub async fn api_login(api: &Api, username: String, password: String) -> Result<Value, String> {
    let origin = base();
    let data = edge(
        &origin,
        "POST",
        "/api/account/loginByPassword",
        Some(json!({ "username": username, "password": password })),
        None,
    )
    .await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "登录返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    if base() != origin {
        return Err("登录期间线路已变化，请重新登录".into());
    }
    api.session.set(&origin, token, user.clone());
    Ok(json!({ "user": user }))
}

pub async fn api_register(
    api: &Api,
    username: String,
    password: String,
    email: String,
) -> Result<Value, String> {
    let origin = base();
    let mut body = json!({
        "username": username,
        "password": password,
        "confirm_password": password,
    });
    if !email.trim().is_empty() {
        body["email"] = json!(email);
    }
    let data = edge(
        &origin,
        "POST",
        "/api/account/registerByPassword",
        Some(body),
        None,
    )
    .await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "注册返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    if base() != origin {
        return Err("注册期间线路已变化，请重新登录".into());
    }
    api.session.set(&origin, token, user.clone());
    Ok(json!({ "user": user }))
}

pub async fn api_logout(api: &Api) -> Result<Value, String> {
    let origin = base();
    let token = api.session.token(&origin);
    let _ = edge(
        &origin,
        "POST",
        "/api/account/logout",
        None,
        token.as_deref(),
    )
    .await;
    api.session.clear();
    Ok(json!({ "ok": true }))
}

pub async fn api_me(api: &Api) -> Result<Value, String> {
    let origin = base();
    let token = api.session.token(&origin);
    let me = edge(&origin, "GET", "/api/account/me", None, token.as_deref()).await?;
    if base() != origin {
        return Err("读取账号期间线路已变化".into());
    }
    if let Some(token) = api.session.token(&origin) {
        api.session.set(&origin, token, me.clone());
    }
    Ok(me)
}

/* ───────────────────────── 列表 ───────────────────────── */

pub async fn api_feed(api: &Api, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!("/api/videos?page={page}&size={size}")).await
}

pub async fn api_category(
    api: &Api,
    cate: String,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    api.get(&format!("/api/videos/category/{cate}?page={page}&size={size}"))
        .await
}

pub async fn api_rank(api: &Api, kind: String) -> Result<Value, String> {
    let kind = match kind.as_str() {
        "recommend" | "potential" => kind,
        _ => "hot".to_string(),
    };
    api.get(&format!("/api/ranks/{kind}")).await
}

pub async fn api_search(api: &Api, q: String, page: u32, size: u32) -> Result<Value, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    api.get(&format!("/api/search?q={encoded}&page={page}&size={size}"))
        .await
}

pub async fn api_posts(api: &Api, page: u32, size: u32) -> Result<Value, String> {
    api.get(&format!("/api/posts?page={page}&size={size}")).await
}

pub async fn api_post(api: &Api, id: i64) -> Result<Value, String> {
    api.get(&format!("/api/posts/{id}")).await
}

/* ───────────────────────── 详情 / 播放 ───────────────────────── */

pub async fn api_video(api: &Api, id: i64) -> Result<Value, String> {
    api.get(&format!("/api/videos/{id}")).await
}

/// 新版播放接口返回 `streams.{main,hevc,preview}`，旧接口的 `video_url` 现在恒为空串。
/// 这里把新字段回填到 `video_url` / `video_url_hevc`，前端和自检继续按原字段取流。
fn normalize_play_info(info: &mut Value) {
    fn non_empty(v: Option<&Value>) -> Option<String> {
        v.and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }
    let main = non_empty(info.get("streams").and_then(|s| s.get("main")));
    let hevc = non_empty(info.get("streams").and_then(|s| s.get("hevc")));
    let Some(obj) = info.as_object_mut() else { return };
    for (field, stream) in [("video_url", main), ("video_url_hevc", hevc)] {
        if let Some(url) = stream {
            if non_empty(obj.get(field)).is_none() {
                obj.insert(field.into(), json!(url));
            }
        }
    }
}

fn has_play_url(info: &Value) -> bool {
    info.get("video_url")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

pub async fn api_play(api: &Api, id: i64, ep: u32) -> Result<Value, String> {
    // 优先走新接口；失败或没有可用地址时再退回旧接口，两者都不行就返回新接口的结果/错误。
    let primary = api
        .post(
            &format!("/api/videos/{id}/episodes/{ep}/playback"),
            Some(json!({})),
        )
        .await
        .map(|mut info| {
            normalize_play_info(&mut info);
            info
        });
    if let Ok(info) = &primary {
        if has_play_url(info) {
            return primary;
        }
    }
    match api
        .get(&format!("/api/videos/{id}/play?ep={ep}&page_texts=0"))
        .await
    {
        Ok(mut legacy) => {
            normalize_play_info(&mut legacy);
            if has_play_url(&legacy) || primary.is_err() {
                Ok(legacy)
            } else {
                primary
            }
        }
        Err(_) => primary,
    }
}

/* ───────────────────────── 评论 / 互动 ───────────────────────── */

pub async fn api_comments(
    api: &Api,
    id: i64,
    ep: u32,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    api.get(&format!(
        "/api/videos/{id}/comments?ep={ep}&page={page}&size={size}"
    ))
    .await
}

pub async fn api_comment_send(
    api: &Api,
    id: i64,
    ep: u32,
    text: String,
    reply_id: Option<i64>,
) -> Result<Value, String> {
    let mut body = json!({ "text": text });
    let path = match reply_id {
        Some(rid) if rid > 0 => {
            body["reply_id"] = json!(rid);
            format!("/api/videos/{id}/comments")
        }
        _ => format!("/api/videos/{id}/comments?ep={ep}"),
    };
    api.authed("POST", &path, Some(body)).await
}

pub async fn api_like(api: &Api, id: i64, ep: u32, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/videos/{id}/like?ep={ep}"), None)
        .await
}

pub async fn api_collect(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/videos/{id}/collect"), None)
        .await
}

pub async fn api_follow(api: &Api, uid: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/users/{uid}/follow"), None)
        .await
}

/* ───────────────────────── 吃瓜帖子互动 ───────────────────────── */

pub async fn api_post_like(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/posts/{id}/like"), None)
        .await
}

pub async fn api_post_collect(api: &Api, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    api.authed(method, &format!("/api/posts/{id}/collect"), None)
        .await
}

pub async fn api_post_comments(
    api: &Api,
    id: i64,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    api.get(&format!("/api/posts/{id}/comments?page={page}&size={size}"))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_playback_streams_fill_legacy_url_fields() {
        let mut info = json!({
            "video_url": "",
            "streams": {"main": "https://x/main.m3u8", "hevc": "https://x/hevc.m3u8", "preview": "https://x/p.m3u8"}
        });
        normalize_play_info(&mut info);
        assert_eq!(info["video_url"], "https://x/main.m3u8");
        assert_eq!(info["video_url_hevc"], "https://x/hevc.m3u8");
        assert!(has_play_url(&info));
    }

    #[test]
    fn existing_video_url_is_kept_and_empty_streams_stay_empty() {
        let mut keep = json!({"video_url": "https://old/a.m3u8", "streams": {"main": "https://x/main.m3u8"}});
        normalize_play_info(&mut keep);
        assert_eq!(keep["video_url"], "https://old/a.m3u8");

        let mut empty = json!({"video_url": "", "streams": {"main": ""}});
        normalize_play_info(&mut empty);
        assert!(!has_play_url(&empty));
        assert!(!has_play_url(&json!({"id": 1})));
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v1.0.2", "1.0.1"));
        assert!(is_newer("v1.1", "1.0.9"));
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(!is_newer("v1.0.1", "1.0.1"));
        assert!(!is_newer("v0.9.9", "1.0.1"));
        assert!(!is_newer("", "1.0.1"));
    }

    #[test]
    fn arguments_are_read_from_the_js_shape() {
        let args = json!({"page": 1, "size": 20, "replyId": 7, "strict": true, "urls": ["https://a", "https://b"]});
        assert_eq!(arg_u32(&args, "page").unwrap(), 1);
        assert_eq!(arg_u32(&args, "size").unwrap(), 20);
        assert_eq!(arg_opt_i64(&args, &["reply_id", "replyId"]), Some(7));
        assert!(arg_bool(&args, "strict").unwrap());
        assert_eq!(arg_str_vec(&args, "urls").unwrap().len(), 2);
        assert!(arg_str(&args, "missing").is_err());
        assert!(arg_u32(&args, "missing").is_err());
        assert!(arg_bool(&args, "missing").is_err());
    }
}
