//! eBPF — 扩展伯克利包过滤器
//!
//! ## 设计
//!
//! 提供 eBPF 程序加载、验证、执行和 Map 管理.
//!
//! ### 与 Linux 的差异
//!
//! 1. **程序类型**: 仅支持 Kprobe/Tracepoint/SocketFilter 三种,
//!    不支持 XDP/SchedAct/Cgroup 等 (后续按需扩展)
//! 2. **验证器**: 采用简化验证 — 有界循环 + 寄存器类型追踪,
//!    不做 Linux 的完整路径敏感分析
//! 3. **JIT**: 当前仅解释执行, JIT 编译后续实现
//! 4. **Helper**: 仅实现 5 个基础 helper, 不支持全部 Linux helper
//!
//! ### 架构
//!
//! ```text
//! 用户态 → sys_bpf() → BpfSubsystem
//!                          ├── BpfMap (HashMap / ArrayMap)
//!                          ├── BpfProg (insns + type)
//!                          ├── BpfVerifier (安全检查)
//!                          └── BpfInterpreter (执行引擎)
//! ```
//!
//! ## SAFETY
//!
//! 本模块属于 privileged/TCB, 允许 unsafe.
//! 验证器保证程序安全 (无越界/无限循环/非法内存访问).
//! Map 操作通过 Mutex 保护.

// P-59 裁定 ①: 本模块 F8 存量清零, 转由编译器硬执法 (无基线、无豁免).
#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::privileged::sync::IrqSpinLock;

// ============================================================================
// 常量
// ============================================================================

/// eBPF 最大指令数
pub const BPF_MAX_INSNS: u32 = 4096;
/// 最大 Map 数量
pub const BPF_MAX_MAPS: u32 = 1024;
/// 最大程序数量
pub const BPF_MAX_PROGS: u32 = 1024;
/// 最大验证路径深度
pub const BPF_MAX_PATH_DEPTH: u32 = 64;
/// 最大栈深度 (字节)
pub const BPF_STACK_SIZE: usize = 512;
/// eBPF 寄存器数量
pub const BPF_REG_NUM: usize = 11;

// ============================================================================
// BPF 指令集
// ============================================================================

/// eBPF 指令操作码 — 高 3 bit = class, 低 5 bit = operation
pub mod opcode {
    // Classes
    /// 直接加载指令类 (含 64 位立即数加载).
    pub const LD: u8 = 0x00;
    /// 以源寄存器加偏移加载的指令类.
    pub const LDX: u8 = 0x01;
    /// 向内存写入立即数的存储指令类.
    pub const ST: u8 = 0x02;
    /// 把源寄存器值写入内存的存储指令类.
    pub const STX: u8 = 0x03;
    /// 32 位算术逻辑指令类.
    pub const ALU: u8 = 0x04;
    /// 64 位跳转指令类.
    pub const JMP: u8 = 0x05;
    /// 32 位跳转指令类.
    pub const JMP32: u8 = 0x06;
    /// 64 位算术逻辑指令类.
    pub const ALU64: u8 = 0x07;

    // Size modifiers
    /// 宽度修饰: 32 位访问.
    pub const W: u8 = 0x00;
    /// 宽度修饰: 16 位访问.
    pub const H: u8 = 0x08;
    /// 宽度修饰: 8 位访问.
    pub const B: u8 = 0x10;
    /// 宽度修饰: 64 位访问.
    pub const DW: u8 = 0x18;

    // Mode modifiers (for LD/ST)
    /// LD/ST 模式位: 立即数寻址.
    pub const IMM: u8 = 0x00;
    /// LD 模式位: 以报文绝对偏移寻址.
    pub const ABS: u8 = 0x20;
    /// LD 模式位: 以源寄存器值加偏移寻址.
    pub const IND: u8 = 0x40;
    /// LD/ST 模式位: 以源寄存器加偏移访问内存.
    pub const MEM: u8 = 0x60;
    /// STX 模式位: 原子内存操作.
    pub const ATOMIC: u8 = 0xc0;

    // ALU operations
    /// 加法.
    pub const ADD: u8 = 0x00;
    /// 减法.
    pub const SUB: u8 = 0x10;
    /// 乘法.
    pub const MUL: u8 = 0x20;
    /// 无符号除法 (除数为 0 时结果取 0).
    pub const DIV: u8 = 0x30;
    /// 按位或.
    pub const OR: u8 = 0x40;
    /// 按位与.
    pub const AND: u8 = 0x50;
    /// 左移.
    pub const LSH: u8 = 0x60;
    /// 逻辑右移.
    pub const RSH: u8 = 0x70;
    /// 取补 (按位取反后加一).
    pub const NEG: u8 = 0x80;
    /// 无符号取模 (除数为 0 时结果取 0).
    pub const MOD: u8 = 0x90;
    /// 按位异或.
    pub const XOR: u8 = 0xa0;
    /// 传送 (源操作数写入目的寄存器).
    pub const MOV: u8 = 0xb0;
    /// 算术右移 (保留符号位).
    pub const ARSH: u8 = 0xc0;
    /// 字节序转换 (按立即数选择 16/32/64 位).
    pub const END: u8 = 0xd0;

    // JMP operations
    /// 无条件跳转.
    pub const JA: u8 = 0x00;
    /// 相等时跳转.
    pub const JEQ: u8 = 0x10;
    /// 无符号大于时跳转.
    pub const JGT: u8 = 0x20;
    /// 无符号大于等于时跳转.
    pub const JGE: u8 = 0x30;
    /// 按位与非零时跳转.
    pub const JSET: u8 = 0x40;
    /// 不相等时跳转.
    pub const JNE: u8 = 0x50;
    /// 有符号大于时跳转.
    pub const JSGT: u8 = 0x60;
    /// 有符号大于等于时跳转.
    pub const JSGE: u8 = 0x70;
    /// 调用 helper 函数.
    pub const CALL: u8 = 0x80;
    /// 结束程序并返回 R0.
    pub const EXIT: u8 = 0x90;
    /// 无符号小于时跳转.
    pub const JLT: u8 = 0xa0;
    /// 无符号小于等于时跳转.
    pub const JLE: u8 = 0xb0;
    /// 有符号小于时跳转.
    pub const JSLT: u8 = 0xc0;
    /// 有符号小于等于时跳转.
    pub const JSLE: u8 = 0xd0;

