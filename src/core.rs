//! 跨平台核心逻辑（无 GUI / 无网络依赖，可在任意平台编译与单元测试）
//! - 简易日期工具（不引入 chrono/time，减小体积）
//! - 平台用量接口返回 JSON 的启发式归一化 -> 按天序列
//! - URL / 请求体里日期区间的改写（把捕获时的旧区间换成最近 N 天）
//! - 余额对象提取

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ───────────────────────────── 日期工具 ─────────────────────────────

/// 一个简单的公历日期（避免引入第三方日期库）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Ymd {
    pub y: i64,
    pub m: i64,
    pub d: i64,
}

impl Ymd {
    pub fn to_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }

    /// 天数 -> 公历（Howard Hinnant 的 civil_from_days 算法）
    pub fn from_days(z: i64) -> Ymd {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // [0, 11]
        let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
        let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
        let y = if m <= 2 { y + 1 } else { y };
        Ymd { y, m, d }
    }

    /// 公历 -> 天数（days_from_civil）
    pub fn to_days(self) -> i64 {
        let y = if self.m <= 2 { self.y - 1 } else { self.y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = if self.m > 2 { self.m - 3 } else { self.m + 9 };
        let doy = (153 * mp + 2) / 5 + self.d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn minus_days(self, n: i64) -> Ymd {
        Ymd::from_days(self.to_days() - n)
    }

    /// 当天 0 点的 Unix 秒
    pub fn midnight_unix(self) -> i64 {
        self.to_days() * 86_400
    }

    /// 当天最后一刻的 Unix 秒
    pub fn end_unix(self) -> i64 {
        self.midnight_unix() + 86_399
    }

    /// 解析 "YYYY-MM-DD" 或 "YYYY/MM/DD"（可含时间后缀，忽略）
    pub fn parse(s: &str) -> Option<Ymd> {
        let b = s.as_bytes();
        if b.len() < 10 {
            return None;
        }
        let g = |a: usize, z: usize| std::str::from_utf8(&b[a..z]).ok()?.parse::<i64>().ok();
        let sep1 = b[4];
        let sep2 = b[7];
        if !(sep1 == b'-' || sep1 == b'/') || !(sep2 == b'-' || sep2 == b'/') {
            return None;
        }
        let y = g(0, 4)?;
        let m = g(5, 7)?;
        let d = g(8, 10)?;
        if (1..=12).contains(&m) && (1..=31).contains(&d) {
            Some(Ymd { y, m, d })
        } else {
            None
        }
    }
}

/// 从 Unix 秒得到当天日期（UTC，用于时间戳型日期字段的归类）
pub fn ymd_from_unix(secs: i64) -> Ymd {
    Ymd::from_days(secs.div_euclid(86_400))
}

// ───────────────────────── 归一化：捕获 -> 按天 ─────────────────────────

/// 单日用量指标
#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq)]
pub struct Day {
    pub requests: i64,
    pub hit: i64,    // 输入（命中缓存）
    pub miss: i64,   // 输入（未命中缓存）
    pub output: i64, // 输出
}

#[derive(Clone, Copy, Default)]
struct DayAcc {
    requests: i64,
    hit: i64,
    miss: i64,
    output: i64,
    input_total: i64,
    tokens_total: i64,
}

#[derive(Clone, Copy)]
enum Metric {
    Requests,
    Hit,
    Miss,
    Output,
    InputTotal,
    TokensTotal,
}

/// 把接口字段名映射到指标（启发式，大小写不敏感）
fn classify_key(k: &str) -> Option<Metric> {
    let k = k.to_ascii_lowercase();
    if k.contains("hit") {
        Some(Metric::Hit)
    } else if k.contains("miss") {
        Some(Metric::Miss)
    } else if k.contains("output") || k.contains("completion") || k.contains("response") {
        Some(Metric::Output)
    } else if k.contains("count") || k.contains("request") || k.contains("call") || k.contains("api")
    {
        Some(Metric::Requests)
    } else if k.contains("input") || k.contains("prompt") {
        Some(Metric::InputTotal)
    } else if k == "total_tokens" || k == "tokens" || k == "token" {
        Some(Metric::TokensTotal)
    } else {
        None
    }
}

/// 尝试把一个 JSON 值解释成日期字符串 "YYYY-MM-DD"
fn value_to_date(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            if let Some(y) = Ymd::parse(s) {
                return Some(y.to_string());
            }
            // 纯数字字符串当时间戳
            if let Ok(n) = s.trim().parse::<i64>() {
                return num_to_date(n as f64);
            }
            None
        }
        Value::Number(n) => n.as_f64().and_then(num_to_date),
        _ => None,
    }
}

