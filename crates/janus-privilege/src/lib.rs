//! Janus Boot 权限提升模块。
//!
//! 写 UEFI 变量需要 root / 管理员权限：
//! - Linux：检测 `euid == 0`；否则优先用 `pkexec`（polkit 图形密码框）
//!   重新执行当前程序，pkexec 不可用时回退到 `sudo`（终端内输入密码）。
//! - Windows：检查令牌提权状态（`TokenElevation`）；未提权时用
//!   `ShellExecuteExW` 的 `runas` 动词弹出 UAC 重新执行，并等待其退出。
//!
//! # 设计原理（Why）
//! 循环防护依赖"提权后的子进程本身就是 root/管理员"这一事实：再次检查
//! 必然通过，因此无需额外的环境变量标记，少一个可被伪造的状态位。
//! pkexec 与 sudo 同步等待子进程退出（`.status()` 内部即 wait），天然完成
//! 子进程状态收割，不会遗留僵尸进程。

// 平台专属 FFI 导入集中在文件顶部，函数体内不做局部 use
#[cfg(unix)]
use std::process::Command;

use janus_core::{JanusError, Result};

/// 当前进程是否已具备 root / 管理员权限。
pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        windows_is_elevated()
    }
}

/// 以 root / 管理员权限重新执行当前程序（携带 `args`），并等待其退出。
///
/// 成功返回子进程退出码（通常调用方随后应 `std::process::exit(code)`）。
///
/// # Errors
/// 无法定位可执行文件、pkexec/sudo 均不可用（Linux）或 UAC 请求被拒绝
/// （Windows）时返回 [`JanusError::Elevation`]。
pub fn reexec_elevated_wait(args: &[String]) -> Result<i32> {
    #[cfg(unix)]
    {
        unix_reexec_elevated(args)
    }
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map_err(|e| JanusError::Elevation(format!("无法定位可执行文件：{e}")))?;
        let exe = exe.to_string_lossy().into_owned();
        windows_reexec_elevated_wait(&exe, args)
    }
}

#[cfg(unix)]
fn unix_reexec_elevated(args: &[String]) -> Result<i32> {
    let exe = std::env::current_exe()
        .map_err(|e| JanusError::Elevation(format!("无法定位可执行文件：{e}")))?;

    // 优先 pkexec：图形会话中弹出 polkit 原生密码输入框
    match Command::new("pkexec").arg(&exe).args(args).status() {
        Ok(status) => Ok(status.code().unwrap_or(0)),
        Err(_) => {
            // 回退 sudo：终端内提示输入密码
            let status = Command::new("sudo")
                .arg("--")
                .arg(&exe)
                .args(args)
                .status()
                .map_err(|e| JanusError::Elevation(format!("pkexec 与 sudo 均不可用：{e}")))?;
            Ok(status.code().unwrap_or(0))
        }
    }
}

/// 通过 `pkexec`（polkit 图形密码框）以 root 身份执行当前可执行文件并等待退出。
///
/// 专供无终端的桌面托盘等场景使用：不回退到需要终端交互的 `sudo`，
/// 将图形提权策略集中在本 crate 统一审计。
///
/// # Errors
/// 无法定位可执行文件、`pkexec` 启动失败、用户取消授权或子进程被信号终止时
/// 返回 [`JanusError::Elevation`]。
#[cfg(target_os = "linux")]
pub fn exec_pkexec_wait(args: &[&str]) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| JanusError::Elevation(format!("无法定位可执行文件：{e}")))?;
    let status = Command::new("pkexec")
        .arg(&exe)
        .args(args)
        .status()
        .map_err(|e| JanusError::Elevation(format!("pkexec 启动失败（需要 polkit agent）：{e}")))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(JanusError::Elevation(format!(
            "特权 helper 退出码 {code}（可能取消了授权）"
        ))),
        None => Err(JanusError::Elevation("特权 helper 被信号终止".into())),
    }
}

#[cfg(windows)]
mod win {
    use std::mem::size_of;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
    };
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use janus_core::{to_wide_null, OwnedHandle};

    /// 检查当前进程令牌是否已提权（UAC 高完整性级别）。
    pub(super) fn is_elevated() -> bool {
        unsafe {
            let mut raw_token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token).is_err() {
                return false;
            }
            let token = OwnedHandle(raw_token);
            let mut elevation = TOKEN_ELEVATION::default();
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                token.0,
                TokenElevation,
                Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
            .is_ok();
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// 以管理员权限重新执行指定程序（弹出 UAC），并等待其退出。
    pub(super) fn reexec_elevated_wait(exe: &str, args: &[String]) -> janus_core::Result<i32> {
        use janus_core::JanusError;

        let params = super::join_windows_args(args);
        let exe_w = to_wide_null(exe);
        let params_w = to_wide_null(&params);

        let mut sei = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe_w.as_ptr()),
            lpParameters: PCWSTR(params_w.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe {
            ShellExecuteExW(&mut sei)
                .map_err(|e| JanusError::Elevation(format!("UAC 提权请求失败：{e}")))?;
            if sei.hProcess.is_invalid() {
                return Ok(0);
            }
            let process = OwnedHandle(sei.hProcess);
            if WaitForSingleObject(process.0, INFINITE) == WAIT_OBJECT_0 {
                let mut code = 0u32;
                let _ = GetExitCodeProcess(process.0, &mut code);
                return Ok(code as i32);
            }
        }
        Ok(0)
    }
}

#[cfg(windows)]
fn windows_is_elevated() -> bool {
    win::is_elevated()
}

#[cfg(windows)]
fn windows_reexec_elevated_wait(exe: &str, args: &[String]) -> Result<i32> {
    win::reexec_elevated_wait(exe, args)
}

/// 按 Win32 `CommandLineToArgvW` 规则转义并拼接命令行参数。
#[cfg(any(windows, test))]
fn join_windows_args(args: &[String]) -> String {
    let mut out = String::new();
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        quote_windows_arg(arg, &mut out);
    }
    out
}

/// 按 Win32 `CommandLineToArgvW` 规则转义单个参数。
#[cfg(any(windows, test))]
fn quote_windows_arg(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '"'));
    if !needs_quotes {
        out.push_str(arg);
        return;
    }

    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // 双引号前的反斜杠需要双倍转义，再加一个反斜杠转义双引号本身
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // 闭合双引号前的末尾反斜杠也需要双倍转义
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::join_windows_args;

    #[test]
    fn windows_arg_quoting_handles_spaces_quotes_and_backslashes() {
        let args = vec![
            "next".to_string(),
            "Windows Boot Manager".to_string(),
            "".to_string(),
            r#"a"b"#.to_string(),
            r"C:\path with space\".to_string(),
            r#"foo\"bar"#.to_string(),
        ];
        assert_eq!(
            join_windows_args(&args),
            r#"next "Windows Boot Manager" "" "a\"b" "C:\path with space\\" "foo\\\"bar""#
        );
    }
}
