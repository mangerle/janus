//! 内置模拟后端：供 `--dry-run` 模式与单元测试使用，不触碰真实固件。
//!
//! # 设计原理（Why）
//! 锁选择 `std::sync::Mutex`：临界区仅是纯内存读写，且 Mock 只被单线程
//! 测试或 dry-run 演示调用，不存在跨 await 场景，轻量同步锁开销最小。

use std::sync::Mutex;

use crate::{BootEntry, BootId, BootManager, Result};

/// 内存模拟的启动管理器。
#[derive(Debug)]
pub struct MockManager {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: Vec<BootEntry>,
    boot_next: Option<BootId>,
}

impl MockManager {
    /// 一组典型的 Windows + Ubuntu 双启动数据（用于演示与测试）。
    pub fn typical_dual_boot() -> Self {
        Self::with_entries(vec![
            BootEntry {
                id: BootId::new(0x0001),
                name: "Windows Boot Manager".into(),
                active: true,
            },
            BootEntry {
                id: BootId::new(0x0002),
                name: "ubuntu".into(),
                active: true,
            },
            BootEntry {
                id: BootId::new(0x0003),
                name: "UEFI: SanDisk Extreme".into(),
                active: false,
            },
        ])
    }

    /// 用自定义启动项构造。
    pub fn with_entries(entries: Vec<BootEntry>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries,
                boot_next: None,
            }),
        }
    }
}

impl BootManager for MockManager {
    fn list_entries(&self) -> Result<Vec<BootEntry>> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .clone())
    }

    fn boot_next(&self) -> Result<Option<BootId>> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .boot_next)
    }

    fn set_boot_next(&self, id: BootId) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .boot_next = Some(id);
        Ok(())
    }

    fn clear_boot_next(&self) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .boot_next = None;
        Ok(())
    }
}