fn num_to_date(mut n: f64) -> Option<String> {
    if n > 1e12 {
        n /= 1000.0; // 毫秒
    }
    if n > 1e9 && n < 4e9 {
        return Some(ymd_from_unix(n as i64).to_string());
    }
    None
}

fn absorb(acc: &mut DayAcc, m: Metric, v: i64) {
    let slot = match m {
        Metric::Requests => &mut acc.requests,
        Metric::Hit => &mut acc.hit,
        Metric::Miss => &mut acc.miss,
        Metric::Output => &mut acc.output,
        Metric::InputTotal => &mut acc.input_total,
        Metric::TokensTotal => &mut acc.tokens_total,
    };
    if v > *slot {
        *slot = v; // 同指标多处出现取最大，避免重复累加
    }
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// 处理一个「按天数组」：元素含日期字段 + 若干数值字段
fn handle_array(arr: &[Value], out: &mut BTreeMap<String, DayAcc>) {
    for item in arr {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        // 找日期：优先键名含 date/day/time/period
        let mut day: Option<String> = None;
        for (k, v) in obj {
            let lk = k.to_ascii_lowercase();
            if lk.contains("date") || lk.contains("day") || lk.contains("time") || lk.contains("period")
            {
                if let Some(d) = value_to_date(v) {
                    day = Some(d);
                    break;
                }
            }
        }
        if day.is_none() {
            for v in obj.values() {
                if let Some(d) = value_to_date(v) {
                    day = Some(d);
                    break;
                }
            }
        }
        let day = match day {
            Some(d) => d,
            None => continue,
        };
        let acc = out.entry(day).or_default();
        for (k, v) in obj {
            if let Some(inner) = v.as_object() {
                // 嵌套一层，如 {tokens:{hit:..,miss:..}}
                for (k2, v2) in inner {
                    if let (Some(m), Some(n)) = (classify_key(k2), as_int(v2)) {
                        absorb(acc, m, n);
                    }
                }
                continue;
            }
            if let (Some(m), Some(n)) = (classify_key(k), as_int(v)) {
                absorb(acc, m, n);
            }
        }
    }
}

/// 在任意 JSON 里递归寻找「按天数组」并归并
fn walk(v: &Value, out: &mut BTreeMap<String, DayAcc>) {
    match v {
        Value::Array(a) => {
            if !a.is_empty() && a.iter().all(|x| x.is_object()) {
                handle_array(a, out);
            }
            for x in a {
                walk(x, out);
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                walk(x, out);
            }
        }
        _ => {}
    }
}

/// 主入口：把若干接口返回 JSON 归一化成 {date: Day}
pub fn normalize_captures(objs: &[Value]) -> BTreeMap<String, Day> {
    let mut acc: BTreeMap<String, DayAcc> = BTreeMap::new();
    for o in objs {
        walk(o, &mut acc);
    }
    let mut out = BTreeMap::new();
    for (day, a) in acc {
        let mut d = Day {
            requests: a.requests,
            hit: a.hit,
            miss: a.miss,
            output: a.output,
        };
        // 兜底：只有输入总量而无 hit/miss -> 全记为未命中；只有 tokens 总量同理
        if d.hit == 0 && d.miss == 0 {
            if a.input_total > 0 {
                d.miss = a.input_total;
            } else if a.tokens_total > 0 && d.output == 0 {
                d.miss = a.tokens_total;
            }
        }
        out.insert(day, d);
    }
    out
}

/// 补齐最近 `days` 天（缺失填 0），返回 (date, Day) 的有序列表
pub fn day_series(day_map: &BTreeMap<String, Day>, today: Ymd, days: i64) -> Vec<(String, Day)> {
    let start = today.minus_days(days - 1);
    (0..days)
        .map(|i| {
            let d = start.minus_days(-i).to_string();
            let v = day_map.get(&d).copied().unwrap_or_default();
            (d, v)
        })
        .collect()
}

// ───────────────────────── 余额提取 ─────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Balance {
    pub currency: String,
    pub total: String,
    pub granted: String,
    pub topped_up: String,
    pub is_available: bool,
    pub from_web: bool,
}

