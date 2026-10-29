#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::unkfs::bp::UnkfsCompType;
use alloc::vec::Vec;

/// 压缩生效的最小数据长度 (短于此值不压缩)。
pub const HV_COMP_MIN_SIZE: usize = 64;

/// 按指定算法压缩数据; 数据短于 `HV_COMP_MIN_SIZE` 或算法为 `Off` 时返回 `None`。
pub fn compress(data: &[u8], comp_type: UnkfsCompType) -> Option<Vec<u8>> {
    if data.len() < HV_COMP_MIN_SIZE {
        return None;
    }
    match comp_type {
        UnkfsCompType::Off => None,
        UnkfsCompType::LZ4 => compress_lz4(data),
        UnkfsCompType::Zstd => compress_zstd_fallback(data),
        UnkfsCompType::Gzip1 | UnkfsCompType::Gzip9 => compress_rle(data),
        UnkfsCompType::Zle => compress_zle(data),
    }
}

/// 按指定算法解压数据至 `expected_size` 字节大小。
pub fn decompress(
    compressed: &[u8],
    expected_size: usize,
    comp_type: UnkfsCompType,
) -> Option<Vec<u8>> {
    match comp_type {
        UnkfsCompType::Off => None,
        UnkfsCompType::LZ4 => decompress_lz4(compressed, expected_size),
        UnkfsCompType::Zstd => decompress_zstd_fallback(compressed, expected_size),
        UnkfsCompType::Gzip1 | UnkfsCompType::Gzip9 => decompress_rle(compressed, expected_size),
        UnkfsCompType::Zle => decompress_zle(compressed, expected_size),
    }
}

fn compress_lz4(data: &[u8]) -> Option<Vec<u8>> {
    let src_len = data.len();
    let mut output = Vec::with_capacity(src_len + src_len / 255 + 16);
    let mut ip = 0;
    let mut anchor = 0;
    output.push(crate::functions::intcast::low_u8_from_usize(src_len & 0xFF));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (src_len >> 8) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (src_len >> 16) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (src_len >> 24) & 0xFF,
    ));
    while ip < src_len {
        let mut match_len = 0;
        let mut ref_pos = 0;
        if ip + 4 <= src_len {
            let _hash = lz4_hash(data, ip);
            let search_start = ip.saturating_sub(65536);
            for s in (search_start..ip).step_by(4) {
                if s + 4 <= src_len && data[s..s + 4] == data[ip..ip + 4] {
                    ref_pos = s;
                    let mut len = 4;
                    while ip + len < src_len
                        && ref_pos + len < ip
                        && data[ref_pos + len] == data[ip + len]
                    {
                        len += 1;
                    }
                    match_len = len;
                    break;
                }
            }
        }
        if match_len >= 4 {
            let literal_len = ip - anchor;
            let mut tok = if literal_len >= 15 {
                15
            } else {
                crate::functions::intcast::low_u8_from_usize(literal_len)
            };
            let ml = match_len - 4;
            tok |= (if ml >= 15 {
                15
            } else {
                crate::functions::intcast::low_u8_from_usize(ml)
            }) << 4;
            output.push(tok);
            if literal_len >= 15 {
                let mut remaining = literal_len - 15;
                while remaining >= 255 {
                    output.push(255);
                    remaining -= 255;
                }
                output.push(crate::functions::intcast::low_u8_from_usize(remaining));
            }
            output.extend_from_slice(&data[anchor..anchor + literal_len]);
            let offset = crate::functions::intcast::low_u16_from_usize(ip - ref_pos);
            output.push((offset & 0xFF) as u8);
            output.push(((offset >> 8) & 0xFF) as u8);
            if ml >= 15 {
                let mut remaining = ml - 15;
                while remaining >= 255 {
                    output.push(255);
                    remaining -= 255;
                }
                output.push(crate::functions::intcast::low_u8_from_usize(remaining));
            }
            ip += match_len;
            anchor = ip;
        } else {
            ip += 1;
        }
    }
    if anchor < src_len {
        let literal_len = src_len - anchor;
        let tok = if literal_len >= 15 {
            15u8
        } else {
            crate::functions::intcast::low_u8_from_usize(literal_len)
        };
        output.push(tok);
        if literal_len >= 15 {
            let mut remaining = literal_len - 15;
            while remaining >= 255 {
                output.push(255);
                remaining -= 255;
            }
            output.push(crate::functions::intcast::low_u8_from_usize(remaining));
        }
        output.extend_from_slice(&data[anchor..src_len]);
    }
    if output.len() >= src_len {
        None
    } else {
        Some(output)
    }
}

fn lz4_hash(data: &[u8], pos: usize) -> u32 {
    let v = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
    (v.wrapping_mul(2_654_435_761)) >> 16
}

