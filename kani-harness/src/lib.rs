//! Kani 形式化验证 harness 试点 (functions 层 `wasm/leb128`).
//!
//! ## 定位
//!
//! 独立 harness crate: 通过 `#[path]` 直接引用内核 functions 层的**真实源码**,
//! **不修改** `src/kernel/` 任何文件, 试点阶段**不接入 CI**.
//!
//! 目标模块: `functions/wasm/leb128.rs` (LEB128 变长整数编解码, 0 unsafe,
//! 无外部依赖), 其依赖 `functions/wasm/types.rs` 的 `WasmError`.
//!
//! 验证目标 (核心安全不变式, 对任意输入成立):
//! - 4 个公开函数 **不 panic** (含移位、算术、切片索引、`to_vec`).
//! - `pos` 指针 **单调不减**, 且调用结束后 **不越界** (`pos <= bytes.len()`).
//!
//! ## 运行
//!
//! ```text
//! cd kani-harness && cargo kani
//! ```
//!
//! ## 已知限制
//!
//! Kani 无法编译整个 kernel crate (privileged 层含 `global_asm!`), 故仅以
//! `#[path]` 纳入本模块所需的纯逻辑源码. 本 crate 在 host (x86_64) 上验证,
//! 因此仅覆盖 64 位 `usize` 语义 (内核仅支持 x86_64/aarch64, 均为 64 位).

#![deny(unsafe_code)]

extern crate alloc;

pub mod wasm;

#[cfg(kani)]
mod proofs {
    use super::wasm::leb128::{read_leb128_i32, read_leb128_i64, read_leb128_u32, read_name};

    /// 符号输入字节切片的最大长度 (足以触达各函数的溢出/截断分支).
    const CAP: usize = 12;

    /// 不变式: `read_leb128_u32` 对任意输入不 panic; `pos` 单调不减且不越界.
    #[kani::proof]
    #[kani::unwind(8)]
    fn read_u32_no_panic_pos_in_bounds() {
        let bytes: [u8; CAP] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let slice = &bytes[..len];
        // 调用方契约: 起始 pos 落在 [0, len].
        let start: usize = kani::any();
        kani::assume(start <= slice.len());

        let mut pos = start;
        let _ = read_leb128_u32(slice, &mut pos);

        assert!(pos >= start, "pos 不应回退");
        assert!(pos <= slice.len(), "pos 不应越界");
    }

    /// 不变式: `read_leb128_i32` 对任意输入不 panic; `pos` 单调不减且不越界.
    #[kani::proof]
    #[kani::unwind(8)]
    fn read_i32_no_panic_pos_in_bounds() {
        let bytes: [u8; CAP] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let slice = &bytes[..len];
        let start: usize = kani::any();
        kani::assume(start <= slice.len());

        let mut pos = start;
        let _ = read_leb128_i32(slice, &mut pos);

        assert!(pos >= start, "pos 不应回退");
        assert!(pos <= slice.len(), "pos 不应越界");
    }

    /// 不变式: `read_leb128_i64` 对任意输入不 panic; `pos` 单调不减且不越界.
    #[kani::proof]
    #[kani::unwind(12)]
    fn read_i64_no_panic_pos_in_bounds() {
        let bytes: [u8; CAP] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let slice = &bytes[..len];
        let start: usize = kani::any();
        kani::assume(start <= slice.len());

        let mut pos = start;
        let _ = read_leb128_i64(slice, &mut pos);

        assert!(pos >= start, "pos 不应回退");
        assert!(pos <= slice.len(), "pos 不应越界");
    }

    /// 不变式: `read_name` 对任意输入不 panic (含 `*pos + len` 般算术与切片
    /// 索引); `pos` 单调不减且不越界.
    #[kani::proof]
    #[kani::unwind(14)]
    fn read_name_no_panic_pos_in_bounds() {
        let bytes: [u8; CAP] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let slice = &bytes[..len];
        let start: usize = kani::any();
        kani::assume(start <= slice.len());

        let mut pos = start;
        let _ = read_name(slice, &mut pos);

        assert!(pos >= start, "pos 不应回退");
        assert!(pos <= slice.len(), "pos 不应越界");
    }
}
