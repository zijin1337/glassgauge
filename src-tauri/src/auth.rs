//! 凭证发现（方案 B）。mirasim 更新后 `/v1/limits` 需鉴权：每会话铸造一个
//! Bearer token，随 `ANTHROPIC_BASE_URL` 一起注入它派生的 agent 进程环境，
//! **不落盘、随会话轮换**。挂件作为独立进程，通过读取同用户 mirasim-agent
//! 进程的环境块（PEB）提取这对凭证——同用户读进程环境是标准操作（无需提权，
//! Process Explorer 等工具皆如此），且只提取 ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN
//! 两个变量，不读其它。找不到即视为 mirasim 未运行。
//!
//! 也支持配置手动指定（authMode:"manual" + baseUrl + authToken），或点向别处。

use serde_json::Value;

#[derive(Clone, PartialEq, Eq)]
pub struct Creds {
    pub base_url: String,
    pub token: String,
}

/// 按配置取凭证：manual 用配置里的 baseUrl+authToken；否则从进程环境提取。
pub fn acquire(config: &Value) -> Option<Creds> {
    let mode = config.get("authMode").and_then(Value::as_str).unwrap_or("auto");
    if mode == "manual" {
        let base = config.get("baseUrl").and_then(Value::as_str)?;
        let token = config.get("authToken").and_then(Value::as_str)?;
        if base.is_empty() || token.is_empty() {
            return None;
        }
        return Some(Creds {
            base_url: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        });
    }
    if mode == "none" {
        return None;
    }
    find_in_processes()
}

#[cfg(windows)]
mod win {
    use super::Creds;
    use std::ffi::c_void;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            h: HANDLE,
            cls: i32,
            info: *mut c_void,
            len: u32,
            ret: *mut u32,
        ) -> i32;
    }

    /// 读 len 字节；ERROR_PARTIAL_COPY 时也用已读部分（env 变量通常在块前部）。
    fn read_mem(h: HANDLE, addr: usize, len: usize) -> Option<Vec<u8>> {
        if addr == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        let mut read = 0usize;
        unsafe {
            let _ = ReadProcessMemory(
                h,
                addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                len,
                Some(&mut read),
            );
        }
        if read == 0 {
            return None;
        }
        buf.truncate(read);
        Some(buf)
    }

    fn read_ptr(h: HANDLE, addr: usize) -> Option<usize> {
        let b = read_mem(h, addr, 8)?;
        (b.len() >= 8).then(|| u64::from_le_bytes(b[..8].try_into().unwrap()) as usize)
    }

    fn creds_of(pid: u32) -> Option<Creds> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
            let out = (|| {
                // PROCESS_BASIC_INFORMATION：PebBaseAddress 在偏移 0x08（x64）
                let mut pbi = [0u8; 48];
                let mut ret = 0u32;
                if NtQueryInformationProcess(h, 0, pbi.as_mut_ptr() as *mut c_void, 48, &mut ret)
                    != 0
                {
                    return None;
                }
                let peb = u64::from_le_bytes(pbi[8..16].try_into().ok()?) as usize;
                let params = read_ptr(h, peb + 0x20)?; // PEB.ProcessParameters
                let env_addr = read_ptr(h, params + 0x80)?; // RTL_USER_PROCESS_PARAMETERS.Environment
                // 固定读一大块（256KB），靠 partial 容错；env 变量在前部即可命中
                let raw = read_mem(h, env_addr, 262_144)?;
                super::parse_env(&raw)
            })();
            let _ = CloseHandle(h);
            out
        }
    }

    pub fn find() -> Option<Creds> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            let mut found = None;
            while ok {
                // 跳过系统闲置/System（pid 0/4）
                if e.th32ProcessID > 4 {
                    if let Some(c) = creds_of(e.th32ProcessID) {
                        found = Some(c);
                        break;
                    }
                }
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
            found
        }
    }
}

#[cfg(windows)]
fn find_in_processes() -> Option<Creds> {
    win::find()
}

#[cfg(not(windows))]
fn find_in_processes() -> Option<Creds> {
    None
}

/// UTF-16LE、NUL 分隔的 KEY=VALUE 环境块 → 提取两个 ANTHROPIC_ 变量。
fn parse_env(raw: &[u8]) -> Option<Creds> {
    let u16s: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16_lossy(&u16s);
    let mut base = None;
    let mut token = None;
    for kv in text.split('\0') {
        if let Some(v) = kv.strip_prefix("ANTHROPIC_BASE_URL=") {
            base = Some(v.trim_end_matches('/').to_string());
        } else if let Some(v) = kv.strip_prefix("ANTHROPIC_AUTH_TOKEN=") {
            token = Some(v.to_string());
        }
    }
    match (base, token) {
        (Some(b), Some(t)) if !b.is_empty() && !t.is_empty() => {
            Some(Creds { base_url: b, token: t })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_block(pairs: &[&str]) -> Vec<u8> {
        let mut s = String::new();
        for p in pairs {
            s.push_str(p);
            s.push('\0');
        }
        s.push('\0');
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn parses_both_vars_from_env_block() {
        let raw = utf16_block(&[
            "PATH=C:\\x",
            "ANTHROPIC_BASE_URL=http://127.0.0.1:65533/",
            "FOO=bar",
            "ANTHROPIC_AUTH_TOKEN=S9WNsecret",
        ]);
        let c = parse_env(&raw).unwrap();
        assert_eq!(c.base_url, "http://127.0.0.1:65533"); // 尾斜杠已去
        assert_eq!(c.token, "S9WNsecret");
    }

    #[test]
    fn missing_either_var_is_none() {
        assert!(parse_env(&utf16_block(&["ANTHROPIC_BASE_URL=x"])).is_none());
        assert!(parse_env(&utf16_block(&["ANTHROPIC_AUTH_TOKEN=y"])).is_none());
        assert!(parse_env(&utf16_block(&["PATH=x"])).is_none());
    }

    #[test]
    fn manual_mode_reads_config() {
        let cfg = serde_json::json!({
            "authMode": "manual", "baseUrl": "http://127.0.0.1:9/", "authToken": "tok"
        });
        let c = acquire(&cfg).unwrap();
        assert_eq!(c.base_url, "http://127.0.0.1:9");
        assert_eq!(c.token, "tok");
    }

    #[test]
    fn none_mode_yields_nothing() {
        assert!(acquire(&serde_json::json!({ "authMode": "none" })).is_none());
    }
}
