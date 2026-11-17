//! 最小 WASI (`wasi_snapshot_preview1`) 编码助手与 errno 值域.
//!
//! 纯函数, 不依赖 wasmi 与 syscall, 可在 host 侧单测; 真正的 host 闭包
//! (读 wasm 线性内存 + 翻译到 Edgine syscall) 见 `main.rs`.

/// WASI errno: 成功.
pub const ERRNO_SUCCESS: i32 = 0;
/// WASI errno: 文件描述符无效.
pub const ERRNO_BADF: i32 = 8;
/// WASI errno: 内存越界或地址非法.
pub const ERRNO_FAULT: i32 = 21;
/// WASI errno: I/O 错误.
pub const ERRNO_IO: i32 = 29;

/// WASI filetype: 字符设备 (标准流 stdin/stdout/stderr).
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;

/// 标准流 rights 集合: datasync/read/seek/`fdstat_set_flags`/sync/tell/write/advise/allocate.
///
/// 与内核 `WasiFdTable` 给 stdio 授全权同口径, 此处收敛到字符设备实际可用的位.
pub const STDIO_RIGHTS: u64 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8);

/// 编码 WASI `fdstat` 结构 (24 字节).
///
/// 布局: `filetype`@0 (u8), `flags`@2 (u16), `rights_base`@8 (u64),
/// `rights_inheriting`@16 (u64); 其余为对齐填充, 置零.
#[must_use]
pub fn encode_fdstat(
    filetype: u8,
    flags: u16,
    rights_base: u64,
    rights_inheriting: u64,
) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0] = filetype;
    b[2..4].copy_from_slice(&flags.to_le_bytes());
    b[8..16].copy_from_slice(&rights_base.to_le_bytes());
    b[16..24].copy_from_slice(&rights_inheriting.to_le_bytes());
    b
}

/// 从 8 字节小端缓冲解码 WASI `iovec` (`buf_ptr`: u32, `buf_len`: u32).
#[must_use]
pub fn iovec_from_le_bytes(bytes: [u8; 8]) -> (u32, u32) {
    let ptr = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (ptr, len)
}

#[cfg(test)]
mod tests {
    use super::{
        ERRNO_BADF, ERRNO_FAULT, ERRNO_IO, ERRNO_SUCCESS, FILETYPE_CHARACTER_DEVICE, STDIO_RIGHTS,
        encode_fdstat, iovec_from_le_bytes,
    };

    #[test]
    fn errno_matches_wasi_snapshot_preview1() {
        assert_eq!(ERRNO_SUCCESS, 0);
        assert_eq!(ERRNO_BADF, 8);
        assert_eq!(ERRNO_FAULT, 21);
        assert_eq!(ERRNO_IO, 29);
    }

    #[test]
    fn fdstat_layout_offsets() {
        let b = encode_fdstat(
            FILETYPE_CHARACTER_DEVICE,
            0x0102,
            0x1122_3344_5566_7788,
            0x99aa,
        );
        assert_eq!(b[0], 2);
        // 填充字节置零
        assert_eq!(b[1], 0);
        assert_eq!(&b[4..8], &[0, 0, 0, 0]);
        // flags @2 小端
        assert_eq!(&b[2..4], &[0x02, 0x01]);
        // rights_base @8 小端
        assert_eq!(&b[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
        // rights_inheriting @16 小端
        assert_eq!(&b[16..24], &0x99aau64.to_le_bytes());
    }

    #[test]
    fn fdstat_stdio_rights_has_write_and_read() {
        let b = encode_fdstat(FILETYPE_CHARACTER_DEVICE, 0, STDIO_RIGHTS, 0);
        let base = u64::from_le_bytes(b[8..16].try_into().unwrap());
        assert_ne!(base & (1 << 1), 0); // fd_read
        assert_ne!(base & (1 << 6), 0); // fd_write
        assert_eq!(u64::from_le_bytes(b[16..24].try_into().unwrap()), 0);
    }

    #[test]
    fn iovec_decode_round_trip() {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&17u32.to_le_bytes());
        assert_eq!(iovec_from_le_bytes(bytes), (0xdead_beef, 17));
    }

    #[test]
    fn iovec_decode_zero() {
        assert_eq!(iovec_from_le_bytes([0u8; 8]), (0, 0));
    }
}
