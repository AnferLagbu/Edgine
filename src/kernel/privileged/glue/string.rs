//! 基础字符串/内存操作库 (String & Memory Utilities)
//!
//! 提供标准 C 库风格的字符串和内存操作函数的 Rust 实现。
//! 替代原来的 lib/string.c，提供更安全的接口。
//!
//! ## 功能清单
//!
//! ### 字符串操作 (String Operations)
//! - `strlen` / `strlen_safe` - 字符串长度
//! - `strcmp` / `strncmp` - 字符串比较
//! - `strcpy` / `strncpy` - 字符串拷贝
//! - `strcat` - 字符串连接
//! - `strchr` / `strrchr` - 字符查找
//! - `strstr` - 子串查找
//!
//! ### 内存操作 (Memory Operations)
//! - `memcpy` - 内存拷贝
//! - `memmove` - 内存移动（处理重叠区域）
//! - `memset` / `memset_optimized` - 内存设置
//! - `memcmp` - 内存比较
//! - `memchr` - 内存字符查找
//!
//! ### 安全函数 (Secure Functions)
//! - `secure_zero` - 安全清零（防止编译器优化）
//!
//! ## 设计原则
//!
//! 1. **FFI 兼容** - 所有 C 函数都有对应的 Rust FFI 实现
//! 2. **类型安全** - 提供 Rust 原生的安全包装版本
//! 3. **性能优化** - 关键路径使用内联和优化算法
//! 4. **边界检查** - 所有数组操作都有安全保证

// D1: strlen 扫描上限统一复用 cstr::MAX_CSTR_LEN, 消除双常量漂移。
use super::cstr::MAX_CSTR_LEN;

// === unsafe 依据单源 (C ABI 字符串/内存胶水) ===
// 本文件是 libc 字符串/内存函数的内核侧 FFI 实现。全部 unsafe 块的依据按下面三类
// 单源化, 逐处注释只引类别 + 写该处特有的不变量。三类的 C 侧契约 (调用方义务):
//
// 类别 A「C 字符串只读扫描」(strlen / strcmp / strncmp / strchr / strrchr / strstr):
//   C 侧须保证实参指向以 NUL 结尾、整段可由内核读取的字节缓冲, 且扫描期间不被
//   并发修改。本侧只读不写。限界各不相同: strlen 上限 MAX_CSTR_LEN (cstr.rs=4096),
//   strncmp 上限 n, 其余四个无长度上限, 终止性完全依赖 C 侧的 NUL 契约。
//
// 类别 B「调用方定容量的写入」(strcpy / strncpy / strcat / memcpy / memmove /
//   memset / memset_optimized / secure_zero 三个填充类):
//   C 侧须保证目标指针指向至少容纳本次写入字节数的可写缓冲, 源指针指向至少容纳
//   读取字节数的可读缓冲, 写入期间不被并发访问; 字符串类还须 NUL 终止。本侧按 C
//   语义不额外校验容量 (与 libc 一致), 越界即 C 侧违约。例外: memmove 显式支持重叠。
//
// 类别 C「n 界定的只读比较/查找」(memcmp / memchr):
//   C 侧须保证指针指向至少 n 字节可读缓冲; 不要求 NUL 终止, 本侧不写任何字节。
//   n=0 或空指针时提前返回, 不解引用。
//
// 共同不变量: 全部函数以 `extern "C"` + `#[unsafe(no_mangle)]` 导出, 参数按 C ABI
// 传值 (指针宽度 = 目标架构字长), 无 Rust 引用/生命周期跨越 FFI 边界; 返回的指针
// 一律是入参指针或其内部偏移, 不新建所有权, 释放责任仍在 C 侧。

// ============================================================================
// 字符串操作函数 (String Operations)
// ============================================================================

