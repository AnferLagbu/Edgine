/// 基础字符串/内存操作库 (String & Memory Utilities)
///
/// 提供标准 C 库风格的字符串和内存操作函数的 Rust 实现。
/// 替代原来的 lib/string.c，提供更安全的接口。
///
/// ## 功能清单
///
/// ### 字符串操作 (String Operations)
/// - `strlen` / `strlen_safe` - 字符串长度
/// - `strcmp` / `strncmp` - 字符串比较
/// - `strcpy` / `strncpy` - 字符串拷贝
/// - `strcat` - 字符串连接
/// - `strchr` / `strrchr` - 字符查找
/// - `strstr` - 子串查找
///
/// ### 内存操作 (Memory Operations)
/// - `memcpy` - 内存拷贝
/// - `memmove` - 内存移动（处理重叠区域）
/// - `memset` / `memset_optimized` - 内存设置
/// - `memcmp` - 内存比较
/// - `memchr` - 内存字符查找
///
/// ### 安全函数 (Secure Functions)
/// - `secure_zero` - 安全清零（防止编译器优化）
///
/// ## 设计原则
///
/// 1. **FFI 兼容** - 所有 C 函数都有对应的 Rust FFI 实现
/// 2. **类型安全** - 提供 Rust 原生的安全包装版本
/// 3. **性能优化** - 关键路径使用内联和优化算法
/// 4. **边界检查** - 所有数组操作都有安全保证

// ============================================================================
// 字符串操作函数 (String Operations)
// ============================================================================

/// `strlen` 上限常量 (防御深度)
///
/// 实际有效路径仅 FFI 测试调用; 上限避免恶意指针读到 #PF。
/// B04-07: 1024 足以覆盖内核内部任何合法字符串 (路径名 ≤ 256, 命令行 ≤ 256)。
const STRLEN_MAX: usize = 1024;

/// 计算字符串长度 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s` - 以 null 结尾的字符串指针
///
/// # Returns
/// 字符串长度（不包括终止符 '\0'）
///
/// # Safety
/// 此函数通过 FFI 暴露给 C 代码使用
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const core::ffi::c_char) -> usize {
    unsafe {
        if s.is_null() {
            return 0;
        }

        let mut len = 0;
        let mut ptr = s;

        // B04-07: 加 MAX_CSTR_LEN 上限 (DECISION-060), 防御恶意指针无上界读取。
        while *ptr != 0 && len < STRLEN_MAX {
            len += 1;
            ptr = ptr.add(1);
        }

        len
    }
}

/// 计算字符串长度 (Rust 安全版本)
///
/// # Arguments
/// * `s` - 字符串切片
///
/// # Returns
/// 字符串长度
#[inline(always)]
pub fn strlen_safe(s: &[i8]) -> usize {
    let mut len = 0;

    for &ch in s {
        if ch == 0 {
            break;
        }
        len += 1;
    }

    len
}

/// 字符串比较 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s1` - 第一个字符串
/// * `s2` - 第二个字符串
///
/// # Returns
/// * `0` - 相等
/// * `<0` - s1 < s2
/// * \>0 - s1 > s2
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `src` 是指向以 NUL 结尾的 C 字符串的有效指针. `dst` 至少有 `max_len` 字节可写内存.
pub unsafe extern "C" fn strcmp(s1: *const i8, s2: *const i8) -> i32 {
    unsafe {
        if s1.is_null() || s2.is_null() {
            if s1.is_null() && s2.is_null() {
                return 0;
            }
            return if s1.is_null() { -1 } else { 1 };
        }

        let mut p1 = s1;
        let mut p2 = s2;

        loop {
            let c1 = *p1;
            let c2 = *p2;

            if c1 == 0 || c2 == 0 || c1 != c2 {
                return i32::from(c1 as u8) - i32::from(c2 as u8);
            }

            p1 = p1.add(1);
            p2 = p2.add(1);
        }
    }
}

