//! EG (`Edgine`) AMD64 CPU 驱动核心 - Rust 完整实现
//!
//! ## 功能概览
//!
//! - **厂商检测**: Intel / AMD / VIA / QEMU 虚拟化
//! - **签名解析**: Stepping / Model / Family / Extended Family
//! - **特性收集**: 基础(Leaf 1) + 扩展(0x80000001) + 高级(Leaf 7)
//! - **缓存检测**: L1/L2/L3 大小、关联度、缓存行大小
//! - **MSR管理**: 读写64位MSR寄存器
//! - **TSC校准**: 频率估算 (Intel/AMD/QEMU)
//! - **多核拓扑**: 物理核心数、逻辑线程数、超线程状态
//!
//! ## 对比 C 版本 (cpu.c, 1060行)
//!
//! **不是翻译**, 而是**重新设计**:
//! ✅ 枚举替代 #define 常量 (编译时检查)
//! ✅ bitflags! 宏替代手动位操作 (类型安全)
//! ✅ Option/Result 替代返回 -1 (强制错误处理)
//! ✅ 模式匹配替代 if-else 链 (exhaustive checking)
//! ✅ trait 抽象替代函数指针 (可扩展性)
//! ✅ const fn 编译时常量计算 (零运行时开销)
//!
//! ## 模块结构
//!
//! ```text
//! cpu/
//! ├── mod.rs          # 类型定义 + 公共API + FFI导出
//! ├── cpuid.rs        # CPUID 指令封装
//! ├── msr.rs          # MSR 寄存器操作 + MSR 常量
//! ├── tsc.rs          # TSC 时间戳校准
//! ├── cache.rs        # 缓存信息检测
//! ├── topology.rs     # 多核拓扑检测
//! └── feature.rs      # 特性标志集 + 特性收集
//! ```

// 子模块声明
pub mod arch;
pub mod cache;
#[cfg(target_arch = "x86_64")]
pub mod cpuid;
mod feature;
#[cfg(target_arch = "x86_64")]
pub mod msr;
pub mod topology;
pub mod tsc;

// ============================================================================
// 公共 API 导出 (便捷访问) — 避免跨子系统直接访问 cpu 内部子模块
// ============================================================================
pub use tsc::{cycles_to_nanoseconds, read_tsc, read_tsc_serialized};
// B03-16 拆分: 类型定义迁入子模块, 顶层 re-export 保持公共 API 不变.
pub use cache::CacheInfo;
pub use feature::CpuFeatures;
pub use topology::TopologyInfo;

// ============================================================================
// 常量定义 (编译时常量)
// ============================================================================

/// 扩展 CPUID leaf 起始值 (`x86_64` 专用)
#[cfg(target_arch = "x86_64")]
const CPUID_LEAF_EXT_BASE: u32 = 0x8000_0000;

/// 厂商字符串长度 (12字节)
const VENDOR_STRING_LEN: usize = 12;

/// 品牌字符串长度 (48字节)
const BRAND_STRING_LEN: usize = 48;

// ============================================================================
// 枚举定义 (类型安全替代 int/#define)
// ============================================================================

/// CPU 厂商标识
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CpuVendor {
    /// 英特尔
    Intel = 0,
    /// AMD
    Amd = 1,
    /// VIA (威盛)
    Via = 2,
    /// Cyrix (已倒闭)
    Cyrix = 3,
    /// Transmeta ( Crusoe处理器)
    Transmeta = 4,
    /// QEMU/KVM 虚拟化
    Qemu = 5,
    /// 未知厂商 (兜底)
    ///
    /// B03-16 语义明确化: 未识别厂商时, 所有厂商特定 CPUID 分支均跳过,
    /// 检测走保守默认值路径 (缓存: L1 32KB/L2 256KB 默认, 见 `cache::detect_cache`;
    /// 拓扑: 按逻辑线程数回退, 见 `topology::detect_topology`;
    /// TSC: 1GHz 经验值, 见 `calibrate_tsc`).
    /// `is_virtualized()` 对 Unknown 返回 true 是保守假设 (VMware 等未收录厂商),
    /// 初始化后调用方应以 `features` 位 (VMX/SVM) 为准.
    Unknown = 255,
}