    // Source modifier
    /// 源操作数修饰: 立即数.
    pub const K: u8 = 0x00;
    /// 源操作数修饰: 寄存器.
    pub const X: u8 = 0x08;
}

/// eBPF 8-byte 指令格式
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BpfInsn {
    /// 操作码: 高 3 位为指令类, 低 5 位为具体操作.
    pub op: u8,
    /// 寄存器打包字节: 低 4 位目的寄存器, 高 4 位源寄存器.
    pub dst_reg: u8,
    /// 跳转偏移或内存访问偏移 (有符号 16 位).
    pub off: i16,
    /// 立即数 (有符号 32 位).
    pub imm: i32,
}

impl BpfInsn {
    /// 按操作码、目的/源寄存器编号、偏移与立即数组装一条指令.
    pub fn new(op: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
        Self {
            op,
            dst_reg: (dst & 0xf) | ((src & 0xf) << 4),
            off,
            imm,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 取目的寄存器编号 (低 4 位).
    pub fn dst(&self) -> u8 {
        self.dst_reg & 0xf
    }
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 取源寄存器编号 (高 4 位).
    pub fn src(&self) -> u8 {
        (self.dst_reg >> 4) & 0xf
    }
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 取指令类码 (操作码低 3 位).
    pub fn class(&self) -> u8 {
        self.op & 0x07
    }
}

// ============================================================================
// BPF 寄存器
// ============================================================================

/// eBPF 寄存器编号
pub mod reg {
    /// 返回值寄存器.
    pub const R0: usize = 0;
    /// 第 1 个参数 (程序入口处为上下文指针).
    pub const R1: usize = 1;
    /// 第 2 个参数.
    pub const R2: usize = 2;
    /// 第 3 个参数.
    pub const R3: usize = 3;
    /// 第 4 个参数.
    pub const R4: usize = 4;
    /// 第 5 个参数.
    pub const R5: usize = 5;
    /// 被调用者保存寄存器 6.
    pub const R6: usize = 6;
    /// 被调用者保存寄存器 7.
    pub const R7: usize = 7;
    /// 被调用者保存寄存器 8.
    pub const R8: usize = 8;
    /// 被调用者保存寄存器 9.
    pub const R9: usize = 9;
    /// 栈帧指针, 只读.
    pub const R10: usize = 10;
}

// ============================================================================
// BPF 程序类型
// ============================================================================

/// eBPF 程序类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfProgType {
    /// Socket 过滤器
    SocketFilter = 1,
    /// Kprobe 跟踪点
    Kprobe = 2,
    /// Tracepoint 跟踪
    Tracepoint = 3,
}

impl BpfProgType {
    /// 由 u32 程序类型值解析枚举; 未知值返回 `None`.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::SocketFilter),
            2 => Some(Self::Kprobe),
            3 => Some(Self::Tracepoint),
            _ => None,
        }
    }
}

// ============================================================================
// BPF Map
// ============================================================================

/// BPF Map 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfMapType {
    /// 哈希表
    Hash = 1,
    /// 数组
    Array = 2,
    /// Per-CPU 哈希表
    PerCpuHash = 3,
    /// Per-CPU 数组
    PerCpuArray = 4,
}

impl BpfMapType {
    /// 由 u32 Map 类型值解析枚举; 未知值返回 `None`.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Hash),
            2 => Some(Self::Array),
            3 => Some(Self::PerCpuHash),
            4 => Some(Self::PerCpuArray),
            _ => None,
        }
    }
}

/// BPF Map 定义
#[derive(Debug)]
pub struct BpfMapDef {
    /// Map 类型 (哈希表或数组).
    pub map_type: BpfMapType,
    /// 键的固定字节数.
    pub key_size: u32,
    /// 值的固定字节数.
    pub value_size: u32,
    /// 最大条目数: 数组为容量, 哈希表为插入上限.
    pub max_entries: u32,
}

/// BPF Map 实例
pub enum BpfMap {
    /// 哈希表: key → value
    Hash {
        /// Map 定义 (键值尺寸与条目上限).
        def: BpfMapDef,
        /// 哈希表存储, 由自旋锁保护.
        data: IrqSpinLock<BTreeMap<Vec<u8>, Vec<u8>>>,
    },
    /// 数组: index → value
    Array {
        /// Map 定义 (键值尺寸与容量).
        def: BpfMapDef,
        /// 数组槽位; 空槽为 `None`.
        data: IrqSpinLock<Vec<Option<Vec<u8>>>>,
    },
}

impl BpfMap {
    /// 创建新的 BPF Map
    pub fn create(
        map_type: BpfMapType,
        key_size: u32,
        value_size: u32,
        max_entries: u32,
    ) -> Option<Self> {
        if key_size == 0 || value_size == 0 || max_entries == 0 {
            return None;
        }
        let def = BpfMapDef {
            map_type,
            key_size,
            value_size,
            max_entries,
        };
        match map_type {
            BpfMapType::Hash | BpfMapType::PerCpuHash => Some(Self::Hash {
                def,
                data: IrqSpinLock::new(BTreeMap::new()),
            }),
            BpfMapType::Array | BpfMapType::PerCpuArray => {
                let data = (0..max_entries).map(|_| None).collect();
                Some(Self::Array {
                    def,
                    data: IrqSpinLock::new(data),
                })
            }
        }
    }

    /// 查找元素
    pub fn lookup(&self, key: &[u8], value_out: &mut [u8]) -> bool {
        match self {
            Self::Hash { def, data } => {
                if key.len() != def.key_size as usize {
                    return false;
                }
                let map = data.lock();
                map.get(key).map_or(false, |v| {
                    let copy_len = core::cmp::min(v.len(), value_out.len());
                    value_out[..copy_len].copy_from_slice(&v[..copy_len]);
                    true
                })
            }
            Self::Array { def, data } => {
                if key.len() != def.key_size as usize {
                    return false;
                }
                let Some(idx) = Self::key_to_index(key, def.key_size) else {
                    return false;
                };
                let map = data.lock();
                if idx < map.len() {
                    map[idx].as_ref().map_or(false, |v| {
                        let copy_len = core::cmp::min(v.len(), value_out.len());
                        value_out[..copy_len].copy_from_slice(&v[..copy_len]);
                        true
                    })
                } else {
                    false
                }
            }
        }
    }

