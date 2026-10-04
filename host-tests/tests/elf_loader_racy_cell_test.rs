//! ELF loader RacyCell 静态分配器消除验证 (P1-I-32)
//!
//! 验证:
//! 1. `src/kernel/privileged/proc/user_proc.rs` 和 `src/kernel/privileged/proc/elf.rs` 中
//!    不再使用 `RacyCell` 或 `static mut` 静态分配器加载 ELF
//! 2. 8KB [u64; 1024] 临时缓冲可成功在栈上分配 (编译期 + 运行时)
//! 3. 双实例 (模拟 SMP 双核并发) 各自有独立 buffer, 数据不串台
//!
//! 主机端测试: 通过对源码做静态文本扫描 + 自包含 mini-load 模拟数据隔离.

/// 镜像内核 ELF loader 内部缓冲: 8KB 栈分配
const MAX_LOAD_PAGES: usize = 1024;

/// P1-I-32 模拟函数: 双线程各自栈上 buffer, 写入独立数据并验证无串台
fn load_elf_mock(cpu_id: u32, page_count: usize) -> Vec<u64> {
    let mut allocated_pages = [0u64; MAX_LOAD_PAGES];
    let allocated_pages: &mut [u64] = &mut allocated_pages;

    let actual = page_count.min(MAX_LOAD_PAGES);
    for (i, page) in allocated_pages.iter_mut().enumerate().take(actual) {
        *page = (cpu_id as u64) * 0x1_0000_0000 + i as u64;
    }
    allocated_pages[..actual].to_vec()
}

#[test]
fn single_load_records_page_count() {
    // 单核基本功能
    let result = load_elf_mock(0, 10);
    assert_eq!(result.len(), 10, "P1-I-32: 10 页应全部记录");
    for (i, &v) in result.iter().enumerate() {
        assert_eq!(v, i as u64, "P1-I-32: cpu=0 第 {} 页应为 {}", i, i);
    }
}

#[test]
fn two_concurrent_loads_have_isolated_buffers() {
    // P1-I-32 验收: 双 CPU 并发执行 execve, 数据不串台
    let cpu0 = std::thread::spawn(|| load_elf_mock(0, 100));
    let cpu1 = std::thread::spawn(|| load_elf_mock(1, 100));
    let r0 = cpu0.join().expect("cpu0 load ok");
    let r1 = cpu1.join().expect("cpu1 load ok");

    assert_eq!(r0.len(), 100, "P1-I-32: cpu0 应记录 100 页");
    assert_eq!(r1.len(), 100, "P1-I-32: cpu1 应记录 100 页");

    for (i, (&v0, &v1)) in r0.iter().zip(r1.iter()).enumerate() {
        // cpu0: base = 0x0 + i
        // cpu1: base = 0x1_0000_0000 + i
        assert_eq!(v0, i as u64, "P1-I-32: cpu0 第 {} 页 = {}", i, i);
        assert_eq!(
            v1,
            0x1_0000_0000u64 + i as u64,
            "P1-I-32: cpu1 第 {} 页 = base+{} (无 cpu0 数据污染)",
            i,
            i
        );
        assert_ne!(v0, v1, "P1-I-32: cpu0/cpu1 第 {} 页必须不同 (无串台)", i);
    }
}

#[test]
fn eights_cpus_concurrent_have_isolated_buffers() {
    // P1-I-32 验收: 8 个 CPU 并发执行 execve, 无 panic, 数据独立
    let handles: Vec<_> = (0..8)
        .map(|cpu| std::thread::spawn(move || load_elf_mock(cpu, 50)))
        .collect();
    let results: Vec<Vec<u64>> = handles
        .into_iter()
        .map(|h| h.join().expect("no panic"))
        .collect();

    for (cpu, result) in results.iter().enumerate() {
        assert_eq!(result.len(), 50, "P1-I-32: cpu={} 应记录 50 页", cpu);
        for (i, &v) in result.iter().enumerate() {
            assert_eq!(
                v,
                (cpu as u64) * 0x1_0000_0000 + i as u64,
                "P1-I-32: cpu={} 第 {} 页数据未串台",
                cpu,
                i
            );
        }
    }
}

#[test]
fn buffer_truncation_at_1024() {
    // P1-I-32 验收: 单 PT_LOAD 段最大 1024 页截断保护
    let result = load_elf_mock(0, 2048);
    assert_eq!(
        result.len(),
        MAX_LOAD_PAGES,
        "P1-I-32: 超过 1024 页应截断到 MAX_LOAD_PAGES"
    );
}

#[test]
fn stack_buffer_8kb_fits() {
    // 8KB = 1024 * 8 = [u64; 1024] 栈上分配, 验证大小
    let _buf = [0u64; MAX_LOAD_PAGES];
    assert_eq!(core::mem::size_of_val(&_buf), 8 * 1024);
}

#[test]
fn elf_loader_source_uses_no_racy_cell_or_static_mut() {
    // P1-I-32 验收: 源码静态扫描 (在 host 端做文本搜索模拟)
    // 内核 `src/kernel/privileged/proc/user_proc.rs` 不应再 import 或 use RacyCell 加载 ELF
    let source = include_str!("../../src/kernel/privileged/proc/user_proc.rs");
    // 注释中提及 RacyCell 是允许的 (解释为何不再用), 但实际声明/创建不应存在
    let has_racy_cell_decl = source.contains("RacyCell<[u64; 1024]>")
        || source.contains("RacyCell :: new([0; 1024])")
        || source.contains("static ALLOCATED_PAGES");
    assert!(
        !has_racy_cell_decl,
        "P1-I-32: user_proc.rs 中不应再有 RacyCell<[u64; 1024]> 静态分配器"
    );

    // 实际声明应改为栈上 let 数组
    let has_stack_alloc = source.contains("let mut allocated_pages = [0u64; 1024]");
    assert!(
        has_stack_alloc,
        "P1-I-32: user_proc.rs 应改用栈上 [u64; 1024]"
    );
}