impl CpuVendor {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 从厂商字符串识别厂商
    ///
    /// # Arguments
    /// * `vendor_str` - 12字节的厂商ID (如 "`GenuineIntel`")
    pub fn from_vendor_string(vendor_str: &[u8; VENDOR_STRING_LEN]) -> Self {
        match vendor_str {
            b"GenuineIntel" => Self::Intel,
            b"AuthenticAMD" => Self::Amd,
            b"CentaurHauls" => Self::Via,
            b"CyrixInstead" => Self::Cyrix,
            _ if &vendor_str[..9] == b"TCGTCGTCG" => Self::Qemu, // QEMU TCG
            _ => Self::Unknown,
        }
    }

    /// 获取厂商名称 (用于显示)
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Intel => "Intel",
            Self::Amd => "AMD",
            Self::Via => "VIA",
            Self::Cyrix => "Cyrix",
            Self::Transmeta => "Transmeta",
            Self::Qemu => "QEMU Virtual",
            Self::Unknown => "Unknown",
        }
    }

    /// 是否为虚拟化环境
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn is_virtualized(&self) -> bool {
        matches!(self, Self::Qemu | Self::Unknown) // Unknown 可能是VMware等
    }
}

// ============================================================================
// 数据结构定义 (聚合体)
// ============================================================================

/// CPU 签名信息 (从 CPUID Leaf 1 EAX 提取)
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CpuSignature {
    /// 步进号 (Stepping, bits 3:0)
    pub stepping: u8,
    /// 型号 (Model, bits 7:4)
    pub model: u8,
    /// 家族号 (Family, bits 11:8)
    pub family: u8,
    /// 处理器类型 (Processor Type, bits 13:12)
    /// - 00: Original OEM
    /// - 01: `OverDrive`
    /// - 10: Dual processor
    /// - 11: Reserved
    pub processor_type: u8,
    /// 扩展型号 (Extended Model, bits 19:16)
    pub ext_model: u8,
    /// 扩展家族 (Extended Family, bits 27:20)
    pub ext_family: u8,
}

impl CpuSignature {
    /// 计算有效的家族号 (处理特殊编码)
    ///
    /// Intel 手册规定:
    /// - 如果 Family != 0xF, `Effective_Family` = Family
    /// - 如果 Family == 0xF, `Effective_Family` = `Extended_Family` + Family
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn effective_family(&self) -> u8 {
        if self.family == 0x0F {
            self.ext_family.saturating_add(self.family)
        } else {
            self.family
        }
    }