/// 从捕获里找余额（键名含 balance 且值可转数字）
pub fn find_balance(objs: &[Value]) -> Option<Balance> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    fn walk_bal(v: &Value, found: &mut BTreeMap<String, String>) {
        match v {
            Value::Object(o) => {
                for (k, val) in o {
                    let lk = k.to_ascii_lowercase();
                    if lk.contains("balance") {
                        let s = match val {
                            Value::String(s) => Some(s.clone()),
                            Value::Number(n) => Some(n.to_string()),
                            _ => None,
                        };
                        if let Some(s) = s {
                            if s.parse::<f64>().is_ok() {
                                found.insert(lk.clone(), s);
                            }
                        }
                    }
                    if lk == "currency" {
                        if let Value::String(s) = val {
                            found.insert("currency".into(), s.clone());
                        }
                    }
                    walk_bal(val, found);
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk_bal(x, found);
                }
            }
            _ => {}
        }
    }
    for o in objs {
        walk_bal(o, &mut found);
    }
    let total = found
        .get("total_balance")
        .or_else(|| found.get("balance"))
        .or_else(|| found.get("available_balance"))?
        .clone();
    Some(Balance {
        currency: found.get("currency").cloned().unwrap_or_else(|| "CNY".into()),
        is_available: total.parse::<f64>().unwrap_or(0.0) > 0.0,
        granted: found.get("granted_balance").cloned().unwrap_or_else(|| "-".into()),
        topped_up: found.get("topped_up_balance").cloned().unwrap_or_else(|| "-".into()),
        total,
        from_web: true,
    })
}

// ───────────────────────── 日期区间改写 ─────────────────────────

/// 把字符串（URL 或请求体）里的日期/时间戳区间改写为 [start, end]。
/// 规则：出现的最小值 -> start，最大值 -> end，中间按大小就近归到两端。
/// 时间戳的 end 用「次日 0 点」（独占式）：DeepSeek 平台要求区间对齐整天边界，
/// 传 23:59:59 会返回 biz_code=1 的空数据。
pub fn rewrite_dates(text: &str, start: Ymd, end: Ymd) -> String {
    let end_excl = end.end_unix() + 1; // 次日 0 点
    let mut s = text.to_string();
    s = rewrite_iso(&s, start, end);
    s = rewrite_epoch(&s, 13, start.midnight_unix() * 1000, end_excl * 1000);
    s = rewrite_epoch(&s, 10, start.midnight_unix(), end_excl);
    s
}

/// 改写所有 YYYY-MM-DD / YYYY/MM/DD
fn rewrite_iso(text: &str, start: Ymd, end: Ymd) -> String {
    let matches = find_iso(text);
    if matches.is_empty() {
        return text.to_string();
    }
    let mut days: Vec<i64> = matches.iter().map(|(_, _, y)| y.to_days()).collect();
    days.sort_unstable();
    let (lo, hi) = (days[0], days[days.len() - 1]);
    let mid = (lo + hi) as f64 / 2.0;
    // 从后往前替换，避免位移
    let mut out = text.to_string();
    for (a, b, y) in matches.into_iter().rev() {
        let repl = if lo == hi || (y.to_days() as f64) > mid {
            end.to_string()
        } else {
            start.to_string()
        };
        out.replace_range(a..b, &repl);
    }
    out
}

/// 找出所有 ISO 日期的 (起, 止, Ymd)
fn find_iso(text: &str) -> Vec<(usize, usize, Ymd)> {
    let b = text.as_bytes();
    let mut res = Vec::new();
    let mut i = 0;
    while i + 10 <= b.len() {
        // 需要前面不是数字，避免匹配 12026-01-01 这种
        let boundary_ok = i == 0 || !b[i - 1].is_ascii_digit();
        if boundary_ok
            && b[i].is_ascii_digit()
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
            && (b[i + 4] == b'-' || b[i + 4] == b'/')
        {
            if let Some(y) = Ymd::parse(&text[i..i + 10]) {
                // 后一位不能是数字（避免 2026-01-011）
                let after_ok = i + 10 >= b.len() || !b[i + 10].is_ascii_digit();
                if after_ok {
                    res.push((i, i + 10, y));
                    i += 10;
                    continue;
                }
            }
        }
        i += 1;
    }
    res
}

