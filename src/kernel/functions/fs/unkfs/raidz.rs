#![deny(unsafe_code)]
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use alloc::vec;
use alloc::vec::Vec;

pub const HV_RAIDZ_MIN_COLS: usize = 2;
pub const HV_RAIDZ_MAX_COLS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsRaidzLevel {
    Single = 0,
    RaidZ1 = 1,
    RaidZ2 = 2,
    RaidZ3 = 3,
    Mirror = 4,
}

impl UnkfsRaidzLevel {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn parity_cols(&self) -> usize {
        match self {
            Self::Single => 0,
            Self::RaidZ1 => 1,
            Self::RaidZ2 => 2,
            Self::RaidZ3 => 3,
            Self::Mirror => 0,
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn max_failures(&self) -> usize {
        match self {
            Self::Single => 0,
            Self::RaidZ1 => 1,
            Self::RaidZ2 => 2,
            Self::RaidZ3 => 3,
            Self::Mirror => 1,
        }
    }
}

pub struct UnkfsRaidzMap {
    pub level: UnkfsRaidzLevel,
    pub ncols: usize,
    pub nparity: usize,
    pub ashift: u8,
    pub cols: Vec<UnkfsRaidzCol>,
}

pub struct UnkfsRaidzCol {
    pub col_id: usize,
    pub devidx: usize,
    pub offset: u64,
    pub size: usize,
    pub data: Vec<u8>,
    pub is_parity: bool,
}

const GF_EXP: [u8; 512] = [
    0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1d, 0x3a, 0x74, 0xe8, 0xcd, 0x87, 0x13, 0x26,
    0x4c, 0x98, 0x2d, 0x5a, 0xb4, 0x75, 0xea, 0xc9, 0x8f, 0x03, 0x06, 0x0c, 0x18, 0x30, 0x60, 0xc0,
    0x9d, 0x27, 0x4e, 0x9c, 0x25, 0x4a, 0x94, 0x35, 0x6a, 0xd4, 0xb5, 0x77, 0xee, 0xc1, 0x9f, 0x23,
    0x46, 0x8c, 0x05, 0x0a, 0x14, 0x28, 0x50, 0xa0, 0x5d, 0xba, 0x69, 0xd2, 0xb9, 0x6f, 0xde, 0xa1,
    0x5f, 0xbe, 0x61, 0xc2, 0x99, 0x2f, 0x5e, 0xbc, 0x65, 0xca, 0x89, 0x0f, 0x1e, 0x3c, 0x78, 0xf0,
    0xfd, 0xe7, 0xd3, 0xbb, 0x6b, 0xd6, 0xb1, 0x7f, 0xfe, 0xe1, 0xdf, 0xa3, 0x5b, 0xb6, 0x71, 0xe2,
    0xd9, 0xaf, 0x43, 0x86, 0x11, 0x22, 0x44, 0x88, 0x0d, 0x1a, 0x34, 0x68, 0xd0, 0xbd, 0x67, 0xce,
    0x81, 0x1f, 0x3e, 0x7c, 0xf8, 0xed, 0xc7, 0x93, 0x3b, 0x76, 0xec, 0xc5, 0x97, 0x33, 0x66, 0xcc,
    0x85, 0x17, 0x2e, 0x5c, 0xb8, 0x6d, 0xda, 0xa9, 0x4f, 0x9e, 0x21, 0x42, 0x84, 0x15, 0x2a, 0x54,
    0xa8, 0x4d, 0x9a, 0x29, 0x52, 0xa4, 0x55, 0xaa, 0x49, 0x92, 0x39, 0x72, 0xe4, 0xd5, 0xb7, 0x73,
    0xe6, 0xd1, 0xbf, 0x63, 0xc6, 0x91, 0x3f, 0x7e, 0xfc, 0xe5, 0xd7, 0xb3, 0x7b, 0xf6, 0xf1, 0xff,
    0xe3, 0xdb, 0xab, 0x4b, 0x96, 0x31, 0x62, 0xc4, 0x95, 0x37, 0x6e, 0xdc, 0xa5, 0x57, 0xae, 0x41,
    0x82, 0x19, 0x32, 0x64, 0xc8, 0x8d, 0x07, 0x0e, 0x1c, 0x38, 0x70, 0xe0, 0xdd, 0xa7, 0x53, 0xa6,
    0x51, 0xa2, 0x59, 0xb2, 0x79, 0xf2, 0xf9, 0xef, 0xc3, 0x9b, 0x2b, 0x56, 0xac, 0x45, 0x8a, 0x09,
    0x12, 0x24, 0x48, 0x90, 0x3d, 0x7a, 0xf4, 0xf5, 0xf7, 0xf3, 0xfb, 0xeb, 0xcb, 0x8b, 0x0b, 0x16,
    0x2c, 0x58, 0xb0, 0x7d, 0xfa, 0xe9, 0xcf, 0x83, 0x1b, 0x36, 0x6c, 0xd8, 0xad, 0x47, 0x8e, 0x01,
    0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1d, 0x3a, 0x74, 0xe8, 0xcd, 0x87, 0x13, 0x26, 0x4c,
    0x98, 0x2d, 0x5a, 0xb4, 0x75, 0xea, 0xc9, 0x8f, 0x03, 0x06, 0x0c, 0x18, 0x30, 0x60, 0xc0, 0x9d,
    0x27, 0x4e, 0x9c, 0x25, 0x4a, 0x94, 0x35, 0x6a, 0xd4, 0xb5, 0x77, 0xee, 0xc1, 0x9f, 0x23, 0x46,
    0x8c, 0x05, 0x0a, 0x14, 0x28, 0x50, 0xa0, 0x5d, 0xba, 0x69, 0xd2, 0xb9, 0x6f, 0xde, 0xa1, 0x5f,
    0xbe, 0x61, 0xc2, 0x99, 0x2f, 0x5e, 0xbc, 0x65, 0xca, 0x89, 0x0f, 0x1e, 0x3c, 0x78, 0xf0, 0xfd,
    0xe7, 0xd3, 0xbb, 0x6b, 0xd6, 0xb1, 0x7f, 0xfe, 0xe1, 0xdf, 0xa3, 0x5b, 0xb6, 0x71, 0xe2, 0xd9,
    0xaf, 0x43, 0x86, 0x11, 0x22, 0x44, 0x88, 0x0d, 0x1a, 0x34, 0x68, 0xd0, 0xbd, 0x67, 0xce, 0x81,
    0x1f, 0x3e, 0x7c, 0xf8, 0xed, 0xc7, 0x93, 0x3b, 0x76, 0xec, 0xc5, 0x97, 0x33, 0x66, 0xcc, 0x85,
    0x17, 0x2e, 0x5c, 0xb8, 0x6d, 0xda, 0xa9, 0x4f, 0x9e, 0x21, 0x42, 0x84, 0x15, 0x2a, 0x54, 0xa8,
    0x4d, 0x9a, 0x29, 0x52, 0xa4, 0x55, 0xaa, 0x49, 0x92, 0x39, 0x72, 0xe4, 0xd5, 0xb7, 0x73, 0xe6,
    0xd1, 0xbf, 0x63, 0xc6, 0x91, 0x3f, 0x7e, 0xfc, 0xe5, 0xd7, 0xb3, 0x7b, 0xf6, 0xf1, 0xff, 0xe3,
    0xdb, 0xab, 0x4b, 0x96, 0x31, 0x62, 0xc4, 0x95, 0x37, 0x6e, 0xdc, 0xa5, 0x57, 0xae, 0x41, 0x82,
    0x19, 0x32, 0x64, 0xc8, 0x8d, 0x07, 0x0e, 0x1c, 0x38, 0x70, 0xe0, 0xdd, 0xa7, 0x53, 0xa6, 0x51,
    0xa2, 0x59, 0xb2, 0x79, 0xf2, 0xf9, 0xef, 0xc3, 0x9b, 0x2b, 0x56, 0xac, 0x45, 0x8a, 0x09, 0x12,
    0x24, 0x48, 0x90, 0x3d, 0x7a, 0xf4, 0xf5, 0xf7, 0xf3, 0xfb, 0xeb, 0xcb, 0x8b, 0x0b, 0x16, 0x2c,
    0x58, 0xb0, 0x7d, 0xfa, 0xe9, 0xcf, 0x83, 0x1b, 0x36, 0x6c, 0xd8, 0xad, 0x47, 0x8e, 0x01, 0x02,
];

const GF_LOG: [u8; 256] = [
    0x00, 0x00, 0x01, 0x19, 0x02, 0x32, 0x1a, 0xc6, 0x03, 0xdf, 0x33, 0xee, 0x1b, 0x68, 0xc7, 0x4b,
    0x04, 0x64, 0xe0, 0x0e, 0x34, 0x8d, 0xef, 0x81, 0x1c, 0xc1, 0x69, 0xf8, 0xc8, 0x08, 0x4c, 0x71,
    0x05, 0x8a, 0x65, 0x2f, 0xe1, 0x24, 0x0f, 0x21, 0x35, 0x93, 0x8e, 0xda, 0xf0, 0x12, 0x82, 0x45,
    0x1d, 0xb5, 0xc2, 0x7d, 0x6a, 0x27, 0xf9, 0xb9, 0xc9, 0x9a, 0x09, 0x78, 0x4d, 0xe4, 0x72, 0xa6,
    0x06, 0xbf, 0x8b, 0x62, 0x66, 0xdd, 0x30, 0xfd, 0xe2, 0x98, 0x25, 0xb3, 0x10, 0x91, 0x22, 0x88,
    0x36, 0xd0, 0x94, 0xce, 0x8f, 0x96, 0xdb, 0xbd, 0xf1, 0xd2, 0x13, 0x5c, 0x83, 0x38, 0x46, 0x40,
    0x1e, 0x42, 0xb6, 0xa3, 0xc3, 0x48, 0x7e, 0x6e, 0x6b, 0x3a, 0x28, 0x54, 0xfa, 0x85, 0xba, 0x3d,
    0xca, 0x5e, 0x9b, 0x9f, 0x0a, 0x15, 0x79, 0x2b, 0x4e, 0xd4, 0xe5, 0xac, 0x73, 0xf3, 0xa7, 0x57,
    0x07, 0x70, 0xc0, 0xf7, 0x8c, 0x80, 0x63, 0x0d, 0x67, 0x4a, 0xde, 0xed, 0x31, 0xc5, 0xfe, 0x18,
    0xe3, 0xa5, 0x99, 0x77, 0x26, 0xb8, 0xb4, 0x7c, 0x11, 0x44, 0x92, 0xd9, 0x23, 0x20, 0x89, 0x2e,
    0x37, 0x3f, 0xd1, 0x5b, 0x95, 0xbc, 0xcf, 0xcd, 0x90, 0x87, 0x97, 0xb2, 0xdc, 0xfc, 0xbe, 0x61,
    0xf2, 0x56, 0xd3, 0xab, 0x14, 0x2a, 0x5d, 0x9e, 0x84, 0x3c, 0x39, 0x53, 0x47, 0x6d, 0x41, 0xa2,
    0x1f, 0x2d, 0x43, 0xd8, 0xb7, 0x7b, 0xa4, 0x76, 0xc4, 0x17, 0x49, 0xec, 0x7f, 0x0c, 0x6f, 0xf6,
    0x6c, 0xa1, 0x3b, 0x52, 0x29, 0x9d, 0x55, 0xaa, 0xfb, 0x60, 0x86, 0xb1, 0xbb, 0xcc, 0x3e, 0x5a,
    0xcb, 0x59, 0x5f, 0xb0, 0x9c, 0xa9, 0xa0, 0x51, 0x0b, 0xf5, 0x16, 0xeb, 0x7a, 0x75, 0x2c, 0xd7,
    0x4f, 0xae, 0xd5, 0xe9, 0xe6, 0xe7, 0xad, 0xe8, 0x74, 0xd6, 0xf4, 0xea, 0xa8, 0x50, 0x58, 0xaf,
];

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let log_sum = (u16::from(GF_LOG[a as usize]) + u16::from(GF_LOG[b as usize])) % 255;
    GF_EXP[log_sum as usize]
}

#[inline(always)]
fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    GF_EXP[255 - GF_LOG[a as usize] as usize]
}

