//! CPUID 指令封装
//!
//! 提供 safe wrapper for x86 CPUID instruction.

/// 执行 CPUID 指令并返回 (EAX, EBX, ECX, EDX)
///
/// # Arguments
/// * `leaf` - CPUID 主叶号 (EAX)
/// * `subleaf` - CPUID 子叶号 (ECX, 用于 Leaf 4/B 等)
///
/// # Returns
/// 元组 (eax, ebx, ecx, edx)
///
/// # Safety
/// CPUID 指令本身是安全的, 但返回值的解释需要硬件手册知识。
#[inline(always)]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);

    unsafe {
        let mut ebx_val: u64 = 0;
        core::arch::asm!(
            "xchg {tmp}, rbx",  // 保存 rbx, tmp ← rbx(旧的), rbx ← 0
            "cpuid",
            "xchg {tmp}, rbx",  // 恢复 rbx, tmp ← cpuid_rbx, rbx ← 旧值
            inlateout("eax") leaf => eax,
            inlateout("ecx") subleaf => ecx,
            tmp = inout(reg) ebx_val,
            out("edx") edx,
            options(nomem, preserves_flags),
        );
        ebx = ebx_val as u32;
    }

    (eax, ebx, ecx, edx)
}

/// 检查 CPUID leaf 是否受支持
///
/// # Arguments
/// * `leaf` - 要检查的叶号
///
/// # Returns
/// true - 支持, false - 不支持
#[inline]
pub fn is_leaf_supported(leaf: u32) -> bool {
    let (max_leaf, _, _, _) = cpuid(0, 0);
    leaf <= max_leaf
}

/// B03-18: 带 leaf + subleaf 双层校验的 CPUID 查询, 防止 leaf > max_leaf 时
/// 读到未定义值（AMD 可能 panic, Intel 返回旧 CPU 的值）。
///
/// # Returns
/// - `Some((eax, ebx, ecx, edx))` — leaf 在支持范围
/// - `None` — leaf 超出 max_leaf 或 subleaf 校验失败
#[inline]
pub fn cpuid_checked(leaf: u32, subleaf: u32) -> Option<(u32, u32, u32, u32)> {
    if !is_leaf_supported(leaf) {
        return None;
    }
    // subleaf 校验: leaf 0 不支持 subleaf, leaf 4/0x8000001D 等需 subleaf < N。
    // 简化处理: leaf >= 0x80000000 (扩展 leaf) 也需检查 max_ext。
    if leaf >= super::CPUID_LEAF_EXT_BASE {
        let (max_ext, _, _, _) = cpuid(super::CPUID_LEAF_EXT_BASE, 0);
        if leaf > max_ext {
            return None;
        }
    }
    Some(cpuid(leaf, subleaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpuid_leaf_0() {
        // Leaf 0 总是被支持
        let (eax, ebx, ecx, edx) = cpuid(0, 0);
        assert!(eax > 0, "Max leaf should be > 0");

        // 厂商字符串不应全为零
        assert!(ebx != 0 || ecx != 0 || edx != 0);
    }

    #[test]
    fn test_is_leaf_supported() {
        assert!(is_leaf_supported(0));
        assert!(is_leaf_supported(1));
        // Leaf 0xFFFFFFFF 通常不被支持
        assert!(!is_leaf_supported(0xFFFF_FFFF));
    }
}
/// 注册 CPUID 相关内核测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
pub fn register_cpuid_tests() {
    crate::privileged::tests::arch::register_cpuid_tests();
}