/// 字符串比较（限制长度）(C 风格 FFI 接口)
///
/// # Arguments
/// * `s1` - 第一个字符串
/// * `s2` - 第二个字符串
/// * `n` - 最大比较字符数
///
/// # Returns
/// * `0` - 相等
/// * `<0` - s1 < s2
/// * \>0 - s1 > s2
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `src` 与 `dst` 均为有效指针. `dst` 至少有 `n` 字节可写内存. 两个区域不可重叠.
pub unsafe extern "C" fn strncmp(s1: *const i8, s2: *const i8, n: usize) -> i32 {
    unsafe {
        if n == 0 {
            return 0;
        }

        if s1.is_null() || s2.is_null() {
            if s1.is_null() && s2.is_null() {
                return 0;
            }
            return if s1.is_null() { -1 } else { 1 };
        }

        let mut p1 = s1;
        let mut p2 = s2;
        let mut remaining = n;

        while remaining > 0 {
            let c1 = *p1;
            let c2 = *p2;

            if c1 == 0 || c2 == 0 || c1 != c2 {
                return i32::from(c1 as u8) - i32::from(c2 as u8);
            }

            p1 = p1.add(1);
            p2 = p2.add(1);
            remaining -= 1;
        }

        0
    }
}

/// 字符串拷贝 (C 风格 FFI 接口)
///
/// # Arguments
/// * `dest` - 目标缓冲区
/// * `src` - 源字符串
///
/// # Returns
/// 目标缓冲区指针
///
/// # Safety
/// 调用者必须确保 dest 有足够的空间
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcpy(dest: *mut i8, src: *const i8) -> *mut i8 {
    unsafe {
        if dest.is_null() || src.is_null() {
            return dest;
        }

        let ret = dest;
        let mut d = dest;
        let mut s = src;

        loop {
            let ch = *s;
            *d = ch;

            if ch == 0 {
                break;
            }

            d = d.add(1);
            s = s.add(1);
        }

        ret
    }
}

/// 字符串拷贝（限制长度）(C 风格 FFI 接口)
///
/// # Arguments
/// * `dest` - 目标缓冲区
/// * `src` - 源字符串
/// * `n` - 最大拷贝字符数
///
/// # Returns
/// 目标缓冲区指针
///
/// # Safety
/// 调用者必须确保 dest 有至少 n 字节的空间
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strncpy(dest: *mut i8, src: *const i8, n: usize) -> *mut i8 {
    unsafe {
        if dest.is_null() || src.is_null() || n == 0 {
            return dest;
        }

        let ret = dest;
        let mut d = dest;
        let mut s = src;
        let mut remaining = n;

        // 拷贝源字符串内容
        while remaining > 0 {
            let ch = *s;
            *d = ch;

            if ch == 0 {
                break; // 遇到终止符停止
            }

            d = d.add(1);
            s = s.add(1);
            remaining -= 1;
        }

        // 用 '\0' 填充剩余空间
        while remaining > 0 {
            *d = 0;
            d = d.add(1);
            remaining -= 1;
        }

        ret
    }
}

/// 字符串连接 (C 风格 FFI 接口)
///
/// 将 src 追加到 dest 末尾
///
/// # Arguments
/// * `dest` - 目标缓冲区（必须有足够空间）
/// * `src` - 要追加的源字符串
///
/// # Returns
/// 目标缓冲区指针
///
/// # Safety
/// 调用者必须确保 dest 有足够的空间容纳结果
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcat(dest: *mut i8, src: *const i8) -> *mut i8 {
    unsafe {
        if dest.is_null() || src.is_null() {
            return dest;
        }

        let ret = dest;

        // 找到 dest 的末尾
        let mut d = dest;
        while *d != 0 {
            d = d.add(1);
        }

        // 追加 src
        let mut s = src;
        loop {
            let ch = *s;
            *d = ch;

            if ch == 0 {
                break;
            }

            d = d.add(1);
            s = s.add(1);
        }

        ret
    }
}

