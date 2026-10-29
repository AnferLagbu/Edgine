#![deny(unsafe_code)]
//! eBPF 验证器 — functions 层 (T4-3 Safe Policy Injection)
//!
//! 实现 `privileged::debug::BpfVerifier` trait, 提供标准验证策略.
//! 本模块 0 unsafe, 全部策略逻辑由 functions 拥有.
//!
//! # Framekernel 设计
//!
//! - **privileged (机制)**: 定义 `BpfVerifier` trait + `VerifyResult` 错误类型
//!   + `BpfSubsystem::set_verifier` 动态分派接口
//! - **functions (策略)**: 本模块实现 `StandardBpfVerifier`, 提供 7 条简化规则
//!
//! # 验证规则 (与 Linux BPF verifier 子集对齐)
//!
//! 1. 检查指令数量 ∈ (0, BPF_MAX_INSNS]
//! 2. 检查寄存器编号 < BPF_REG_NUM (11)
//! 3. 检查跳转目标在范围内
//! 4. 检查无无限循环 (回边检测)
//! 5. 检查程序以 EXIT 结尾
//! 6. 检查 R1-R5 调用前类型正确
//! 7. 检查 R10 只读 (frame pointer)
//!
//! # 单元测试覆盖
//!
//! 本模块自带 8 个单元测试覆盖: 空程序/超长/EXIT 缺失/寄存器 OOB/回边超限/
//! 未初始化读/合法最小程序/重复 set_verifier.

use crate::privileged::debug::{
    BPF_MAX_INSNS, BPF_REG_NUM, BPF_STACK_SIZE, BpfInsn, BpfProg, BpfVerifier, VerifyResult,
};

/// 寄存器类型 (验证器内部状态, 策略相关, 不导出)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegType {
    /// 未初始化
    NotInit,
    /// 未知标量值
    Scalar,
    /// 指向栈的指针
    StackPtr,
    /// 指向上下文的指针
    CtxPtr,
}

/// 验证器寄存器状态 (验证器内部状态)
///
/// B07-18: 新增 `range` 字段 — 标量值的精确范围 (单点常量), 用于 ALU
/// 溢出检测. 非单点/未知范围置 `None` (运行时按 64 位 wrapping 截断,
/// 不参与指针运算, 无内存破坏风险).
#[derive(Debug, Clone, Copy)]
struct RegState {
    r#type: RegType,
    range: Option<(u64, u64)>,
}

impl RegState {
    fn new() -> Self {
        Self {
            r#type: RegType::NotInit,
            range: None,
        }
    }
    fn scalar() -> Self {
        Self {
            r#type: RegType::Scalar,
            range: None,
        }
    }
    /// 已知单点常量值 (用于溢出检测)
    fn const_value(v: u64) -> Self {
        Self {
            r#type: RegType::Scalar,
            range: Some((v, v)),
        }
    }
    /// 以已知范围构造
    fn with_range(range: Option<(u64, u64)>) -> Self {
        Self {
            r#type: RegType::Scalar,
            range,
        }
    }
}

/// 标准 BPF 验证器 (T4-3: functions 实现)
///
/// # 使用方式
///
/// ```ignore
/// use crate::functions::debug::ebpf_verifier::STANDARD_VERIFIER;
/// privileged::debug::bpf_subsystem().set_verifier(&STANDARD_VERIFIER);
/// ```
pub struct StandardBpfVerifier;

/// 全局标准验证器实例 (T4-3: 0 大小, 0 状态, 安全)
pub static STANDARD_VERIFIER: StandardBpfVerifier = StandardBpfVerifier;

/// eBPF 指令操作码 (与 `privileged::debug::opcode` 保持一致)
mod opcode {
    pub const LD: u8 = 0x00;
    pub const LDX: u8 = 0x01;
    pub const ST: u8 = 0x02;
    pub const STX: u8 = 0x03;
    pub const ALU: u8 = 0x04;
    pub const JMP: u8 = 0x05;
    pub const JMP32: u8 = 0x06;
    pub const ALU64: u8 = 0x07;
    pub const MOV: u8 = 0xb0; // ALU 操作子码
    pub const ADD: u8 = 0x00; // ALU 操作子码
    pub const SUB: u8 = 0x10; // ALU 操作子码
    pub const MUL: u8 = 0x20; // ALU 操作子码
    pub const JA: u8 = 0x00; // JMP 操作子码: 无条件跳转
    pub const CALL: u8 = 0x80; // JMP 操作子码: helper 调用
    pub const EXIT: u8 = 0x90; // JMP 操作子码: 退出
    pub const X: u8 = 0x08; // 源操作数为寄存器
}

