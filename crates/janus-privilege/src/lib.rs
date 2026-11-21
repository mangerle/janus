//! Janus 权限提升模块。
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

#[cfg(windows)]
mod win {
    use std::mem::size_of;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
    };
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// UTF-16LE + NUL 结尾；容量已知，预分配避免多次扩容拷贝。
    pub(super) fn to_wide(s: &str) -> Vec<u16> {
        let mut wide = Vec::with_capacity(s.len() + 1);
        wide.extend(s.encode_utf16());
        wide.push(0);
        wide
    }

    /// 检查当前进程令牌是否已提权（UAC 高完整性级别）。
    pub(super) fn is_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    /// 以管理员权限重新执行指定程序（弹出 UAC），并等待其退出。
    pub(super) fn reexec_elevated_wait(exe: &str, args: &[String]) -> janus_core::Result<i32> {
        use janus_core::JanusError;

        // 参数中含空格的加引号（如启动项名称）
        let params = args
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let exe_w = to_wide(exe);
        let params_w = to_wide(&params);

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
            if WaitForSingleObject(sei.hProcess, INFINITE) == WAIT_OBJECT_0 {
                let mut code = 0u32;
                let _ = GetExitCodeProcess(sei.hProcess, &mut code);
                let _ = CloseHandle(sei.hProcess);
                return Ok(code as i32);
            }
            let _ = CloseHandle(sei.hProcess);
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
