//! relay 端口发现（spec §4.1）。
//! 端口由 mirasim 每次启动动态分配且磁盘无记录，只能扫描本机监听端口逐个认领。

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

pub fn cache_path() -> PathBuf {
    crate::window::appdata_dir().join("endpoint.json")
}

pub fn load_cached() -> Option<u16> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cache_path()).ok()?).ok()?;
    u16::try_from(v.get("port")?.as_u64()?).ok()
}

pub fn save_cached(port: u16) {
    let _ = std::fs::write(cache_path(), format!("{{\"port\":{port}}}"));
}

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

/// 探一个端口；命中返回响应原文。
pub async fn probe(client: &reqwest::Client, port: u16) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/v1/limits");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    looks_like_relay(&v).then_some(text)
}

/// 解析一行 `netstat -ano -p tcp`。
/// 监听态的判断用外部地址 `0.0.0.0:0` 特征而非状态列文案 —— 状态列在
/// 非英文 Windows 上可能被本地化，外部地址不会。
pub fn parse_netstat_line(line: &str) -> Option<u16> {
    let mut it = line.split_whitespace();
    let proto = it.next()?;
    if !proto.eq_ignore_ascii_case("tcp") {
        return None;
    }
    let local = it.next()?;
    let foreign = it.next()?;
    if foreign != "0.0.0.0:0" {
        return None;
    }
    let (addr, port) = local.rsplit_once(':')?;
    if addr != "127.0.0.1" && addr != "0.0.0.0" {
        return None;
    }
    port.parse().ok()
}

/// 枚举本机 TCP 监听端口（回环可达的）。
pub fn listening_ports() -> Vec<u16> {
    let mut cmd = Command::new("netstat");
    cmd.args(["-ano", "-p", "tcp"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：别闪控制台
    }
    let Ok(out) = cmd.output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut ports: Vec<u16> = text.lines().filter_map(parse_netstat_line).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// 全量扫描：并发 ≤32 逐批探测，命中即停。返回 (port, 响应原文)。
pub async fn scan(client: &reqwest::Client) -> Option<(u16, String)> {
    let ports = tauri::async_runtime::spawn_blocking(listening_ports)
        .await
        .unwrap_or_default();
    for chunk in ports.chunks(32) {
        let probes = chunk.iter().map(|&p| {
            let c = client.clone();
            tauri::async_runtime::spawn(async move { probe(&c, p).await.map(|t| (p, t)) })
        });
        for h in probes {
            if let Ok(Some(hit)) = h.await {
                return Some(hit);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const FIXTURE: &str = include_str!("../../ui/tests/fixtures/limits.json");

    /// 起一个一次性假 HTTP 服务，返回给定 body。
    fn fake_server(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        port
    }

    #[test]
    fn netstat_parse_accepts_loopback_listeners_only() {
        assert_eq!(
            parse_netstat_line("  TCP    127.0.0.1:51678    0.0.0.0:0    LISTENING    1234"),
            Some(51678)
        );
        // 状态列本地化也认得出（不依赖 LISTENING 文案）
        assert_eq!(
            parse_netstat_line("  TCP    0.0.0.0:8080    0.0.0.0:0    侦听    99"),
            Some(8080)
        );
        // 已建立连接：外部地址非 0.0.0.0:0
        assert_eq!(
            parse_netstat_line("  TCP    127.0.0.1:51678    127.0.0.1:60000    ESTABLISHED  1"),
            None
        );
        // 非回环绑定不测（只从 127.0.0.1 访问）
        assert_eq!(
            parse_netstat_line("  TCP    192.168.1.5:445    0.0.0.0:0    LISTENING    4"),
            None
        );
        assert_eq!(parse_netstat_line("  UDP    127.0.0.1:5353    *:*"), None);
    }

    #[test]
    fn relay_shape_recognition() {
        let good: Value = serde_json::from_str(FIXTURE).unwrap();
        assert!(looks_like_relay(&good));
        // 缺字段 → 拒绝
        let bad: Value =
            serde_json::from_str(r#"{"windows":[{"name":"5h","used":1,"budget":100}]}"#).unwrap();
        assert!(!looks_like_relay(&bad));
        // windows 非数组 / 空数组 → 拒绝
        assert!(!looks_like_relay(&serde_json::json!({ "windows": [] })));
        assert!(!looks_like_relay(&serde_json::json!({ "ok": true })));
    }

    #[tokio::test]
    async fn probe_claims_fixture_and_rejects_lookalike() {
        let client = probe_client();
        let good_port = fake_server(FIXTURE);
        let hit = probe(&client, good_port).await;
        assert!(hit.is_some(), "should claim the fake relay");

        // 相似但缺 reset_at 的服务：200 + JSON，仍须拒绝
        let bad_port =
            fake_server(r#"{"windows":[{"name":"5h","used":1,"budget":100,"reset":9}]}"#);
        assert!(probe(&client, bad_port).await.is_none(), "must reject lookalike");
    }
}
