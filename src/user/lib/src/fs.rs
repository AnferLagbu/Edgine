/// 文件操作辅助
use crate::sys;
use crate::sys::{O_CREAT, O_RDONLY, O_WRONLY};

pub fn file_open(path: &[u8], flags: i32) -> i32 {
    let mut p = [0u8; 256];
    let len = core::cmp::min(path.len(), 255);
    p[..len].copy_from_slice(&path[..len]);
    p[len] = 0;
    // mode 仅在 O_CREAT 时有效, 其他情况传 0 (POSIX 语义)
    let mode = if (flags & O_CREAT) != 0 { 0o644 } else { 0 };
    sys::fs_open(&p[..len + 1], flags, mode)
}

pub fn file_copy(src_path: &[u8], dst_path: &[u8]) -> bool {
    let rfd = file_open(src_path, O_RDONLY);
    if rfd < 0 {
        return false;
    }
    let wfd = file_open(dst_path, O_CREAT | O_WRONLY);
    if wfd < 0 {
        sys::fs_close(rfd);
        return false;
    }
    let mut buf = [0u8; 4096];
    loop {
        let n = sys::fs_read(rfd, &mut buf);
        if n <= 0 {
            break;
        }
        sys::fs_write(wfd, &buf[..n as usize]);
    }
    sys::fs_close(rfd);
    sys::fs_close(wfd);
    true
}
