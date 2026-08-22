//! 内建网页仪表盘：http://127.0.0.1:{webPort}/ 显示剩余 credits。
//! - `GET /` 单文件页面（编译期内嵌）；
//! - `GET /api/limits` 复用 relay 的发现/自愈取 limits，并顺带探 `/v1/credits`
//!   （mirasim 尚未上线的端点，app 自己也在轮询它）——哪天返回 200 就原样透传，
//!   页面自动切换成官方余额。
//! 只绑回环地址；浏览器同源访问，无需鉴权。

use tauri::Manager;
use tiny_http::{Header, Response, Server};

const DASHBOARD: &str = include_str!("../../ui/dashboard.html");

pub fn start(app: tauri::AppHandle, port: u16) {
    std::thread::spawn(move || {
        let server = match Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("webui: bind 127.0.0.1:{port} failed: {e}");
                return;
            }
        };
        for req in server.incoming_requests() {
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or("/");
            match path {
                "/" | "/index.html" => {
                    let _ = req.respond(with_type(
                        Response::from_string(DASHBOARD),
                        "text/html; charset=utf-8",
                    ));
                }
                "/api/limits" => {
                    let state = app.state::<crate::relay::RelayState>();
                    let body = tauri::async_runtime::block_on(api_limits(state.inner()));
                    let _ = req.respond(with_type(
                        Response::from_string(body),
                        "application/json; charset=utf-8",
                    ));
                }
                _ => {
                    let _ = req.respond(Response::empty(404));
                }
            }
        }
    });
}

fn with_type(resp: Response<std::io::Cursor<Vec<u8>>>, ct: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    resp.with_header(Header::from_bytes("Content-Type", ct).expect("header"))
        .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").expect("header"))
        .with_header(Header::from_bytes("Cache-Control", "no-store").expect("header"))
}

async fn api_limits(state: &crate::relay::RelayState) -> String {
    match crate::relay::fetch_limits_core(state).await {
        Ok(r) => {
            let credits = probe_credits(&r.endpoint).await;
            format!(
                "{{\"ok\":true,\"endpoint\":{},\"limits\":{},\"credits\":{},\"plan\":{}}}",
                serde_json::to_string(&r.endpoint).unwrap_or_else(|_| "\"\"".into()),
                r.json,
                credits.unwrap_or_else(|| "null".into()),
                plan_json()
            )
        }
        Err(e) => format!(
            "{{\"ok\":false,\"error\":{},\"plan\":{}}}",
            serde_json::to_string(&e).unwrap_or_else(|_| "\"relay-not-found\"".into()),
            plan_json()
        ),
    }
}

/// 套餐信息（无接口可取，配置填报）：planLabel / validUntil / totalCredits。
fn plan_json() -> String {
    let cfg: serde_json::Value =
        serde_json::from_str(&crate::window::get_config()).unwrap_or(serde_json::Value::Null);
    let pick = serde_json::json!({
        "label": cfg.get("planLabel").cloned().unwrap_or(serde_json::Value::Null),
        "validUntil": cfg.get("validUntil").cloned().unwrap_or(serde_json::Value::Null),
        "totalCredits": cfg.get("totalCredits").cloned().unwrap_or(serde_json::Value::Null),
    });
    pick.to_string()
}

/// /v1/credits 目前 404；一旦上线且返回合法 JSON 就透传。
async fn probe_credits(endpoint: &str) -> Option<String> {
    let client = crate::discovery::probe_client();
    let resp = client
        .get(format!("{endpoint}/v1/credits"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    serde_json::from_str::<serde_json::Value>(&text).ok()?;
    Some(text)
}