    /// 计算有效的型号 (同上逻辑)
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn effective_model(&self) -> u8 {
        if self.family == 0x06 || self.family == 0x0F {
            (self.ext_model << 4).saturating_add(self.model)
        } else {
            self.model
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 格式化为人类可读字符串 (如 "6-158-10" 表示 Family 6, Model 158, Stepping 10)
    /// 返回一个静态数组 (避免堆分配)
    pub fn to_string(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        let fam = self.effective_family();
        let mod_ = self.effective_model();
        let step = self.stepping;

        // 简单的整数转字符串 (无堆分配)
        let mut i = 0usize;

        // Family
        if fam >= 100 {
            buf[i] = (fam / 100) + b'0';
            i += 1;
        }
        if fam >= 10 {
            buf[i] = ((fam / 10) % 10) + b'0';
            i += 1;
        }
        buf[i] = (fam % 10) + b'0';
        i += 1;
        buf[i] = b'-';
        i += 1;

        // Model
        if mod_ >= 100 {
            buf[i] = (mod_ / 100) + b'0';
            i += 1;
        }
        if mod_ >= 10 {
            buf[i] = ((mod_ / 10) % 10) + b'0';
            i += 1;
        }
        buf[i] = (mod_ % 10) + b'0';
        i += 1;
        buf[i] = b'-';
        i += 1;

        // Stepping
        buf[i] = (step / 10) + b'0';
        i += 1;
        buf[i] = (step % 10) + b'0';

        buf
    }
}

/// CPU 信息聚合体 (全局单例实例)
#[derive(Debug)]
#[repr(C)]
pub struct CpuInfo {
    /// 是否已完成初始化
    pub initialized: bool,
    /// 厂商标识
    pub vendor: CpuVendor,
    /// 厂商字符串 (12字节, null终止)
    pub vendor_string: [u8; VENDOR_STRING_LEN],
    /// 品牌/型号字符串 (48字节, null终止)
    pub brand_string: [u8; BRAND_STRING_LEN],
    /// CPU 签名 (步进/型号/家族)
    pub signature: CpuSignature,
    /// 特性标志集合
    pub features: CpuFeatures,
    /// 缓存信息
    pub cache: CacheInfo,
    /// 多核拓扑
    pub topology: TopologyInfo,
    /// 最大标准 CPUID leaf 号
    pub max_standard_leaf: u32,
    /// 最大扩展 CPUID leaf 号
    pub max_ext_leaf: u32,
    /// TSC 频率估算值 (Hz, 0表示未知)
    pub tsc_frequency_hz: u64,
}

impl Default for CpuInfo {
    fn default() -> Self {
        Self {
            initialized: false,
            vendor: CpuVendor::Unknown,
            vendor_string: [0; VENDOR_STRING_LEN],
            brand_string: [0; BRAND_STRING_LEN], // 初始化为全零
            signature: CpuSignature::default(),
            features: CpuFeatures::default(),
            cache: CacheInfo::default(),
            topology: TopologyInfo::default(),
            max_standard_leaf: 0,
            max_ext_leaf: 0,
            tsc_frequency_hz: 0,
        }
    }
}

impl CpuInfo {
    /// 检查是否为 Intel CPU
    #[inline]
    pub const fn is_intel(&self) -> bool {
        matches!(self.vendor, CpuVendor::Intel)
    }

    /// 检查是否为 AMD CPU
    #[inline]
    pub const fn is_amd(&self) -> bool {
        matches!(self.vendor, CpuVendor::Amd)
    }

    /// 检查是否在虚拟化环境中运行
    #[inline]
    pub fn is_virtualized(&self) -> bool {
        self.vendor.is_virtualized() || self.features.contains(CpuFeatures::VMX | CpuFeatures::SVM)
    }

    /// 检查是否支持指定特性
    #[inline]
    pub fn has_feature(&self, feature: CpuFeatures) -> bool {
        self.features.contains(feature)
    }

    /// 获取品牌字符串的可读引用 (去除尾部null和空格)
    pub fn brand_name(&self) -> &str {
        let end = self
            .brand_string
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(BRAND_STRING_LEN);

        let trimmed = &self.brand_string[..end];
        let start = trimmed.iter().position(|&c| c != b' ').unwrap_or(0);

        core::str::from_utf8(&trimmed[start..]).unwrap_or("Unknown")
    }
}

// ============================================================================
// 全局状态 (静态单例, 使用 OnceCell 保证只初始化一次)
// ============================================================================

use crate::privileged::sync::once_lock::OnceLock;

/// 全局 CPU 信息实例 (延迟初始化, 线程安全)
static CPU_INFO: OnceLock<CpuInfo> = OnceLock::new();

/// 获取全局 CPU 信息引用 (必须先调用 `cpu_init()`)
///
/// # Returns
/// * Some(&CpuInfo) - 成功获取
/// * None - 尚未初始化
#[inline]
pub fn get_cpu_info() -> Option<&'static CpuInfo> {
    CPU_INFO.get()
}

// ============================================================================
// 公共 API - 初始化与查询
// ============================================================================

/// 初始化 CPU 驱动子系统
///
/// **必须在内核启动早期调用一次**, 在任何其他 CPU 函数之前。
///
/// # 功能
/// 1. 检测 CPU 厂商 (Intel/AMD/VIA/QEMU)
/// 2. 解析 CPU 签名 (Family/Model/Stepping)
/// 3. 收集特性标志 (SSE/AVX/NX/Virtualization...)
/// 4. 检测缓存配置 (L1/L2/L3)
/// 5. 探测多核拓扑 (核心数/线程数)
/// 6. 配置关键 MSR (EFER/NX/SSE)
/// 7. 校准 TSC 频率
///
/// # Returns
/// * Ok(()) - 初始化成功
/// * Err(&str) - 错误描述 (通常不会失败)
///
/// # Safety
/// 此函数执行内联汇编和 MSR 写入, 必须在特权级(Ring 0)调用.
/// FFI 导出函数 (C 可调用)
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn cpu_init() -> i32 {
    use crate::privileged::klog::{LogCategory, LogLevel, klog_write};

    static INIT_MSG: &[u8] = b"Initializing EG AMD64 CPU driver...\0";
    // SAFETY: FFI 日志调用; INIT_MSG 是带尾部 NUL 的静态字节切片,
    // C 端 klog_write 按 C 字符串读取.
    unsafe {
        klog_write(
            LogLevel::Info as u8,
            LogCategory::Boot as u8,
            core::ptr::null(),
            core::ptr::null(),
            0,
            INIT_MSG.as_ptr() as *const u8,
        );
    }

    // 创建新的 CpuInfo 实例
    let mut info = CpuInfo::default();

    // Step 1: 检测厂商
    info.vendor = detect_vendor(&mut info.vendor_string);

    // Step 2: 获取签名
    get_signature(
        &mut info.signature,
        &mut info.topology.apic_id,
        &mut info.topology.logical_threads,
    );

    // Step 3: 收集特性
    self::feature::collect_features(
        &mut info.features,
        &mut info.brand_string,
        &mut info.max_standard_leaf,
        &mut info.max_ext_leaf,
    );

    // Step 4: 检测缓存
    self::cache::detect_cache(
        &mut info.cache,
        info.max_standard_leaf,
        info.max_ext_leaf,
        info.vendor,
    );

    // Step 5: 探测拓扑
    self::topology::detect_topology(
        &mut info.topology,
        &info.signature,
        &info.features,
        info.max_standard_leaf,
        info.max_ext_leaf,
        info.vendor,
    );

    // Step 6: 初始化 MSR (可选, 可能失败于虚拟机)
    if let Err(e) = init_msr(&info.features) {
        let mut msg_buf = [0u8; 128];
        let e_bytes = e.as_bytes();
        let len = e_bytes.len().min(100);
        msg_buf[..len].copy_from_slice(&e_bytes[..len]);
        msg_buf[len] = 0;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            klog_write(
                LogLevel::Warn as u8,
                LogCategory::Kernel as u8,
                core::ptr::null(),
                core::ptr::null(),
                0,
                msg_buf.as_ptr() as *const u8,
            );
        }
    }