fn gf_invert_matrix(mat: &mut [Vec<u8>], n: usize) -> bool {
    let mut pivot: [usize; HV_RAIDZ_MAX_COLS] = [0; HV_RAIDZ_MAX_COLS];
    for col in 0..n {
        let mut row = col;
        while row < n && mat[row][col] == 0 {
            row += 1;
        }
        if row >= n {
            return false;
        }
        if row != col {
            mat.swap(row, col);
        }
        pivot[col] = row;
        let inv = gf_inv(mat[col][col]);
        for j in 0..2 * n {
            mat[col][j] = gf_mul(mat[col][j], inv);
        }
        for i in 0..n {
            if i == col {
                continue;
            }
            let factor = mat[i][col];
            if factor == 0 {
                continue;
            }
            for j in 0..2 * n {
                mat[i][j] ^= gf_mul(factor, mat[col][j]);
            }
        }
    }
    true
}

impl UnkfsRaidzMap {
    pub fn new(level: UnkfsRaidzLevel, ncols: usize, ashift: u8) -> Self {
        let nparity = level.parity_cols();
        Self {
            level,
            ncols: ncols.clamp(HV_RAIDZ_MIN_COLS, HV_RAIDZ_MAX_COLS),
            nparity,
            ashift,
            cols: Vec::new(),
        }
    }