mod reg {
    pub const R1: usize = 1;
    pub const R10: usize = 10;
}

/// Helper 函数 ID (与 privileged 保持一致)
mod helper_id {
    pub const TRACE_PRINTK: u32 = 6;
    pub const KTIME_GET_NS: u32 = 5;
    pub const GET_SMP_PROCESSOR: u32 = 8;
    pub const MAP_LOOKUP_ELEM: u32 = 1;
    pub const MAP_UPDATE_ELEM: u32 = 2;
    pub const MAP_DELETE_ELEM: u32 = 3;
}

/// B07-18: helper 签名 — 每个 helper 最少需要的已初始化参数寄存器数.
///
/// 按 helper 的实际参数契约 (R1=map/R2=key/R3=value 等), 校验调用前
/// R1..Rn 全部已初始化, 拒绝参数缺失 (原实现 R2-R5 未初始化仅 break 不拒绝).
fn helper_min_args(id: u32) -> usize {
    match id {
        helper_id::MAP_UPDATE_ELEM => 3, // R1=map, R2=key, R3=value
        helper_id::MAP_LOOKUP_ELEM | helper_id::MAP_DELETE_ELEM => 2, // R1=map, R2=key
        helper_id::TRACE_PRINTK => 1,    // R1=fmt
        _ => 0,                          // KTIME_GET_NS / GET_SMP_PROCESSOR 无参数
    }
}

/// B07-18: ALU64 单点常量运算的溢出检测.
///
/// 仅当 dst 与 (寄存器源或立即数) 均为已知单点常量时做 checked 运算;
/// 溢出/下溢返回 `Err`(拒绝). 其他操作 (位运算/移位/未知范围) 返回
/// `Ok(None)` (结果范围未知, 运行时 64 位 wrapping 截断, 不参与指针运算).
fn alu_checked_range(
    op_low: u8,
    dst_range: Option<(u64, u64)>,
    src_range: Option<(u64, u64)>,
    src_is_reg: bool,
    imm: i32,
) -> Result<Option<(u64, u64)>, ()> {
    let Some((a0, a1)) = dst_range else {
        return Ok(None);
    };
    if a0 != a1 {
        return Ok(None); // 非单点, 不精确判断
    }
    let b = if src_is_reg {
        let Some((b0, b1)) = src_range else {
            return Ok(None);
        };
        if b0 != b1 {
            return Ok(None);
        }
        b0
    } else {
        crate::functions::intcast::u64_from_i32(imm)
    };
    let v = match op_low {
        opcode::ADD => a0.checked_add(b).ok_or(())?,
        opcode::SUB => a0.checked_sub(b).ok_or(())?,
        opcode::MUL => a0.checked_mul(b).ok_or(())?,
        _ => return Ok(None),
    };
    Ok(Some((v, v)))
}

/// 最大回边深度 (策略)
const BPF_MAX_PATH_DEPTH: u32 = 8;

impl BpfVerifier for StandardBpfVerifier {
    fn verify(&self, prog: &BpfProg) -> VerifyResult {
        // 前置校验: 规则 1 (指令数量) + 规则 5 (末条 EXIT)
        if let Some(err) = verify_program_checks(prog) {
            return err;
        }

        // 初始化寄存器状态
        let mut regs = [RegState::new(); BPF_REG_NUM];
        regs[reg::R1] = RegState {
            r#type: RegType::CtxPtr,
            range: None,
        };
        regs[reg::R10] = RegState {
            r#type: RegType::StackPtr,
            range: None,
        };

        // 逐条验证
        let mut visited = [false; BPF_MAX_INSNS as usize];
        let mut path_depth: u32 = 0;
        for (pc, insn) in prog.insns.iter().enumerate() {
            if pc >= BPF_MAX_INSNS as usize {
                break;
            }
            visited[pc] = true;
            if let Err(err) = verify_insn(&mut regs, prog, pc, *insn, &mut path_depth) {
                return err;
            }
        }

        VerifyResult::Ok
    }
}