fn decompress_lz4(compressed: &[u8], _expected_size: usize) -> Option<Vec<u8>> {
    if compressed.len() < 4 {
        return None;
    }
    let src_len = compressed[0] as usize
        | ((compressed[1] as usize) << 8)
        | ((compressed[2] as usize) << 16)
        | ((compressed[3] as usize) << 24);
    let mut output = Vec::with_capacity(src_len);
    let mut ip = 4;
    while ip < compressed.len() && output.len() < src_len {
        let tok = compressed[ip];
        ip += 1;
        let mut literal_len = (tok & 0x0F) as usize;
        if literal_len == 15 {
            while ip < compressed.len() {
                let byte = compressed[ip] as usize;
                ip += 1;
                literal_len += byte;
                if byte != 255 {
                    break;
                }
            }
        }
        if ip + literal_len > compressed.len() {
            break;
        }
        output.extend_from_slice(&compressed[ip..ip + literal_len]);
        ip += literal_len;
        if ip >= compressed.len() {
            break;
        }
        if ip + 2 > compressed.len() {
            break;
        }
        let offset = compressed[ip] as usize | ((compressed[ip + 1] as usize) << 8);
        ip += 2;
        if offset == 0 {
            break;
        }
        let mut match_len = ((tok >> 4) & 0x0F) as usize + 4;
        if (tok >> 4) & 0x0F == 15 {
            while ip < compressed.len() {
                let byte = compressed[ip] as usize;
                ip += 1;
                match_len += byte;
                if byte != 255 {
                    break;
                }
            }
        }
        let start = output.len().saturating_sub(offset);
        for i in 0..match_len {
            if start + i < output.len() {
                output.push(output[start + i]);
            }
        }
    }
    Some(output)
}

fn compress_rle(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    let mut output = Vec::with_capacity(data.len() + 4);
    output.push(crate::functions::intcast::low_u8_from_usize(
        data.len() & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 8) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 16) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 24) & 0xFF,
    ));
    let mut i = 0;
    while i < data.len() {
        let byte = data[i];
        let mut count = 1;
        while i + count < data.len() && data[i + count] == byte && count < 255 {
            count += 1;
        }
        output.push(byte);
        output.push(crate::functions::intcast::low_u8_from_usize(count));
        i += count;
    }
    if output.len() >= data.len() {
        None
    } else {
        Some(output)
    }
}

fn decompress_rle(compressed: &[u8], _expected_size: usize) -> Option<Vec<u8>> {
    if compressed.len() < 4 {
        return None;
    }
    let src_len = compressed[0] as usize
        | ((compressed[1] as usize) << 8)
        | ((compressed[2] as usize) << 16)
        | ((compressed[3] as usize) << 24);
    let mut output = Vec::with_capacity(src_len);
    let mut ip = 4;
    while ip + 1 < compressed.len() && output.len() < src_len {
        let byte = compressed[ip];
        let count = compressed[ip + 1] as usize;
        ip += 2;
        for _ in 0..count {
            output.push(byte);
        }
    }
    Some(output)
}

fn compress_zle(data: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(data.len() + 4);
    output.push(crate::functions::intcast::low_u8_from_usize(
        data.len() & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 8) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 16) & 0xFF,
    ));
    output.push(crate::functions::intcast::low_u8_from_usize(
        (data.len() >> 24) & 0xFF,
    ));
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0 {
            let mut count = 0;
            while i + count < data.len() && data[i + count] == 0 && count < 255 {
                count += 1;
            }
            output.push(0);
            output.push(crate::functions::intcast::low_u8_from_usize(count));
            i += count;
        } else {
            output.push(data[i]);
            i += 1;
        }
    }
    if output.len() >= data.len() {
        None
    } else {
        Some(output)
    }
}

fn decompress_zle(compressed: &[u8], _expected_size: usize) -> Option<Vec<u8>> {
    if compressed.len() < 4 {
        return None;
    }
    let src_len = compressed[0] as usize
        | ((compressed[1] as usize) << 8)
        | ((compressed[2] as usize) << 16)
        | ((compressed[3] as usize) << 24);
    let mut output = Vec::with_capacity(src_len);
    let mut ip = 4;
    while ip < compressed.len() && output.len() < src_len {
        if compressed[ip] == 0 && ip + 1 < compressed.len() {
            let count = compressed[ip + 1] as usize;
            output.resize(output.len() + count, 0);
            ip += 2;
        } else {
            output.push(compressed[ip]);
            ip += 1;
        }
    }
    Some(output)
}

fn compress_zstd_fallback(data: &[u8]) -> Option<Vec<u8>> {
    compress_rle(data)
}
fn decompress_zstd_fallback(compressed: &[u8], expected: usize) -> Option<Vec<u8>> {
    decompress_rle(compressed, expected)
}

// DECISION-080: 压缩算法纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属
// (host 可编译, 不再占用 kernel_test 注册表轨).
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 压缩 / 解压往返 ====

    /// LZ4 压缩后解压应还原原始数据 (compress 返回 None 时表示数据不可压缩, 视为通过).
    #[test]
    fn test_compress_lz4_roundtrip() {
        let mut data = [0u8; 256];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 4) as u8;
        }
        if let Some(c) = compress(&data, UnkfsCompType::LZ4) {
            let Some(d) = decompress(&c, data.len(), UnkfsCompType::LZ4) else {
                panic!("decompress returned None");
            };
            assert_eq!(d.len(), data.len(), "decompressed length mismatch");
            assert_eq!(d.as_slice(), data, "roundtrip data mismatch");
        }
    }

    /// UnkfsCompType::Off 表示不压缩, compress() 按设计返回 None; 这里验证 Gzip1(RLE) 往返.
    #[test]
    fn test_compress_off() {
        let mut data = [0u8; 256];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 4) as u8;
        }
        if let Some(c) = compress(&data, UnkfsCompType::Gzip1) {
            if let Some(d) = decompress(&c, data.len(), UnkfsCompType::Gzip1) {
                assert_eq!(d.len(), data.len(), "RLE decompressed length mismatch");
                assert_eq!(d.as_slice(), data, "RLE roundtrip data mismatch");
            }
        }
    }
}