    // Step 7: 校准 TSC
    info.tsc_frequency_hz = calibrate_tsc(info.max_standard_leaf, info.vendor);

    // 标记初始化完成
    info.initialized = true;

    // 存储到全局单例 (OnceLock::set 失败表示已初始化, 不允许重复调用)
    if CPU_INFO.set(info).is_err() {
        static ERR_MSG: &[u8] = b"ERROR: cpu_init called twice!\0";
        // SAFETY: FFI logging call; ERR_MSG is a static NUL-terminated byte slice.
        unsafe {
            klog_write(
                LogLevel::Error as u8,
                LogCategory::Kernel as u8,
                core::ptr::null(),
                core::ptr::null(),
                0,
                ERR_MSG.as_ptr() as *const u8,
            );
        }
        return -1;
    }

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    static OK_MSG: &[u8] = b"CPU driver initialized successfully\0";
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
    unsafe {
        klog_write(
            LogLevel::Info as u8,
            LogCategory::Kernel as u8,
            core::ptr::null(),
            core::ptr::null(),
            0,
            OK_MSG.as_ptr() as *const u8,
        );
    }

    0 // 成功
}

/// AArch64 CPU 初始化 stub — ARMv8-A 在启动代码中已完成 EL 初始化。
#[cfg(not(target_arch = "x86_64"))]
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn cpu_init() -> i32 {
    // AArch64 的 CPU 特性初始化在 boot/aarch64/start.S 和 entry.rs 中完成
    // (EL3→EL2→EL1, FP/SIMD/Timer enable)，无需此 x86 CR0/CR4/FPU 初始化
    0
}

/// 获取 CPU 信息指针 (FFI兼容)
///
/// # Returns
/// * 非 NULL - 指向全局 `CpuInfo` 的指针
/// * NULL - 未初始化
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_info() -> *const CpuInfo {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(core::ptr::null(), |info| info as *const CpuInfo)
}