    /// 更新/插入元素
    pub fn update(&self, key: &[u8], value: &[u8]) -> bool {
        match self {
            Self::Hash { def, data } => {
                if key.len() != def.key_size as usize || value.len() != def.value_size as usize {
                    return false;
                }
                let mut map = data.lock();
                if map.len() >= def.max_entries as usize && !map.contains_key(key) {
                    return false; // 满
                }
                map.insert(key.to_vec(), value.to_vec());
                true
            }
            Self::Array { def, data } => {
                if key.len() != def.key_size as usize || value.len() != def.value_size as usize {
                    return false;
                }
                let Some(idx) = Self::key_to_index(key, def.key_size) else {
                    return false;
                };
                let mut map = data.lock();
                if idx < map.len() {
                    map[idx] = Some(value.to_vec());
                    true
                } else {
                    false
                }
            }
        }
    }

    /// 删除元素
    pub fn delete(&self, key: &[u8]) -> bool {
        match self {
            Self::Hash { def, data } => {
                if key.len() != def.key_size as usize {
                    return false;
                }
                data.lock().remove(key).is_some()
            }
            Self::Array { def, data } => {
                let Some(idx) = Self::key_to_index(key, def.key_size) else {
                    return false;
                };
                let mut map = data.lock();
                if idx < map.len() {
                    map[idx] = None;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// 获取 Map 定义
    pub fn def(&self) -> &BpfMapDef {
        match self {
            Self::Hash { def, .. } | Self::Array { def, .. } => def,
        }
    }

    /// 将 key 字节解释为数组索引
    fn key_to_index(key: &[u8], key_size: u32) -> Option<usize> {
        match key_size {
            1 => Some(key[0] as usize),
            2 => Some(u16::from_ne_bytes([key[0], key[1]]) as usize),
            4 => Some(u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize),
            8 => {
                let v = u64::from_ne_bytes([
                    key[0], key[1], key[2], key[3], key[4], key[5], key[6], key[7],
                ]);
                usize::try_from(v).ok()
            }
            _ => None,
        }
    }
}

// ============================================================================
// BPF 程序
// ============================================================================

/// BPF 程序实例
pub struct BpfProg {
    /// 程序类型
    pub prog_type: BpfProgType,
    /// 指令序列
    pub insns: Vec<BpfInsn>,
    /// 指令数量
    pub insn_cnt: u32,
    /// 程序名称 (调试用)
    pub name: [u8; 16],
    /// 是否已验证
    pub verified: AtomicBool,
    /// 引用计数
    pub refcnt: AtomicU32,
}

impl BpfProg {
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    /// 以程序类型与指令序列创建程序实例, 引用计数初始化为 1.
    pub fn new(prog_type: BpfProgType, insns: Vec<BpfInsn>) -> Self {
        let insn_cnt = insns.len() as u32;
        Self {
            prog_type,
            insns,
            insn_cnt,
            name: [0u8; 16],
            verified: AtomicBool::new(false),
            refcnt: AtomicU32::new(1),
        }
    }
}

// ============================================================================
// BPF 验证器 — Safe Policy Injection 接口 (T4-3)
// ============================================================================
//
// Framekernel 设计: privileged 提供 trait (机制接口), functions 提供实现 (策略).
// 验证逻辑 (`RegType`/`RegState`/逐条规则) 在 functions/debug/ebpf_verifier.rs,
// privileged 仅持有 trait 接口 + VerifyResult 错误类型 + 动态分派 (BpfSubsystem).

/// 验证结果 (T4-3: 仍定义在 privileged, 作为 trait 接口的契约类型)
#[derive(Debug)]
pub enum VerifyResult {
    /// 验证通过, 程序可被加载执行.
    Ok,
    /// 验证失败, 载荷为面向用户的错误说明.
    Err(Vec<u8>),
}

/// eBPF 验证器 trait (T4-3: 从 struct 转为 trait)
///
/// # Framekernel 设计
///
/// - **privileged (本模块)**: 定义 trait 接口, 不包含任何策略细节
/// - **functions (`functions::debug::ebpf_verifier`)**: 实现具体策略
///   (如 `StandardBpfVerifier`), 0 unsafe
///
/// # 注册流程
///
/// 1. privileged 在 `BpfSubsystem` 中持有 `Option<&'static dyn BpfVerifier>`
/// 2. functions 在启动时调用 `BpfSubsystem::set_verifier(&STANDARD_VERIFIER)`
/// 3. `prog_load` 通过动态分派调用注册的 verifier
///
/// # 默认行为
///
/// 若未注册 verifier, `prog_load` 默认拒绝所有程序 (拒绝 = 安全默认).
pub trait BpfVerifier: Sync + Send {
    /// 验证 BPF 程序
    fn verify(&self, prog: &BpfProg) -> VerifyResult;
}

// ============================================================================
// BPF Helper 函数
// ============================================================================

/// Helper 函数 ID
pub mod helper_id {
    /// bpf_trace_printk 的 helper 编号.
    pub const TRACE_PRINTK: u32 = 6;
    /// bpf_ktime_get_ns 的 helper 编号.
    pub const KTIME_GET_NS: u32 = 5;
    /// bpf_get_smp_processor_id 的 helper 编号.
    pub const GET_SMP_PROCESSOR: u32 = 8;
    /// bpf_map_lookup_elem 的 helper 编号.
    pub const MAP_LOOKUP_ELEM: u32 = 1;
    /// bpf_map_update_elem 的 helper 编号.
    pub const MAP_UPDATE_ELEM: u32 = 2;
    /// bpf_map_delete_elem 的 helper 编号.
    pub const MAP_DELETE_ELEM: u32 = 3;
}

/// BPF Helper 函数
pub struct BpfHelper;

impl BpfHelper {
    /// 检查 helper ID 是否合法
    pub fn is_valid(id: u32) -> bool {
        matches!(
            id,
            helper_id::TRACE_PRINTK
                | helper_id::KTIME_GET_NS
                | helper_id::GET_SMP_PROCESSOR
                | helper_id::MAP_LOOKUP_ELEM
                | helper_id::MAP_UPDATE_ELEM
                | helper_id::MAP_DELETE_ELEM
        )
    }

    /// 执行 helper 函数
    ///
    /// # 参数
    /// - `id`: helper 函数 ID
    /// - `r1`-`r5`: eBPF 寄存器值
    /// - `ctx`: 程序上下文
    /// - `maps`: Map FD → `Arc<BpfMap>` 映射
    ///
    /// 返回: R0 的值
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::bool_to_int_with_if,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn execute(
        id: u32,
        r1: u64,
        r2: u64,
        r3: u64,
        _r4: u64,
        _r5: u64,
        _ctx: &[u8],
        maps: &BTreeMap<u32, Arc<BpfMap>>,
    ) -> u64 {
        match id {
            helper_id::KTIME_GET_NS => {
                // 返回纳秒级时间戳 (使用 TSC 近似)
                #[cfg(target_arch = "x86_64")]
                {
                    // SAFETY: rdtsc 是用户态安全指令, 无副作用
                    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
                    tsc
                }
                #[cfg(target_arch = "aarch64")]
                {
                    let cnt: u64;
                    // SAFETY: cntvct_el0 是 EL0 可读的虚拟计数器, 无副作用
                    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) cnt) };
                    cnt
                }
            }
            helper_id::GET_SMP_PROCESSOR => u64::from(crate::privileged::smp::current_cpu_index()),
            helper_id::TRACE_PRINTK => {
                // r1 = fmt 指针, r2 = fmt 长度, r3 = arg1
                // 简化: 仅记录到 ftrace
                crate::privileged::debug::ftrace::record_named(
                    crate::privileged::debug::ftrace::fnv1a_32(b"bpf_trace"),
                    r3,
                    0,
                    0,
                    0,
                );
                r2 // 返回写入字节数
            }
            helper_id::MAP_LOOKUP_ELEM => {
                // r1 = map_fd, r2 = key_ptr
                // 简化: 在内核态直接操作, 不做 copy_from_user
                let map_fd = r1 as u32;
                maps.get(&map_fd).map_or(0, |map| {
                    // key 从 r2 指向的内存读取 (内核态直接访问)
                    let key_size = map.def().key_size as usize;
                    let key_ptr = r2 as *const u8;
                    // SAFETY: 验证器保证 key_ptr 指向有效内存, key_size 与 map 定义一致
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, key_size) };
                    let mut value = alloc::vec![0u8; map.def().value_size as usize];
                    if map.lookup(key, &mut value) {
                        // 返回 value 指针 (简化: 返回 1 表示找到)
                        1
                    } else {
                        0
                    }
                })
            }
            helper_id::MAP_UPDATE_ELEM => {
                // 寄存器分配: r1=map_fd, r2=key_ptr, r3=value_ptr, r4=flags
                let map_fd = r1 as u32;
                maps.get(&map_fd).map_or((-(1i64)).cast_unsigned(), |map| {
                    let key_size = map.def().key_size as usize;
                    let val_size = map.def().value_size as usize;
                    let key_ptr = r2 as *const u8;
                    let val_ptr = r3 as *const u8;
                    // SAFETY: 验证器保证 key_ptr/val_ptr 指向有效内存, 大小与 map 定义一致
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, key_size) };
                    // SAFETY: 同上
                    let value = unsafe { core::slice::from_raw_parts(val_ptr, val_size) };
                    if map.update(key, value) {
                        0
                    } else {
                        (-(1i64)).cast_unsigned()
                    }
                })
            }
            helper_id::MAP_DELETE_ELEM => {
                let map_fd = r1 as u32;
                maps.get(&map_fd).map_or((-(1i64)).cast_unsigned(), |map| {
                    let key_size = map.def().key_size as usize;
                    let key_ptr = r2 as *const u8;
                    // SAFETY: 验证器保证 key_ptr 指向有效内存
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, key_size) };
                    if map.delete(key) {
                        0
                    } else {
                        (-(1i64)).cast_unsigned()
                    }
                })
            }
            _ => 0,
        }
    }
}

