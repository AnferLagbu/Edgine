#![deny(unsafe_code)]
use crate::services::fs::nestfs::bp::NestCksumType;

pub const HV_CKSUM_FLETCHER2: usize = 1;
pub const HV_CKSUM_FLETCHER4: usize = 2;
pub const HV_CKSUM_SHA256: usize = 3;

// I-04: 引入 `Checksum` trait, 让 spa/dedup 等调用方依赖抽象而非具体类型.
// 这样单元测试可注入 mock 实现, 验证 DMU 在不真实存储上的逻辑.

// SAFETY: 该 trait 在 no_std 内核环境下使用, 方法均无内存分配 / 阻塞,
// 可在中断上下文调用. 实现方必须保证 `compute` 与 `verify` 对同一输入
// 返回稳定结果 (无内部可变状态).
pub trait Checksum: Send + Sync {
    /// 给定算法类型与数据, 计算校验和
    fn compute(&self, kind: NestCksumType, data: &[u8]) -> [u64; 4];
    /// 验证 `expected` 与 `data` 在同一算法下结果是否一致
    fn verify(&self, kind: NestCksumType, data: &[u8], expected: &[u64; 4]) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub struct NestChecksum {
    pub kind: NestCksumType,
    pub value: [u64; 4],
}

impl NestChecksum {
    pub fn new(kind: NestCksumType) -> Self {
        Self {
            kind,
            value: [0; 4],
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn compute(kind: NestCksumType, data: &[u8]) -> Self {
        let mut ck = Self::new(kind);
        match kind {
            NestCksumType::Off => {}
            NestCksumType::Fletcher2 => ck.fletcher2(data),
            NestCksumType::Fletcher4 => ck.fletcher4(data),
            NestCksumType::SHA256 => ck.sha256(data),
            NestCksumType::EdonR => ck.fletcher4(data),
        }
        ck
    }

    pub fn verify(&self, data: &[u8]) -> bool {
        let computed = Self::compute(self.kind, data);
        self.value == computed.value
    }

    fn fletcher2(&mut self, data: &[u8]) {
        let (words, _) = data.as_chunks::<8>();
        let mut a: u64 = 0;
        let mut b: u64 = 0;
        for chunk in words {
            let w = u64::from_le_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]);
            a = a.wrapping_add(w);
            b = b.wrapping_add(a);
        }
        let rem = data.len() % 8;
        if rem > 0 {
            let start = data.len() - rem;
            let mut last = [0u8; 8];
            last[..rem].copy_from_slice(&data[start..]);
            let w = u64::from_le_bytes(last);
            a = a.wrapping_add(w);
            b = b.wrapping_add(a);
        }
        self.value[0] = a;
        self.value[1] = b;
        self.value[2] = 0;
        self.value[3] = 0;
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    fn fletcher4(&mut self, data: &[u8]) {
        let (words, _) = data.as_chunks::<8>();
        let mut a: u64 = 0;
        let mut b: u64 = 0;
        let mut c: u64 = 0;
        let mut d: u64 = 0;
        for chunk in words {
            let w = u64::from_le_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]);
            a = a.wrapping_add(w);
            b = b.wrapping_add(a);
            c = c.wrapping_add(b);
            d = d.wrapping_add(c);
        }
        let rem = data.len() % 8;
        if rem > 0 {
            let start = data.len() - rem;
            let mut last = [0u8; 8];
            last[..rem].copy_from_slice(&data[start..]);
            let w = u64::from_le_bytes(last);
            a = a.wrapping_add(w);
            b = b.wrapping_add(a);
            c = c.wrapping_add(b);
            d = d.wrapping_add(c);
        }
        self.value[0] = a;
        self.value[1] = b;
        self.value[2] = c;
        self.value[3] = d;
    }

    fn sha256(&mut self, data: &[u8]) {
        let hash = crate::framework::credo::sha256::sha256(data);
        self.value[0] = u64::from_be_bytes(hash[0..8].try_into().unwrap_or_else(|_| [0u8; 8]));
        self.value[1] = u64::from_be_bytes(hash[8..16].try_into().unwrap_or_else(|_| [0u8; 8]));
        self.value[2] = u64::from_be_bytes(hash[16..24].try_into().unwrap_or_else(|_| [0u8; 8]));
        self.value[3] = u64::from_be_bytes(hash[24..32].try_into().unwrap_or_else(|_| [0u8; 8]));
    }
}

// I-04: 为 NestChecksum 实现 Checksum trait, 使其成为 trait object / 泛型可注入.
impl Checksum for NestChecksum {
    fn compute(&self, kind: NestCksumType, data: &[u8]) -> [u64; 4] {
        Self::compute(kind, data).value
    }

    fn verify(&self, kind: NestCksumType, data: &[u8], expected: &[u64; 4]) -> bool {
        let computed = Self::compute(kind, data);
        &computed.value == expected
    }
}

