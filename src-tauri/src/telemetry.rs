//! 遥测组装与输出（spec §3/§4）：把 limits + insights + history 组装成 __ggTelemetry，
//! 每 ~5s 原子写 telemetry.json（浮窗/网页）与 skins/_shared/telemetry.js（皮肤胶囊）。
//! build_snapshot 是纯函数（吃已取好的数据），便于单测；start() 是后台循环。

use crate::history::Sample;
use crate::insights::{Price, Record};
use chrono::{Datelike, Local, TimeZone, Timelike};
use serde_json::{json, Value};
use std::collections::HashMap;

fn window_len(name: &str) -> Option<i64> {
    match name {
        "5h" => Some(18000),
        "7d" | "7d_fable" => Some(604800),
        "30d" => Some(2592000),
        _ => None,
    }
}
fn window_label(name: &str) -> &str {
    match name {
        "5h" => "5 小时",
        "7d" => "7 天",
        "7d_fable" => "7 天 · Fable",
        "30d" => "30 天",
        other => other,
    }
}

/// 组装 __ggTelemetry 值。now 为 Unix 秒；periods 为 (今日/本周/本月) 起点。
#[allow(clippy::too_many_arguments)]
pub fn build_snapshot(
    limits: &Value,
    connected: bool,
    config: &Value,
    records: &[Record],
    prices: &HashMap<String, Price>,
    samples: &[Sample],
    now: i64,
    periods: (i64, i64, i64), // (today_start, week_start, month_start)
) -> Value {
    let cpu = config.get("centsPerUnit").and_then(Value::as_f64).unwrap_or(0.31);
    let to_usd = |units: f64| units * cpu / 100.0;

    let account = limits
        .get("subject")
        .and_then(Value::as_str)
        .map(|s| {
            let tail: String = s.chars().rev().take(6).collect::<String>().chars().rev().collect();
            format!("…{tail}")
        })
        .unwrap_or_default();

    let mut windows = Vec::new();
    for w in limits.get("windows").and_then(Value::as_array).into_iter().flatten() {
        let name = w.get("name").and_then(Value::as_str).unwrap_or("");
        let Some(len) = window_len(name) else { continue };
        let used = w.get("used").and_then(Value::as_f64).unwrap_or(0.0);
        let budget = w.get("budget").and_then(Value::as_f64).unwrap_or(0.0);
        let reset_at = w.get("reset_at").and_then(Value::as_i64).unwrap_or(now);
        if !(budget > 0.0) {
            continue;
        }
        let used_pct = used / budget * 100.0;
        let remain_pct = (100.0 - used_pct).max(0.0);
        let remaining = (reset_at - now).max(0);
        let pace_pct = (((len - remaining) as f64 / len as f64) * 100.0).clamp(0.0, 100.0);
        let delta = used_pct - pace_pct;
        let delta_text = if delta >= 0.0 {
            format!("匀速快 {:.0}%", delta.abs())
        } else {
            format!("匀速省 {:.0}%", delta.abs())
        };
        let win_start = reset_at - len;
        let fable_only = name == "7d_fable";
        let req_count = crate::insights::req_count(records, win_start, now, fable_only);
        let burn = crate::history::burn_per_hour(samples, name, budget, now, 3600).unwrap_or(0.0);
        let exhaust_text = crate::history::exhaust_at(remain_pct, burn, reset_at, now)
            .map(fmt_exhaust)
            .unwrap_or_else(|| "够到重置".into());

        windows.push(json!({
            "name": name,
            "label": window_label(name),
            "usedUnits": used, "budgetUnits": budget, "remainUnits": (budget - used).max(0.0),
            "usedPct": round1(used_pct), "remainPct": round1(remain_pct),
            "pacePct": round1(pace_pct), "deltaText": delta_text,
            "usedUsd": to_usd(used), "budgetUsd": to_usd(budget), "remainUsd": to_usd((budget - used).max(0.0)),
            "reqCount": req_count,
            "burnPerHour": round2(burn),
            "resetText": fmt_reset(remaining),
            "exhaustText": exhaust_text,
            "spark": crate::history::spark(samples, name, 40),
        }));
    }

    let (dt, dr) = crate::insights::spend(records, prices, periods.0, now);
    let (wt, wr) = crate::insights::spend(records, prices, periods.1, now);
    let (mt, mr) = crate::insights::spend(records, prices, periods.2, now);
    // 滚动 30 天：给面板做稳定的大字（日历"本月"会在月初归零，不适合当主数字）
    let (r30t, r30r) = crate::insights::spend(records, prices, now - 30 * 86400, now);
    let live = crate::insights::throughput(records, now, 3600);

    json!({
        "at": now,
        "connected": connected,
        "plan": {
            "label": config.get("planLabel").cloned().unwrap_or(Value::Null),
            "validUntil": config.get("validUntil").cloned().unwrap_or(Value::Null),
            "account": account,
        },
        "centsPerUnit": cpu,
        "windows": windows,
        "totals": {
            "today": { "usd": round2(dt), "reqs": dr },
            "week":  { "usd": round2(wt), "reqs": wr },
            "month": { "usd": round2(mt), "reqs": mr },
            "rolling30": { "usd": round2(r30t), "reqs": r30r },
        },
        "live": live.map(|(m, tps, spt)| json!({
            "model": m, "tokPerSec": round1(tps), "secPerTurn": round1(spt)
        })).unwrap_or(Value::Null),
    })
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// 剩余秒 → "6天15:06:46" / "01:48:53"
fn fmt_reset(sec: i64) -> String {
    let s = sec.max(0);
    let (d, h, m, ss) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60, s % 60);
    if d > 0 {
        format!("{d}天{h:02}:{m:02}:{ss:02}")
    } else {
        format!("{h:02}:{m:02}:{ss:02}")
    }
}

