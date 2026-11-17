#![deny(unsafe_code)]
#![deny(missing_docs)]
//! wasm 入口桩选择与魔数判定 (W-7 批次 B; D4 严格零嵌入)
//!
//! `exec.rs` 按魔数识别 `\0asm` 后, 从 VFS 读出入口桩 ELF 替换进程映像; 桩在用户态
//! 经 `EG_WASM_*` 驱动内核侧解释器 (见 `src/user/wasmstub`)。
//!
//! 选桩 (`wasm_userspace` feature): 默认读 `/bin/wasmstub` (内核解释器桩); 点亮后读
//! `/bin/wasmrun` (用户态 wasmi 运行时桩, 见 `src/user/wasmrun`, wasmi vendored)。
//! 两桩恒由 `make user` 产出并打进 initramfs cpio (`bin/wasmstub` / `bin/wasmrun`),
//! 内核镜像不嵌任何用户态字节 (D4), 故本模块只给路径与魔数判定, 不含镜像常量。
//! P4 将翻转默认到 wasmrun。

/// 本构建选用的 wasm 入口桩在 initramfs 里的路径 (经 VFS 读, 不编译期嵌入).
#[must_use]
pub fn stub_path() -> &'static str {
    if cfg!(feature = "wasm_userspace") {
        "/bin/wasmrun"
    } else {
        "/bin/wasmstub"
    }
}

/// 判定字节流是否为 wasm 模块头 (`\0asm` 魔数, WASM 1.0).
#[must_use]
pub fn is_wasm_image(bytes: &[u8]) -> bool {
    let Some(head) = bytes.get(0..4) else {
        return false;
    };
    let Ok(head) = <[u8; 4]>::try_from(head) else {
        return false;
    };
    u32::from_le_bytes(head) == super::types::WASM_MAGIC
}