    pub fn data_cols(&self) -> usize {
        self.ncols.saturating_sub(self.nparity)
    }

    pub fn generate_parity(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let unit_size = HV_POOL_BLOCK_SIZE as usize;
        let data_cols = self.data_cols();
        if data_cols == 0 {
            return Vec::new();
        }
        let total_units = data.len().div_ceil(unit_size * data_cols);
        let mut result: Vec<Vec<u8>> = (0..self.ncols)
            .map(|_| vec![0u8; total_units * unit_size])
            .collect();

        for unit_idx in 0..total_units {
            for col in 0..data_cols {
                let src_off = unit_idx * unit_size * data_cols + col * unit_size;
                let src_end = (src_off + unit_size).min(data.len());
                let src_len = src_end.saturating_sub(src_off);
                let dst_off = unit_idx * unit_size;
                if src_len > 0 {
                    result[self.nparity + col][dst_off..dst_off + src_len]
                        .copy_from_slice(&data[src_off..src_end]);
                }
            }

            for byte_idx in 0..unit_size {
                let off = unit_idx * unit_size + byte_idx;
                for p in 0..self.nparity {
                    let mut val: u8 = 0;
                    for col in 0..data_cols {
                        let d = result[self.nparity + col][off];
                        let coeff = GF_EXP[(p * data_cols + col) % 255];
                        val ^= gf_mul(coeff, d);
                    }
                    result[p][off] = val;
                }
            }
        }

        result
    }

