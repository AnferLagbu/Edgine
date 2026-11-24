//! mmap/munmap 无 mm 路径回归锁.
//!
//! 动态链接器第一件事就是 `mmap`。裸页回退会把**物理地址**当返回值交给用户态，
//! 绕过 VMA 与页表；本文件把「无 mm 必须 ENOMEM」钉成源码契约，防止旁路复活。

use std::fs;

const MMAP_RS: &str = "../src/kernel/functions/mm/mmap.rs";
const SYSCALL_API_RS: &str = "../src/kernel/privileged/syscall/api.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

#[test]
fn mmap_entry_has_no_raw_page_fallback() {
    let src = read(MMAP_RS);
    assert!(
        !src.contains("mmap_get_mm_or_alloc"),
        "mmap 入口不得再调用裸页分配旁路"
    );
    assert!(
        !src.contains("munmap_free_pages"),
        "munmap 入口不得再调用裸页释放旁路"
    );
    assert!(
        src.contains("let Some(mm) = crate::privileged::mm::vma_get_current_mm() else")
            && src.contains("return Errno::Enomem.as_ret();"),
        "无 mm 必须 fail-closed 到 ENOMEM"
    );
}

#[test]
fn syscall_api_no_longer_exports_raw_mmap_bypass() {
    let src = read(SYSCALL_API_RS);
    assert!(
        !src.contains("pub fn mmap_get_mm_or_alloc") && !src.contains("pub fn munmap_free_pages"),
        "裸页 mmap/munmap 机制已无消费者，必须一并删除"
    );
}