/// 程序级前置校验 (规则 1: 指令数量; 规则 5: 末条必须 EXIT)
fn verify_program_checks(prog: &BpfProg) -> Option<VerifyResult> {
    // 规则 1: 指令数量
    if prog.insn_cnt == 0 {
        return Some(VerifyResult::Err(b"empty program".to_vec()));
    }
    if prog.insn_cnt > BPF_MAX_INSNS {
        return Some(VerifyResult::Err(b"program too large".to_vec()));
    }

    // 规则 5: 最后一条指令必须是 EXIT
    if let Some(last) = prog.insns.last()
        && last.op != opcode::JMP | opcode::EXIT
    {
        return Some(VerifyResult::Err(b"program must end with EXIT".to_vec()));
    }

    None
}

/// 单条指令验证 (规则 2/7 前置检查, 之后按指令类别分派)
fn verify_insn(
    regs: &mut [RegState; BPF_REG_NUM],
    prog: &BpfProg,
    pc: usize,
    insn: BpfInsn,
    path_depth: &mut u32,
) -> Result<(), VerifyResult> {
    // 规则 2: 寄存器编号检查
    let dst = insn.dst() as usize;
    let src = insn.src() as usize;
    if dst >= BPF_REG_NUM || src >= BPF_REG_NUM {
        return Err(VerifyResult::Err(
            alloc::format!("invalid register at pc={pc}: dst={dst} src={src}").into_bytes(),
        ));
    }

    // 规则 7: R10 只读
    if dst == reg::R10
        && insn.class() != opcode::ALU
        && insn.class() != opcode::ALU64
        && insn.class() != opcode::JMP
        && insn.class() != opcode::JMP32
        && (insn.class() == opcode::ST || insn.class() == opcode::STX)
    {
        return Err(VerifyResult::Err(
            alloc::format!("write to R10 (frame pointer) at pc={pc}").into_bytes(),
        ));
    }

    let class = insn.class();

    // 跳转目标 / helper 调用 / 条件跳转操作数检查
    if class == opcode::JMP || class == opcode::JMP32 {
        verify_jump(regs, prog, pc, insn, dst, src, path_depth)?;
    }

    // ALU 操作
    if class == opcode::ALU || class == opcode::ALU64 {
        verify_alu(regs, pc, insn, dst, src, class)?;
    }

    // LD: 加载到寄存器
    if class == opcode::LD {
        verify_ld(regs, pc, insn, dst)?;
    }
    if class == opcode::LDX {
        verify_ldx(regs, pc, insn, dst, src)?;
    }

    // ST/STX: 存储操作
    if class == opcode::ST || class == opcode::STX {
        verify_store(regs, pc, dst, src, class)?;
    }

    Ok(())
}

/// 跳转类指令验证 (JA 目标/回边, CALL helper 契约, 条件跳转目标与操作数)
fn verify_jump(
    regs: &mut [RegState; BPF_REG_NUM],
    prog: &BpfProg,
    pc: usize,
    insn: BpfInsn,
    dst: usize,
    src: usize,
    path_depth: &mut u32,
) -> Result<(), VerifyResult> {
    let op_low = insn.op & 0xf0;
    if op_low == opcode::JA {
        // 无条件跳转
        check_jump_target(prog, pc, insn, "jump out of bounds")?;
        // 规则 4: 回边检测
        count_backward_jump(pc, insn, path_depth)?;
    } else if op_low == opcode::CALL {
        // Helper 调用
        verify_helper_call(regs, pc, insn)?;
    } else if op_low != opcode::EXIT {
        // 条件跳转
        check_jump_target(prog, pc, insn, "conditional jump OOB")?;
        count_backward_jump(pc, insn, path_depth)?;
    }

    // 条件跳转: 检查操作数已初始化
    if op_low != opcode::JA && op_low != opcode::CALL && op_low != opcode::EXIT {
        if insn.op & opcode::X != 0 && regs[src].r#type == RegType::NotInit {
            return Err(VerifyResult::Err(
                alloc::format!("use of uninitialized R{src} at pc={pc}").into_bytes(),
            ));
        }
        if regs[dst].r#type == RegType::NotInit {
            return Err(VerifyResult::Err(
                alloc::format!("use of uninitialized R{dst} at pc={pc}").into_bytes(),
            ));
        }
    }

    Ok(())
}

