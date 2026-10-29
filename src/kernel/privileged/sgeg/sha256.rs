#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯算法实现。
//! SHA-256 哈希实现 — privileged 层权威定义 (安全原语)
//!
//! ## 归属记录
//!
//! 纯算法实现 (SHA-256 哈希), 0 unsafe.
//! T6-8 曾于 2026-06-16 提取到 functions; 第二十五批 (DECISION-K 项 5 sgeg
//! 判据: 安全原语归 privileged — privileged/sgeg/secure_boot 直接消费) 反转
//! 归位本文件. functions/sgeg/sha256.rs 为 re-export 壳.

use super::types::PWM_DIGEST_LEN;

/// SHA-256 轮常量 (前 64 个素数立方根小数部分的前 32 位)
const K: [u32; 64] = [
    0x428A_2F98,
    0x7137_4491,
    0xB5C0_FBCF,
    0xE9B5_DBA5,
    0x3956_C25B,
    0x59F1_11F1,
    0x923F_82A4,
    0xAB1C_5ED5,
    0xD807_AA98,
    0x1283_5B01,
    0x2431_85BE,
    0x550C_7DC3,
    0x72BE_5D74,
    0x80DE_B1FE,
    0x9BDC_06A7,
    0xC19B_F174,
    0xE49B_69C1,
    0xEFBE_4786,
    0x0FC1_9DC6,
    0x240C_A1CC,
    0x2DE9_2C6F,
    0x4A74_84AA,
    0x5CB0_A9DC,
    0x76F9_88DA,
    0x983E_5152,
    0xA831_C66D,
    0xB003_27C8,
    0xBF59_7FC7,
    0xC6E0_0BF3,
    0xD5A7_9147,
    0x06CA_6351,
    0x1429_2967,
    0x27B7_0A85,
    0x2E1B_2138,
    0x4D2C_6DFC,
    0x5338_0D13,
    0x650A_7354,
    0x766A_0ABB,
    0x81C2_C92E,
    0x9272_2C85,
    0xA2BF_E8A1,
    0xA81A_664B,
    0xC24B_8B70,
    0xC76C_51A3,
    0xD192_E819,
    0xD699_0624,
    0xF40E_3585,
    0x106A_A070,
    0x19A4_C116,
    0x1E37_6C08,
    0x2748_774C,
    0x34B0_BCB5,
    0x391C_0CB3,
    0x4ED8_AA4A,
    0x5B9C_CA4F,
    0x682E_6FF3,
    0x748F_82EE,
    0x78A5_636F,
    0x84C8_7814,
    0x8CC7_0208,
    0x90BE_FFFA,
    0xA450_6CEB,
    0xBEF9_A3F7,
    0xC671_78F2,
];

/// 初始哈希值 (前 8 个素数平方根小数部分的前 32 位)
const INITIAL_STATE: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

/// 右旋运算
#[inline]
fn rotr(x: u32, n: u32) -> u32 {
    x.rotate_right(n)
}

/// 处理单个 512 位块
fn sha256_transform(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0u32; 64];

    // 准备消息调度
    for idx in 0..16 {
        schedule[idx] = (u32::from(block[idx * 4]) << 24)
            | (u32::from(block[idx * 4 + 1]) << 16)
            | (u32::from(block[idx * 4 + 2]) << 8)
            | u32::from(block[idx * 4 + 3]);
    }

    for idx in 16..64 {
        let s0 =
            rotr(schedule[idx - 15], 7) ^ rotr(schedule[idx - 15], 18) ^ (schedule[idx - 15] >> 3);
        let s1 =
            rotr(schedule[idx - 2], 17) ^ rotr(schedule[idx - 2], 19) ^ (schedule[idx - 2] >> 10);
        schedule[idx] = schedule[idx - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[idx - 7])
            .wrapping_add(s1);
    }

    // 初始化工作变量
    let mut va = state[0];
    let mut vb = state[1];
    let mut vc = state[2];
    let mut vd = state[3];
    let mut ve = state[4];
    let mut vf = state[5];
    let mut vg = state[6];
    let mut vh = state[7];

    // 压缩函数
    for idx in 0..64 {
        let s1 = rotr(ve, 6) ^ rotr(ve, 11) ^ rotr(ve, 25);
        let ch = (ve & vf) ^ ((!ve) & vg);
        let t1 = vh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[idx])
            .wrapping_add(schedule[idx]);

        let s0 = rotr(va, 2) ^ rotr(va, 13) ^ rotr(va, 22);
        let maj = (va & vb) ^ (va & vc) ^ (vb & vc);
        let t2 = s0.wrapping_add(maj);

        vh = vg;
        vg = vf;
        vf = ve;
        ve = vd.wrapping_add(t1);
        vd = vc;
        vc = vb;
        vb = va;
        va = t1.wrapping_add(t2);
    }

    // 累加压缩结果到当前哈希值
    state[0] = state[0].wrapping_add(va);
    state[1] = state[1].wrapping_add(vb);
    state[2] = state[2].wrapping_add(vc);
    state[3] = state[3].wrapping_add(vd);
    state[4] = state[4].wrapping_add(ve);
    state[5] = state[5].wrapping_add(vf);
    state[6] = state[6].wrapping_add(vg);
    state[7] = state[7].wrapping_add(vh);
}

