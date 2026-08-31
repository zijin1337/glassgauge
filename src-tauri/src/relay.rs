//! 取数命令（spec §4.2）：语义不解析，把 /v1/limits 原文交给前端。
//! 凭证发现见 auth.rs（mirasim 更新后需鉴权）。两级策略：先试缓存凭证，
//! 失效则重新取凭证（重扫进程 / 重读配置）并认领。

use crate::auth::{self, Creds};
use crate::discovery;
use serde::Serialize;
use serde_json::Value;
use std::sync::Mutex;
use tauri::State;

pub struct RelayState(pub Mutex<Option<Creds>>);

impl Default for RelayState {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

#[derive(Serialize)]
pub struct LimitsResult {
    pub json: String,
    pub endpoint: String,
}

#[tauri::command]
pub async fn fetch_limits(state: State<'_, RelayState>) -> Result<LimitsResult, String> {
    fetch_limits_core(state.inner()).await
}

/// 同一逻辑的非命令入口（webui 等后台消费方用）。
pub async fn fetch_limits_core(state: &RelayState) -> Result<LimitsResult, String> {
    let client = discovery::probe_client();

    // 1) 试缓存凭证
    let cached = state.0.lock().unwrap().clone();
    if let Some(c) = cached {
        if let Some(json) = discovery::probe(&client, &c).await {
            return Ok(LimitsResult {
                json,
                endpoint: c.base_url.clone(),
            });
        }
    }

    // 2) 重新取凭证并逐个认领（多个 agent 进程可能共存，含带失效旧凭证的）
    let config: Value =
        serde_json::from_str(&crate::window::get_config()).unwrap_or(Value::Null);
    for c in auth::acquire_all(&config) {
        if let Some(json) = discovery::probe(&client, &c).await {
            *state.0.lock().unwrap() = Some(c.clone());
            return Ok(LimitsResult {
                json,
                endpoint: c.base_url,
            });
        }
    }

    *state.0.lock().unwrap() = None;
    Err("relay-not-found".into())
}