/// 耗尽 Unix 秒 → 本地 "M/D HH:MM尽"
fn fmt_exhaust(at: i64) -> String {
    match Local.timestamp_opt(at, 0).single() {
        Some(dt) => format!("{}/{} {:02}:{:02}尽", dt.month(), dt.day(), dt.hour(), dt.minute()),
        None => "".into(),
    }
}

/// (今日, 本周一, 本月一) 起点的 Unix 秒（本地日历）。
pub fn local_period_starts(now: i64) -> (i64, i64, i64) {
    let dt = Local.timestamp_opt(now, 0).single().unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap());
    let day = dt.date_naive();
    let today = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|d| d.timestamp())
        .unwrap_or(now);
    let week = today - (day.weekday().num_days_from_monday() as i64) * 86400;
    let month_day = day.with_day(1).unwrap_or(day);
    let month = Local
        .from_local_datetime(&month_day.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .map(|d| d.timestamp())
        .unwrap_or(today);
    (today, week, month)
}

/// 原子写：同目录 temp + rename。
fn atomic_write(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// 后台循环：每 5s 取数、按 60s 采样历史、组装、双写。
pub fn start(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let prices = crate::insights::load_prices();
        let skins_shared = std::path::PathBuf::from(
            std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into()),
        )
        .join(".mirasim")
        .join("skins")
        .join("_shared");
        loop {
            // 每轮包 catch_unwind：任何坏数据导致的 panic 只丢一帧，绝不拖垮线程/进程
            let once = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tick(&app, &prices, &skins_shared);
            }));
            if once.is_err() {
                eprintln!("telemetry: tick panicked, skipping this cycle");
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

fn tick(
    app: &tauri::AppHandle,
    prices: &HashMap<String, Price>,
    skins_shared: &std::path::Path,
) {
    use tauri::Manager;
    {
            let now = chrono::Local::now().timestamp();
            let state = app.state::<crate::relay::RelayState>();
            let res = tauri::async_runtime::block_on(crate::relay::fetch_limits_core(state.inner()));
            let config: Value =
                serde_json::from_str(&crate::window::get_config()).unwrap_or(Value::Null);
            let (limits, connected) = match &res {
                Ok(r) => (serde_json::from_str::<Value>(&r.json).unwrap_or(Value::Null), true),
                Err(_) => (last_limits(), false),
            };

            // 历史采样（内部 60s 节流）
            let mut samples = crate::history::load();
            if limits.is_object() {
                let mut used = HashMap::new();
                for w in limits.get("windows").and_then(Value::as_array).into_iter().flatten() {
                    if let (Some(n), Some(u)) = (
                        w.get("name").and_then(Value::as_str),
                        w.get("used").and_then(Value::as_f64),
                    ) {
                        used.insert(n.to_string(), u);
                    }
                }
                if !used.is_empty() {
                    samples = crate::history::maybe_append(samples, now, used);
                }
            }

            let records = crate::insights::load_records();
            let snap = build_snapshot(
                &limits,
                connected,
                &config,
                &records,
                prices,
                &samples,
                now,
                local_period_starts(now),
            );

            // ① telemetry.json
            let json_str = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
            let _ = atomic_write(&crate::window::appdata_dir().join("telemetry.json"), &json_str);
            // 缓存最后 limits 供断连时兜底
            if connected {
                let _ = atomic_write(
                    &crate::window::appdata_dir().join("last-limits.json"),
                    &serde_json::to_string(&limits).unwrap_or_default(),
                );
            }
            // ② skins/_shared/telemetry.js（皮肤在则写）
            if skins_shared.is_dir() {
                let js = format!(
                    "window.__ggTelemetry={};window.dispatchEvent(new Event('gg-telemetry'));\n",
                    json_str
                );
                let _ = atomic_write(&skins_shared.join("telemetry.js"), &js);
            }
    }
}

fn last_limits() -> Value {
    std::fs::read_to_string(crate::window::appdata_dir().join("last-limits.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Value {
        json!({ "centsPerUnit": 0.31, "planLabel": "MAX", "validUntil": "2027-08-11" })
    }
    fn limits() -> Value {
        json!({ "subject": "usr_abcdef123456", "windows": [
            { "name": "5h", "used": 28577.0, "budget": 156800.0, "reset_at": 100000 },
            { "name": "7d", "used": 182308.0, "budget": 560000.0, "reset_at": 700000 }
        ]})
    }

    #[test]
    fn snapshot_shape_and_usd() {
        let snap = build_snapshot(&limits(), true, &cfg(), &[], &HashMap::new(), &[], 50000, (0, 0, 0));
        assert_eq!(snap["connected"], true);
        assert_eq!(snap["plan"]["account"], "…123456");
        let w = &snap["windows"][0];
        assert_eq!(w["name"], "5h");
        // 28577 * 0.31 / 100 ≈ 88.59
        let used_usd = w["usedUsd"].as_f64().unwrap();
        assert!((used_usd - 88.5887).abs() < 0.01);
        assert_eq!(w["remainUnits"].as_f64().unwrap(), 156800.0 - 28577.0);
    }

    #[test]
    fn delta_vocabulary_matches_screenshot() {
        // 5h：reset 在 now+6533s（18000s 窗），pace≈63.7%，used 18.2% → 匀速省 ~45%
        let lim = json!({ "windows": [
            { "name": "5h", "used": 18.2, "budget": 100.0, "reset_at": 6533 }
        ]});
        let snap = build_snapshot(&lim, true, &cfg(), &[], &HashMap::new(), &[], 0, (0, 0, 0));
        let dt = snap["windows"][0]["deltaText"].as_str().unwrap();
        assert!(dt.starts_with("匀速省"), "got {dt}");
    }

    #[test]
    fn reset_and_exhaust_formatting() {
        assert_eq!(fmt_reset(6 * 86400 + 15 * 3600 + 6 * 60 + 46), "6天15:06:46");
        assert_eq!(fmt_reset(1 * 3600 + 48 * 60 + 53), "01:48:53");
    }

    #[test]
    fn skips_unknown_windows_and_zero_budget() {
        let lim = json!({ "windows": [
            { "name": "weird", "used": 1.0, "budget": 100.0, "reset_at": 100 },
            { "name": "7d", "used": 1.0, "budget": 0.0, "reset_at": 100 }
        ]});
        let snap = build_snapshot(&lim, true, &cfg(), &[], &HashMap::new(), &[], 0, (0, 0, 0));
        assert_eq!(snap["windows"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn no_token_in_output() {
        let snap = build_snapshot(&limits(), true, &cfg(), &[], &HashMap::new(), &[], 50000, (0, 0, 0));
        let s = serde_json::to_string(&snap).unwrap();
        assert!(!s.to_lowercase().contains("bearer"));
        assert!(!s.contains("ANTHROPIC_AUTH_TOKEN"));
    }
}