    pub fn reconstruct_data(
        &self,
        parity_data: &[Vec<u8>],
        failed_cols: &[usize],
    ) -> Option<Vec<u8>> {
        if failed_cols.len() > self.level.max_failures() {
            return None;
        }
        if failed_cols.is_empty() {
            let mut result = Vec::new();
            for col in self.nparity..self.ncols {
                result.extend_from_slice(&parity_data[col]);
            }
            return Some(result);
        }

        let unit_size = HV_POOL_BLOCK_SIZE as usize;
        let data_cols = self.data_cols();
        let total_units = parity_data
            .get(self.nparity)
            .map_or(0, |d| d.len() / unit_size);

        let nfailed = failed_cols.len();
        let mut mat: Vec<Vec<u8>> = (0..nfailed)
            .map(|i| {
                let mut row = vec![0u8; 2 * nfailed];
                row[i] = 1;
                for (j, &col) in failed_cols.iter().enumerate() {
                    if col < self.nparity {
                        let p = col;
                        // 第 i 行, 校验列 p: Vandermonde 系数 α^(p*data_cols + i).
                        row[nfailed + j] = GF_EXP[(p * data_cols + i) % 255];
                    } else {
                        let d_col = col - self.nparity;
                        // 第 i 行, 数据列 d_col: 系数 α^(d_col).
                        // (p=0, 故校验乘子项为零.)
                        row[nfailed + j] = GF_EXP[d_col % 255];
                    }
                }
                row
            })
            .collect();

        for (row_idx, &col) in failed_cols.iter().enumerate() {
            if col < self.nparity {
                for (j, &fc) in failed_cols.iter().enumerate() {
                    mat[row_idx][nfailed + j] =
                        GF_EXP[(col * data_cols + (fc - self.nparity)) % 255];
                }
            } else {
                let d_col = col - self.nparity;
                for (j, &fc) in failed_cols.iter().enumerate() {
                    if fc < self.nparity {
                        mat[row_idx][nfailed + j] = GF_EXP[(fc * data_cols + d_col) % 255];
                    } else {
                        let fc_d = fc - self.nparity;
                        mat[row_idx][nfailed + j] = u8::from(fc_d == d_col);
                    }
                }
            }
        }

        if !gf_invert_matrix(&mut mat, nfailed) {
            return None;
        }

        let mut result: Vec<Vec<u8>> = (0..self.ncols)
            .map(|i| {
                if failed_cols.contains(&i) {
                    Vec::new()
                } else {
                    parity_data[i].clone()
                }
            })
            .collect();

        for &fc in failed_cols {
            result[fc] = vec![0u8; total_units * unit_size];
        }

        for unit_idx in 0..total_units {
            let off = unit_idx * unit_size;
            for byte_idx in 0..unit_size {
                let pos = off + byte_idx;
                for (row_idx, &fc) in failed_cols.iter().enumerate() {
                    let mut val: u8 = 0;
                    for col in 0..self.ncols {
                        if failed_cols.contains(&col) {
                            continue;
                        }
                        let d = parity_data[col].get(pos).copied().unwrap_or(0);
                        let coeff = mat[row_idx][nfailed + col.min(nfailed - 1 + self.nparity)];
                        val ^= gf_mul(coeff, d);
                    }
                    result[fc][pos] = val;
                }
            }
        }

        let mut output = Vec::new();
        for col in self.nparity..self.ncols {
            output.extend_from_slice(&result[col]);
        }
        Some(output)
    }

