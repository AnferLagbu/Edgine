//! E-2 两段式 ELF 前缀读回归锁.
//!
//! 覆盖 `loadable_prefix_len` 之后的真实 VFS 读入面:
//! 1. 文件尾带 >1 MiB 非装载字节，只返回可加载前缀；
//! 2. phdr 表落在 128 KiB 探测窗口外 ⇒ `Malformed`；
//! 3. `PT_LOAD` 前缀超过 1 MiB 预算 ⇒ `Malformed`；
//! 4. `p_filesz > p_memsz` ⇒ `Malformed`；
//! 5. 文件短于计算前缀 ⇒ 第二段短读 `Malformed`；
//! 6. syscall fd 路径同样只读前缀；
//! 7. E-3: >1 MiB wasm 模块的魔数探测只读 4 字节.

use std::sync::Mutex;

use edgine::kernel::functions::fs;
use edgine::kernel::functions::fs::api::read_file_prefix_to_vec;
use edgine::kernel::functions::fs::api::write_file_kernel;
use edgine::kernel::functions::fs::ramfs_core;
use edgine::kernel::functions::proc::elf::{
    ElfPrefixError, read_elf_prefix_kernel, read_elf_prefix_to_vec,
};
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};

static GLOBAL: Mutex<()> = Mutex::new(());

struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Scheduler::set_current(pid);
        Self { pid }
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
        Scheduler::set_current(0);
    }
}

fn init_fs_once() {
    fs::init();
    ramfs_core::init();
    let _mount: i32 = fs::vfs_mount_safe("/", "ramfs");
}

fn cpath(path: &str) -> Vec<u8> {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn write_file(name: &str, bytes: &[u8]) {
    let path = format!("/{name}");
    assert!(
        write_file_kernel(&path, bytes, 0),
        "write_file_kernel {path} 失败"
    );
}

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const PT_LOAD: u32 = 1;

fn put_u16(buf: &mut [u8], off: usize, value: u16) {
    buf[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, value: u64) {
    buf[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

/// 构造 `phdrs = (p_type, p_offset, p_filesz, p_memsz)` 的最小 ELF；
/// `file_len` 可大于头部+phdr 表，用于放置非装载文件尾.
fn elf_image(phdrs: &[(u32, u64, u64, u64)], file_len: usize) -> Vec<u8> {
    let table_end = EHDR_SIZE + phdrs.len() * PHDR_SIZE;
    let len = file_len.max(table_end);
    let mut buf = vec![0u8; len];

    buf[0..4].copy_from_slice(b"\x7FELF");
    buf[4] = 2; // ELFCLASS64
    put_u16(&mut buf, 18, 0x3E); // e_machine = x86_64
    put_u32(&mut buf, 20, 1); // e_version
    put_u64(
        &mut buf,
        32,
        u64::try_from(EHDR_SIZE).expect("ELF 头大小应可表示"),
    ); // e_phoff
    put_u16(
        &mut buf,
        52,
        u16::try_from(EHDR_SIZE).expect("ELF 头大小应可表示"),
    ); // e_ehsize
    put_u16(
        &mut buf,
        54,
        u16::try_from(PHDR_SIZE).expect("phdr 大小应可表示"),
    ); // e_phentsize
    put_u16(
        &mut buf,
        56,
        u16::try_from(phdrs.len()).expect("phdr 数量超出 u16"),
    ); // e_phnum

    for (i, (ty, offset, filesz, memsz)) in phdrs.iter().enumerate() {
        let phdr = EHDR_SIZE + i * PHDR_SIZE;
        put_u32(&mut buf, phdr, *ty);
        put_u64(&mut buf, phdr + 8, *offset);
        put_u64(&mut buf, phdr + 32, *filesz);
        put_u64(&mut buf, phdr + 40, *memsz);
    }
    buf
}

#[test]
fn prefix_read_ignores_non_load_file_tail() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let image = elf_image(&[(PT_LOAD, 0, 512, 512)], 1024 * 1024 + 4096);
    write_file("e2_big_tail", &image);

    let prefix = read_elf_prefix_kernel("/e2_big_tail", 0).expect("大文件尾不应使前缀读失败");
    assert_eq!(prefix.len(), 512);
    assert_eq!(&prefix[0..4], b"\x7FELF");
}

#[test]
fn syscall_prefix_read_uses_same_boundary() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let image = elf_image(&[(PT_LOAD, 0, 512, 512)], 1024 * 1024 + 4096);
    write_file("e2_big_tail_fd", &image);
    let _proc = TestProc::new("e2-prefix-fd");

    let path = cpath("/e2_big_tail_fd");
    let prefix = read_elf_prefix_to_vec(path.as_ptr(), 0).expect("fd 路径前缀读失败");
    assert_eq!(prefix.len(), 512);
}

#[test]
fn phdr_outside_probe_is_malformed() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let mut image = elf_image(&[(PT_LOAD, 0, 1, 1)], 128 * 1024);
    // 把 phdr 表推到探测窗口之后；文件本身仍是 128 KiB.
    put_u64(
        &mut image,
        32,
        u64::try_from(128 * 1024).expect("窗口大小应可表示"),
    );
    write_file("e2_phdr_oob", &image);

    assert_eq!(
        read_elf_prefix_kernel("/e2_phdr_oob", 0),
        Err(ElfPrefixError::Malformed)
    );
}

#[test]
fn load_prefix_over_budget_is_malformed() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let prefix_len = 1024 * 1024 + 1;
    let image = elf_image(
        &[(PT_LOAD, 0, prefix_len, prefix_len)],
        EHDR_SIZE + PHDR_SIZE,
    );
    write_file("e2_over_budget", &image);

    assert_eq!(
        read_elf_prefix_kernel("/e2_over_budget", 0),
        Err(ElfPrefixError::Malformed)
    );
}

#[test]
fn file_size_exceeding_mem_size_is_malformed() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let image = elf_image(&[(PT_LOAD, 0, 200, 100)], EHDR_SIZE + PHDR_SIZE);
    write_file("e2_filesz_gt_memsz", &image);

    assert_eq!(
        read_elf_prefix_kernel("/e2_filesz_gt_memsz", 0),
        Err(ElfPrefixError::Malformed)
    );
}

#[test]
fn wasm_magic_probe_reads_only_four_bytes() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    let mut module = vec![0u8; 1024 * 1024 + 4096];
    module[0..4].copy_from_slice(b"\0asm");
    write_file("e3_wasm_big", &module);
    let _proc = TestProc::new("e3-wasm-magic");

    let path = cpath("/e3_wasm_big");
    let magic = read_file_prefix_to_vec(path.as_ptr(), 0, 4).expect("4 字节魔数读失败");
    assert_eq!(magic, b"\0asm");
}

#[test]
fn short_second_segment_is_malformed() {
    let _guard = GLOBAL.lock().expect("ELF 前缀测试全局锁被毒化");
    init_fs_once();
    // phdr 声称 PT_LOAD 到 200B，但文件只有 120B（头 + phdr）.
    let image = elf_image(&[(PT_LOAD, 0, 200, 200)], EHDR_SIZE + PHDR_SIZE);
    write_file("e2_short", &image);

    assert_eq!(
        read_elf_prefix_kernel("/e2_short", 0),
        Err(ElfPrefixError::Malformed)
    );
}
