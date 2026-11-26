//! 命令的产品交互输出：列表/JSON 渲染、下载进度条与人类可读格式化。
//!
//! # 日志豁免说明（依据 AGENTS.md 分歧确认）
//! 本模块的 stdout/stderr 输出属于产品交互输出（数据展示与进度 UI），
//! 不是日志：list 必须保持管道友好（无时间戳、走 stdout），下载进度条
//! 需要 `\r` 原地刷新。因此豁免于 log 门面；诊断类信息一律走 log 门面。

use janus_core::BootEntry;
use janus_updater::UpdateEvent;

/// 渲染启动项列表；`json` 为真时输出机器可读 JSON。
pub(crate) fn print_entries(
    entries: &[BootEntry],
    boot_next: Option<janus_core::BootId>,
    json: bool,
) {
    if json {
        let json_entries: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id.to_string(),
                    "num": e.id,
                    "name": e.name,
                    "active": e.active,
                    "boot_next": boot_next == Some(e.id),
                })
            })
            .collect();
        // json! 宏构造的 Value 无自定义序列化器，序列化不存在失败路径
        println!(
            "{}",
            serde_json::to_string_pretty(&json_entries).expect("json! 构造的 Value 序列化必然成功")
        );
        return;
    }

    println!("{:<10} {:<34} 状态", "编号", "名称");
    if entries.is_empty() {
        println!("（没有可显示的启动项；--all 可查看全部，janus-boot settings 可调整显示设置）");
        return;
    }
    for e in entries {
        let mut status = if e.active { "活跃" } else { "未启用" }.to_string();
        if boot_next == Some(e.id) {
            status.push_str("  [下次启动]");
        }
        println!("{:<10} {:<34} {status}", e.id, e.name);
    }
}

/// `janus-boot update` 的进度渲染：进度行用 \r 原地刷新，阶段切换时换行输出。
pub(crate) fn progress_printer() -> impl FnMut(UpdateEvent) {
    let mut last_was_progress = false;
    move |event| match event {
        UpdateEvent::DownloadProgress {
            percent,
            speed_bytes_per_sec,
            ..
        } => {
            let pct = percent
                .map(|p| format!("{p:.1}%"))
                .unwrap_or_else(|| "…".into());
            match speed_bytes_per_sec {
                Some(speed) => eprint!("\r  下载中 {:>8}（{}/s）", pct, human_size(speed)),
                None => eprint!("\r  下载中 {:>8}", pct),
            }
            last_was_progress = true;
        }
        UpdateEvent::Stage(stage) => {
            if last_was_progress {
                eprintln!();
                last_was_progress = false;
            }
            let label = stage.label();
            if !label.is_empty() {
                eprintln!("  {label}");
            }
        }
        UpdateEvent::Retrying {
            attempt,
            max_retries,
            error,
        } => {
            eprintln!("  重试 {attempt}/{max_retries}：{error}");
        }
        UpdateEvent::Failed { reason } => {
            eprintln!("  失败：{reason}");
        }
        _ => {}
    }
}

/// 人类可读的文件大小（B/KiB/MiB/GiB/TiB）。
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