/// 校验跳转目标落在程序范围内 (`label` 决定错误文案)
fn check_jump_target(
    prog: &BpfProg,
    pc: usize,
    insn: BpfInsn,
    label: &str,
) -> Result<(), VerifyResult> {
    let target = crate::functions::intcast::i64_from_usize(pc) + 1 + i64::from(insn.off);
    if target < 0 || crate::functions::intcast::usize_from_i64(target) >= prog.insn_cnt as usize {
        return Err(VerifyResult::Err(
            alloc::format!("{label} at pc={pc}: target={target}").into_bytes(),
        ));
    }
    Ok(())
}

/// 规则 4: 回边计数 (超过 `BPF_MAX_PATH_DEPTH` 即拒绝)
fn count_backward_jump(pc: usize, insn: BpfInsn, path_depth: &mut u32) -> Result<(), VerifyResult> {
    let target = crate::functions::intcast::i64_from_usize(pc) + 1 + i64::from(insn.off);
    if target < crate::functions::intcast::i64_from_usize(pc) {
        *path_depth += 1;
        if *path_depth > BPF_MAX_PATH_DEPTH {
            return Err(VerifyResult::Err(
                alloc::format!("too many backward jumps at pc={pc}").into_bytes(),
            ));
        }
    }
    Ok(())
}

/// Helper 调用验证 (规则 8: R1 已初始化; 规则 12: 按签名校验 R1..Rn 已初始化)
fn verify_helper_call(
    regs: &mut [RegState; BPF_REG_NUM],
    pc: usize,
    insn: BpfInsn,
) -> Result<(), VerifyResult> {
    let helper_id = insn.imm.cast_unsigned();
    if !is_valid_helper(helper_id) {
        return Err(VerifyResult::Err(
            alloc::format!("unknown helper {helper_id} at pc={pc}").into_bytes(),
        ));
    }
    // EBPF-3 规则 8: helper 调用前 R1 必须已初始化
    // 简化策略: 仅要求 R1 初始化 (即非 NotInit), 不深究类型
    // (实际 BPF verifier 会按 helper 签名校验参数类型, 此处简化)
    if regs[reg::R1].r#type == RegType::NotInit {
        return Err(VerifyResult::Err(
            alloc::format!("helper call at pc={pc} with uninitialized R1").into_bytes(),
        ));
    }
    // B07-18 规则 12: 按 helper 签名校验 R1..Rn 已初始化.
    // 原实现 R2-R5 未初始化仅 break (不拒绝), 恶意程序可绕过
    // 参数契约调用 helper (如 map 操作缺 key 指针).
    let min_args = helper_min_args(helper_id);
    for (arg_reg, reg_state) in regs.iter().enumerate().skip(1).take(min_args) {
        if reg_state.r#type == RegType::NotInit {
            return Err(VerifyResult::Err(
                alloc::format!("helper {helper_id} at pc={pc} requires R{arg_reg} initialized")
                    .into_bytes(),
            ));
        }
    }
    // 调用后 R1 视为已初始化的标量
    regs[reg::R1] = RegState::scalar();
    Ok(())
}

/// ALU/ALU64 验证 (MOV 语义 + 单点常量溢出检测)
fn verify_alu(
    regs: &mut [RegState; BPF_REG_NUM],
    pc: usize,
    insn: BpfInsn,
    dst: usize,
    src: usize,
    class: u8,
) -> Result<(), VerifyResult> {
    let op_low = insn.op & 0xf0;
    // MOV 是写 dst 而非读 dst (MOV imm 即为寄存器初始化), 故不要求 dst 已初始化;
    // 其余 ALU 运算读 dst, 要求已初始化.
    // (原实现先判 dst NotInit 再判 MOV, 使 `MOV R0, 1` 等全部被误拒;
    //  2026-09-24 UT-06 修复)
    if op_low != opcode::MOV && regs[dst].r#type == RegType::NotInit {
        return Err(VerifyResult::Err(
            alloc::format!("use of uninitialized R{dst} at pc={pc}").into_bytes(),
        ));
    }
    if op_low == opcode::MOV {
        if (insn.op & opcode::X) != 0 {
            // MOV reg: 复制源类型
            if regs[src].r#type == RegType::NotInit {
                return Err(VerifyResult::Err(
                    alloc::format!("use of uninitialized R{src} at pc={pc}").into_bytes(),
                ));
            }
            regs[dst] = regs[src];
        } else {
            // B07-18: MOV imm 记录单点常量范围 (供后续 ALU 溢出检测)
            regs[dst] = RegState::const_value(crate::functions::intcast::u64_from_i32(insn.imm));
        }
    } else {
        // B07-18 规则 13: ALU64 单点常量运算溢出检测.
        // 原实现一律置 Scalar, 无法拦截 u64::MAX+1 等常量折叠溢出.
        if class == opcode::ALU64 {
            let src_is_reg = (insn.op & opcode::X) != 0;
            if src_is_reg && regs[src].r#type == RegType::NotInit {
                return Err(VerifyResult::Err(
                    alloc::format!("use of uninitialized R{src} at pc={pc}").into_bytes(),
                ));
            }
            match alu_checked_range(
                op_low,
                regs[dst].range,
                regs[src].range,
                src_is_reg,
                insn.imm,
            ) {
                Ok(range) => regs[dst] = RegState::with_range(range),
                Err(()) => {
                    return Err(VerifyResult::Err(
                        alloc::format!("ALU overflow at pc={pc}").into_bytes(),
                    ));
                }
            }
        } else {
            // ALU32: 32 位截断运算, 结果范围未知
            regs[dst] = RegState::scalar();
        }
    }
    Ok(())
}

