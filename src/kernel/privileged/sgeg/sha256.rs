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
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// 初始哈希值 (前 8 个素数平方根小数部分的前 32 位)
const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 右旋运算
#[inline(always)]
fn rotr(x: u32, n: u32) -> u32 {
    x.rotate_right(n)
}

#[expect(
    clippy::many_single_char_names,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 处理单个 512 位块
fn sha256_transform(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];

    // 准备消息调度
    for i in 0..16 {
        w[i] = (u32::from(block[i * 4]) << 24)
            | (u32::from(block[i * 4 + 1]) << 16)
            | (u32::from(block[i * 4 + 2]) << 8)
            | u32::from(block[i * 4 + 3]);
    }

    for i in 16..64 {
        let s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        let s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    // 初始化工作变量
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    // 压缩函数
    for i in 0..64 {
        let s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);

        let s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    // 累加压缩结果到当前哈希值
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
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
        hash[j * 4 + 1] = (state[j] >> 16) as u8;
        hash[j * 4 + 2] = (state[j] >> 8) as u8;
        hash[j * 4 + 3] = state[j] as u8;
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
