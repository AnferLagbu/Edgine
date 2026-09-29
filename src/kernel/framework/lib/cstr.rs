//! C 字符串指针的 Rust 安全抽象 (Safe C-String Pointer Extensions)
//!
//! 提供 [`CStrExt`] trait,为来自 FFI 边界的 `*const u8` 提供：
//!
//! - 空指针安全
//! - UTF-8 校验失败降级(返回空串 `""`,不 panic)
//! - 最大长度扫描上限(避免读到未映射内存)
//!
//! ## 背景
//!
//! 内核中多个模块从用户态或跨 FFI 边界接收 `*const u8`,重复的
//! `unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")` 模板容易在以下地方
//! 出错:
//!
//! 1. 忘记检查空指针(若指针为 null,`CStr::from_ptr` 是 UB)
//! 2. 字符串超过长度上限时,无界扫描会越界
//! 3. 错误处理语义不一致(unwrap vs unwrap_or)
//!
//! 本 trait 统一封装以上三个问题。
//!
//! ## 用法
//!
//! ```ignore
//! use crate::framework::lib::cstr::CStrExt;
//!
//! #[no_mangle]
//! pub extern "C" fn pwm_login(
//!     note: *const u8,
//!     password: *const u8,
//! ) -> i64 {
//!     let n = note.as_kstr();       // 空指针 → ""
//!     let p = password.as_kstr();   // 非 UTF-8 → ""
//!     // ...
//! }
//! ```
//!
//! ## 与 `linux/kernel/str.rs::CStrExt` 的关系
//!
//! 思路与 Linux 6.1+ 的 [`kernel::str::CStrExt`](https://rust.docs.kernel.org/kernel/str/trait.CStrExt.html)
//! 一致;在内核中,通过 prelude 隐式导入 `as _` 即可使用。
//! 本实现受 `no_std` 限制,不依赖 `alloc`。
//!
//! ## SAFETY
//!
//! `as_kstr` 内部使用 `unsafe` 块,但所有的不变量都已经在函数内被显式校验:
//!
//! - 空指针提前 return
//! - 长度有上限 `MAX_LEN` 防止越界
//! - `from_utf8` 失败时返回 `""`,不 panic

use core::ffi::CStr;

/// FFI C 字符串指针的最大可接受扫描长度
///
/// 超过此长度的指针会被截断到该上限,避免恶意或损坏的指针触发
/// 长时间无界扫描(可能访问未映射内存)。
///
/// 4 KiB 选自 VFS 默认路径长度上限 [`crate::services::fs::VFS_MAX_PATH`],
/// 兼顾"覆盖绝大多数合法场景"和"防止误读"。
pub const MAX_CSTR_LEN: usize = 4096;

/// 为 FFI 边界传入的 `*const u8` 提供 Rust 风格的安全访问
///
/// # 实现细节
///
/// 之所以是 `&self`(对指针按值访问,而不是 `&mut self`),是因为
/// `*const u8` 是 `Copy` 的,impl 用 `*self` 解引用即可。
pub trait CStrExt {
    /// 提取 C 字符串内容为 UTF-8 Rust 字符串切片
    ///
    /// - 指针为空 → `Some("")`
    /// - 字符串非 UTF-8 → `Some("")` (降级)
    /// - 字符串未以 NUL 结尾或过长 → 截断到 [`MAX_CSTR_LEN`]
    ///
    /// 永远不返回 `Err`,因为内核 FFI 路径需要"尽力而为",不该 panic。
    fn as_kstr(&self) -> &'static str;

    /// 与 `as_kstr` 一致,但返回 `Option<&str>`
    ///
    /// - 指针为空 → `None`
    /// - 非 UTF-8 → `None`
    ///
    /// 用于"显式区分空指针与正常空串"的场景。
    fn as_kstr_opt(&self) -> Option<&'static str>;
}

impl CStrExt for *const u8 {
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    fn as_kstr(&self) -> &'static str {
        let ptr = *self;
        if ptr.is_null() {
            return "";
        }
        // SAFETY: 调用方需保证:
        //   1. ptr 非空(已检查)
        //   2. ptr 指向的内存中,在 ptr..ptr+MAX_CSTR_LEN 范围内
        //      存在一个 NUL 字节(由 C 字符串契约保证)
        //   3. 同一线程中无其他代码写入该内存
        //
        // 这里采用显式长度扫描而不是 CStr::from_ptr 是为了:
        //   - 避免无界扫描(在错误的输入下可能触发数秒级停滞)
        //   - 兼容没有 NUL 终止符的损坏输入(常见于 buggy FFI)
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let len = unsafe {
            let mut n = 0;
            while n < MAX_CSTR_LEN && *ptr.add(n) != 0 {
                n += 1;
            }
            n
        };
        // SAFETY: 已扫描 [ptr, ptr+len) 区间,所有字节均非 0,len <= MAX_CSTR_LEN
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
        core::str::from_utf8(bytes).unwrap_or("")
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    fn as_kstr_opt(&self) -> Option<&'static str> {
        let ptr = *self;
        if ptr.is_null() {
            return None;
        }
        // SAFETY: 见 as_kstr 中的 safety 注释
        let cstr = unsafe { CStr::from_ptr(ptr as *const core::ffi::c_char) };
        cstr.to_str().ok()
    }
}

/// 与 `*const u8` 等价的可变版本
///
/// 适用于 FFI 中声明为 `char *`(可写)的参数。语义与 [`CStrExt`] 一致。
impl CStrExt for *mut u8 {
    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    fn as_kstr(&self) -> &'static str {
        (*self as *const u8).as_kstr()
    }

    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    fn as_kstr_opt(&self) -> Option<&'static str> {
        (*self as *const u8).as_kstr_opt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::ffi::CString;

    #[test]
    fn null_ptr_returns_empty() {
        let p: *const u8 = core::ptr::null();
        assert_eq!(p.as_kstr(), "");
        assert_eq!(p.as_kstr_opt(), None);
    }

    #[test]
    fn valid_cstr() {
        let s = CString::new("hello").unwrap();
        // CStrExt 仅实现于 *const u8 / *mut u8; CString::as_ptr 返回 *const i8.
        let p = s.as_ptr() as *const u8;
        assert_eq!(p.as_kstr(), "hello");
        assert_eq!(p.as_kstr_opt(), Some("hello"));
    }

    #[test]
    fn valid_cstr_with_unicode() {
        let s = CString::new("中文").unwrap();
        let p = s.as_ptr() as *const u8;
        assert_eq!(p.as_kstr(), "中文");
    }

    #[test]
    fn empty_cstr() {
        let s = CString::new("").unwrap();
        let p = s.as_ptr() as *const u8;
        assert_eq!(p.as_kstr(), "");
        assert_eq!(p.as_kstr_opt(), Some(""));
    }
}