    pub fn verify_parity(&self, parity_data: &[Vec<u8>]) -> bool {
        let unit_size = HV_POOL_BLOCK_SIZE as usize;
        let data_cols = self.data_cols();
        if parity_data.len() < self.ncols {
            return false;
        }
        let total_units = parity_data
            .get(self.nparity)
            .map_or(0, |d| d.len() / unit_size);

        for unit_idx in 0..total_units {
            for byte_idx in 0..unit_size {
                let off = unit_idx * unit_size + byte_idx;
                for p in 0..self.nparity {
                    let mut computed: u8 = 0;
                    for col in 0..data_cols {
                        let d = parity_data[self.nparity + col]
                            .get(off)
                            .copied()
                            .unwrap_or(0);
                        let coeff = GF_EXP[(p * data_cols + col) % 255];
                        computed ^= gf_mul(coeff, d);
                    }
                    let stored = parity_data[p].get(off).copied().unwrap_or(0);
                    if computed != stored {
                        return false;
                    }
                }
            }
        }
        true
    }
}

pub struct UnkfsRaidzEngine;

impl UnkfsRaidzEngine {
    pub fn create_stripe(level: UnkfsRaidzLevel, ncols: usize, ashift: u8) -> UnkfsRaidzMap {
        UnkfsRaidzMap::new(level, ncols, ashift)
    }

    pub fn scrub_block(map: &UnkfsRaidzMap, parity_data: &[Vec<u8>]) -> UnkfsScrubResult {
        if map.verify_parity(parity_data) {
            UnkfsScrubResult::Clean
        } else {
            UnkfsScrubResult::Corrupted {
                failed_cols: vec![0],
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum UnkfsScrubResult {
    Clean,
    Corrupted { failed_cols: Vec<usize> },
    Repaired { repaired_cols: Vec<usize> },
    Unrepairable,
}
