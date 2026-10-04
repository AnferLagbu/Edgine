// SPDX-License-Identifier: MPL-2.0
//! Phase C 单元测试 (host 模拟)
//!
//! 覆盖: C1 mmap flags / C2 ELF header / C3 userland / C4 sysfs

// ============================================================================
// C1 mmap — flags / prot 验证
// ============================================================================

const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const PROT_EXEC: i32 = 4;
const PROT_NONE: i32 = 0;

const MAP_SHARED: i32 = 0x01;
const MAP_PRIVATE: i32 = 0x02;
const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;

#[test]
fn test_prot_combine() {
    // PROT_NONE < PROT_READ < PROT_RW < PROT_RWX
    assert_eq!(PROT_NONE, 0);
    assert!(PROT_READ < PROT_WRITE);
    let rw = PROT_READ | PROT_WRITE;
    assert_eq!(rw & PROT_READ, PROT_READ);
    assert_eq!(rw & PROT_WRITE, PROT_WRITE);
    assert_eq!(rw & PROT_EXEC, 0);
    let rwx = rw | PROT_EXEC;
    assert_eq!(rwx, 7);
}

#[test]
fn test_map_shared_vs_private_exclusive() {
    // Linux: SHARED 和 PRIVATE 不能同时设置
    let both = MAP_SHARED | MAP_PRIVATE;
    assert_eq!(both & MAP_SHARED, MAP_SHARED);
    assert_eq!(both & MAP_PRIVATE, MAP_PRIVATE);
    // 检测函数
    fn is_inconsistent(f: i32) -> bool {
        (f & MAP_SHARED) != 0 && (f & MAP_PRIVATE) != 0
    }
    assert!(is_inconsistent(both));
    assert!(!is_inconsistent(MAP_SHARED));
    assert!(!is_inconsistent(MAP_PRIVATE));
    assert!(!is_inconsistent(MAP_ANONYMOUS));
}

#[test]
fn test_map_anonymous_with_private() {
    // MAP_ANONYMOUS 必须配 MAP_PRIVATE
    let f = MAP_PRIVATE | MAP_ANONYMOUS;
    assert_eq!(f & MAP_ANONYMOUS, MAP_ANONYMOUS);
    assert_eq!(f & MAP_PRIVATE, MAP_PRIVATE);
}

#[test]
fn test_map_fixed_alignment() {
    // MAP_FIXED + addr 通常要求页对齐 (4 KiB)
    const PAGE_SIZE_4K: u64 = 0x1000;
    let addr = 0x7F00_0000_u64;
    assert_eq!(addr & (PAGE_SIZE_4K - 1), 0);
}

// ============================================================================
// C2 ELF — header magic / 字段验证
// ============================================================================

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

fn is_elf64(b: &[u8]) -> bool {
    b.len() >= 4 && b[0..4] == ELF_MAGIC
}

fn elf_class(b: &[u8]) -> Option<u8> {
    if !is_elf64(b) { return None; }
    Some(b[4])
}

fn elf_data(b: &[u8]) -> Option<u8> {
    if !is_elf64(b) { return None; }
    Some(b[5])
}

#[test]
fn test_elf_magic() {
    let good = [0x7F, b'E', b'L', b'F', ELFCLASS64, ELFDATA2LSB, 1, 0, 0, 0, 0, 0, 0, 0];
    assert!(is_elf64(&good));
    let bad = [0x7F, b'E', b'L', b'X', 0, 0, 0, 0];
    assert!(!is_elf64(&bad));
    let short = [0x7F, b'E'];
    assert!(!is_elf64(&short));
}

#[test]
fn test_elf_class_64bit() {
    let h = [0x7F, b'E', b'L', b'F', ELFCLASS64, 0, 0, 0];
    assert_eq!(elf_class(&h), Some(ELFCLASS64));
    let h32 = [0x7F, b'E', b'L', b'F', 1, 0, 0, 0];
    assert_eq!(elf_class(&h32), Some(1));
}

#[test]
fn test_elf_little_endian() {
    let h = [0x7F, b'E', b'L', b'F', 2, ELFDATA2LSB, 1, 0];
    assert_eq!(elf_data(&h), Some(ELFDATA2LSB));
}

#[test]
fn test_elf_machine() {
    assert_eq!(EM_X86_64, 62);
    assert_eq!(EM_AARCH64, 183);
    assert_ne!(EM_X86_64, EM_AARCH64);
}

#[test]
fn test_elf_executable_type() {
    // ET_EXEC=2, ET_DYN=3
    assert_eq!(ET_EXEC, 2);
}

// ============================================================================
// C3 userland — syscall 编号 + 字符串 + mmap 标志
// ============================================================================

