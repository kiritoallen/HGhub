//! 播放诊断日志（网页版 stub）。
//!
//! 桌面端 `src-tauri/src/playback_log.rs` 依赖 tauri 命令宏，这里只保留核心用途：
//! 复用进来的 `net.rs` / `media.rs` / `handlers.rs` 需要
//! [`enabled`] / [`resource_id`] / [`record`] / [`error_kind`] / [`bind`] 这几个入口，
//! 并且网页版前端也会调用 `playback_log_status|set|write|read` 四个命令。
//!
//! 行为与桌面端一致：默认关闭；开关落盘在 `data/playback-diagnostics.json`；
//! 开启后事件写 `data/playback.log`（2MB 轮转到 `playback.previous.log`）。
//! 写盘前统一 [`scrub`]，绝不让 URL / token / 请求体落到日志里。

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

const MAX_FILE: u64 = 2 * 1024 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);
static DIR: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

/* ───────────────────────── 对复用模块暴露的接口 ───────────────────────── */

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 只输出资源指纹，便于对照「同一个分片」而不泄露真实地址。
pub fn resource_id(raw: &str) -> String {
    let hash = raw
        .as_bytes()
        .iter()
        .fold(2166136261u32, |h, b| (h ^ *b as u32).wrapping_mul(16777619));
    format!("r{hash:08x}")
}

/// 错误归类（与桌面端一致），`net.rs` 用它判断哪些错误值得重试。
pub fn error_kind(error: &str) -> &'static str {
    if error.contains("禁止") || error.contains("只允许") || error.contains("控制字符") {
        if error.contains("降级") {
            "https_downgrade_denied"
        } else if error.contains("内网") || error.contains("保留地址") {
            "private_address_denied"
        } else {
            "target_policy_denied"
        }
    } else if error.contains("DNS") {
        "dns_error"
    } else if error.contains("超时") || error.to_ascii_lowercase().contains("timeout") {
        "timeout"
    } else if error.starts_with("HTTP ") {
        "http_error"
    } else if error.contains("重定向") {
        "redirect_error"
    } else {
        "transport_error"
    }
}