/// 改写指定位数的连续数字（时间戳）。仅替换恰好 `digits` 位、且前后非数字的整数。
fn rewrite_epoch(text: &str, digits: usize, start_val: i64, end_val: i64) -> String {
    let spans = find_epoch(text, digits);
    if spans.is_empty() {
        return text.to_string();
    }
    let vals: Vec<i64> = spans.iter().map(|(_, _, v)| *v).collect();
    let lo = *vals.iter().min().unwrap();
    let hi = *vals.iter().max().unwrap();
    let mid = (lo + hi) as f64 / 2.0;
    let mut out = text.to_string();
    for (a, b, v) in spans.into_iter().rev() {
        let repl = if lo == hi || (v as f64) > mid {
            end_val
        } else {
            start_val
        };
        out.replace_range(a..b, &repl.to_string());
    }
    out
}

/// 找出所有「恰好 digits 位、前后非数字、以 1 开头（2001~ 的时间戳）」的整数
fn find_epoch(text: &str, digits: usize) -> Vec<(usize, usize, i64)> {
    let b = text.as_bytes();
    let mut res = Vec::new();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if b[i].is_ascii_digit() {
            let s = i;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
            let len = i - s;
            if len == digits && b[s] == b'1' {
                if let Ok(v) = text[s..i].parse::<i64>() {
                    res.push((s, i, v));
                }
            }
        } else {
            i += 1;
        }
    }
    res
}