// ============================================================================
// BPF 解释器
// ============================================================================

/// eBPF 解释器执行上下文
pub struct BpfCtx<'a> {
    /// 输入数据 (如 socket buffer / tracepoint 数据)
    pub data: &'a [u8],
    /// Map FD → `Arc<BpfMap>` 映射
    pub maps: &'a BTreeMap<u32, Arc<BpfMap>>,
}

/// eBPF 解释器
pub struct BpfInterpreter;

impl BpfInterpreter {
    /// 执行 BPF 程序
    ///
    /// 返回: R0 的值 (程序返回值)
    pub fn run(prog: &BpfProg, ctx: &BpfCtx) -> u64 {
        let mut regs = [0u64; BPF_REG_NUM];
        let mut stack = [0u8; BPF_STACK_SIZE];

        // R1 = ctx 指针, R10 = 栈顶
        regs[reg::R1] = ctx.data.as_ptr() as u64;
        // SAFETY: stack 通过 R10 指针被 eBPF ST/STX 写入
        regs[reg::R10] = stack.as_mut_ptr() as u64 + BPF_STACK_SIZE as u64;

        let mut pc: usize = 0;
        let max_pc = prog.insn_cnt as usize;

        while pc < max_pc {
            let insn = &prog.insns[pc];
            let class = insn.class();

            match class {
                opcode::ALU | opcode::ALU64 => {
                    Self::exec_alu(insn, &mut regs);
                    pc += 1;
                }
                opcode::LD | opcode::LDX => {
                    Self::exec_ld(insn, &mut regs);
                    pc += 1;
                }
                opcode::ST | opcode::STX => {
                    Self::exec_st(insn, &mut regs);
                    pc += 1;
                }
                opcode::JMP | opcode::JMP32 => {
                    if let Some(next) = Self::exec_jmp(insn, &mut regs, pc, ctx.maps) {
                        pc = next;
                    } else {
                        break; // EXIT
                    }
                }
                _ => {
                    // 未知指令类: 终止
                    break;
                }
            }
        }

        regs[reg::R0]
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    fn exec_alu(insn: &BpfInsn, regs: &mut [u64; BPF_REG_NUM]) {
        let dst = insn.dst() as usize;
        let src = insn.src() as usize;
        let is_64 = insn.class() == opcode::ALU64;
        let op_low = insn.op & 0xf0;
        let use_src = (insn.op & opcode::X) != 0;

        let src_val = if use_src {
            regs[src]
        } else {
            crate::privileged::lib::u64_from_i32(insn.imm)
        };

        let result = match op_low {
            opcode::ADD => regs[dst].wrapping_add(src_val),
            opcode::SUB => regs[dst].wrapping_sub(src_val),
            opcode::MUL => regs[dst].wrapping_mul(src_val),
            opcode::DIV => {
                if src_val == 0 {
                    0
                } else {
                    regs[dst] / src_val
                }
            }
            opcode::OR => regs[dst] | src_val,
            opcode::AND => regs[dst] & src_val,
            opcode::LSH => regs[dst].wrapping_shl(src_val as u32),
            opcode::RSH => regs[dst].wrapping_shr(src_val as u32),
            opcode::NEG => (!regs[dst]).wrapping_add(1),
            opcode::MOD => {
                if src_val == 0 {
                    0
                } else {
                    regs[dst] % src_val
                }
            }
            opcode::XOR => regs[dst] ^ src_val,
            opcode::MOV => src_val,
            opcode::ARSH => {
                // 算术右移
                if is_64 {
                    regs[dst]
                        .cast_signed()
                        .wrapping_shr(src_val as u32)
                        .cast_unsigned()
                } else {
                    crate::privileged::lib::u64_from_i32(
                        (regs[dst] as i32).wrapping_shr(src_val as u32),
                    )
                }
            }
            opcode::END => {
                // 字节序转换: 简化为截断
                match insn.imm {
                    16 => u64::from(regs[dst] as u16),
                    32 => u64::from(regs[dst] as u32),
                    _ => regs[dst],
                }
            }
            _ => regs[dst],
        };

        if is_64 {
            regs[dst] = result;
        } else {
            regs[dst] = u64::from(result as u32);
        }
    }

    fn exec_ld(insn: &BpfInsn, regs: &mut [u64; BPF_REG_NUM]) {
        let dst = insn.dst() as usize;
        let op = insn.op;

        if op == opcode::LD | opcode::IMM | opcode::DW {
            // 64-bit immediate load (2 条指令)
            regs[dst] = u64::from(insn.imm.cast_unsigned())
                | (u64::from(crate::privileged::lib::u32_from_i16(insn.off)) << 32);
        } else if op == opcode::LD | opcode::ABS | opcode::W {
            // LD_ABS_W: 从 packet 偏移 insn.imm 加载 32-bit
            // 简化: 返回 0
            regs[dst] = 0;
        } else if op == opcode::LD | opcode::IND | opcode::W {
            // LD_IND_W: 从 packet 偏移 R4 + insn.off 加载
            regs[dst] = 0;
        } else if op == opcode::LDX | opcode::MEM | opcode::W {
            // LDX_MEM_W: 从 *(u32*)(src + off) 加载
            let src = insn.src() as usize;
            let addr = regs[src].wrapping_add(i64::from(insn.off).cast_unsigned());
            let ptr = addr as *const u32;
            if !ptr.is_null() {
                // SAFETY: 验证器保证 addr 指向有效内存 (MapValue/Stack/Ctx)
                regs[dst] = u64::from(unsafe { core::ptr::read_unaligned(ptr) });
            }
        } else if op == opcode::LDX | opcode::MEM | opcode::H {
            let src = insn.src() as usize;
            let addr = regs[src].wrapping_add(i64::from(insn.off).cast_unsigned());
            let ptr = addr as *const u16;
            if !ptr.is_null() {
                // SAFETY: 同 LDX_MEM_W
                regs[dst] = u64::from(unsafe { core::ptr::read_unaligned(ptr) });
            }
        } else if op == opcode::LDX | opcode::MEM | opcode::B {
            let src = insn.src() as usize;
            let addr = regs[src].wrapping_add(i64::from(insn.off).cast_unsigned());
            let ptr = addr as *const u8;
            if !ptr.is_null() {
                // SAFETY: 同 LDX_MEM_W
                regs[dst] = u64::from(unsafe { core::ptr::read_unaligned(ptr) });
            }
        } else if op == opcode::LDX | opcode::MEM | opcode::DW {
            let src = insn.src() as usize;
            let addr = regs[src].wrapping_add(i64::from(insn.off).cast_unsigned());
            let ptr = addr as *const u64;
            if !ptr.is_null() {
                // SAFETY: 同 LDX_MEM_W
                regs[dst] = unsafe { core::ptr::read_unaligned(ptr) };
            }
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn exec_st(insn: &BpfInsn, regs: &mut [u64; BPF_REG_NUM]) {
        let dst = insn.dst() as usize;
        let addr = regs[dst].wrapping_add(i64::from(insn.off).cast_unsigned());

        if insn.class() == opcode::ST {
            // ST_IMM: *(size*)(dst + off) = imm
            match insn.op & 0x18 {
                opcode::W => {
                    let ptr = addr as *mut u32;
                    if !ptr.is_null() {
                        // SAFETY: 验证器保证 addr 指向有效可写内存 (MapValue/Stack)
                        unsafe { core::ptr::write_unaligned(ptr, insn.imm.cast_unsigned()) };
                    }
                }
                opcode::H => {
                    let ptr = addr as *mut u16;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe {
                            core::ptr::write_unaligned(
                                ptr,
                                crate::privileged::lib::u16_from_i32(insn.imm),
                            );
                        };
                    }
                }
                opcode::B => {
                    let ptr = addr as *mut u8;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe {
                            core::ptr::write_unaligned(
                                ptr,
                                crate::privileged::lib::u8_from_i32(insn.imm),
                            );
                        };
                    }
                }
                opcode::DW => {
                    let ptr = addr as *mut u64;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe {
                            core::ptr::write_unaligned(
                                ptr,
                                crate::privileged::lib::u64_from_i32(insn.imm),
                            );
                        };
                    }
                }
                _ => {}
            }
        } else {
            // STX: *(size*)(dst + off) = src
            let src = insn.src() as usize;
            match insn.op & 0x18 {
                opcode::W => {
                    let ptr = addr as *mut u32;
                    if !ptr.is_null() {
                        // SAFETY: 验证器保证 addr 指向有效可写内存
                        unsafe { core::ptr::write_unaligned(ptr, regs[src] as u32) };
                    }
                }
                opcode::H => {
                    let ptr = addr as *mut u16;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe { core::ptr::write_unaligned(ptr, regs[src] as u16) };
                    }
                }
                opcode::B => {
                    let ptr = addr as *mut u8;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe { core::ptr::write_unaligned(ptr, regs[src] as u8) };
                    }
                }
                opcode::DW => {
                    let ptr = addr as *mut u64;
                    if !ptr.is_null() {
                        // SAFETY: 同上
                        unsafe { core::ptr::write_unaligned(ptr, regs[src]) };
                    }
                }
                _ => {}
            }
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    fn exec_jmp(
        insn: &BpfInsn,
        regs: &mut [u64; BPF_REG_NUM],
        pc: usize,
        maps: &BTreeMap<u32, Arc<BpfMap>>,
    ) -> Option<usize> {
        let dst = insn.dst() as usize;
        let src = insn.src() as usize;
        let op_low = insn.op & 0xf0;
        let is_32 = insn.class() == opcode::JMP32;

        match op_low {
            opcode::JA => Some(crate::privileged::lib::usize_from_i64(
                crate::privileged::lib::i64_from_usize(pc) + 1 + i64::from(insn.off),
            )),
            opcode::EXIT => None,
            opcode::CALL => {
                let helper_id = insn.imm.cast_unsigned();
                regs[reg::R0] = BpfHelper::execute(
                    helper_id,
                    regs[reg::R1],
                    regs[reg::R2],
                    regs[reg::R3],
                    regs[reg::R4],
                    regs[reg::R5],
                    &[],
                    maps,
                );
                Some(pc + 1)
            }
            _ => {
                // 条件跳转
                let dst_val = regs[dst];
                let src_val = if (insn.op & opcode::X) != 0 {
                    regs[src]
                } else {
                    crate::privileged::lib::u64_from_i32(insn.imm)
                };
                let taken = if is_32 {
                    Self::eval_cond32(op_low, dst_val as u32, src_val as u32)
                } else {
                    Self::eval_cond64(op_low, dst_val, src_val)
                };
                if taken {
                    Some(crate::privileged::lib::usize_from_i64(
                        crate::privileged::lib::i64_from_usize(pc) + 1 + i64::from(insn.off),
                    ))
                } else {
                    Some(pc + 1)
                }
            }
        }
    }