/// 检查 CPU 是否支持指定特性 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_has_feature(feature_bit: u32) -> bool {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(false, |info| {
        info.features
            .contains(CpuFeatures::from_bits_truncate(u128::from(feature_bit)))
    })
}

/// 检查是否为 Intel CPU (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_is_intel() -> bool {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(false, CpuInfo::is_intel)
}

/// 检查是否为 AMD CPU (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_is_amd() -> bool {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(false, CpuInfo::is_amd)
}

/// 检查是否在虚拟化环境中 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_is_virtualized() -> bool {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(false, CpuInfo::is_virtualized)
}

/// 获取最大标准 CPUID leaf 号 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_max_cpuid_leaf() -> u32 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(0, |info| info.max_standard_leaf)
}

/// 获取最大扩展 CPUID leaf 号 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_max_ext_cpuid_leaf() -> u32 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(0, |info| info.max_ext_leaf)
}

/// 获取 APIC ID (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_apic_id() -> u32 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(0, |info| u32::from(info.topology.apic_id))
}

/// 获取逻辑线程数 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_logical_cores() -> u8 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(1, |info| info.topology.logical_threads)
}

/// 获取物理核心数 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_physical_cores() -> u8 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(1, |info| info.topology.physical_cores)
}

/// 获取 CPU 签名 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_signature() -> CpuSignature {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(CpuSignature::default(), |info| info.signature)
}

/// 获取缓存信息指针 (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_cache_info() -> *const CacheInfo {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(core::ptr::null(), |info| &info.cache as *const CacheInfo)
}

/// 获取 TSC 频率 (Hz) (FFI兼容)
/// FFI 导出函数 (C 可调用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// FFI 导出函数 (C 可调用)
pub extern "C" fn cpu_get_tsc_frequency() -> u64 {
    // SAFETY: `as_ref` 是有效的 C ABI 函数指针; 参数列表与声明一致
    get_cpu_info().map_or(0, |info| info.tsc_frequency_hz)
}

// ============================================================================
// 内部实现函数 (private, 不暴露给外部)
// ============================================================================

/// 检测 CPU 厂商 (通过 CPUID Leaf 0)
#[cfg(target_arch = "x86_64")]
fn detect_vendor(vendor_out: &mut [u8; VENDOR_STRING_LEN]) -> CpuVendor {
    let (_, ebx, ecx, edx) = cpuid::cpuid(0, 0);

    // CPUID 返回厂商字符串在 EBX:EDX:ECX 寄存器中 (注意顺序!)
    vendor_out[0..4].copy_from_slice(&ebx.to_le_bytes());
    vendor_out[4..8].copy_from_slice(&edx.to_le_bytes());
    vendor_out[8..12].copy_from_slice(&ecx.to_le_bytes());
    vendor_out[11] = 0; // null 终止

    CpuVendor::from_vendor_string(vendor_out)
}

/// 获取 CPU 签名 (通过 CPUID Leaf 1 EAX)
#[cfg(target_arch = "x86_64")]
fn get_signature(sig_out: &mut CpuSignature, apic_id_out: &mut u8, logical_cores_out: &mut u8) {
    let (eax, ebx, _, edx) = cpuid::cpuid(1, 0);

    // 解析 EAX 位域
    sig_out.stepping = (eax & 0xF) as u8;
    sig_out.model = ((eax >> 4) & 0xF) as u8;
    sig_out.family = ((eax >> 8) & 0xF) as u8;
    sig_out.processor_type = ((eax >> 12) & 0x3) as u8;
    sig_out.ext_model = ((eax >> 16) & 0xF) as u8;
    sig_out.ext_family = ((eax >> 20) & 0xFF) as u8;

    // EBX: 逻辑处理器数 (bits 24:16), APIC ID (bits 31:24)
    *logical_cores_out = if edx & (1 << 28) != 0 {
        // HTT bit
        ((ebx >> 16) & 0xFF) as u8
    } else {
        1
    };

    *apic_id_out = ((ebx >> 24) & 0xFF) as u8;
}

#[cfg(target_arch = "x86_64")]
// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn syscall_entry();
}

