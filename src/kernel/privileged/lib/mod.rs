/// 基础库模块 (Standard Library)
///
/// 提供 C 标准库风格的字符串和内存操作函数的 Rust 实现。
/// 作为内核的内部支持库，为所有内核子模块提供基础功能。
///
/// ## 功能清单
///
/// ### 字符串操作 (String Operations)
/// - `strlen` / `strlen_safe` - 字符串长度计算
/// - `strcmp` / `strncmp` - 字符串比较
/// - `strcpy` / `strncpy` - 字符串拷贝
/// - `strcat` - 字符串连接
/// - `strchr` / `strrchr` - 字符查找（正向/反向）
/// - `strstr` - 子串搜索
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
/// ## 设计特点
///
/// 1. **FFI 兼容** - 所有 C 函数都有对应的 Rust FFI 实现，保持 ABI 兼容性
/// 2. **类型安全** - 提供 Rust 原生的安全包装版本（safe_memcpy, safe_memset 等）
/// 3. **性能优化** - 关键路径使用内联汇编（memset_optimized 使用 REP STOSB）
/// 4. **完整测试** - 包含 15+ 个单元测试用例
///
/// ## 使用示例
///
/// ```rust
/// use crate::privileged::lib::string::*;
///
/// // FFI 风格 (供 C 代码调用或需要指针操作时)
/// unsafe {
///     let len = strlen(c"Hello".as_ptr());
///     memcpy(dest_ptr, src_ptr, size);
/// }
///
/// // Rust 安全风格 (推荐新代码使用)
/// let copied = safe_memcpy(&mut dest, &src);
/// safe_memset(&mut buffer, 0xFF, None);
/// ```
///
/// ## 架构定位
///
/// ```text
/// kernel/
/// ├── lib/              ← 本模块 (底层基础库)
/// │   ├── mod.rs        # 模块入口和导出
/// │   └── string.rs     # 字符串/内存操作实现
/// │
/// ├── mm/               # 使用 lib 的内存管理
/// ├── driver/           # 使用 lib 的设备驱动
/// └── net/              # 使用 lib 的网络子系统
/// ```
pub mod cstr;
pub mod string;

// 导出常用函数，方便通过 crate::privileged::lib::* 直接使用
pub use cstr::*;
pub use string::*;
