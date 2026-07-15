//! 配置、平台会话、用量缓存的读写，以及基于会话的接口重放。
//! 仅 Windows 编译。

use crate::core::{self, Balance, Day, Ymd};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

pub const APP_NAME: &str = "DeepSeekWidget";

// ───────── 路径 ─────────

pub fn data_dir() -> PathBuf {
    // %APPDATA%\DeepSeekWidget
    let base = directories::BaseDirs::new()
        .map(|b| b.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join(APP_NAME);
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn p(name: &str) -> PathBuf {
    data_dir().join(name)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Option<T> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_json<T: Serialize>(path: PathBuf, v: &T) {
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = fs::write(path, s);
    }
}

// ───────── 配置 ─────────

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "def_refresh")]
    pub refresh_seconds: u64,
    #[serde(default = "def_days")]
    pub days: i64,
    #[serde(default = "def_w")]
    pub window_width: f32,
    #[serde(default = "def_h")]
    pub window_height: f32,
}
fn def_refresh() -> u64 {
    60
}
fn def_days() -> i64 {
    30
}
fn def_w() -> f32 {
    340.0
}
fn def_h() -> f32 {
    420.0
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: String::new(),
            refresh_seconds: 60,
            days: 30,
            window_width: 340.0,
            window_height: 420.0,
        }
    }
}

pub fn load_config() -> Config {
    read_json(p("config.json")).unwrap_or_default()
}
pub fn save_config(c: &Config) {
    write_json(p("config.json"), c);
}

// ───────── 会话（捕获到的接口 + 鉴权） ─────────

#[derive(Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub content_type: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub cookie: String,
    #[serde(default)]
    pub saved_at: String,
}

pub fn session_path() -> PathBuf {
    p("web_session.json")
}
pub fn has_session() -> bool {
    session_path().exists()
}
pub fn load_session() -> Option<Session> {
    read_json(session_path())
}
pub fn save_session(s: &Session) {
    write_json(session_path(), s);
}

// ───────── 用量缓存 ─────────

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Cache {
    pub day_map: BTreeMap<String, Day>,
    pub balance: Option<Balance>,
    pub fetched_at: String,
}
pub fn load_cache() -> Option<Cache> {
    read_json(p("usage_cache.json"))
}
pub fn save_cache(c: &Cache) {
    write_json(p("usage_cache.json"), c);
}

// ───────── 今天（本地时区，取整到日） ─────────

pub fn today_local() -> Ymd {
    // 本地时区偏移：用 std 计算较麻烦，这里用 UTC 天 + 环境时区近似。
    // 对“最近 N 天”的边界足够；若跨零点误差一天，下次刷新即修正。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 尝试读取本地偏移（Windows：通过 GetTimeZoneInformation 太重，简单用 UTC）
    core::ymd_from_unix(secs)
}

// ───────── 重放 ─────────

/// 只有这些接口值得捕获/重放：用量统计 + 账户摘要（余额）。
/// 登录、发短信、埋点等接口绝不能重放（会重复发送验证码短信）。
pub fn is_replayable(url: &str) -> bool {
    url.contains("platform.deepseek.com/api/v0/usage/")
        || url.contains("platform.deepseek.com/api/v0/users/get_user_summary")
}

pub struct FetchResult {
    pub series: Vec<(String, Day)>,
    pub balance: Option<Balance>,
    pub source: &'static str, // "web" | "cache" | "none"
    pub note: String,
}