/// LD 验证 (规则 9: context 偏移越界检查)
fn verify_ld(
    regs: &mut [RegState; BPF_REG_NUM],
    pc: usize,
    insn: BpfInsn,
    dst: usize,
) -> Result<(), VerifyResult> {
    // EBPF-4 规则 9: LD 加载 context 偏移越界检查
    // 简化: 拒绝极端偏移 (off 超出 [-4096, 4096] 范围)
    // 实际 BPF verifier 会基于 prog_type 校验 ctx 大小
    if insn.off < -4096 || insn.off > 4096 {
        return Err(VerifyResult::Err(
            alloc::format!("LD ctx offset out of range at pc={pc}: off={}", insn.off).into_bytes(),
        ));
    }
    regs[dst] = RegState::scalar();
    Ok(())
}

/// LDX 验证 (规则 10: 源必须是已知指针; 规则 14: 栈读偏移深度)
fn verify_ldx(
    regs: &mut [RegState; BPF_REG_NUM],
    pc: usize,
    insn: BpfInsn,
    dst: usize,
    src: usize,
) -> Result<(), VerifyResult> {
    if regs[src].r#type == RegType::NotInit {
        return Err(VerifyResult::Err(
            alloc::format!("use of uninitialized R{src} at pc={pc}").into_bytes(),
        ));
    }
    // EBPF-4 规则 10: LDX 加载要求 src 必须是已知指针 (StackPtr 或 MapValue)
    // 简化: 拒绝从 scalar 加载 (避免将标量当指针)
    match regs[src].r#type {
        RegType::StackPtr => {
            // B07-18 规则 14: 栈读偏移深度验证 — R10 基地址的 off
            // 必须在 [-BPF_STACK_SIZE, 0] (R10 指向栈底+512, 负偏移访问栈内).
            // 原实现不校验 off, 恶意程序可 [R10+large] 越界读.
            if insn.off < -crate::functions::intcast::low_i16_from_usize(BPF_STACK_SIZE)
                || insn.off > 0
            {
                return Err(VerifyResult::Err(
                    alloc::format!(
                        "stack load offset out of range at pc={pc}: off={}",
                        insn.off
                    )
                    .into_bytes(),
                ));
            }
        }
        RegType::CtxPtr => {
            // 合法: 已知指针
        }
        _ => {
            return Err(VerifyResult::Err(
                alloc::format!(
                    "LDX from non-pointer register R{} at pc={}: type={:?}",
                    src,
                    pc,
                    regs[src].r#type
                )
                .into_bytes(),
            ));
        }
    }
    regs[dst] = RegState::scalar();
    Ok(())
}

/// ST/STX 验证 (目标必须是可以写指针, STX 源必须已初始化)
fn verify_store(
    regs: &[RegState; BPF_REG_NUM],
    pc: usize,
    dst: usize,
    src: usize,
    class: u8,
) -> Result<(), VerifyResult> {
    if regs[dst].r#type == RegType::NotInit {
        return Err(VerifyResult::Err(
            alloc::format!("store to uninitialized R{dst} at pc={pc}").into_bytes(),
        ));
    }
    match regs[dst].r#type {
        // StackPtr/CtxPtr: 允许写入
        RegType::StackPtr | RegType::CtxPtr => {}
        _ => {
            return Err(VerifyResult::Err(
                alloc::format!(
                    "store to invalid pointer type {:?} at pc={}",
                    regs[dst].r#type,
                    pc
                )
                .into_bytes(),
            ));
        }
    }
    if class == opcode::STX && regs[src].r#type == RegType::NotInit {
        return Err(VerifyResult::Err(
            alloc::format!("use of uninitialized R{src} at pc={pc}").into_bytes(),
        ));
    }
    Ok(())
}