/// 启动时绑定数据目录：读回开关；`HG_WEB_PLAYBACK_LOG=1` 可强制开启。
pub fn bind(dir: &Path) {
    if let Ok(mut slot) = DIR.lock() {
        *slot = Some(dir.to_path_buf());
    }
    let from_env = std::env::var("HG_WEB_PLAYBACK_LOG")
        .map(|value| value.trim().to_ascii_lowercase())
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| matches!(value.as_str(), "1" | "true" | "on" | "yes"))
        .unwrap_or(false);
    let saved = fs::read_to_string(dir.join("playback-diagnostics.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false);
    ENABLED.store(from_env || saved, Ordering::Relaxed);
    if enabled() {
        record(
            "process.start",
            json!({"version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS, "server": "http"}),
        );
    }
}

/// 写一条事件；未开启或尚未初始化时是空操作（热路径不能因为日志失败而报错）。
pub fn record(event: &str, data: Value) {
    if !enabled() {
        return;
    }
    let Some(dir) = data_dir() else {
        return;
    };
    let line = json!({
        "ts": crate::logging::utc_timestamp().unwrap_or_default(),
        "pid": std::process::id(),
        "seq": SEQ.fetch_add(1, Ordering::Relaxed),
        "event": event,
        "strict": crate::network_policy::strict_security(),
        "data": scrub(&data, 0),
    })
    .to_string()
        + "\n";
    let _ = append(&dir, &line);
}

/* ───────────────────────── HTTP 命令用到的读写接口 ───────────────────────── */

pub fn status() -> Value {
    json!({
        "enabled": enabled(),
        "path": data_dir().map(|dir| dir.join("playback.log").display().to_string()),
        "error": Value::Null,
    })
}

pub fn set(on: bool) -> Result<Value, String> {
    let dir = data_dir().ok_or("日志目录尚未初始化")?;
    if on {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("playback.log"))
            .map_err(|_| "无法创建诊断日志")?;
    }
    let temp = dir.join("playback-diagnostics.json.tmp");
    fs::write(&temp, json!({"enabled": on}).to_string())
        .and_then(|_| fs::rename(&temp, dir.join("playback-diagnostics.json")))
        .map_err(|_| "无法保存日志开关")?;
    ENABLED.store(on, Ordering::Relaxed);
    record(
        "diagnostics.toggle",
        json!({"enabled": on, "version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS}),
    );
    Ok(status())
}

pub fn write(entries: Value) -> Result<(), String> {
    if !enabled() {
        return Ok(());
    }
    let dir = data_dir().ok_or("日志目录尚未初始化")?;
    let items = entries.as_array().ok_or("日志条目无效")?;
    if items.len() > 128 {
        return Err("日志批次过大".into());
    }
    for entry in items {
        let event = entry
            .get("event")
            .and_then(Value::as_str)
            .ok_or("日志条目无效")?;
        if event.len() > 80
            || !event
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err("日志条目无效".into());
        }
        let data = entry.get("data").cloned().unwrap_or(Value::Null);
        if data.to_string().len() > 8192 {
            return Err("日志条目无效".into());
        }
        let line = json!({
            "ts": crate::logging::utc_timestamp().unwrap_or_default(),
            "pid": std::process::id(),
            "seq": SEQ.fetch_add(1, Ordering::Relaxed),
            "event": format!("web.{event}"),
            "strict": crate::network_policy::strict_security(),
            "data": scrub(&data, 0),
        })
        .to_string()
            + "\n";
        append(&dir, &line)?;
    }
    Ok(())
}

pub fn read() -> Result<String, String> {
    let dir = data_dir().ok_or("日志目录尚未初始化")?;
    let mut text = String::new();
    for name in ["playback.previous.log", "playback.log"] {
        let path = dir.join(name);
        if path.exists() {
            if fs::metadata(&path).map_err(|_| "无法读取日志")?.len() > MAX_FILE + 16384 {
                return Err("日志文件超出大小限制".into());
            }
            text.push_str(&fs::read_to_string(&path).map_err(|_| "无法读取日志")?);
        }
    }
    Ok(text)
}

/* ───────────────────────── 内部实现 ───────────────────────── */

fn data_dir() -> Option<PathBuf> {
    DIR.lock().ok().and_then(|slot| slot.clone())
}

fn append(dir: &Path, line: &str) -> Result<(), String> {
    let path = dir.join("playback.log");
    if fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) + line.len() as u64 > MAX_FILE {
        let previous = dir.join("playback.previous.log");
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&path, &previous);
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|_| "无法写入诊断日志".to_string())
}

/// 前端条目同样要做防御：任意 URL / token / 请求体字段都不落盘。
fn scrub(value: &Value, depth: usize) -> Value {
    if depth > 5 {
        return Value::Null;
    }
    match value {
        Value::String(text) => {
            if text.contains("://") || text.to_ascii_lowercase().contains("%3a") {
                json!("[redacted]")
            } else {
                json!(text.chars().take(160).collect::<String>())
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(24)
                .map(|item| scrub(item, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .take(64)
                .filter_map(|(key, item)| {
                    let lower = key.to_ascii_lowercase();
                    if [
                        "url",
                        "src",
                        "token",
                        "password",
                        "cookie",
                        "authorization",
                        "body",
                        "message",
                        "reason",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle))
                    {
                        None
                    } else {
                        Some((key.chars().take(64).collect(), scrub(item, depth + 1)))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_are_stable_fingerprints() {
        assert_eq!(resource_id("hello"), "r4f9f2cab");
        assert_eq!(resource_id("hello"), resource_id("hello"));
        assert_ne!(resource_id("hello"), resource_id("hello2"));
    }

    #[test]
    fn errors_are_classified_like_the_desktop_client() {
        assert_eq!(error_kind("禁止访问内网"), "private_address_denied");
        assert_eq!(error_kind("禁止 HTTPS 降级重定向"), "https_downgrade_denied");
        assert_eq!(error_kind("DNS 解析失败"), "dns_error");
        assert_eq!(error_kind("请求超时"), "timeout");
        assert_eq!(error_kind("HTTP 503 Service Unavailable"), "http_error");
        assert_eq!(error_kind("connection refused"), "transport_error");
    }

    #[test]
    fn scrub_drops_urls_and_secret_fields_but_keeps_fingerprints() {
        let value = scrub(
            &json!({
                "resource": "r123",
                "url": "https://a/?token=secret",
                "token": "secret",
                "nested": {"message": "secret", "detail": "hg://localhost/seg?u=secret"},
            }),
            0,
        )
        .to_string();
        assert!(!value.contains("secret"));
        assert!(value.contains("r123"));
    }

    #[test]
    fn journal_writes_rotate_and_replay_like_the_desktop_client() {
        let dir = std::env::temp_dir().join(format!(
            "hghub-web-playback-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        bind(&dir);
        assert!(!enabled());
        assert!(!dir.join("playback.log").exists());
        set(true).unwrap();
        assert!(enabled());
        write(json!([{"event": "seek.request", "data": {"seek": 1}}])).unwrap();
        let text = read().unwrap();
        assert!(text.contains("web.seek.request"));
        set(false).unwrap();
        assert!(!enabled());
        let before = fs::metadata(dir.join("playback.log")).unwrap().len();
        record("test.off", json!({"n": 1}));
        assert_eq!(fs::metadata(dir.join("playback.log")).unwrap().len(), before);
        if let Ok(mut slot) = DIR.lock() {
            *slot = None;
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