/// 用保存的会话重放捕获接口，日期改写为最近 `days` 天。
/// 失败返回 Err(原因)。
fn replay(days: i64) -> Result<(BTreeMap<String, Day>, Option<Balance>), String> {
    let sess = load_session().ok_or("no_session")?;
    if sess.endpoints.is_empty() {
        return Err("no_endpoints".into());
    }
    let today = today_local();
    let start = today.minus_days(days - 1);

    let mut objs: Vec<Value> = Vec::new();
    let mut ok = 0;
    for ep in &sess.endpoints {
        if !is_replayable(&ep.url) {
            continue;
        }
        let url = core::rewrite_dates(&ep.url, start, today);
        let body = ep
            .body
            .as_ref()
            .map(|b| core::rewrite_dates(b, start, today));

        let mut req = if ep.method.eq_ignore_ascii_case("POST") {
            ureq::post(&url)
        } else {
            ureq::get(&url)
        };
        req = req
            .set("User-Agent", UA)
            .set("Referer", USAGE_URL)
            .set("Origin", "https://platform.deepseek.com");
        if !ep.auth.is_empty() {
            req = req.set("Authorization", &ep.auth);
        }
        if !sess.cookie.is_empty() {
            req = req.set("Cookie", &sess.cookie);
        }

        let resp = if let Some(b) = &body {
            let ct = if ep.content_type.is_empty() {
                "application/json"
            } else {
                &ep.content_type
            };
            req.set("Content-Type", ct).send_string(b)
        } else {
            req.call()
        };

        match resp {
            Ok(r) => {
                if let Ok(v) = r.into_json::<Value>() {
                    // 平台出错时也返回 HTTP 200，但 biz_code != 0 且无数据
                    let biz_ok = v
                        .pointer("/data/biz_code")
                        .and_then(|b| b.as_i64())
                        .map_or(true, |b| b == 0);
                    if biz_ok {
                        objs.push(v);
                        ok += 1;
                    }
                }
            }
            Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
                return Err("auth_expired".into());
            }
            Err(_) => { /* 单个接口失败忽略 */ }
        }
    }

    if ok == 0 {
        return Err("all_failed".into());
    }
    let day_map = core::normalize_captures(&objs);
    let balance = core::find_balance(&objs);
    Ok((day_map, balance))
}

/// 统一取数：会话重放 -> 缓存 -> 空。
pub fn get_usage(days: i64) -> FetchResult {
    let today = today_local();
    if has_session() {
        match replay(days) {
            Ok((day_map, balance)) => {
                save_cache(&Cache {
                    day_map: day_map.clone(),
                    balance: balance.clone(),
                    fetched_at: now_hms(),
                });
                return FetchResult {
                    series: core::day_series(&day_map, today, days),
                    balance,
                    source: "web",
                    note: String::new(),
                };
            }
            Err(e) => {
                if let Some(c) = load_cache() {
                    let note = if e == "auth_expired" {
                        "平台登录已过期，请在托盘重新登录（下方为缓存）".to_string()
                    } else {
                        format!("平台同步失败（{}），显示缓存", e)
                    };
                    return FetchResult {
                        series: core::day_series(&c.day_map, today, days),
                        balance: c.balance,
                        source: "cache",
                        note,
                    };
                }
                return FetchResult {
                    series: core::day_series(&BTreeMap::new(), today, days),
                    balance: None,
                    source: "none",
                    note: format!("平台会话不可用（{}），请在托盘重新登录", e),
                };
            }
        }
    }
    FetchResult {
        series: core::day_series(&BTreeMap::new(), today, days),
        balance: None,
        source: "none",
        note: String::new(),
    }
}

/// 余额：优先官方 API Key，否则用网页缓存。
pub fn fetch_balance(cfg: &Config) -> Result<Balance, String> {
    if !cfg.api_key.is_empty() {
        let resp = ureq::get("https://api.deepseek.com/user/balance")
            .set("Authorization", &format!("Bearer {}", cfg.api_key))
            .call();
        match resp {
            Ok(r) => {
                if let Ok(v) = r.into_json::<Value>() {
                    let info = v
                        .get("balance_infos")
                        .and_then(|a| a.get(0))
                        .cloned()
                        .unwrap_or(Value::Null);
                    return Ok(Balance {
                        is_available: v
                            .get("is_available")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                        currency: sv(&info, "currency"),
                        total: sv2(&info, "total_balance", "0.00"),
                        granted: sv2(&info, "granted_balance", "0.00"),
                        topped_up: sv2(&info, "topped_up_balance", "0.00"),
                        from_web: false,
                    });
                }
                return Err("解析余额失败".into());
            }
            Err(ureq::Error::Status(code, _)) => {
                // 落到网页缓存
                if let Some(b) = load_cache().and_then(|c| c.balance) {
                    return Ok(b);
                }
                return Err(format!("HTTP {}", code));
            }
            Err(_) => {
                if let Some(b) = load_cache().and_then(|c| c.balance) {
                    return Ok(b);
                }
                return Err("网络错误".into());
            }
        }
    }
    load_cache()
        .and_then(|c| c.balance)
        .ok_or_else(|| "未设置 API Key，且暂无平台余额缓存".into())
}

fn sv(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn sv2(v: &Value, k: &str, def: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or(def).to_string()
}

pub fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
pub const USAGE_URL: &str = "https://platform.deepseek.com/usage";