    fn eval_cond64(op: u8, dst: u64, src: u64) -> bool {
        match op {
            opcode::JEQ => dst == src,
            opcode::JGT => dst > src,
            opcode::JGE => dst >= src,
            opcode::JSET => dst & src != 0,
            opcode::JNE => dst != src,
            opcode::JSGT => dst.cast_signed() > src.cast_signed(),
            opcode::JSGE => dst.cast_signed() >= src.cast_signed(),
            opcode::JLT => dst < src,
            opcode::JLE => dst <= src,
            opcode::JSLT => dst.cast_signed() < src.cast_signed(),
            opcode::JSLE => dst.cast_signed() <= src.cast_signed(),
            _ => false,
        }
    }

    fn eval_cond32(op: u8, dst: u32, src: u32) -> bool {
        match op {
            opcode::JEQ => dst == src,
            opcode::JGT => dst > src,
            opcode::JGE => dst >= src,
            opcode::JSET => dst & src != 0,
            opcode::JNE => dst != src,
            opcode::JSGT => dst.cast_signed() > src.cast_signed(),
            opcode::JSGE => dst.cast_signed() >= src.cast_signed(),
            opcode::JLT => dst < src,
            opcode::JLE => dst <= src,
            opcode::JSLT => dst.cast_signed() < src.cast_signed(),
            opcode::JSLE => dst.cast_signed() <= src.cast_signed(),
            _ => false,
        }
    }
}

