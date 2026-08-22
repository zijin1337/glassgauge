//! 取数命令（spec §4.2）：语义不解析，把 /v1/limits 原文交给前端。

use crate::discovery;
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;

pub struct RelayState(pub Mutex<Option<u16>>);

impl Default for RelayState {
    fn default() -> Self {
        Self(Mutex::new(discovery::load_cached()))
    }
}

#[derive(Serialize)]
pub struct LimitsResult {
    pub json: String,
    pub endpoint: String,
}

/// 取一次 limits：先试缓存端口，失败全量重扫认领（spec §4.1 的两级策略）。
/// 返回 Err 让前端进入"未连接"降级态。
#[tauri::command]
pub async fn fetch_limits(state: State<'_, RelayState>) -> Result<LimitsResult, String> {
    fetch_limits_core(state.inner()).await
}

/// 同一逻辑的非命令入口（webui 等后台消费方用）。
pub async fn fetch_limits_core(state: &RelayState) -> Result<LimitsResult, String> {
    let client = discovery::probe_client();

    let cached = *state.0.lock().unwrap();
    if let Some(port) = cached {
        if let Some(json) = discovery::probe(&client, port).await {
            return Ok(LimitsResult {
                json,
                endpoint: format!("http://127.0.0.1:{port}"),
            });
        }
    }

    match discovery::scan(&client).await {
        Some((port, json)) => {
            *state.0.lock().unwrap() = Some(port);
            discovery::save_cached(port);
            Ok(LimitsResult {
                json,
                endpoint: format!("http://127.0.0.1:{port}"),
            })
        }
        None => {
            *state.0.lock().unwrap() = None;
            Err("relay-not-found".into())
        }
    }
}