/// 计算数据的 SHA-256 哈希
///
/// # 参数
/// * `data` - 待哈希的输入数据
///
/// # 返回
/// 32 字节哈希数组
///
/// B07-07: 返回类型由 `[u8; PWM_HASH_LEN]`(48) 修正为标准 32 字节输出.
/// 原实现返回 48 字节数组但仅填充前 32 字节, 尾部 16 字节恒 0, 属误导性契约.
pub fn sha256(data: &[u8]) -> [u8; PWM_DIGEST_LEN] {
    let mut state = INITIAL_STATE;
    let len = data.len();

    // 处理完整块
    let mut i = 0;
    while i + 64 <= len {
        let mut block = [0u8; 64];
        block.copy_from_slice(&data[i..i + 64]);
        sha256_transform(&mut state, &block);
        i += 64;
    }

    // 处理剩余字节与填充
    let remaining = len - i;
    let mut block = [0u8; 64];

    // 复制剩余数据
    if remaining > 0 {
        block[..remaining].copy_from_slice(&data[i..i + remaining]);
    }

    // 追加填充位
    block[remaining] = 0x80;

    // 若剩余数据已超长度字段空间, 需先处理本块
    if remaining >= 56 {
        sha256_transform(&mut state, &block);
        block = [0u8; 64]; // 新块用于存放长度
    }

    // 追加原始消息长度 (比特, 大端序)
    let bit_len = (len as u64) * 8;
    block[56..64].copy_from_slice(&bit_len.to_be_bytes());

    // 最终块
    sha256_transform(&mut state, &block);

    // 生成最终哈希值 (大端序)
    let mut hash = [0u8; PWM_DIGEST_LEN];
    for j in 0..8 {
        hash[j * 4] = (state[j] >> 24) as u8;
        hash[j * 4 + 1] = crate::privileged::glue::low_u8_from_u32(state[j] >> 16);
        hash[j * 4 + 2] = crate::privileged::glue::low_u8_from_u32(state[j] >> 8);
        hash[j * 4 + 3] = crate::privileged::glue::low_u8_from_u32(state[j]);
    }

    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_empty() {
        // 空字符串的已知哈希值
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];

        assert_eq!(sha256(b""), expected);
    }

    #[test]
    fn test_sha256_abc() {
        // Known hash of "abc"
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];

        assert_eq!(sha256(b"abc"), expected);
    }

    // UT-07 (2026-09-26): 注册侧 pwm::sha256 组全量迁入 — privileged/tests/sys.rs 的 15 例
    // (长消息/确定性/不同输入/单字节/边界长度 55、56、63、64/多块/全零/全一/雪崩/大输入)
    // 与 privileged/tests/test_pwm.rs::known_vectors 的字节级判据; 源侧原仅空串/abc 两例.
    // 逐例归并等价判据, 断言数净增不净减.

    #[test]
    fn test_sha256_long_message() {
        let expected: [u8; 32] = [
            0x24, 0x8d, 0x6a, 0x61, 0xd2, 0x06, 0x38, 0xb8, 0xe5, 0xc0, 0x26, 0x93, 0x0c, 0x3e,
            0x60, 0x39, 0xa3, 0x3c, 0xe4, 0x59, 0x64, 0xff, 0x21, 0x67, 0xf6, 0xec, 0xed, 0xd4,
            0x19, 0xdb, 0x06, 0xc1,
        ];
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            expected
        );
    }

    #[test]
    fn test_sha256_deterministic() {
        assert_eq!(sha256(b"hello world"), sha256(b"hello world"));
    }

    #[test]
    fn test_sha256_different_inputs() {
        assert_ne!(sha256(b"hello"), sha256(b"world"));
    }

    #[test]
    fn test_sha256_single_byte() {
        assert_ne!(sha256(b"a"), [0u8; 32]);
    }

    #[test]
    fn test_sha256_block_boundaries() {
        // 55/56/63/64 字节: 单块尾补齐与跨块边界各一 (注册侧 boundary_55/56/63/64 四例)
        for len in [55usize, 56, 63, 64] {
            let data: alloc::vec::Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            assert_ne!(sha256(&data), [0u8; 32], "len {len} 非零");
        }
    }

    #[test]
    fn test_sha256_multi_block_deterministic() {
        let data: alloc::vec::Vec<u8> = (0..200u32).map(|i| (i % 256) as u8).collect();
        assert_eq!(sha256(&data), sha256(&data));
    }

    #[test]
    fn test_sha256_all_zeros_and_ones() {
        assert_ne!(sha256(&[0u8; 64]), [0u8; 32]);
        assert_ne!(sha256(&[0xFFu8; 64]), [0u8; 32]);
    }

    #[test]
    fn test_sha256_avalanche_effect() {
        let h1 = sha256(b"hello");
        let h2 = sha256(b"hellp");
        let diff_bits: u32 = h1
            .iter()
            .zip(h2.iter())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();
        assert!(diff_bits > 32);
    }

    #[test]
    fn test_sha256_large_input() {
        let data: alloc::vec::Vec<u8> = (0..10_000u32).map(|i| (i % 256) as u8).collect();
        assert_ne!(sha256(&data), [0u8; 32]);
    }

    #[test]
    fn test_sha256_known_vector_prefix_bytes() {
        // 注册侧 privileged/tests/test_pwm.rs::test_sha256_vectors 的字节级判据迁入
        let h = sha256(b"");
        assert_eq!(h[0], 0xe3);
        assert_eq!(h[1], 0xb0);
        assert_eq!(h[2], 0xc4);

        let h2 = sha256(b"abc");
        assert_eq!(h2[0], 0xba);
        assert_eq!(h2[1], 0x78);
    }
}
