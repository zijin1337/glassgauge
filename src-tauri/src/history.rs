//! used 采样历史（spec §4）：环形缓冲 %APPDATA%/glassgauge/history.json，每 ~60s 追加
//! 各窗口 used 值，供火花线 / 燃烧率(%/h) / 预计耗尽。纯函数吃样本切片，便于单测；
//! 只有 load/save 碰磁盘。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const CAP: usize = 500; // 约 8 小时 @ 60s
const MIN_GAP_SECS: i64 = 55; // 采样节流

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Sample {
    pub t: i64,                    // Unix 秒
    pub u: HashMap<String, f64>,   // 窗口名 -> used
}

fn path() -> std::path::PathBuf {
    crate::window::appdata_dir().join("history.json")
}

/// 读历史；损坏即当空（不崩）。
pub fn load() -> Vec<Sample> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 距上次采样超过 MIN_GAP_SECS 才追加；写时截断到 CAP。返回更新后的序列。
pub fn maybe_append(mut samples: Vec<Sample>, now: i64, used: HashMap<String, f64>) -> Vec<Sample> {
    if samples.last().is_some_and(|s| now - s.t < MIN_GAP_SECS) {
        return samples;
    }
    samples.push(Sample { t: now, u: used });
    if samples.len() > CAP {
        let drop = samples.len() - CAP;
        samples.drain(0..drop);
    }
    let _ = std::fs::write(
        path(),
        serde_json::to_string(&samples).unwrap_or_else(|_| "[]".into()),
    );
    samples
}

/// 某窗口的 used 序列降采样到约 count 点（供火花线）。
pub fn spark(samples: &[Sample], name: &str, count: usize) -> Vec<f64> {
    let series: Vec<f64> = samples.iter().filter_map(|s| s.u.get(name).copied()).collect();
    if series.len() <= count || count == 0 {
        return series;
    }
    // 等间隔抽样，保留首尾
    let step = (series.len() - 1) as f64 / (count - 1) as f64;
    (0..count)
        .map(|i| series[(i as f64 * step).round() as usize])
        .collect()
}

/// 最近 lookback 秒内 usedPct 对时间(小时)的最小二乘斜率 = 燃烧率 %/h；样本不足返回 None。
pub fn burn_per_hour(
    samples: &[Sample],
    name: &str,
    budget: f64,
    now: i64,
    lookback_secs: i64,
) -> Option<f64> {
    if !(budget > 0.0) {
        return None;
    }
    let pts: Vec<(f64, f64)> = samples
        .iter()
        .filter(|s| s.t >= now - lookback_secs)
        .filter_map(|s| s.u.get(name).map(|&u| ((s.t as f64) / 3600.0, u / budget * 100.0)))
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-12 {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    Some(slope.max(0.0)) // 负斜率（刚重置）当 0
}

/// 预计耗尽的 Unix 秒：按燃烧率线性外推，落点在重置之前才返回。
pub fn exhaust_at(remain_pct: f64, burn_per_hour: f64, reset_at: i64, now: i64) -> Option<i64> {
    if burn_per_hour <= 1e-6 || remain_pct <= 0.0 {
        return None;
    }
    let secs = (remain_pct / burn_per_hour * 3600.0) as i64;
    let at = now + secs;
    (at < reset_at).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: i64, name: &str, used: f64) -> Sample {
        let mut u = HashMap::new();
        u.insert(name.to_string(), used);
        Sample { t, u }
    }

    #[test]
    fn spark_downsamples_keeping_ends() {
        let samples: Vec<Sample> = (0..100).map(|i| s(i, "7d", i as f64)).collect();
        let sp = spark(&samples, "7d", 5);
        assert_eq!(sp.len(), 5);
        assert_eq!(sp[0], 0.0);
        assert_eq!(*sp.last().unwrap(), 99.0);
    }

    #[test]
    fn spark_returns_all_when_fewer() {
        let samples = vec![s(0, "7d", 1.0), s(60, "7d", 2.0)];
        assert_eq!(spark(&samples, "7d", 40), vec![1.0, 2.0]);
    }

    #[test]
    fn burn_rate_linear() {
        // 每小时 used 涨 100（budget 1000 → 10%/h）
        let samples = vec![
            s(0, "7d", 0.0),
            s(3600, "7d", 100.0),
            s(7200, "7d", 200.0),
        ];
        let b = burn_per_hour(&samples, "7d", 1000.0, 7200, 100000).unwrap();
        assert!((b - 10.0).abs() < 1e-6);
    }

    #[test]
    fn burn_rate_clamps_negative_to_zero() {
        let samples = vec![s(0, "7d", 500.0), s(3600, "7d", 10.0)]; // 重置后骤降
        assert_eq!(burn_per_hour(&samples, "7d", 1000.0, 3600, 100000).unwrap(), 0.0);
    }

    #[test]
    fn exhaust_before_reset_only() {
        // 剩 20%，10%/h → 2h 后耗尽
        let now = 1000;
        let at = exhaust_at(20.0, 10.0, now + 3 * 3600, now).unwrap();
        assert_eq!(at, now + 2 * 3600);
        // 若重置在 1h 后（早于耗尽）→ None（烧不完）
        assert!(exhaust_at(20.0, 10.0, now + 3600, now).is_none());
        // 零燃烧 → None
        assert!(exhaust_at(20.0, 0.0, now + 99999, now).is_none());
    }
}