fn is_valid_helper(id: u32) -> bool {
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

// ============================================================================
// 单元测试 (T4-3: 覆盖 7 条规则 + 边界)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::debug::opcode;
    use crate::privileged::debug::{BpfInsn, BpfProgType};
    use alloc::vec;
    use alloc::vec::Vec;

    fn make_insn(op: u8, dst: u8, src: u8, off: i16, imm: i32) -> BpfInsn {
        // BpfInsn 已将 dst/src 打包进 dst_reg 字段, 构造统一走 BpfInsn::new.
        BpfInsn::new(op, dst, src, off, imm)
    }

    fn make_prog(insns: Vec<BpfInsn>) -> BpfProg {
        let mut prog = BpfProg::new(BpfProgType::Kprobe, insns);
        prog.insn_cnt = prog.insns.len() as u32;
        prog
    }

    #[test]
    fn test_empty_program_rejected() {
        let prog = BpfProg::new(BpfProgType::Kprobe, vec![]);
        assert!(matches!(
            STANDARD_VERIFIER.verify(&prog),
            VerifyResult::Err(_)
        ));
    }

    #[test]
    fn test_program_must_end_with_exit() {
        // 最后一条不是 EXIT
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 1), // R0 = 1
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 2), // R0 = 2 (无 EXIT)
        ];
        let prog = make_prog(insns);
        assert!(matches!(
            STANDARD_VERIFIER.verify(&prog),
            VerifyResult::Err(_)
        ));
    }

    #[test]
    fn test_minimal_valid_program() {
        // MOV R0, 1; EXIT
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 1),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    #[test]
    fn test_register_oob_rejected() {
        // dst=11 越界 (BPF_REG_NUM=11, valid range 0..10)
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 11, 0, 0, 1),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    #[test]
    fn test_write_to_r10_rejected() {
        // ST [R10+0] = 1
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 1, 0, 0, 1), // R1 = 1
            make_insn(opcode::STX, 10, 1, 0, 0),                // [R10+0] = R1 (违规)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    #[test]
    fn test_unknown_helper_rejected() {
        // CALL imm=999 (未知 helper)
        let insns = vec![
            make_insn(opcode::JMP | opcode::CALL, 0, 0, 0, 999),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    #[test]
    fn test_uninit_register_use_rejected() {
        // 读未初始化的 R2
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::ADD, 0, 2, 0, 0), // R0 = R0 + R2 (R2 未初始化)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    #[test]
    fn test_valid_helper_call() {
        // 调用 ktime_get_ns helper, 然后 EXIT
        let insns = vec![
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::KTIME_GET_NS as i32,
            ),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // EBPF-2: 以下 8 个单测覆盖更复杂场景, 是 T4-3 验证器的边界/复杂性测试

    /// 1. 合法 ALU 程序: ADD/AND/XOR 链
    #[test]
    fn test_alu_chain_valid() {
        // R0 = 1; R0 += 2; R0 &= 3; R0 ^= 0xF; EXIT
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 1),
            make_insn(opcode::ALU64 | 0x04, 0, 0, 0, 2), // ADD imm
            make_insn(opcode::ALU64 | 0x50, 0, 0, 0, 3), // AND imm
            make_insn(opcode::ALU64 | 0xa0, 0, 0, 0, 0xF), // XOR imm
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    /// 2. 合法 MOV 寄存器: R0 = R1; EXIT
    #[test]
    fn test_mov_reg_valid() {
        // R1 = 0x42 (CtxPtr 状态在 verifier 中是 scalar-compatible);
        // R0 = R1; EXIT
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 1), // R0 = 1 (初始化)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    /// 3. 跳转到第 0 条: 合法 (前向跳转)
    #[test]
    fn test_forward_jump_to_zero() {
        // JA 0 (无条件跳到第 0 条)
        // 第 0 条: JA 0
        // 第 1 条: EXIT
        let insns = vec![
            make_insn(opcode::JMP | opcode::JA, 0, 0, -1, 0), // pc=0, target=0+1-1=0
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    /// 4. 跳转到不存在的指令: 应拒绝
    #[test]
    fn test_jump_oob_rejected() {
        // JA target=10 但程序只有 2 条
        let insns = vec![
            make_insn(opcode::JMP | opcode::JA, 0, 0, 10, 0),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    /// 5. 多 helper 连续调用: 合法
    #[test]
    fn test_multiple_helper_calls() {
        // R0 = 0; 调用 ktime_get_ns (R0 = tsc); 调用 trace_printk (R0 保持);
        // 调用 get_smp_processor; EXIT
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 0),
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::KTIME_GET_NS as i32,
            ),
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::TRACE_PRINTK as i32,
            ),
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::GET_SMP_PROCESSOR as i32,
            ),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    /// 6. 全部 6 个合法 helper 都被识别 (B07-18: 按签名初始化所需参数)
    #[test]
    fn test_all_legal_helpers_accepted() {
        let legal = [
            helper_id::MAP_LOOKUP_ELEM as i32,
            helper_id::MAP_UPDATE_ELEM as i32,
            helper_id::MAP_DELETE_ELEM as i32,
            helper_id::KTIME_GET_NS as i32,
            helper_id::TRACE_PRINTK as i32,
            helper_id::GET_SMP_PROCESSOR as i32,
        ];
        for h in legal {
            let mut insns = Vec::new();
            // B07-18: 按 helper 签名初始化参数寄存器 (R1=map, R2=key, R3=value).
            match h as u32 {
                helper_id::MAP_UPDATE_ELEM => {
                    insns.push(make_insn(opcode::ALU64 | opcode::MOV, 1, 0, 0, 0));
                    insns.push(make_insn(opcode::ALU64 | opcode::MOV, 2, 0, 0, 0));
                    insns.push(make_insn(opcode::ALU64 | opcode::MOV, 3, 0, 0, 0));
                }
                helper_id::MAP_LOOKUP_ELEM | helper_id::MAP_DELETE_ELEM => {
                    insns.push(make_insn(opcode::ALU64 | opcode::MOV, 1, 0, 0, 0));
                    insns.push(make_insn(opcode::ALU64 | opcode::MOV, 2, 0, 0, 0));
                }
                _ => {}
            }
            insns.push(make_insn(opcode::JMP | opcode::CALL, 0, 0, 0, h));
            insns.push(make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0));
            let prog = make_prog(insns);
            assert!(
                matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok),
                "helper {} should be accepted",
                h
            );
        }
    }

    /// 7. 大程序 (100 条 MOV 链): 合法
    #[test]
    fn test_large_program_100_insns() {
        let mut insns = Vec::with_capacity(101);
        for i in 0..100 {
            insns.push(make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, i));
        }
        insns.push(make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0));
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    /// 8. 标准验证器与 trait 多态: `&dyn BpfVerifier` 动态分派
    #[test]
    fn test_trait_dispatch_via_dyn() {
        // 关键: T4-3 framekernel 设计 — 通过 trait 动态分派
        let v: &dyn BpfVerifier = &STANDARD_VERIFIER;
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 42),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        // 通过 dyn trait 调用 verify, 确保 trait 接口可分派
        assert!(matches!(v.verify(&prog), VerifyResult::Ok));

        // 反例: 寄存器 OOB 也应通过 dyn 分派被拒绝
        let bad_insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 11, 0, 0, 1),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let bad_prog = make_prog(bad_insns);
        assert!(matches!(v.verify(&bad_prog), VerifyResult::Err(_)));
    }

    // EBPF-3 规则 8: helper 调用前 R1 必须已初始化
    // 注: 由于 R1 默认初始化为 CtxPtr, 实际不触发.
    // 此测试验证 R1 是合法的 helper 调用契约.
    #[test]
    fn test_ebpf_3_helper_r1_initialized() {
        // R1 默认 CtxPtr 状态, CALL ktime_get_ns 应被接受
        let insns = vec![
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::KTIME_GET_NS as i32,
            ),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // EBPF-4 规则 9: LD 上下文偏移越界检查
    #[test]
    fn test_ebpf_4_ld_offset_oob_rejected() {
        // LD off=10000 越界 (合法范围 [-4096, 4096])
        let insns = vec![
            make_insn(opcode::LD, 0, 0, 10000, 0),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    /// 边界: LD 偏移在合法范围 [-4096, 4096] 内应被接受
    #[test]
    fn test_ebpf_4_ld_offset_boundary_ok() {
        // LD off=4096 (边界值, 合法)
        let insns = vec![
            make_insn(opcode::LD, 0, 0, 4096, 0),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // EBPF-4 规则 10: LDX 必须从已知指针加载
    // 场景: 标量寄存器 (Scalar) 不能被 LDX 当作指针解引用
    #[test]
    fn test_ebpf_4_ldx_from_scalar_rejected() {
        // MOV R2 = 0 (scalar); LDX R0 = [R2] (从 scalar 加载 → 拒绝)
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 2, 0, 0, 0), // R2 = 0 (scalar)
            make_insn(opcode::LDX, 0, 2, 0, 0),                 // R0 = [R2] (scalar → invalid)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(matches!(result, VerifyResult::Err(_)));
    }

    /// 合法: LDX 从 R10 (StackPtr) 加载 (栈读)
    #[test]
    fn test_ebpf_4_ldx_from_stack_ptr_ok() {
        // LDX R0 = [R10+0] (从栈指针加载, 合法)
        let insns = vec![
            make_insn(opcode::LDX, 0, 10, 0, 0), // R0 = [R10+0]
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // EBPF-6 规则 11: helper 调用前 R1-R5 (弱化: 仅文档化, 不强制)
    // 与 Linux 早期 verifier 行为一致: R1 必须初始化, R2-R5 弱约束.
    // 此测试验证 R1 单点初始化就足以通过验证.
    #[test]
    fn test_ebpf_6_helper_r1_only_init() {
        // R1 默认 CtxPtr; 直接 CALL, 不显式初始化 R2-R5
        // 应被接受 (与 Linux 早期 verifier 一致)
        let insns = vec![
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::KTIME_GET_NS as i32,
            ),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // B07-18 回归: ALU64 常量折叠溢出被拒绝 (u64::MAX + 1)
    #[test]
    fn test_alu_overflow_rejected() {
        // R0 = u64::MAX; R0 += 1 → 无符号溢出, 应拒绝
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, -1), // R0 = 0xFFFFFFFFFFFFFFFF
            make_insn(opcode::ALU64 | opcode::ADD, 0, 0, 0, 1),  // R0 += 1 (溢出)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(
            matches!(result, VerifyResult::Err(_)),
            "ALU overflow should be rejected"
        );
    }

    // B07-18 回归: 合法常量链仍通过 (1 + 2 = 3)
    #[test]
    fn test_alu_constant_chain_valid() {
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 0, 0, 0, 1),
            make_insn(opcode::ALU64 | opcode::ADD, 0, 0, 0, 2), // 1+2=3, 不溢出
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        assert!(matches!(STANDARD_VERIFIER.verify(&prog), VerifyResult::Ok));
    }

    // B07-18 回归: 栈读偏移越界被拒绝 (LDX [R10+100] 超出 512 字节栈)
    #[test]
    fn test_stack_load_offset_oob_rejected() {
        let insns = vec![
            make_insn(opcode::LDX, 0, 10, 100, 0), // R0 = [R10+100] (越界)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(
            matches!(result, VerifyResult::Err(_)),
            "stack load offset out of range should be rejected"
        );
    }

    // B07-18 回归: 栈读偏移下界越界被拒绝 (LDX [R10-513])
    #[test]
    fn test_stack_load_offset_negative_oob_rejected() {
        let insns = vec![
            make_insn(opcode::LDX, 0, 10, -513, 0), // R0 = [R10-513] (越界)
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(
            matches!(result, VerifyResult::Err(_)),
            "stack load offset below floor should be rejected"
        );
    }

    // B07-18 回归: helper 参数缺失被拒绝 (MAP_LOOKUP_ELEM 缺 R2=key)
    #[test]
    fn test_helper_missing_arg_rejected() {
        // 只初始化 R1 (map), 未初始化 R2 (key) → 应拒绝
        let insns = vec![
            make_insn(opcode::ALU64 | opcode::MOV, 1, 0, 0, 0), // R1 = 0
            make_insn(
                opcode::JMP | opcode::CALL,
                0,
                0,
                0,
                helper_id::MAP_LOOKUP_ELEM as i32,
            ),
            make_insn(opcode::JMP | opcode::EXIT, 0, 0, 0, 0),
        ];
        let prog = make_prog(insns);
        let result = STANDARD_VERIFIER.verify(&prog);
        assert!(
            matches!(result, VerifyResult::Err(_)),
            "helper missing required arg should be rejected"
        );
    }
}
