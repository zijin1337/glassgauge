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

/// 按配置取所有候选凭证：manual 用配置里的 baseUrl+authToken；否则从进程环境
/// 提取全部（多个 mirasim-agent 进程可能共存，含带失效旧凭证的；调用方逐个探测）。
pub fn acquire_all(config: &Value) -> Vec<Creds> {
    let mode = config.get("authMode").and_then(Value::as_str).unwrap_or("auto");
    if mode == "manual" {
        let (Some(base), Some(token)) = (
            config.get("baseUrl").and_then(Value::as_str),
            config.get("authToken").and_then(Value::as_str),
        ) else {
            return vec![];
        };
        return valid(base, token).into_iter().collect();
    }
    if mode == "none" {
        return vec![];
    }
    find_in_processes()
}

/// 校验一对凭证：base_url 必须是纯 ASCII 的 http(s) URL（滤掉乱码/半读的环境块），
/// token 非空 ASCII。返回规范化后的 Creds。
fn valid(base: &str, token: &str) -> Option<Creds> {
    let base = base.trim().trim_end_matches('/');
    let token = token.trim();
    let url_ok = base.is_ascii()
        && (base.starts_with("http://") || base.starts_with("https://"))
        && !base.contains(char::is_whitespace);
    let tok_ok = token.is_ascii() && token.len() >= 8 && !token.contains(char::is_whitespace);
    (url_ok && tok_ok).then(|| Creds {
        base_url: base.to_string(),
        token: token.to_string(),
    })
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

    /// 收集所有进程里的合法且互异的凭证（含可能失效的旧凭证，调用方逐个探测）。
    pub fn find_all() -> Vec<Creds> {
        let mut out: Vec<Creds> = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return out;
            };
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                if e.th32ProcessID > 4 {
                    if let Some(c) = creds_of(e.th32ProcessID) {
                        if !out.iter().any(|x| *x == c) {
                            out.push(c);
                        }
                    }
                }
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        out
    }
}

#[cfg(windows)]
fn find_in_processes() -> Vec<Creds> {
    win::find_all()
}

#[cfg(not(windows))]
fn find_in_processes() -> Vec<Creds> {
    Vec::new()
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
        (Some(b), Some(t)) => valid(&b, &t), // 校验滤掉半读/乱码的环境块
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
            "authMode": "manual", "baseUrl": "http://127.0.0.1:9/", "authToken": "tok12345"
        });
        let all = acquire_all(&cfg);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].base_url, "http://127.0.0.1:9");
        assert_eq!(all[0].token, "tok12345");
    }

    #[test]
    fn none_mode_yields_nothing() {
        assert!(acquire_all(&serde_json::json!({ "authMode": "none" })).is_empty());
    }

    #[test]
    fn rejects_garbled_or_short() {
        // 非 ASCII（半读乱码）
        assert!(valid("http://127.0.0৥xyz", "tok12345").is_none());
        // 非 http
        assert!(valid("ftp://x", "tok12345").is_none());
        // token 过短
        assert!(valid("http://127.0.0.1:9", "abc").is_none());
        // 带路径的正常 base_url（新版桥）应通过
        let c = valid("http://127.0.0.1:64263/xGzuqvWFiO04MDM", "S9WNtoken12").unwrap();
        assert_eq!(c.base_url, "http://127.0.0.1:64263/xGzuqvWFiO04MDM");
    }
}
