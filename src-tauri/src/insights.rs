//! insights 日志聚合（spec §4 花费视角）：读 ~/.mirasim/insights/usage-*.ndjson
//! 与 models-dev-cache.json，算真实列表价花费/次数/吞吐。纯函数吃"已解析记录 +
//! 价目表"，便于 fixture 单测；只有 load_* 碰磁盘。

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// 一条 relay 请求（只留聚合要用的字段）。
#[derive(Clone, Debug)]
pub struct Record {
    pub ts: i64, // Unix 秒
    pub model: String,
    pub via_relay: bool,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub duration_ms: f64,
}

/// 价目表项：(input, output, cache_read, cache_write) 美元/百万 token。
pub type Price = (f64, f64, f64, f64);

fn home() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into()))
}

/// 解析一行 ndjson；非 200 / 非 relay / 缺时戳的返回 None（聚合只关心成功的 relay 流量）。
pub fn parse_line(line: &str) -> Option<Record> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("status").and_then(Value::as_i64) != Some(200) {
        return None;
    }
    let host = v.get("upstreamHost").and_then(Value::as_str).unwrap_or("");
    let via_relay = host.contains("relay");
    let ts = parse_ts(v.get("ts")?)?;
    let num = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    Some(Record {
        ts,
        model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
        via_relay,
        input: num("input"),
        output: num("output"),
        cache_read: num("cacheRead"),
        cache_write: num("cacheWrite"),
        duration_ms: num("durationMs"),
    })
}

/// ts 可能是 ISO 字符串或 epoch（秒/毫秒）。
fn parse_ts(v: &Value) -> Option<i64> {
    if let Some(s) = v.as_str() {
        return chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp());
    }
    if let Some(n) = v.as_f64() {
        return Some(if n > 1e12 { (n / 1000.0) as i64 } else { n as i64 });
    }
    None
}

/// 读并解析全部 relay 记录（跨 usage-*.ndjson）。
pub fn load_records() -> Vec<Record> {
    let dir = home().join(".mirasim").join("insights");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("usage-") && name.ends_with(".ndjson")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(e.path()) {
            for line in text.lines() {
                if let Some(r) = parse_line(line) {
                    if r.via_relay {
                        out.push(r);
                    }
                }
            }
        }
    }
    out
}

/// models.dev 镜像价目表（只取 data.anthropic + openai/moonshotai 补漏，避开异价 provider）。
pub fn load_prices() -> HashMap<String, Price> {
    let mut prices = HashMap::new();
    let path = home()
        .join(".mirasim")
        .join("models-dev-cache.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return prices;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return prices;
    };
    for prov in ["anthropic", "openai", "moonshotai"] {
        let Some(models) = v.pointer(&format!("/data/{prov}/models")).and_then(Value::as_object)
        else {
            continue;
        };
        for (id, m) in models {
            let Some(c) = m.get("cost") else { continue };
            let Some(inp) = c.get("input").and_then(Value::as_f64) else {
                continue;
            };
            prices.entry(id.clone()).or_insert((
                inp,
                c.get("output").and_then(Value::as_f64).unwrap_or(0.0),
                c.get("cache_read").and_then(Value::as_f64).unwrap_or(inp * 0.1),
                c.get("cache_write").and_then(Value::as_f64).unwrap_or(inp * 1.25),
            ));
        }
    }
    prices
}

fn record_usd(r: &Record, prices: &HashMap<String, Price>) -> Option<f64> {
    let (pi, po, pcr, pcw) = prices.get(&r.model)?;
    Some((r.input * pi + r.output * po + r.cache_read * pcr + r.cache_write * pcw) / 1e6)
}

/// 窗口内 relay 请求数；fable_only 时只数 fable 模型。
pub fn req_count(records: &[Record], start: i64, end: i64, fable_only: bool) -> u32 {
    records
        .iter()
        .filter(|r| r.ts >= start && r.ts < end)
        .filter(|r| !fable_only || r.model.contains("fable"))
        .count() as u32
}

/// [start,end) 内真实列表价花费与次数。
pub fn spend(records: &[Record], prices: &HashMap<String, Price>, start: i64, end: i64) -> (f64, u32) {
    let mut usd = 0.0;
    let mut n = 0u32;
    for r in records.iter().filter(|r| r.ts >= start && r.ts < end) {
        if let Some(u) = record_usd(r, prices) {
            usd += u;
            n += 1;
        }
    }
    (usd, n)
}