// ───────────────────────────── 测试 ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ymd(y: i64, m: i64, d: i64) -> Ymd {
        Ymd { y, m, d }
    }

    #[test]
    fn date_roundtrip() {
        for (y, m, d) in [(2026, 7, 15), (2000, 1, 1), (1999, 12, 31), (2024, 2, 29)] {
            let a = ymd(y, m, d);
            assert_eq!(Ymd::from_days(a.to_days()), a);
        }
        assert_eq!(ymd(2026, 7, 15).minus_days(30).to_string(), "2026-06-15");
        assert_eq!(ymd(2026, 3, 1).minus_days(1).to_string(), "2026-02-28");
        assert_eq!(ymd(2024, 3, 1).minus_days(1).to_string(), "2024-02-29"); // 闰年
    }

    #[test]
    fn parse_dates() {
        assert_eq!(Ymd::parse("2026-07-08").unwrap(), ymd(2026, 7, 8));
        assert_eq!(Ymd::parse("2026/07/08T12:00").unwrap(), ymd(2026, 7, 8));
        assert!(Ymd::parse("2026-13-40").is_none());
        assert!(Ymd::parse("abc").is_none());
    }

    #[test]
    fn normalize_structure_a() {
        // 细分字段直接给出
        let cap = json!({"data":{"list":[
            {"date":"2026-07-08","prompt_cache_hit_tokens":959488,
             "prompt_cache_miss_tokens":19557815,"completion_tokens":2490835,"request_count":1200},
            {"date":"2026-07-09","cache_hit":1000,"cache_miss":5000,
             "output_tokens":800,"api_calls":300}
        ]}});
        let m = normalize_captures(&[cap]);
        let a = m.get("2026-07-08").unwrap();
        assert_eq!(a.hit, 959488);
        assert_eq!(a.miss, 19557815);
        assert_eq!(a.output, 2490835);
        assert_eq!(a.requests, 1200);
        let b = m.get("2026-07-09").unwrap();
        assert_eq!(b.output, 800);
        assert_eq!(b.requests, 300);
    }

    #[test]
    fn normalize_nested_and_epoch() {
        // 毫秒时间戳 + 嵌套 tokens
        let cap = json!({"usage":[
            {"time": 1_783_996_800_000i64,
             "tokens":{"hit_tokens":111,"miss_tokens":222,"completion":333},
             "count":9}
        ]});
        let m = normalize_captures(&[cap]);
        assert_eq!(m.len(), 1);
        let d = m.values().next().unwrap();
        assert_eq!(*d, Day { requests: 9, hit: 111, miss: 222, output: 333 });
    }

    #[test]
    fn normalize_input_total_fallback() {
        // 只有输入总量，无 hit/miss -> 归为未命中
        let cap = json!({"items":[{"day":"2026/07/10","input_tokens":7777,
                                    "output_tokens":555,"requests":42}]});
        let m = normalize_captures(&[cap]);
        let d = m.get("2026-07-10").unwrap();
        assert_eq!(d.miss, 7777);
        assert_eq!(d.output, 555);
        assert_eq!(d.requests, 42);
        assert_eq!(d.hit, 0);
    }

    #[test]
    fn series_fills_gaps() {
        let mut map = BTreeMap::new();
        map.insert("2026-07-15".to_string(), Day { requests: 8, hit: 5, miss: 6, output: 7 });
        let s = day_series(&map, ymd(2026, 7, 15), 30);
        assert_eq!(s.len(), 30);
        assert_eq!(s[0].0, "2026-06-16");
        assert_eq!(s[29].0, "2026-07-15");
        assert_eq!(s[29].1.requests, 8);
        assert_eq!(s[0].1.requests, 0); // 空缺填 0
    }

    #[test]
    fn parse_real_captures() {
        // 用真实抓包文件验证（仅本机存在该文件时运行）
        let path = std::path::PathBuf::from(match std::env::var("APPDATA") {
            Ok(p) => p,
            Err(_) => return,
        })
        .join("DeepSeekWidget")
        .join("captures_debug.json");
        let s = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return,
        };
        let arr: Vec<Value> = serde_json::from_str(&s).unwrap();
        let jsons: Vec<Value> = arr.iter().filter_map(|e| e.get("json").cloned()).collect();
        let m = normalize_captures(&jsons);
        for (d, v) in &m {
            println!("{d}: {v:?}");
        }
        let d = m.get("2026-07-08").expect("2026-07-08 应存在");
        assert_eq!(d.hit, 959488, "hit");
        assert_eq!(d.miss, 19557815, "miss");
        assert_eq!(d.output, 2490835, "output");
        assert_eq!(d.requests, 1241, "requests");
    }

    #[test]
    fn balance_extract() {
        let cap = json!({"balance_infos":[{"currency":"CNY","total_balance":"88.50",
            "granted_balance":"10.00","topped_up_balance":"78.50"}]});
        let b = find_balance(&[cap]).unwrap();
        assert_eq!(b.total, "88.50");
        assert_eq!(b.currency, "CNY");
        assert!(b.is_available);
        assert!(b.from_web);
    }

    #[test]
    fn rewrite_iso_range() {
        let s = ymd(2026, 6, 16);
        let e = ymd(2026, 7, 15);
        let url = "https://x/api?start_date=2026-05-01&end_date=2026-05-30&g=day";
        let r = rewrite_dates(url, s, e);
        assert!(r.contains("2026-06-16"), "{}", r);
        assert!(r.contains("2026-07-15"), "{}", r);
        assert!(!r.contains("2026-05-01"), "{}", r);
    }

    #[test]
    fn rewrite_epoch_range() {
        let s = ymd(2026, 6, 16);
        let e = ymd(2026, 7, 15);
        let body = r#"{"begin":1746057600000,"end":1748649600000}"#;
        let r = rewrite_dates(body, s, e);
        let v: Value = serde_json::from_str(&r).unwrap();
        let begin = v["begin"].as_i64().unwrap();
        let end = v["end"].as_i64().unwrap();
        assert!(begin < end, "begin={} end={}", begin, end);
        // begin 应对应 6/16 0 点毫秒；end 为 7/16 0 点毫秒（独占）
        assert_eq!(begin, s.midnight_unix() * 1000);
        assert_eq!(end, (e.end_unix() + 1) * 1000);
    }

    #[test]
    fn rewrite_does_not_touch_other_numbers() {
        let s = ymd(2026, 6, 16);
        let e = ymd(2026, 7, 15);
        // 短数字（如分页 page=2、size=100）不应被当成时间戳
        let url = "https://x/api?page=2&size=100&start=2026-05-01&end=2026-05-30";
        let r = rewrite_dates(url, s, e);
        assert!(r.contains("page=2"));
        assert!(r.contains("size=100"));
    }
}