/// 查找字符首次出现位置 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s` - 字符串
/// * `c` - 要查找的字符
///
/// # Returns
/// 找到的字符指针，或 NULL 如果未找到
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是有效指针. 若 `n` 非零, 则从 `ptr` 起至少有 `n` 字节可读.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
pub unsafe extern "C" fn strchr(s: *const i8, c: i32) -> *mut i8 {
    unsafe {
        if s.is_null() {
            return core::ptr::null_mut();
        }

        let target = c as i8;
        let mut p = s;

        loop {
            let ch = *p;

            if ch == target {
                return p as *mut i8;
            }

            if ch == 0 {
                return core::ptr::null_mut(); // 未找到
            }

            p = p.add(1);
        }
    }
}

/// 查找字符最后出现位置 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s` - 字符串
/// * `c` - 要查找的字符
///
/// # Returns
/// 找到的字符指针，或 NULL 如果未找到
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `a` 与 `b` 均为有效指针. 各自至少有 `n` 字节可读.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
pub unsafe extern "C" fn strrchr(s: *const i8, c: i32) -> *mut i8 {
    unsafe {
        if s.is_null() {
            return core::ptr::null_mut();
        }

        let target = c as i8;
        let mut p = s;
        let mut last: *const i8 = core::ptr::null();

        loop {
            let ch = *p;

            if ch == target {
                last = p;
            }

            if ch == 0 {
                break;
            }

            p = p.add(1);
        }

        if last.is_null() {
            core::ptr::null_mut()
        } else {
            last as *mut i8
        }
    }
}

/// 查找子串 (C 风格 FFI 接口)
///
/// 在 haystack 中查找 needle 子串
///
/// # Arguments
/// * `haystack` - 被搜索的字符串
/// * `needle` - 要查找的子串
///
/// # Returns
/// 找到的子串指针，或 NULL 如果未找到
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
///
/// # Safety
///
/// `ptr` 是有效指针. `value` 将被写入从 `ptr` 起 `n` 个连续字节.
pub unsafe extern "C" fn strstr(haystack: *const i8, needle: *const i8) -> *mut i8 {
    unsafe {
        if haystack.is_null() {
            return core::ptr::null_mut();
        }

        // 空子串返回 haystack 本身
        if needle.is_null() || *needle == 0 {
            return haystack as *mut i8;
        }

        let mut h = haystack;

        loop {
            let h_ch = *h;

            if h_ch == 0 {
                return core::ptr::null_mut(); // 到达 haystack 末尾
            }

            // 尝试从当前位置匹配 needle
            let mut h_temp = h;
            let mut n = needle;

            loop {
                let n_ch = *n;

                if n_ch == 0 {
                    return h as *mut i8; // 匹配成功
                }

                if *h_temp != n_ch {
                    break; // 不匹配，继续下一个位置
                }

                h_temp = h_temp.add(1);
                n = n.add(1);
            }

            h = h.add(1);
        }
    }
}

// ============================================================================
// 内存操作函数 (Memory Operations)
// ============================================================================