// ============================================================================
// BPF 子系统
// ============================================================================

/// BPF 子系统 — 管理 Map 和程序的全局状态
pub struct BpfSubsystem {
    /// Map FD → `BpfMap`
    maps: IrqSpinLock<BTreeMap<u32, Arc<BpfMap>>>,
    /// 程序 FD → `BpfProg`
    progs: IrqSpinLock<BTreeMap<u32, Arc<BpfProg>>>,
    /// 下一个 Map FD
    next_map_fd: AtomicU32,
    /// 下一个程序 FD
    next_prog_fd: AtomicU32,
    /// 是否已初始化
    initialized: AtomicBool,
    /// T4-3: 已注册的 verifier (None = 拒绝所有, 安全默认)
    /// 使用 `IrqSpinLock`<Option<&'static dyn `BpfVerifier`>>, 支持运行时注册/查询
    verifier: IrqSpinLock<Option<&'static dyn BpfVerifier>>,
}

impl BpfSubsystem {
    /// 构造空的子系统状态: 无 Map、无程序、未初始化、未注册 verifier.
    pub const fn new() -> Self {
        Self {
            maps: IrqSpinLock::new(BTreeMap::new()),
            progs: IrqSpinLock::new(BTreeMap::new()),
            next_map_fd: AtomicU32::new(1),
            next_prog_fd: AtomicU32::new(1),
            initialized: AtomicBool::new(false),
            // T4-3: 默认无 verifier, 拒绝所有程序
            verifier: IrqSpinLock::new(None),
        }
    }

