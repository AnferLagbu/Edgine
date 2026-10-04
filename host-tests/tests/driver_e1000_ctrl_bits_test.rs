//! driver: e1000 控制/接收寄存器位域常量 — 静态契约测试
//!
//! 追踪: ISSUE-RT-001
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! ISSUE-RT-001 根因: x86_64 + QEMU 默认 e1000 (82540EM, 8254x 家族) 初始化挂起.
//! 定位为 `privileged/driver/net/e1000_io.rs` 中若干寄存器**位域常量写错**:
//!
//! - `E1000_CTRL_RST` 误用 `1 << 31`: 该位实为 `E1000_CTRL_PHY_RST` (PHY 复位),
//!   82540EM 不会因它触发全局复位 -> 复位轮询超时 -> 初始化失败.
//!   正确值 `0x0400_0000` (bit26, Global Reset).
//! - `E1000_CTRL_FRCDPX` 误用 `1 << 14`: 正确值 `1 << 12` (bit12, Force Duplex).
//! - `E1000_RCTL_BSIZE_2048` 误用 `1 << 25`: bit25 是 `RCTL_BSEX` (缓冲区尺寸
//!   扩展位), 并非尺寸编码. 正确值 `0x0` (BSEX=0, BSIZE bits[17:16]=00b -> 2048B).
//!
//! 权威依据: Intel 8254x 数据手册 / `e1000_defines.h`
//! (`E1000_CTRL_RST 0x04000000`, `E1000_CTRL_PHY_RST 0x80000000`,
//!  `E1000_CTRL_FRCDPX 0x00001000`, `E1000_RCTL_SZ_2048 0x00000000`).
//!
//! ## 为何用静态契约
//!
//! 内核该常量为 `pub(crate)`, host-tests (host-test feature, 非 kernel_test)
//! 无法数值引用; 且驱动模块 `e1000_impl` 被
//! `#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]` 门控.
//! 故采用**源码文本分析**方式固化位域契约, 防止回归重新引入错误位.

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine workspace root
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 取出指定常量的定义行 (含 `: u32 =`, 排除文档注释等提及处).
fn const_line<'a>(src: &'a str, name: &str) -> &'a str {
    src.lines()
        .find(|l| l.contains(name) && l.contains(": u32 ="))
        .unwrap_or_else(|| panic!("未找到常量定义行: {}", name))
}

const IO_RS: &str = "src/kernel/privileged/driver/net/e1000_io.rs";
const DRIVER_RS: &str = "src/kernel/functions/driver/net/e1000.rs";

#[test]
fn test_ctrl_rst_is_bit26_global_reset() {
    let src = read(IO_RS);
    let line = const_line(&src, "E1000_CTRL_RST");
    assert!(
        line.contains("1 << 26"),
        "E1000_CTRL_RST 必须为 bit26 (0x0400_0000, Global Reset); 当前: {}",
        line.trim()
    );
    assert!(
        !line.contains("1 << 31"),
        "E1000_CTRL_RST 不得使用 bit31 (那是 E1000_CTRL_PHY_RST); 当前: {}",
        line.trim()
    );
}

#[test]
fn test_ctrl_frcdpx_is_bit12() {
    let src = read(IO_RS);
    let line = const_line(&src, "E1000_CTRL_FRCDPX");
    assert!(
        line.contains("1 << 12"),
        "E1000_CTRL_FRCDPX 必须为 bit12 (Force Duplex); 当前: {}",
        line.trim()
    );
    assert!(
        !line.contains("1 << 14"),
        "E1000_CTRL_FRCDPX 不得使用 bit14; 当前: {}",
        line.trim()
    );
}

#[test]
fn test_rctl_bsize_2048_is_zero() {
    let src = read(IO_RS);
    let line = const_line(&src, "E1000_RCTL_BSIZE_2048");
    assert!(
        line.contains("0x0"),
        "E1000_RCTL_BSIZE_2048 必须为 0x0 (BSEX=0, BSIZE=00b); 当前: {}",
        line.trim()
    );
    assert!(
        !line.contains("1 << 25"),
        "E1000_RCTL_BSIZE_2048 不得使用 bit25 (那是 RCTL_BSEX 扩展位); 当前: {}",
        line.trim()
    );
}

#[test]
fn test_driver_still_references_these_constants() {
    // 契约闭环: functions 驱动仍引用这三个常量, 避免常量与实装脱节.
    let src = read(DRIVER_RS);
    for name in [
        "E1000_CTRL_RST",
        "E1000_CTRL_FRCDPX",
        "E1000_RCTL_BSIZE_2048",
    ] {
        assert!(src.contains(name), "functions e1000.rs 未引用 {}", name);
    }
}