/// 内存拷贝 (C 风格 FFI 接口)
///
/// # Arguments
/// * `dest` - 目标地址
/// * `src` - 源地址
/// * `n` - 拷贝字节数
///
/// # Returns
/// 目标地址
///
/// # Safety
/// 调用者必须确保:
/// - dest 和 src 都有效
/// - 区域不能重叠（否则应使用 memmove）
/// - dest 有足够空间
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn memcpy(
    dest: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> *mut core::ffi::c_void {
    unsafe {
        if dest.is_null() || src.is_null() || n == 0 {
            return dest;
        }

        let d = dest as *mut u8;
        let s = src as *const u8;

        for i in 0..n {
            *d.add(i) = *s.add(i);
        }

        dest
    }
}

/// 内存移动（处理重叠区域）(C 风格 FFI 接口)
///
/// 当源和目标区域重叠时使用此函数代替 memcpy
///
/// # Arguments
/// * `dest` - 目标地址
/// * `src` - 源地址
/// * `n` - 移动字节数
///
/// # Returns
/// 目标地址
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
///
/// # Safety
///
/// `src` 与 `dst` 均为有效指针. `dst` 至少有 `n` 字节可写内存. 两个区域可重叠.
pub unsafe extern "C" fn memmove(
    dest: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
) -> *mut core::ffi::c_void {
    unsafe {
        if dest.is_null() || src.is_null() || n == 0 {
            return dest;
        }

        let d = dest as *mut u8;
        let s = src as *const u8;

        // 判断是否需要反向拷贝（处理重叠区域）
        if d < s as *mut u8 {
            // 正向拷贝
            for i in 0..n {
                *d.add(i) = *s.add(i);
            }
        } else {
            // 反向拷贝
            for i in (0..n).rev() {
                *d.add(i) = *s.add(i);
            }
        }

        dest
    }
}

/// 内存设置 (C 风格 FFI 接口)
///
/// 将内存区域填充为指定值
///
/// # Arguments
/// * `s` - 目标地址
/// * `c` - 填充值（会被截断为 unsigned char）
/// * `n` - 设置字节数
///
/// # Returns
/// 目标地址
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `src` 与 `dst` 均为有效指针. `dst` 至少有 `n` 字节可写内存. 两个区域不可重叠.
pub unsafe extern "C" fn memset(
    s: *mut core::ffi::c_void,
    c: i32,
    n: usize,
) -> *mut core::ffi::c_void {
    unsafe {
        if s.is_null() || n == 0 {
            return s;
        }

        #[cfg(target_arch = "aarch64")]
        {
            let val = (c & 0xFF) as u8;
            let mut dst = s as *mut u8;
            let mut remaining = n;

            // 逐字节对齐到 16 字节边界
            while remaining > 0 && (dst as usize) & 0xF != 0 {
                *dst = val;
                dst = dst.add(1);
                remaining -= 1;
            }

            // 16 字节批量写入 (stp 一次写 16 字节)
            if remaining >= 16 {
                let val64 = u64::from_le_bytes([val; 8]);
                let mut blocks = remaining / 16;
                // SAFETY: asm 使用 stp 指令, 仅写入 dst 指向的内存
                core::arch::asm!(
                    "1:",
                    "stp {val}, {val}, [{dst}], #16",
                    "subs {blocks}, {blocks}, #1",
                    "b.ne 1b",
                    val = in(reg) val64,
                    dst = inout(reg) dst,
                    blocks = inout(reg) blocks,
                    options(nostack, readonly),
                );
                // blocks 在汇编中被递减至 0, 明确丢弃
                let _ = blocks;
                remaining &= 0xF;
            }

            // 处理剩余字节
            for _ in 0..remaining {
                *dst = val;
                dst = dst.add(1);
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let p = s as *mut u8;
            let val = (c & 0xFF) as u8;
            for i in 0..n {
                *p.add(i) = val;
            }
        }

        s
    }
}

/// 优化的内存设置（使用 x86 汇编指令）
///
/// 使用 REP STOSB 指令进行批量设置，比普通 memset 更快
///
/// # Arguments
/// * `s` - 目标地址
/// * `c` - 填充值
/// * `n` - 设置字节数
///
/// # Returns
/// 目标地址
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `src` 是指向至少有 `n` 字节可读内存的有效指针.
pub unsafe extern "C" fn memset_optimized(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe {
        if s.is_null() || n == 0 {
            return s;
        }

        let val = (c & 0xFF) as u64; // 转换为 u64 以适配 rax 寄存器
        let dest = s as *mut u8;

        // 使用内联汇编调用 REP STOSB
        core::arch::asm!(
            "rep stosb",
            in("rdi") dest,
            in("rax") val,
            in("rcx") n,
            options(nostack, nomem)
        );

        s
    }
}

/// 内存比较 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s1` - 第一个内存区域
/// * `s2` - 第二个内存区域
/// * `n` - 比较字节数
///
/// # Returns
/// * `0` - 相等
/// * `<0` - s1 < s2
/// * \>0 - s1 > s2
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `dst` 是指向至少有 `n` 字节可写内存的有效指针. `value` 须能放入 `u8`.
pub unsafe extern "C" fn memcmp(
    s1: *const core::ffi::c_void,
    s2: *const core::ffi::c_void,
    n: usize,
) -> i32 {
    unsafe {
        if n == 0 {
            return 0;
        }

        if s1.is_null() || s2.is_null() {
            if s1.is_null() && s2.is_null() {
                return 0;
            }
            return if s1.is_null() { -1 } else { 1 };
        }

        let p1 = s1 as *const u8;
        let p2 = s2 as *const u8;

        for i in 0..n {
            let b1 = *p1.add(i);
            let b2 = *p2.add(i);

            if b1 != b2 {
                return i32::from(b1) - i32::from(b2);
            }
        }

        0
    }
}

/// 在内存中查找字符 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s` - 内存区域起始地址
/// * `c` - 要查找的字符值
/// * `n` - 搜索范围大小
///
/// # Returns
/// 找到的字符指针，或 NULL 如果未找到
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
///
/// # Safety
///
/// `src` 是指向以 NUL 结尾的 C 字符串的有效指针.
pub unsafe extern "C" fn memchr(s: *const u8, c: i32, n: usize) -> *mut u8 {
    unsafe {
        if s.is_null() || n == 0 {
            return core::ptr::null_mut();
        }

        let p = s as *const u8;
        let target = (c & 0xFF) as u8;

        for i in 0..n {
            if *p.add(i) == target {
                return p.add(i) as *mut u8;
            }
        }

        core::ptr::null_mut()
    }
}

// ============================================================================
// 安全函数 (Secure Functions)
// ============================================================================

/// 安全清零（防止编译器优化掉）
///
/// 用于清除敏感数据（密码、密钥等），即使编译器认为这是"死代码"
/// 也不会被优化掉。
///
/// # Arguments
/// * `ptr` - 要清零的内存区域
/// * `len` - 清零的字节数
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `src` 是合法指针, 至少指向 `max_len` 字节可读内存.
pub unsafe extern "C" fn secure_zero(ptr: *mut u8, len: usize) {
    unsafe {
        if ptr.is_null() || len == 0 {
            return;
        }

        // 使用 volatile 指针强制写入，防止编译器优化
        let p = ptr as *mut core::sync::atomic::AtomicU8;

        for i in 0..len {
            (*p.add(i)).store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ============================================================================
// Rust 原生安全接口 (Safe Wrappers)
// ============================================================================

/// Rust 安全版本的 memcpy
///
/// # Arguments
/// * `dest` - 目标缓冲区
/// * `src` - 源数据
///
/// # Returns
/// 实际拷贝的字节数
pub fn safe_memcpy(dest: &mut [u8], src: &[u8]) -> usize {
    let len = dest.len().min(src.len());
    dest[..len].copy_from_slice(&src[..len]);
    len
}

/// Rust 安全版本的 memset
///
/// # Arguments
/// * `buf` - 目标缓冲区
/// * `val` - 填充值
/// * `len` - 填充长度（0 表示填充整个缓冲区）
pub fn safe_memset(buf: &mut [u8], val: u8, len: Option<usize>) {
    let actual_len = len.unwrap_or(buf.len()).min(buf.len());
    for i in 0..actual_len {
        buf[i] = val;
    }
}

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// Rust 安全版本的 memcmp
///
/// # Returns
/// * `Ordering::Equal` - 相等
/// * `Ordering::Less` - a < b  // 标准比较结果枚举
/// * `Ordering::Greater` - a > b  // 标准比较结果枚举
pub fn safe_memcmp(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
    let min_len = a.len().min(b.len());

    for i in 0..min_len {
        match a[i].cmp(&b[i]) {
            core::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }

    a.len().cmp(&b.len())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strlen_basic() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            assert_eq!(strlen(c"Hello".as_ptr()), 5);
            assert_eq!(strlen(c"".as_ptr()), 0);
            assert_eq!(strlen(c"A longer test string".as_ptr()), 20);
        }
    }

    #[test]
    fn test_strcmp_operations() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 相等
            assert_eq!(strcmp(c"test".as_ptr(), c"test".as_ptr()), 0);

            // 小于
            assert!(strcmp(c"abc".as_ptr(), c"abd".as_ptr()) < 0);

            // 大于
            assert!(strcmp(c"xyz".as_ptr(), c"xya".as_ptr()) > 0);
        }
    }

    #[test]
    fn test_strncmp_limit() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 前3个字符相等
            assert_eq!(strncmp(c"abcdef".as_ptr(), c"abcxyz".as_ptr(), 3), 0);

            // 前4个字符不等
            assert!(strncmp(c"abcdef".as_ptr(), c"abcxyz".as_ptr(), 4) < 0);
        }
    }

    #[test]
    fn test_strcpy_and_strncpy() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let mut buffer = [0i8; 20];

            // 测试 strcpy
            strcpy(buffer.as_mut_ptr(), c"Hello World".as_ptr());
            assert_eq!(strlen(buffer.as_ptr()), 11);

            // 测试 strncpy
            let mut buffer2 = [0i8; 10];
            strncpy(buffer2.as_mut_ptr(), c"Testing".as_ptr(), 5);
            assert_eq!(strlen(buffer2.as_ptr()), 5); // 只拷贝了5个字符

            // 测试 strncpy 的填充行为
            strncpy(buffer2.as_mut_ptr(), c"Hi".as_ptr(), 5);
            assert_eq!(buffer2[2], 0); // 第3个位置应该是 '\0'
        }
    }

    #[test]
    fn test_strcat() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let mut buffer = [0i8; 30];
            strcpy(buffer.as_mut_ptr(), c"Hello ".as_ptr());
            strcat(buffer.as_mut_ptr(), c"World!".as_ptr());

            assert_eq!(strlen(buffer.as_ptr()), 12); // "Hello World!"
        }
    }

    #[test]
    fn test_strchr_and_strrchr() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let s = b"Hello World\0";

            // strchr - 查找 'o' 的第一次出现
            let result = strchr(s.as_ptr() as *const i8, 'o' as i32);
            assert!(!result.is_null());
            assert_eq!(*result, 'o' as i8);

            // strrchr - 查找 'o' 的最后一次出现
            let result = strrchr(s.as_ptr() as *const i8, 'o' as i32);
            assert!(!result.is_null());
            // 应该指向 "World" 中的 'o'
        }
    }

    #[test]
    fn test_strstr() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let haystack = b"The quick brown fox jumps over the lazy dog\0";

            // 找到子串
            let result = strstr(haystack.as_ptr() as *const i8, c"brown fox".as_ptr());
            assert!(!result.is_null());

            // 未找到子串
            let result = strstr(haystack.as_ptr() as *const i8, c"cat".as_ptr());
            assert!(result.is_null());

            // 空子串
            let result = strstr(haystack.as_ptr() as *const i8, c"".as_ptr());
            assert!(!result.is_null()); // 应该返回原字符串
        }
    }

    #[test]
    fn test_memcpy_and_memmove() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let mut dest = [0u8; 10];
            let src = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

            // 测试 memcpy
            memcpy(
                dest.as_mut_ptr().cast::<core::ffi::c_void>(),
                src.as_ptr().cast::<core::ffi::c_void>(),
                10,
            );
            assert_eq!(dest, src);

            // 测试 memmove（重叠区域）
            // 元素类型必须显式为 u8: memmove 按字节搬运, 若推断为 [i32; 10]
            // 则 n=8 只覆盖 2 个元素 (2026-09-24 UT-06 实测).
            let mut overlap = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            // 将 overlap[2..] 移动到 overlap[0..]
            memmove(
                overlap.as_mut_ptr().cast::<core::ffi::c_void>(),
                overlap.as_ptr().add(2).cast::<core::ffi::c_void>(),
                8,
            );
            assert_eq!(overlap, [3, 4, 5, 6, 7, 8, 9, 10, 9, 10]);
        }
    }

    #[test]
    fn test_memset_operations() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let mut buffer = [0xABu8; 20];

            // 测试 memset
            memset(buffer.as_mut_ptr().cast::<core::ffi::c_void>(), 0x00, 20);
            assert_eq!(buffer, [0u8; 20]);

            // 测试 memset with specific value
            memset(buffer.as_mut_ptr().cast::<core::ffi::c_void>(), 0xFF, 10);
            for i in 0..10 {
                assert_eq!(buffer[i], 0xFF);
            }
            for i in 10..20 {
                assert_eq!(buffer[i], 0x00);
            }
        }
    }

    #[test]
    fn test_memcmp() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 元素类型必须显式为 u8: memcmp 按字节比较, 若推断为 [i32; 5]
            // 则 a/c 的前 5 字节相同 (小端) 而失去区分度 (2026-09-24 UT-06 实测).
            let a = [1u8, 2, 3, 4, 5];
            let b = [1u8, 2, 3, 4, 5];
            let c = [1u8, 2, 3, 4, 6];

            // 相等
            assert_eq!(
                memcmp(
                    a.as_ptr().cast::<core::ffi::c_void>(),
                    b.as_ptr().cast::<core::ffi::c_void>(),
                    5
                ),
                0
            );

            // 小于
            assert!(
                memcmp(
                    a.as_ptr().cast::<core::ffi::c_void>(),
                    c.as_ptr().cast::<core::ffi::c_void>(),
                    5
                ) < 0
            );
        }
    }

    #[test]
    fn test_memchr() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let data = [1, 2, 3, 4, 5, 3, 7, 8];

            // 找到第一个出现的 3
            let result = memchr(data.as_ptr() as *const u8, 3, 8);
            assert!(!result.is_null());
            let offset = (result as *const u8).offset_from(data.as_ptr()) as usize;
            assert_eq!(offset, 2); // 第一个 3 在索引 2

            // 未找到
            let result = memchr(data.as_ptr() as *const u8, 9, 8);
            assert!(result.is_null());
        }
    }

    #[test]
    fn test_secure_zero() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 元素类型必须显式为 u8: secure_zero 按字节清零 (2026-09-24 UT-06 实测).
            let mut secret = [0xDEu8, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
            secure_zero(secret.as_mut_ptr() as *mut u8, 6);

            for byte in secret.iter() {
                assert_eq!(*byte, 0);
            }
        }
    }

    #[test]
    fn test_rust_safe_interfaces() {
        // 测试 safe_memcpy
        let mut dest = [0u8; 10];
        let src = [1, 2, 3, 4, 5];
        let copied = safe_memcpy(&mut dest, &src);
        assert_eq!(copied, 5);
        assert_eq!(&dest[..5], &src[..]);

        // 测试 safe_memset
        safe_memset(&mut dest, 0xFF, None);
        assert_eq!(dest, [0xFF; 10]);

        // 测试 safe_memcmp
        assert_eq!(
            safe_memcmp(&[1, 2, 3], &[1, 2, 3]),
            core::cmp::Ordering::Equal
        );
        assert_eq!(
            safe_memcmp(&[1, 2, 3], &[1, 2, 4]),
            core::cmp::Ordering::Less
        );
    }
}