/// 初始化关键 MSR 寄存器 (FPU/SSE 使能 + SYSCALL 入口 MSR).
///
/// 除 BSP 的 `cpu_init` 调用外, 次核 (AP) 上电路径亦须逐核调用: 这些 MSR
/// (CR0/CR4 及 IA32_STAR/LSTAR/SFMASK/EFER) 均为 per-logical-processor 状态,
/// BSP 的设置不随 AP 上电继承 (见 `arch/x86_64/smp_init.rs::ap_entry`)。
#[cfg(target_arch = "x86_64")]
pub(crate) fn init_msr(features: &CpuFeatures) -> Result<(), &'static str> {
    // 检查 MSR 支持
    if !features.contains(CpuFeatures::MSR) {
        return Err("CPU does not support MSR");
    }

    // 启用 SSE/SSE2 + SMEP/SMAP (设置 CR4.OSFXSR + OSXMMEXCPT + 可选 SMEP/SMAP)
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let cr4: u64;
        core::arch::asm!("mov {0}, cr4", out(reg) cr4, options(nostack, nomem));

        // bit 9 = OSFXSR, bit 10 = OSXMMEXCPT (SSE/SSE2 必需).
        // P4.B.3: bit 20 = SMEP (Ring 0 拒绝执行 USER 页), bit 21 = SMAP (Ring 0 拒绝访问 USER 页).
        // SMAP 启用后, 所有用户内存代理点 (copy_user.rs 5 函数 + userptr.rs 2 函数)
        // 必须用 stac/clac 包裹用户访问. 见 DECISION-054.
        let mut new_cr4 = cr4 | (1 << 9) | (1 << 10);
        if features.contains(CpuFeatures::SMEP) {
            new_cr4 |= 1 << 20;
        }
        if features.contains(CpuFeatures::SMAP) {
            new_cr4 |= 1 << 21;
        }
        core::arch::asm!("mov cr4, {0}", in(reg) new_cr4, options(nostack, nomem, preserves_flags));
    }

    // 启用 FPU (清除 CR0.TS + CR0.EM, 设置 CR0.MP)
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let cr0: u64;
        core::arch::asm!("mov {0}, cr0", out(reg) cr0, options(nostack, nomem));

        let new_cr0 = (cr0 & !((1 << 3) | (1 << 2))) | (1 << 1); // Clear TS/EM, Set MP
        core::arch::asm!("mov cr0, {0}", in(reg) new_cr0, options(nostack, nomem, preserves_flags));

        // 初始化 FPU 状态
        core::arch::asm!("fninit", options(nostack, nomem, preserves_flags));
    }

    // 配置 SYSCALL/SYSRET 指令
    if features.contains(CpuFeatures::SYSCALL) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 启用 SYSCALL 指令 (设置 EFER.SCE)
            let efer = self::msr::read_msr(self::msr::IA32_EFER);
            self::msr::write_msr(self::msr::IA32_EFER, efer | self::msr::EFER_SCE);

            // STAR: [63:48] = SYSRET 用户基址 (0x10), [47:32] = SYSCALL 内核 CS (0x08)
            // SYSCALL: CS = 0x08 (内核代码), SS = 0x10 (内核数据)
            // SYSRET:  CS = (0x10+16)|3 = 0x23 (用户代码), SS = 0x10+8 = 0x18 (用户数据)
            let star = (0x10u64 << 48) | (0x08u64 << 32);
            self::msr::write_msr(self::msr::IA32_STAR, star);

            // LSTAR: 64 位 syscall 入口点 (高半部分地址, KPTI 用户页表只映射高半区)
            let entry_addr = syscall_entry as *const () as u64;
            let entry_hi = entry_addr + crate::privileged::mm::KERNEL_BASE as u64;
            self::msr::write_msr(self::msr::IA32_LSTAR, entry_hi);

            // SFMASK: 进入内核时清除 IF (bit 9) 以禁用中断
            #[expect(
                clippy::items_after_statements,
                reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
            )]
            const SFMASK_IF: u64 = 1 << 9;
            self::msr::write_msr(self::msr::IA32_SFMASK, SFMASK_IF);
        }
    }

    Ok(())
}

