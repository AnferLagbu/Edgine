//! L1-05 契约断言: aarch64 EL1 视图只承载用户页, 不含内核 DRAM 块.
//!
//! 迁移前内核驻低半区 (`KERNEL_BASE = 0`), 故 EL1 视图必须以 `L1_el1[1]` =
//! 内核 `L1_IDMAP[1]` 的 DRAM 1 GiB 块把镜像/数据/内核栈纳入 `TTBR0`. 迁移到
//! 高半区后内核经 `TTBR1` 可达, 该块成为冗余映射面 —— 用户进程页表不该附带
//! DRAM/MMIO 面. 本测试按源码结构锁定 `build_el1_view` 的形态, 防止 DRAM 块回潮.

use std::fs;

const VMM: &str = "../src/kernel/privileged/mm/vmm_aarch64.rs";
const EXCEPTION: &str = "../src/kernel/privileged/arch/aarch64/exception.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 压缩空白, 便于跨缩进/换行匹配.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 截取 `begin` 到 `end` 之间的源码 (不含 `end` 标记).
fn slice_between<'a>(src: &'a str, begin: &str, end: &str) -> &'a str {
    let start = src
        .find(begin)
        .unwrap_or_else(|| panic!("未找到起点标记: {begin}"));
    let rest = &src[start..];
    let stop = rest
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点标记: {end}"));
    &rest[..stop]
}

#[test]
fn test_build_el1_view_has_no_dram_block() {
    let src = read(VMM);
    // 起点取函数签名 (doc 注释中的历史说明不参与断言).
    let body = norm(slice_between(
        &src,
        "pub fn build_el1_view",
        "fn destroy_el1_view",
    ));

    // ① 不得再查/写内核 L1_IDMAP 的 DRAM 块描述符.
    assert!(
        !body.contains("l1_idmap"),
        "L1-05: build_el1_view 不得再查内核 L1_IDMAP (DRAM 块已从 EL1 视图移除)"
    );
    assert!(
        !body.contains("dram_desc"),
        "L1-05: build_el1_view 不得再出现 dram_desc"
    );
    assert!(
        !body.contains("L1_IDMAP"),
        "L1-05: build_el1_view 不得再引用 L1_IDMAP"
    );
    // ② L1_el1 只写槽位 0 (→ 共享 L2_u), 不得写其余槽位 (如 DRAM 块槽位 1).
    assert!(
        !body.contains("el1_l1_ptr.add"),
        "L1-05: L1_el1 只应有槽位 0 (→ L2_u); 不得写其它槽位"
    );
    assert!(
        body.contains("write_volatile(el1_l1_ptr, l2_u_desc)"),
        "L1-05: L1_el1[0] 必须指向共享的 L2_u"
    );
}

#[test]
fn test_el0_sync_entry_comment_drops_dram_claim() {
    // 入口注释同步: 不得再宣称 EL1 视图含内核 DRAM 块.
    let src = read(EXCEPTION);
    let body = norm(slice_between(
        &src,
        "handle_el0_sync:",
        "// -------- EL0 IRQ handler --------",
    ));
    assert!(
        !body.contains("内核 DRAM 块"),
        "L1-05: handle_el0_sync 注释不得再称 EL1 视图含内核 DRAM 块"
    );
}