    /// T4-3: 注册 verifier (服务层在启动时调用)
    ///
    /// # Framekernel 契约
    ///
    /// - 仅 functions 层应调用此函数
    /// - 注册后, 所有 `prog_load` 调用通过动态分派到注册的 verifier
    /// - 重复注册: 覆盖前一个 (警告日志)
    pub fn set_verifier(&self, v: &'static dyn BpfVerifier) {
        let mut slot = self.verifier.lock();
        if slot.is_some() {
            crate::klog_ffi!(
                klog_ffi_warn,
                "[BPF] verifier replaced (T4-3: dynamic policy injection)"
            );
        }
        *slot = Some(v);
        // 必须先释放锁: 下方 debug_assert 会经 `self.verifier()` 再次加锁, 而
        // `IrqSpinLockGuard` 仅在作用域结束时 Drop 释放 — 持锁重入即自死锁.
        drop(slot);
        crate::klog_ffi!(klog_ffi_info, "[BPF] verifier registered");
        // 注册后验证: 确认 verifier 可读回 (交叉校验)
        debug_assert!(
            self.verifier().is_some(),
            "BPF set_verifier: 注册后验证失败"
        );
    }

    /// T4-3: 获取已注册的 verifier
    pub fn verifier(&self) -> Option<&'static dyn BpfVerifier> {
        *self.verifier.lock()
    }

    /// 初始化 BPF 子系统
    pub fn init(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        self.initialized.store(true, Ordering::Release);
        crate::klog_ffi!(klog_ffi_info, "[BPF] subsystem initialized");
    }

    /// 创建 Map
    pub fn map_create(
        &self,
        map_type: BpfMapType,
        key_size: u32,
        value_size: u32,
        max_entries: u32,
    ) -> i64 {
        let map = BpfMap::create(map_type, key_size, value_size, max_entries);
        match map {
            Some(m) => {
                let fd = self.next_map_fd.fetch_add(1, Ordering::AcqRel);
                let mut maps = self.maps.lock();
                if maps.len() >= BPF_MAX_MAPS as usize {
                    return -(22i64); // EINVAL
                }
                maps.insert(fd, Arc::new(m));
                i64::from(fd)
            }
            None => -(22i64), // EINVAL
        }
    }

    /// 加载程序
    pub fn prog_load(&self, prog_type: BpfProgType, insns: Vec<BpfInsn>) -> i64 {
        let prog = BpfProg::new(prog_type, insns);

        // T4-3: 验证通过动态分派到注册的 verifier
        // 安全默认: 未注册时拒绝所有程序
        let verifier_slot = self.verifier.lock();
        let Some(verifier) = *verifier_slot else {
            drop(verifier_slot);
            crate::klog_ffi!(
                klog_ffi_warn,
                "[BPF] no verifier registered, rejecting all programs"
            );
            return -(1i64); // EPERM
        };
        let result = verifier.verify(&prog);
        drop(verifier_slot);

        match result {
            VerifyResult::Ok => {
                prog.verified.store(true, Ordering::Release);
            }
            VerifyResult::Err(msg) => {
                crate::klog_ffi!(
                    klog_ffi_warn,
                    "[BPF] verifier rejected: {}",
                    core::str::from_utf8(&msg).unwrap_or("???")
                );
                return -(22i64); // EINVAL
            }
        }

        let fd = self.next_prog_fd.fetch_add(1, Ordering::AcqRel);
        let mut progs = self.progs.lock();
        if progs.len() >= BPF_MAX_PROGS as usize {
            return -(22i64);
        }
        progs.insert(fd, Arc::new(prog));
        i64::from(fd)
    }

    /// Map 操作: lookup
    pub fn map_lookup_elem(&self, map_fd: u32, key: &[u8], value_out: &mut [u8]) -> i64 {
        let maps = self.maps.lock();
        maps.get(&map_fd).map_or(-(9i64), |map| {
            if map.lookup(key, value_out) {
                0
            } else {
                -(2i64)
            }
        })
    }

    /// Map 操作: update
    pub fn map_update_elem(&self, map_fd: u32, key: &[u8], value: &[u8]) -> i64 {
        let maps = self.maps.lock();
        maps.get(&map_fd).map_or(
            -(9i64),
            |map| {
                if map.update(key, value) { 0 } else { -(22i64) }
            },
        )
    }

    /// Map 操作: delete
    pub fn map_delete_elem(&self, map_fd: u32, key: &[u8]) -> i64 {
        let maps = self.maps.lock();
        maps.get(&map_fd)
            .map_or(-(9i64), |map| if map.delete(key) { 0 } else { -(2i64) })
    }

    /// 执行程序
    pub fn prog_run(&self, prog_fd: u32, ctx_data: &[u8]) -> i64 {
        let (prog, maps_snapshot) = {
            let progs = self.progs.lock();
            let maps = self.maps.lock();
            match progs.get(&prog_fd) {
                Some(p) => {
                    if !p.verified.load(Ordering::Acquire) {
                        return -(22i64);
                    }
                    (Arc::clone(p), maps.clone())
                }
                None => return -(9i64),
            }
        };

        let ctx = BpfCtx {
            data: ctx_data,
            maps: &maps_snapshot,
        };
        BpfInterpreter::run(&prog, &ctx).cast_signed()
    }

    /// 获取 Map (供 Helper 使用)
    pub fn get_map(&self, fd: u32) -> Option<Arc<BpfMap>> {
        self.maps.lock().get(&fd).map(Arc::clone)
    }

    /// 获取程序
    pub fn get_prog(&self, fd: u32) -> Option<Arc<BpfProg>> {
        self.progs.lock().get(&fd).map(Arc::clone)
    }
}

/// 全局 BPF 子系统实例
static BPF_SUBSYSTEM: BpfSubsystem = BpfSubsystem::new();

/// 初始化 BPF 子系统
pub fn bpf_init() {
    BPF_SUBSYSTEM.init();
}

/// 获取全局 BPF 子系统
pub fn bpf_subsystem() -> &'static BpfSubsystem {
    &BPF_SUBSYSTEM
}

