#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 内嵌 wasm 用户态入口桩镜像 (W-7 批次 B)
//!
//! `exec.rs` 按魔数识别 `\0asm` 后, 以本镜像替换进程映像; 桩在用户态经
//! `EG_WASM_*` 驱动内核侧解释器 (见 `src/user/wasmstub`)。
//!
//! 产物按架构分目录 (S-6 三条防线): `other/build/<arch>/user/wasmstub.bin` 由
//! Makefile 从本架构 ELF 复制, `include_bytes!` 与 build.rs 的
//! `rerun-if-changed` 使用同一路径 (绝对路径由 build.rs 发出)。
//!
//! host target 取空切片: host-test / kernel_test 两维与 host-tests 都会编译本
//! 模块, 不桩化即让 host 编译硬依赖 `other/build/` 下的裸机产物。

#[cfg(all(target_os = "none", target_arch = "x86_64"))]
const WASM_STUB_IMAGE: &[u8] = include_bytes!("../../../../other/build/x86_64/user/wasmstub.bin");
#[cfg(all(target_os = "none", target_arch = "aarch64"))]
const WASM_STUB_IMAGE: &[u8] = include_bytes!("../../../../other/build/aarch64/user/wasmstub.bin");
#[cfg(not(target_os = "none"))]
const WASM_STUB_IMAGE: &[u8] = &[];

/// 取本架构的 wasm 桩 ELF 镜像 (host 构建为空切片).
#[must_use]
pub fn stub_image() -> &'static [u8] {
    WASM_STUB_IMAGE
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
