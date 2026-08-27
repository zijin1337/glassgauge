//! relay 探测。mirasim 更新后 `/v1/limits` 需鉴权，凭证来自 auth 模块
//! （进程环境提取或手动配置）。此处只负责"带 Bearer 打一次、认领响应形状"。

use crate::auth::Creds;
use serde_json::Value;
use std::time::Duration;

/// 探测用 HTTP 客户端：800ms 超时，绕过系统代理（目标是回环地址）。
pub fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .no_proxy()
        .build()
        .expect("reqwest client")
}

/// 认领特征：`windows` 为非空数组，每个元素都有 name/used/budget/reset_at 四字段。
/// 不能只看 HTTP 200 —— 别的本地服务也可能应答这个路径。
pub fn looks_like_relay(v: &Value) -> bool {
    match v.get("windows").and_then(Value::as_array) {
        Some(ws) if !ws.is_empty() => ws.iter().all(|w| {
            w.get("name").is_some_and(Value::is_string)
                && w.get("used").is_some_and(Value::is_number)
                && w.get("budget").is_some_and(Value::is_number)
                && w.get("reset_at").is_some_and(Value::is_number)
        }),
        _ => false,
    }
}

/// 用给定凭证打一次 limits；命中返回响应原文。
pub async fn probe(client: &reqwest::Client, creds: &Creds) -> Option<String> {
    let url = format!("{}/v1/limits", creds.base_url);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", creds.token))
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    looks_like_relay(&v).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../ui/tests/fixtures/limits.json");

    #[test]
    fn relay_shape_recognition() {
        let good: Value = serde_json::from_str(FIXTURE).unwrap();
        assert!(looks_like_relay(&good));
        let bad: Value =
            serde_json::from_str(r#"{"windows":[{"name":"5h","used":1,"budget":100}]}"#).unwrap();
        assert!(!looks_like_relay(&bad));
        assert!(!looks_like_relay(&serde_json::json!({ "windows": [] })));
        assert!(!looks_like_relay(&serde_json::json!({ "ok": true })));
    }
}