/// 最近 lookback_secs 内的吞吐：当前模型、平均 tok/s、平均每轮秒。空则 None。
pub fn throughput(records: &[Record], now: i64, lookback_secs: i64) -> Option<(String, f64, f64)> {
    let recent: Vec<&Record> = records
        .iter()
        .filter(|r| r.ts >= now - lookback_secs && r.output > 0.0 && r.duration_ms > 0.0)
        .collect();
    if recent.is_empty() {
        return None;
    }
    let model = recent.last().unwrap().model.clone();
    let tok_per_s: f64 = recent
        .iter()
        .map(|r| r.output / (r.duration_ms / 1000.0))
        .sum::<f64>()
        / recent.len() as f64;
    let sec_per_turn: f64 =
        recent.iter().map(|r| r.duration_ms / 1000.0).sum::<f64>() / recent.len() as f64;
    Some((pretty_model(&model), tok_per_s, sec_per_turn))
}

/// "claude-opus-4-8" → "Opus 4.8"、"claude-fable-5" → "Fable 5"。
/// 通用：去 claude- 前缀，首段首字母大写作名字，其余（版本号）用 . 连接。
fn pretty_model(id: &str) -> String {
    let s = id.strip_prefix("claude-").unwrap_or(id);
    let parts: Vec<&str> = s.split('-').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return id.into();
    }
    let mut name = parts[0].to_string();
    if let Some(c) = name.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    if parts.len() > 1 {
        format!("{} {}", name, parts[1..].join("."))
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> HashMap<String, Price> {
        let mut p = HashMap::new();
        p.insert("claude-fable-5".to_string(), (10.0, 50.0, 1.0, 12.5));
        p.insert("claude-opus-5".to_string(), (5.0, 25.0, 0.5, 6.25));
        p
    }

    fn rec(ts: i64, model: &str, inp: f64, out: f64, dur: f64) -> Record {
        Record {
            ts,
            model: model.into(),
            via_relay: true,
            input: inp,
            output: out,
            cache_read: 0.0,
            cache_write: 0.0,
            duration_ms: dur,
        }
    }

    #[test]
    fn parses_iso_and_epoch_ts() {
        // ISO → epoch（值由 chrono rfc3339 决定，此处锁定回归基准）
        let iso = parse_ts(&serde_json::json!("2026-08-22T11:53:19Z")).unwrap();
        assert_eq!(iso, 1787399599);
        // 秒 / 毫秒两种数值都归一到秒
        assert_eq!(parse_ts(&serde_json::json!(1787399599)).unwrap(), 1787399599);
        assert_eq!(parse_ts(&serde_json::json!(1787399599000i64)).unwrap(), 1787399599);
    }

    #[test]
    fn parse_line_filters_non200_and_extracts() {
        let ok = r#"{"ts":"2026-08-22T00:00:00Z","status":200,"upstreamHost":"relay.mirasim.ai","model":"claude-fable-5","input":100,"output":20,"durationMs":2000}"#;
        let r = parse_line(ok).unwrap();
        assert_eq!(r.model, "claude-fable-5");
        assert!(r.via_relay);
        assert_eq!(r.output, 20.0);
        let bad = r#"{"ts":"2026-08-22T00:00:00Z","status":400,"model":"x"}"#;
        assert!(parse_line(bad).is_none());
    }

    #[test]
    fn req_count_windows_and_fable_filter() {
        let rs = vec![
            rec(100, "claude-fable-5", 1.0, 1.0, 1.0),
            rec(150, "claude-opus-5", 1.0, 1.0, 1.0),
            rec(999, "claude-fable-5", 1.0, 1.0, 1.0),
        ];
        assert_eq!(req_count(&rs, 0, 200, false), 2);
        assert_eq!(req_count(&rs, 0, 200, true), 1); // 只 fable
        assert_eq!(req_count(&rs, 0, 10000, false), 3);
    }

    #[test]
    fn spend_uses_list_price() {
        let rs = vec![rec(100, "claude-fable-5", 1_000_000.0, 0.0, 1.0)]; // 1M input @ $10/M
        let (usd, n) = spend(&rs, &prices(), 0, 200);
        assert!((usd - 10.0).abs() < 1e-9);
        assert_eq!(n, 1);
        // 窗口外不计
        assert_eq!(spend(&rs, &prices(), 0, 50).1, 0);
    }

    #[test]
    fn throughput_averages_recent() {
        let rs = vec![
            rec(1000, "claude-fable-5", 10.0, 100.0, 5000.0), // 20 tok/s, 5s
            rec(1010, "claude-fable-5", 10.0, 200.0, 5000.0), // 40 tok/s, 5s
        ];
        let (m, tps, spt) = throughput(&rs, 1010, 100).unwrap();
        assert_eq!(m, "Fable 5");
        assert!((tps - 30.0).abs() < 1e-9);
        assert!((spt - 5.0).abs() < 1e-9);
        assert!(throughput(&rs, 999999, 100).is_none()); // 太旧
    }
}