/// 计算字符串长度 (C 风格 FFI 接口)
///
/// # Arguments
/// * `s` - 以 null 结尾的字符串指针
///
/// # Returns
/// 字符串长度（不包括终止符 '\0'）
///
/// # Safety
///
/// `s` 必须为 null, 或指向一段可读缓冲: 内核至多读取其前 [`MAX_CSTR_LEN`] 字节,
/// 且读取期间该缓冲不被并发修改。缓冲无 NUL 终止符时返回 [`MAX_CSTR_LEN`]。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const core::ffi::c_char) -> usize {
    // SAFETY: 类别 A。`s` 为 null 时提前返回 0 不解引用; 循环先判上限 `len <
    // MAX_CSTR_LEN` 再解引用 `*ptr`, 故最多读取下标 [0, MAX_CSTR_LEN) 共
    // MAX_CSTR_LEN 字节, 不会多读上限之外的 1 字节 (与 `glue/cstr.rs` 同形态)。
    unsafe {
        if s.is_null() {
            return 0;
        }

        let mut len = 0;
        let mut ptr = s;

        // B04-07: 加 MAX_CSTR_LEN 上限 (DECISION-060), 防御恶意指针无上界读取。
        while len < MAX_CSTR_LEN && *ptr != 0 {
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
#[inline]
#[must_use]
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
    // SAFETY: 类别 A。空指针组合提前返回 (双 null→0, 单 null→-1/1) 不解引用;
    // 循环逐字节读 `*p1` / `*p2`, 遇任一 NUL 或不等即返回无符号差值, 不写内存。
    // 无长度上限, 终止性依赖 C 侧两段缓冲均 NUL 终止。
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
                return i32::from(c1.cast_unsigned()) - i32::from(c2.cast_unsigned());
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
    // SAFETY: 类别 A。n=0 与空指针组合均提前返回; `remaining` 从 n 递减, 循环最多
    // 读 n 字节, 且遇任一 NUL 或不等即提前返回 ⇒ 读取范围是 min(n, 到首个 NUL)。
    // 只读不写。C 侧须保证两段缓冲各自至少可读该范围。
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
                return i32::from(c1.cast_unsigned()) - i32::from(c2.cast_unsigned());
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
    // SAFETY: 类别 B。按 C `strcpy` 语义逐字节复制 src 到 dest 直到并含 NUL,
    // 写入长度 = src 串长 + 1, 由 C 侧的 dest 容量契约兜住 (本侧不校验, 与 libc 一致)。
    // 空指针提前返回 dest; 返回值为入参 dest, 不新建所有权。
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
    // SAFETY: 类别 B。复制循环以 `remaining` (初值 n) 为界且遇 src 的 NUL 即停,
    // 随后零填充循环把 dest 剩余字节补 0 ⇒ 总写入恰为 n 字节, 不越出 C 侧声明的
    // dest 容量。src 只需可读至首个 NUL 或 n 字节 (取先到者)。返回入参 dest。
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
    // SAFETY: 类别 B。先扫 dest 找到其 NUL (只读, 依赖 dest 已 NUL 终止), 再从该
    // 位置按 `strcpy` 语义追加 src 直到并含 NUL ⇒ 写入范围是 dest 串长 + src 串长 + 1,
    // 容量由 C 侧契约兜住。空指针提前返回; 返回入参 dest。
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
pub unsafe extern "C" fn strchr(s: *const i8, c: i32) -> *mut i8 {
    // SAFETY: 类别 A。null 提前返回; `low_i8_from_i32(c)` 是纯截断 (取低 8 位),
    // 循环只读 `*p` 并前进, 命中 target 或遇 NUL 即返回 ⇒ 读取不越过首个 NUL。
    // target 为 0 时按 C 语义返回指向终止符的指针。返回指针是入参 s 的内部偏移,
    // `cast_mut` 只改指针可变性不产生新所有权 (C 侧 `strchr` 签名即如此)。
    unsafe {
        if s.is_null() {
            return core::ptr::null_mut();
        }

        let target = crate::privileged::glue::low_i8_from_i32(c);
        let mut p = s;

        loop {
            let ch = *p;

            if ch == target {
                return p.cast_mut();
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
pub unsafe extern "C" fn strrchr(s: *const i8, c: i32) -> *mut i8 {
    // SAFETY: 类别 A。null 提前返回; 循环只读到首个 NUL 为止 (含该字节), 用 `last`
    // 记录最后一次命中位置, 不写内存。返回的 `last.cast_mut()` 是入参 s 的内部偏移,
    // 未命中返回 null。终止性依赖 C 侧的 NUL 契约。
    unsafe {
        if s.is_null() {
            return core::ptr::null_mut();
        }

        let target = crate::privileged::glue::low_i8_from_i32(c);
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
            last.cast_mut()
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
///
/// # Safety
///
/// `ptr` 是有效指针. `value` 将被写入从 `ptr` 起 `n` 个连续字节.
pub unsafe extern "C" fn strstr(haystack: *const i8, needle: *const i8) -> *mut i8 {
    // SAFETY: 类别 A。haystack 为 null 提前返回; needle 为 null 或空串按 C 语义返回
    // haystack。外层只读到 haystack 首个 NUL 为止; 内层比较在 `*h_temp != n_ch` 时
    // break, 而 haystack 的 NUL 必与任何非零 n_ch 不等 ⇒ 内层也不会读过 haystack
    // 的终止符。needle 须由 C 侧保证 NUL 终止 (内层以其 NUL 判匹配成功)。只读不写。
    unsafe {
        if haystack.is_null() {
            return core::ptr::null_mut();
        }

        // 空子串返回 haystack 本身
        if needle.is_null() || *needle == 0 {
            return haystack.cast_mut();
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
                    return h.cast_mut(); // 匹配成功
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
pub unsafe extern "C" fn memcpy(
    dest: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> *mut core::ffi::c_void {
    // SAFETY: 类别 B。空指针或 n=0 提前返回 dest; `for i in 0..n` 逐字节
    // `*d.add(i) = *s.add(i)` ⇒ 读 src[0..n]、写 dest[0..n], 两侧容量与
    // 「区域不重叠」均由 C 侧契约兜住 (重叠须改用 memmove)。返回入参 dest。
    unsafe {
        if dest.is_null() || src.is_null() || n == 0 {
            return dest;
        }

        let d = dest.cast::<u8>();
        let s = src.cast::<u8>();

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
///
/// # Safety
///
/// `src` 与 `dst` 均为有效指针. `dst` 至少有 `n` 字节可写内存. 两个区域可重叠.
pub unsafe extern "C" fn memmove(
    dest: *mut core::ffi::c_void,
    src: *const core::ffi::c_void,
    n: usize,
) -> *mut core::ffi::c_void {
    // SAFETY: 类别 B (重叠例外)。空指针或 n=0 提前返回; 按 `d < s` 选正序否则逆序
    // 拷贝, 两种次序都保证「源字节在被覆盖前已读出」⇒ 重叠时语义与 C `memmove`
    // 一致。读写范围各为 n 字节, 容量由 C 侧契约兜住。返回入参 dest。
    unsafe {
        if dest.is_null() || src.is_null() || n == 0 {
            return dest;
        }

        let d = dest.cast::<u8>();
        let s = src.cast::<u8>();

        // 判断是否需要反向拷贝（处理重叠区域）
        if d < s.cast_mut() {
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
    // SAFETY: 类别 B。空指针或 n=0 提前返回。aarch64 分支先逐字节写到 16 对齐,
    // 再以 `stp` 每轮写 16 字节 (轮数 = remaining/16), 尾部 `remaining &= 0xF` 字节
    // 逐字节补齐 ⇒ 总写入恰为 n 字节; 非 aarch64 分支直接 `for i in 0..n` 逐字节写。
    // 两分支都只写 s[0..n], 容量由 C 侧契约兜住。`u8_from_i32(c & 0xFF)` 是纯截断。
    // 内层 `stp` 确实写内存, 故 asm 只声明 `nostack` (不带 `readonly`/`nomem`):
    // 编译器按「可能读写任意内存」处理, 不得重排或消除后续对该缓冲的读。
    unsafe {
        if s.is_null() || n == 0 {
            return s;
        }

        #[cfg(target_arch = "aarch64")]
        {
            let val = crate::privileged::glue::u8_from_i32(c & 0xFF);
            let mut dst = s.cast::<u8>();
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
                    options(nostack),
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
            let p = s.cast::<u8>();
            let val = crate::privileged::glue::u8_from_i32(c & 0xFF);
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
///
/// # Safety
///
/// `src` 是指向至少有 `n` 字节可读内存的有效指针.
pub unsafe extern "C" fn memset_optimized(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    // SAFETY: 类别 B (x86_64 专属, 函数带 `#[cfg(target_arch = "x86_64")]`)。
    // 空指针或 n=0 提前返回; `rep stosb` 从 rdi 起写 rcx 个 al 字节 ⇒ 写入范围恰为
    // s[0..n], 容量由 C 侧契约兜住。依赖 DF=0 (内核异常存根 `boot/isr.asm` 入口 `cld`)。
    // `rep stosb` 会递增 rdi、递减 rcx 至 0, 故二者以 `inout(... => _)` 声明为被改写;
    // 不声明 `readonly`/`nomem`, 编译器按「可能改写内存」处理; `nostack` 成立 (不压栈)。
    // 返回入参 s。
    unsafe {
        if s.is_null() || n == 0 {
            return s;
        }

        let val = crate::privileged::glue::u64_from_i32(c & 0xFF); // 转换为 u64 以适配 rax 寄存器
        let dest = s.cast::<u8>();

        // 使用内联汇编调用 REP STOSB
        core::arch::asm!(
            "rep stosb",
            inout("rdi") dest => _,
            in("rax") val,
            inout("rcx") n => _,
            options(nostack)
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
///
/// # Safety
///
/// `dst` 是指向至少有 `n` 字节可写内存的有效指针. `value` 须能放入 `u8`.
pub unsafe extern "C" fn memcmp(
    s1: *const core::ffi::c_void,
    s2: *const core::ffi::c_void,
    n: usize,
) -> i32 {
    // SAFETY: 类别 C。n=0 与空指针组合提前返回 (双 null→0, 单 null→-1/1);
    // `for i in 0..n` 只读比较两段 u8, 遇不等即返回差值, 全等返回 0。不写内存,
    // 不要求 NUL 终止; C 侧须保证两段各至少 n 字节可读。
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

        let p1 = s1.cast::<u8>();
        let p2 = s2.cast::<u8>();

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
///
/// # Safety
///
/// `src` 是指向以 NUL 结尾的 C 字符串的有效指针.
pub unsafe extern "C" fn memchr(s: *const u8, c: i32, n: usize) -> *mut u8 {
    // SAFETY: 类别 C。null 或 n=0 提前返回; `u8_from_i32(c & 0xFF)` 纯截断,
    // `for i in 0..n` 只读扫描, 命中即返回 `p.add(i).cast_mut()` (入参内部偏移),
    // 未命中返回 null。读取严格以 n 为界, 不依赖 NUL 终止, 不写内存。
    unsafe {
        if s.is_null() || n == 0 {
            return core::ptr::null_mut();
        }

        let p = s.cast::<u8>();
        let target = crate::privileged::glue::u8_from_i32(c & 0xFF);

        for i in 0..n {
            if *p.add(i) == target {
                return p.add(i).cast_mut();
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
///
/// # Safety
///
/// `src` 是合法指针, 至少指向 `max_len` 字节可读内存.
pub unsafe extern "C" fn secure_zero(ptr: *mut u8, len: usize) {
    // SAFETY: 类别 B。null 或 len=0 提前返回; 把 `*mut u8` 视作 `*mut AtomicU8`
    // (AtomicU8 对齐为 1, 与 u8 同布局同尺寸, 故 cast 不引入对齐/别名问题), 逐字节
    // `store(0, Relaxed)` ⇒ 写入范围恰为 ptr[0..len], 容量由 C 侧契约兜住。
    // 用原子存储而非普通写, 使编译器不能把清零当死存储消除 (本函数的目的)。
    // Relaxed 不建立跨核同步次序, 需要可见性保证时由 C 侧另加屏障。
    unsafe {
        if ptr.is_null() || len == 0 {
            return;
        }

        // 使用 volatile 指针强制写入，防止编译器优化
        let p = ptr.cast::<core::sync::atomic::AtomicU8>();

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
    for b in &mut buf[..actual_len] {
        *b = val;
    }
}

/// Rust 安全版本的 memcmp
///
/// # Returns
/// * `Ordering::Equal` - 相等
/// * `Ordering::Less` - a < b  // 标准比较结果枚举
/// * `Ordering::Greater` - a > b  // 标准比较结果枚举
#[must_use]
pub fn safe_memcmp(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
    let min_len = a.len().min(b.len());

    for i in 0..min_len {
        match a[i].cmp(&b[i]) {
            core::cmp::Ordering::Equal => {}
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