/// 校准 TSC 频率 (Hz)
#[cfg(target_arch = "x86_64")]
fn calibrate_tsc(max_std: u32, vendor: CpuVendor) -> u64 {
    // 方法 1: Intel CPUID Leaf 0x15 (精确频率)
    if vendor == CpuVendor::Intel && max_std >= 0x15 {
        let (eax, ebx, ecx, _) = cpuid::cpuid(0x15, 0);

        if eax != 0 && ebx != 0 && ecx != 0 {
            // TSC 频率 = (crystal_freq * ebx) / eax
            // crystal_freq 通常需要额外查询, 这里简化处理
            let estimated = (u64::from(ecx) * u64::from(ebx)) / u64::from(eax);
            if estimated > 0 {
                return estimated * 1_000_000; // MHz → Hz
            }
        }
    }

    // 方法 2: 经验估计 (不精确但可用)
    match vendor {
        CpuVendor::Intel => 2_500_000_000, // 2.5 GHz 典型值
        CpuVendor::Amd => 3_000_000_000,   // 3.0 GHz 典型值
        _ => 1_000_000_000,                // 1.0 GHz (QEMU/其他)
    }
}

// ============================================================================
// 单元测试 (仅在 cargo test 时编译)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_vendor_recognition() {
        assert_eq!(
            CpuVendor::from_vendor_string(b"GenuineIntel"),
            CpuVendor::Intel
        );
        assert_eq!(
            CpuVendor::from_vendor_string(b"AuthenticAMD"),
            CpuVendor::Amd
        );
        assert_eq!(
            CpuVendor::from_vendor_string(b"CentaurHauls"),
            CpuVendor::Via
        );
        // from_vendor_string 取定长 12 字节数组; QEMU 分支只判前 9 字节前缀.
        assert_eq!(
            CpuVendor::from_vendor_string(b"TCGTCGTCGXYZ"),
            CpuVendor::Qemu
        );
        assert_eq!(
            CpuVendor::from_vendor_string(b"Unrecognized"),
            CpuVendor::Unknown
        );
    }

    #[test]
    fn test_signature_effective_values() {
        // 测试普通家族 (Family != 0xF)
        let sig = CpuSignature {
            family: 6,
            model: 0x9E,
            ext_family: 0,
            ext_model: 0,
            ..Default::default()
        };
        assert_eq!(sig.effective_family(), 6);
        assert_eq!(sig.effective_model(), 0x9E);

        // 测试扩展家族 (Family == 0xF)
        let sig_ext = CpuSignature {
            family: 0xF,
            model: 0x07,
            ext_family: 0x06,
            ext_model: 0x09,
            ..Default::default()
        };
        // Intel 规范: EffectiveFamily = ExtFamily + BaseFamily = 6 + 15 = 21
        // (原用例断言 0x0F, 漏加 BaseFamily; UT-06 实测修正)
        assert_eq!(sig_ext.effective_family(), 21);
        assert_eq!(sig_ext.effective_model(), 0x97); // (9 << 4) + 7
    }

    #[test]
    fn test_cache_info_total() {
        let cache = CacheInfo {
            l1d_size: 32 * 1024,
            l1i_size: 32 * 1024,
            l2_size: 256 * 1024,
            l3_size: 8 * 1024 * 1024, // 8MB
            ..Default::default()
        };

        assert_eq!(cache.total_size(), (32 + 32 + 256 + 8192) * 1024);
        assert!(cache.has_l3());

        let no_l3 = CacheInfo {
            l3_size: 0,
            ..Default::default()
        };
        assert!(!no_l3.has_l3());
    }

    #[test]
    fn test_topology_threads_per_core() {
        // 无超线程
        let single = TopologyInfo {
            physical_cores: 4,
            logical_threads: 4,
            hyperthreading_enabled: false,
            ..Default::default()
        };
        assert_eq!(single.threads_per_core(), 1);
        assert!(!single.is_single_core());

        // 有超线程 (2 threads/core)
        let ht = TopologyInfo {
            physical_cores: 4,
            logical_threads: 8,
            hyperthreading_enabled: true,
            ..Default::default()
        };
        assert_eq!(ht.threads_per_core(), 2);

        // 单核
        let mono = TopologyInfo {
            physical_cores: 1,
            logical_threads: 1,
            ..Default::default()
        };
        assert!(mono.is_single_core());
    }
}
#[cfg(feature = "kernel_test")]
pub fn register_cpu_tests() {
    crate::privileged::tests::arch::register_cpu_tests();
}