// DECISION-080: 校验和纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属.
// E-06 (2026-09-07): 原 host-tests/src/checksum.rs 去重载体用例合入本套件 —
// 同被测对象 (services::fs::nestfs::checksum::NestChecksum) 的双端共享用例统一收口.
// 覆盖 Fletcher2/4 全族、verify 往返/损坏检测、Off/EdonR/SHA256 变体与边界长度.
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    // ==== Fletcher2 / Fletcher4 基础 ====

    /// 相同数据在同算法下应产生相同校验和.
    #[test]
    fn test_checksum_fletcher4_basic() {
        let data = b"hello world test data for checksum verification";
        let ck_a = NestChecksum::compute(NestCksumType::Fletcher4, data);
        let ck_b = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert_eq!(
            ck_a.value, ck_b.value,
            "same data should produce same checksum"
        );
    }

    /// 不同数据应产生不同校验和.
    #[test]
    fn test_checksum_different_data() {
        let ck_a = NestChecksum::compute(NestCksumType::Fletcher4, b"hello");
        let ck_b = NestChecksum::compute(NestCksumType::Fletcher4, b"world");
        assert_ne!(ck_a.value, ck_b.value, "different data should differ");
    }

    /// 空输入的 Fletcher2 校验和应全零.
    #[test]
    fn test_checksum_fletcher2_empty() {
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, b"");
        assert!(ck.value[0] == 0 && ck.value[1] == 0, "empty fletcher2 zero");
    }

    /// Fletcher2 对相同输入应确定.
    #[test]
    fn test_checksum_fletcher2_deterministic() {
        let data = b"hello world";
        let ck1 = NestChecksum::compute(NestCksumType::Fletcher2, data);
        let ck2 = NestChecksum::compute(NestCksumType::Fletcher2, data);
        assert_eq!(ck1.value, ck2.value, "fletcher2 deterministic");
    }

    /// 空输入的 Fletcher4 校验和应全零.
    #[test]
    fn test_checksum_fletcher4_empty() {
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, b"");
        assert!(
            ck.value[0] == 0 && ck.value[1] == 0 && ck.value[2] == 0 && ck.value[3] == 0,
            "empty fletcher4 zero"
        );
    }

    /// Fletcher4 对相同输入应确定.
    #[test]
    fn test_checksum_fletcher4_deterministic() {
        let data = b"test data for fletcher4";
        let ck1 = NestChecksum::compute(NestCksumType::Fletcher4, data);
        let ck2 = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert_eq!(ck1.value, ck2.value, "fletcher4 deterministic");
    }

    // ==== verify 往返 / 损坏检测 ====

    /// Fletcher2 校验和可验证原数据.
    #[test]
    fn test_checksum_verify_roundtrip_fletcher2() {
        let data = b"some test data for verification";
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, data);
        assert!(ck.verify(data), "fletcher2 verifies");
    }

    /// Fletcher4 校验和可验证原数据.
    #[test]
    fn test_checksum_verify_roundtrip_fletcher4() {
        let data = b"some test data for verification";
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert!(ck.verify(data), "fletcher4 verifies");
    }

    /// 数据被篡改后校验应失败.
    #[test]
    fn test_checksum_verify_detects_corruption() {
        let data = b"original data";
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, data);
        let corrupted = b"corrupted data";
        assert!(!ck.verify(corrupted), "corruption detected");
    }

    /// Off 算法恒返回全零校验和.
    #[test]
    fn test_checksum_off_always_zero() {
        let ck = NestChecksum::compute(NestCksumType::Off, b"any data");
        assert_eq!(ck.value, [0u64; 4], "Off checksum zero");
    }

    /// EdonR 当前复用 Fletcher4 实现, 结果应一致.
    #[test]
    fn test_checksum_edonr_uses_fletcher4() {
        let data = b"test data";
        let ck_edonr = NestChecksum::compute(NestCksumType::EdonR, data);
        let ck_f4 = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert_eq!(ck_edonr.value, ck_f4.value, "EdonR == Fletcher4");
    }

    // ==== 边界长度 ====

    /// Fletcher2 单字节输入应产生非零校验和.
    #[test]
    fn test_checksum_fletcher2_single_byte() {
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, b"A");
        assert_ne!(ck.value[0], 0, "single byte fletcher2 non-zero");
    }

    /// Fletcher4 单字节输入应产生非零校验和.
    #[test]
    fn test_checksum_fletcher4_single_byte() {
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, b"A");
        assert_ne!(ck.value[0], 0, "single byte fletcher4 non-zero");
    }

    /// Fletcher2 奇数长度输入可往返验证.
    #[test]
    fn test_checksum_fletcher2_odd_length() {
        let data = b"hello";
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, data);
        assert!(ck.verify(data), "odd-length fletcher2 verifies");
    }

    /// Fletcher4 奇数长度输入可往返验证.
    #[test]
    fn test_checksum_fletcher4_odd_length() {
        let data = b"odd data length test";
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert!(ck.verify(data), "odd-length fletcher4 verifies");
    }

    /// Fletcher2 恰好 8 字节输入可往返验证.
    #[test]
    fn test_checksum_fletcher2_exact_8_bytes() {
        let data = b"12345678";
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, data);
        assert!(ck.verify(data), "8-byte fletcher2 verifies");
    }

    /// Fletcher4 恰好 8 字节输入可往返验证.
    #[test]
    fn test_checksum_fletcher4_exact_8_bytes() {
        let data = b"abcdefgh";
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, data);
        assert!(ck.verify(data), "8-byte fletcher4 verifies");
    }

    /// Fletcher2 大块数据 (4 KiB) 可往返验证.
    #[test]
    fn test_checksum_fletcher2_large_data() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        let ck = NestChecksum::compute(NestCksumType::Fletcher2, &data);
        assert!(ck.verify(&data), "large fletcher2 verifies");
    }

    /// Fletcher4 大块数据 (8 KiB) 可往返验证.
    #[test]
    fn test_checksum_fletcher4_large_data() {
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 256) as u8).collect();
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, &data);
        assert!(ck.verify(&data), "large fletcher4 verifies");
    }

    /// Fletcher2 不同长度输入应产生不同校验和.
    #[test]
    fn test_checksum_fletcher2_different_lengths() {
        let ck1 = NestChecksum::compute(NestCksumType::Fletcher2, b"hello");
        let ck2 = NestChecksum::compute(NestCksumType::Fletcher2, b"hello world");
        assert_ne!(ck1.value, ck2.value, "different lengths differ");
    }

    /// Fletcher4 不同长度输入应产生不同校验和.
    #[test]
    fn test_checksum_fletcher4_different_lengths() {
        let ck1 = NestChecksum::compute(NestCksumType::Fletcher4, b"hello");
        let ck2 = NestChecksum::compute(NestCksumType::Fletcher4, b"hello world");
        assert_ne!(ck1.value, ck2.value, "different lengths differ");
    }

    /// 单比特翻转应被检出.
    #[test]
    fn test_checksum_verify_single_bit_flip() {
        let data = b"some test data for verification";
        let ck = NestChecksum::compute(NestCksumType::Fletcher4, data);
        let mut corrupted: Vec<u8> = Vec::new();
        corrupted.extend_from_slice(data);
        corrupted[5] ^= 0x01;
        assert!(!ck.verify(&corrupted), "single bit flip detected");
    }

    /// Off 算法对任意数据均验证通过.
    #[test]
    fn test_checksum_off_verify_always_true() {
        let ck = NestChecksum::compute(NestCksumType::Off, b"any data");
        assert!(ck.verify(b"different data"), "Off verifies any data");
    }

    // ==== SHA256 ====

    /// SHA256 短输入应确定.
    #[test]
    fn test_checksum_sha256_short_data() {
        let data = b"ab";
        let ck1 = NestChecksum::compute(NestCksumType::SHA256, data);
        let ck2 = NestChecksum::compute(NestCksumType::SHA256, data);
        assert_eq!(ck1.value, ck2.value, "SHA256 short deterministic");
    }

    /// SHA256('abc') 应匹配 FIPS 180-4 标准向量.
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: FIPS 180-4 SHA-256('abc') 测试向量按 u64 四字面值书写, 有明确标准出处"
    )]
    #[test]
    fn test_checksum_sha256_known_vector() {
        let ck = NestChecksum::compute(NestCksumType::SHA256, b"abc");
        let expected: [u64; 4] = [
            0xba7816bf8f01cfea,
            0x414140de5dae2223,
            0xb00361a396177a9c,
            0xb410ff61f20015ad,
        ];
        assert_eq!(ck.value, expected, "SHA256('abc') FIPS vector");
    }

    /// SHA256('') 应匹配 FIPS 180-4 标准向量.
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: FIPS 180-4 SHA-256('') 测试向量按 u64 四字面值书写, 有明确标准出处"
    )]
    #[test]
    fn test_checksum_sha256_empty() {
        let ck = NestChecksum::compute(NestCksumType::SHA256, b"");
        let expected: [u64; 4] = [
            0xe3b0c44298fc1c14,
            0x9afbf4c8996fb924,
            0x27ae41e4649b934c,
            0xa495991b7852b855,
        ];
        assert_eq!(ck.value, expected, "SHA256('') FIPS vector");
    }

    /// SHA256 长输入应确定.
    #[test]
    fn test_checksum_sha256_long_data() {
        let data =
            b"sha256 via checksum module - this is a longer string that spans multiple blocks";
        let ck1 = NestChecksum::compute(NestCksumType::SHA256, data);
        let ck2 = NestChecksum::compute(NestCksumType::SHA256, data);
        assert_eq!(ck1.value, ck2.value, "SHA256 long deterministic");
    }
}