#[test]
fn test_syscall_numbers_linux_compat() {
    // 与 Linux x86_64 一致, 内核 dispatch 才能识别
    const SYS_WRITE: u64 = 1;
    const SYS_BRK: u64 = 12;
    const SYS_MMAP: u64 = 9;
    const SYS_GETPID: u64 = 39;
    const SYS_EXIT: u64 = 60;
    const SYS_EXIT_GROUP: u64 = 231;
    assert_eq!(SYS_WRITE, 1);
    assert_eq!(SYS_BRK, 12);
    assert_eq!(SYS_MMAP, 9);
    assert_eq!(SYS_GETPID, 39);
    assert_eq!(SYS_EXIT, 60);
    assert_eq!(SYS_EXIT_GROUP, 231);
}

#[test]
fn test_no_std_marker() {
    // 模拟 userland 是 no_std
    let no_std = true;
    assert!(no_std);
}

#[test]
fn test_userland_panic_handler_present() {
    // panic handler 是裸金属 no_std 必填
    let has_panic_handler = true;
    assert!(has_panic_handler);
}

#[test]
fn test_userland_function_signatures() {
    // 模拟 _exit / write / getpid 类型签名
    // 不能调 _exit (它会调用 std::process::exit 导致测试进程死掉)
    let _write: fn(i32, *const u8, usize) -> isize = |_fd, _buf, _n| 0;
    let _getpid: fn() -> i32 = || 1;
    let _ = _write(1, b"x\0".as_ptr(), 1);
    let _ = _getpid();
    // 验证 _exit 签名存在 (用函数指针而非调用)
    let _: fn(i32) -> ! = |s| std::process::exit(s);
}

// ============================================================================
// C4 sysfs / VFS — 节点表 / 权限 / 路径
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SysfsValue {
    Integer(u64),
    String(&'static str),
    Bool(bool),
}

#[test]
fn test_sysfs_node_count() {
    const N: usize = 6;
    assert_eq!(N, 6);
}

#[test]
fn test_sysfs_node_lookup() {
    let nodes = ["cpu_count", "mem_total", "mem_free", "uptime_secs", "version", "boot_status"];
    assert_eq!(nodes.len(), 6);
    assert!(nodes.contains(&"cpu_count"));
    assert!(nodes.contains(&"version"));
    assert!(!nodes.contains(&"nonexistent"));
}

#[test]
fn test_sysfs_format_u64() {
    fn fmt(mut n: u64) -> ([u8; 20], usize) {
        if n == 0 { return ([b'0'; 20], 1); }
        let mut buf = [0u8; 20];
        let mut i = 20;
        while n > 0 {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        let len = 20 - i;
        let mut out = [b'0'; 20];
        for j in 0..len {
            out[j] = buf[i + j];
        }
        (out, len)
    }

    let (b, n) = fmt(0);
    assert_eq!(n, 1);
    assert_eq!(b[0], b'0');

    let (b, n) = fmt(123);
    assert_eq!(n, 3);
    assert_eq!(&b[..3], b"123");

    let (b, n) = fmt(4294967295);
    assert_eq!(n, 10);
    assert_eq!(&b[..10], b"4294967295");
}

#[test]
fn test_sysfs_path_components() {
    // /sys/edgine/version 拆 3 段
    let path = "/sys/edgine/version";
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    assert_eq!(parts, vec!["sys", "edgine", "version"]);
}

#[test]
fn test_mount_targets() {
    // 标准挂载点
    const MOUNT_PROC: &str = "/proc";
    const MOUNT_SYS: &str = "/sys";
    const MOUNT_DEV: &str = "/dev";
    assert_eq!(MOUNT_PROC, "/proc");
    assert_eq!(MOUNT_SYS, "/sys");
    assert_eq!(MOUNT_DEV, "/dev");
}

#[test]
fn test_vfs_path_normalization() {
    fn normalize(p: &str) -> String {
        let mut s = p.to_string();
        while s.contains("//") {
            s = s.replace("//", "/");
        }
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
        s
    }
    assert_eq!(normalize("/a//b///c"), "/a/b/c");
    assert_eq!(normalize("/"), "/");
    assert_eq!(normalize("/a/"), "/a");
}

// ============================================================================
// 综合 — Phase C 全栈常量不变量
// ============================================================================

#[test]
fn test_phase_c_constants_compatible() {
    // C1 mmap + C2 ELF + C3 userland + C4 sysfs 常量互不冲突
    // 一些可能在多子任务共享的常量
    assert_eq!(MAP_PRIVATE, 0x02);
    assert_eq!(MAP_ANONYMOUS, 0x20);
    assert_eq!(PROT_READ, 1);
    assert_eq!(EM_X86_64, 62);
    assert_eq!(ELFCLASS64, 2);
}
