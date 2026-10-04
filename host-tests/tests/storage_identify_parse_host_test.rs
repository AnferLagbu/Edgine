//! NVMe Identify 解析 host 集成测试 (§6.4 storage 专项 0 号子步)
//!
//! §6.4 直接方案 B: `nvme_read_identify_*` 解析 helper 从 privileged 迁 functions
//! (纯逻辑, 输入为 DMA 缓冲区字节切片). 本测试直接引用内核 functions 真实
//! `parse_identify_controller` / `parse_identify_namespace` 纯函数 (B08-12 路线 C),
//! 验证 offset 提取与边界语义, 无测试/生产分叉.
//!
//! 覆盖:
//! 1. Identify Controller: nn (offset 516, LE u32) + mn (offset 24, 40 字节空终止)
//! 2. Identify Namespace: nsze (offset 0, LE u64) + flbas (offset 26) +
//!    lbaf_data (offset 128 + lbaf_idx*4, LE u32)
//! 3. 短缓冲区: 长度不足返回 None
//! 4. lbaf_idx >= 16: lbaf_data = 0

use edgine::kernel::functions::driver::storage::nvme::{
    parse_identify_controller, parse_identify_namespace,
};

// ============================================================================
// CC 寄存器位域回归 (QEMU 存储冒烟 MSIX-04: IOSQES 6<<24 笔误致 create_cq
// 被 QEMU 以 MAX_QSIZE_EXCEEDED 拒绝 — IOSQES 实际在 bit 16, 非 bit 24)
// ============================================================================

#[test]
fn cc_iosqes_iocqes_field_positions_match_nvme_spec() {
    use edgine::kernel::functions::driver::storage::nvme::{
        CC_IOCQES_MASK, CC_IOCQES_VAL, CC_IOSQES_MASK, CC_IOSQES_VAL,
    };

    // IOSQES @ CC bits 19:16, 64 字节 SQ 条目 → 6
    assert_eq!(
        (CC_IOSQES_VAL & CC_IOSQES_MASK) >> 16,
        6,
        "CC.IOSQES 编码应落在 bit 16-19 且值 = log2(64) = 6"
    );
    // IOCQES @ CC bits 23:20, 16 字节 CQ 条目 → 4
    assert_eq!(
        (CC_IOCQES_VAL & CC_IOCQES_MASK) >> 20,
        4,
        "CC.IOCQES 编码应落在 bit 20-23 且值 = log2(16) = 4"
    );
    // IOSQES 不得越界污染 bit 20+ (bit 24+ 为保留域)
    assert_eq!(
        CC_IOSQES_VAL & !(CC_IOSQES_MASK | CC_IOCQES_MASK),
        0,
        "IOSQES 编码不得污染 IOCQES 及保留域"
    );
}

#[test]
fn cc_composed_value_decodes_to_required_entry_sizes() {
    use edgine::kernel::functions::driver::storage::nvme::{
        CC_AMS_RR, CC_CSS_NVM, CC_EN, CC_IOCQES_MASK, CC_IOCQES_VAL, CC_IOSQES_MASK, CC_IOSQES_VAL,
        CC_MPS_SHIFT,
    };

    // 与 functions nvme init_controller 的 CC 组成保持同构:
    // create_cq/create_sq 依赖 QEMU 侧检查 CC.IOSQES == 6 且 CC.IOCQES == 4
    let cc =
        CC_EN | CC_CSS_NVM | (0u32 << CC_MPS_SHIFT) | CC_AMS_RR | CC_IOCQES_VAL | CC_IOSQES_VAL;

    assert_eq!(
        (cc & CC_IOSQES_MASK) >> 16,
        6,
        "CC.IOSQES 必须为 6 (64B SQE)"
    );
    assert_eq!(
        (cc & CC_IOCQES_MASK) >> 20,
        4,
        "CC.IOCQES 必须为 4 (16B CQE)"
    );
}

#[test]
fn identify_controller_parse_full() {
    let mut data = [0u8; 520];
    let model = b"Edgine NVMe";
    data[24..24 + model.len()].copy_from_slice(model);
    data[516..520].copy_from_slice(&3u32.to_le_bytes());

    let (nn, mn) = parse_identify_controller(&data).expect("长度足够应解析成功");
    assert_eq!(nn, 3);
    assert_eq!(&mn[..model.len()], model);
    assert_eq!(mn[model.len()], 0, "mn 应为空终止");
}

#[test]
fn identify_controller_short_buffer() {
    let data = [0u8; 100]; // 不足 520
    assert!(parse_identify_controller(&data).is_none());
}

#[test]
fn identify_controller_empty() {
    let data = [0u8; 0];
    assert!(parse_identify_controller(&data).is_none());
}

#[test]
fn identify_namespace_parse_full() {
    let mut data = [0u8; 192];
    data[0..8].copy_from_slice(&1_048_576u64.to_le_bytes());
    data[26] = 2; // lbaf_idx = 2
    data[136..140].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // lbaf[2]: LBADS=9 → 512B

    let (nsze, flbas, lbaf_data) = parse_identify_namespace(&data).expect("长度足够应解析成功");
    assert_eq!(nsze, 1_048_576);
    assert_eq!(flbas, 2);
    assert_eq!(lbaf_data, 0x0001_0000);
}

#[test]
fn identify_namespace_lbaf_index_zero() {
    let mut data = [0u8; 192];
    data[0..8].copy_from_slice(&512u64.to_le_bytes());
    data[26] = 0x1F; // flbas = 0x1F → lbaf_idx = 15
    data[128 + 15 * 4..128 + 15 * 4 + 4].copy_from_slice(&0x0001_0000u32.to_le_bytes());

    let (nsze, flbas, lbaf_data) = parse_identify_namespace(&data).unwrap();
    assert_eq!(nsze, 512);
    assert_eq!(flbas, 0x1F);
    assert_eq!(lbaf_data, 0x0001_0000);
}

#[test]
fn identify_namespace_lbaf_idx_beyond_table() {
    // flbas & 0xF = 0, lbaf[0] 未设置 → 0
    let mut data = [0u8; 192];
    data[26] = 0x10; // 高半字节置位, 低半字节 0 → lbaf_idx = 0
    let (_, flbas, lbaf_data) = parse_identify_namespace(&data).unwrap();
    assert_eq!(flbas, 0x10);
    assert_eq!(lbaf_data, 0);
}

#[test]
fn identify_namespace_short_buffer() {
    let data = [0u8; 100]; // 不足 192
    assert!(parse_identify_namespace(&data).is_none());
}