/// BPF 是否已初始化
pub fn bpf_is_initialized() -> bool {
    BPF_SUBSYSTEM.initialized.load(Ordering::Acquire)
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_bpf` — BPF 系统调用多路复用
///
/// `a0`: cmd (`BPF_CMD`_*)
/// `a1`: attr 指针
/// `a2`: attr 大小
///
/// cmd 值:
///   0 = `MAP_CREATE`
///   1 = `MAP_LOOKUP_ELEM`
///   2 = `MAP_UPDATE_ELEM`
///   3 = `MAP_DELETE_ELEM`
///   5 = `PROG_LOAD`
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
pub extern "C" fn sys_bpf(cmd: u64, attr: u64, size: u64) -> i64 {
    if !bpf_is_initialized() {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // BPF_MAP_CREATE
            // attr 指向: [map_type:u32, key_size:u32, value_size:u32, max_entries:u32]
            if size < 16 || attr == 0 {
                return -(22i64);
            }
            let attr_ptr = attr as *const u32;
            // SAFETY: attr 指针由 syscall 入口保证有效, size 已校验 ≥ 16
            let (map_type, key_size, value_size, max_entries) = unsafe {
                (
                    core::ptr::read_unaligned(attr_ptr),
                    core::ptr::read_unaligned(attr_ptr.add(1)),
                    core::ptr::read_unaligned(attr_ptr.add(2)),
                    core::ptr::read_unaligned(attr_ptr.add(3)),
                )
            };
            let Some(mt) = BpfMapType::from_u32(map_type) else {
                return -(22i64);
            };
            bpf_subsystem().map_create(mt, key_size, value_size, max_entries)
        }
        1 => {
            // BPF_MAP_LOOKUP_ELEM
            // attr 布局: [map_fd:u32, key_ptr:u64, value_ptr:u64]
            if size < 20 || attr == 0 {
                return -(22i64);
            }
            let attr_ptr = attr as *const u64;
            // SAFETY: attr 指针由 syscall 入口保证有效, size 已校验
            let (map_fd, key_ptr, val_ptr) = unsafe {
                (
                    core::ptr::read_unaligned(attr_ptr) as u32,
                    core::ptr::read_unaligned(attr_ptr.add(1)),
                    core::ptr::read_unaligned(attr_ptr.add(2)),
                )
            };
            // 获取 map 的 key_size
            let key_size = {
                let maps = bpf_subsystem().maps.lock();
                match maps.get(&map_fd) {
                    Some(m) => m.def().key_size as usize,
                    None => return -(9i64),
                }
            };
            let val_size = {
                let maps = bpf_subsystem().maps.lock();
                match maps.get(&map_fd) {
                    Some(m) => m.def().value_size as usize,
                    None => return -(9i64),
                }
            };
            // SAFETY: key_ptr 由用户态传入, 大小与 map 定义一致
            let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
            let mut value = alloc::vec![0u8; val_size];
            let result = bpf_subsystem().map_lookup_elem(map_fd, key, &mut value);
            if result == 0 {
                // SAFETY: val_ptr 由用户态传入, 大小与 map 定义一致
                unsafe {
                    core::ptr::copy_nonoverlapping(value.as_ptr(), val_ptr as *mut u8, val_size);
                }
            }
            result
        }
        2 => {
            // BPF_MAP_UPDATE_ELEM
            // attr 布局: [map_fd:u32, key_ptr:u64, value_ptr:u64, flags:u64]
            if size < 28 || attr == 0 {
                return -(22i64);
            }
            let attr_ptr = attr as *const u64;
            // SAFETY: attr 指针由 syscall 入口保证有效, size 已校验
            let (map_fd, key_ptr, val_ptr) = unsafe {
                (
                    core::ptr::read_unaligned(attr_ptr) as u32,
                    core::ptr::read_unaligned(attr_ptr.add(1)),
                    core::ptr::read_unaligned(attr_ptr.add(2)),
                )
            };
            let (key_size, val_size) = {
                let maps = bpf_subsystem().maps.lock();
                match maps.get(&map_fd) {
                    Some(m) => (m.def().key_size as usize, m.def().value_size as usize),
                    None => return -(9i64),
                }
            };
            // SAFETY: key_ptr/val_ptr 由用户态传入, 大小与 map 定义一致
            let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
            // SAFETY: 同上
            let value = unsafe { core::slice::from_raw_parts(val_ptr as *const u8, val_size) };
            bpf_subsystem().map_update_elem(map_fd, key, value)
        }
        3 => {
            // BPF_MAP_DELETE_ELEM
            if size < 12 || attr == 0 {
                return -(22i64);
            }
            let attr_ptr = attr as *const u64;
            // SAFETY: attr 指针由 syscall 入口保证有效, size 已校验
            let (map_fd, key_ptr) = unsafe {
                (
                    core::ptr::read_unaligned(attr_ptr) as u32,
                    core::ptr::read_unaligned(attr_ptr.add(1)),
                )
            };
            let key_size = {
                let maps = bpf_subsystem().maps.lock();
                match maps.get(&map_fd) {
                    Some(m) => m.def().key_size as usize,
                    None => return -(9i64),
                }
            };
            // SAFETY: key_ptr 由用户态传入, 大小与 map 定义一致
            let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
            bpf_subsystem().map_delete_elem(map_fd, key)
        }
        5 => {
            // BPF_PROG_LOAD
            // attr 布局: [prog_type:u32, insn_cnt:u32, insns_ptr:u64, name:u64]
            if size < 24 || attr == 0 {
                return -(22i64);
            }
            let attr_ptr = attr as *const u64;
            // SAFETY: attr 指针由 syscall 入口保证有效, size 已校验
            let (prog_type, insn_cnt, insns_ptr) = unsafe {
                let lo = core::ptr::read_unaligned(attr_ptr) as u32;
                let hi = core::ptr::read_unaligned(attr_ptr.add(0)) >> 32;
                (lo, hi as u32, core::ptr::read_unaligned(attr_ptr.add(1)))
            };
            let Some(pt) = BpfProgType::from_u32(prog_type) else {
                return -(22i64);
            };
            if insn_cnt == 0 || insn_cnt > BPF_MAX_INSNS {
                return -(22i64);
            }
            // SAFETY: insns_ptr 由用户态传入, insn_cnt 已校验范围
            let insns_slice = unsafe {
                core::slice::from_raw_parts(insns_ptr as *const BpfInsn, insn_cnt as usize)
            };
            let insns: Vec<BpfInsn> = insns_slice.to_vec();
            bpf_subsystem().prog_load(pt, insns)
        }
        _ => -(38i64), // ENOSYS
    }
}
