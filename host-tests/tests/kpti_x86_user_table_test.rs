// SPDX-License-Identifier: MPL-2.0
// KPTI-07 / KPTI-08 / KPTI-10 / KPTI-11 回归测试: 用户页表内核映射装配面.
//
// 背景:
//   - KPTI-07: 用户页表只映射 `.text` 的**入口区段**
//     `_kernel_text_start ~ _kpti_trampoline_end` (收窄), 不再映射整段 `.text`.
//   - KPTI-08: 移除三处 `KERNEL_PML4[256..511]` 高半区整段复制, 改为逐页显式
//     映射"入口依赖面"必需内核页; 装配入口统一为 `kpti::assemble_kernel_half`,
//     每任务内核栈顶页由 `kpti::map_rsp0_page` 按任务追加.
//   - KPTI-10: `kpti_init` (共享 `USER_PML4` 模板) 与
//     `VirtualMemoryManager::create_user_page_table` (每进程页表) 共用同一装配函数,
//     保证两条路径的映射面恒等.
//   - KPTI-11: 用静态断言锁定上述不变式, 防止后续修改重新放大映射面.
//
// 验收:
//   1. `kpti.rs` 声明链接脚本符号 `_kpti_trampoline_end`
//   2. `map_kernel_pages_in_user_pml4` 以 `_kpti_trampoline_end` 作为映射上界
//   3. `map_text_region_in_user_pml4` 含收窄不变式断言 (fail-closed)
//   4. 装配入口统一: `kpti_init` / `create_user_page_table` / COW fork 三条路径
//      均经统一入口, 且**不再**出现高半区整段复制 (fail-closed)
//   5. 链接脚本把 `*(.kpti_trampoline)` 与 `other/build/isr.o(.text)` 排在
//      `_kpti_trampoline_end` 之前 (入口代码可取指)
//   6. 入口依赖面必需页 (USER_CR3_SAVE / IDT / per-CPU GDT 头区 / IST+栈顶页)
//      仍由 `map_kpti_data_pages` 逐页映射
//   7. `map_rsp0_page` 已接线到调度切换与用户态入口两条路径
//   8. `sys_fork` 经 `user_proc_clone` 为子进程注册 `UserProc` 镜像 (APS-05 根因回归)

use std::fs;

const KPTI: &str = "../src/kernel/framework/mm/kpti.rs";
const VMM_X86: &str = "../src/kernel/framework/mm/vmm_x86_64.rs";
const COW: &str = "../src/kernel/framework/mm/cow.rs";
const SCHED: &str = "../src/kernel/framework/proc/scheduler.rs";
const USER_PROC: &str = "../src/kernel/framework/proc/user_proc.rs";
const PROC_OPS: &str = "../src/kernel/framework/proc/proc_ops.rs";
const TSS: &str = "../src/kernel/framework/arch/x86_64/tss.rs";
const LINKER_LD: &str = "../src/kernel/framework/link/x86_64.ld";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释 (含 `///` 文档注释), 避免注释文本被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 断言 `a` 在 `b` 之前出现 (用于链接脚本排序).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay
        .find(a)
        .unwrap_or_else(|| panic!("{a:?} 未在链接脚本中找到"));
    let ib = hay
        .find(b)
        .unwrap_or_else(|| panic!("{b:?} 未在链接脚本中找到"));
    assert!(ia < ib, "{a:?} 必须排在 {b:?} 之前");
}

/// 从 `start` 起取最多 `len` 字节的窗口, 尾部对齐 UTF-8 字符边界
/// (源码含中文注释, 直接按字节切片会 panic).
fn window(src: &str, start: usize, len: usize) -> &str {
    let mut end = (start + len).min(src.len());
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    &src[start..end]
}

#[test]
fn test_kpti_declares_trampoline_end_symbol() {
    // 收窄上界依赖链接脚本符号 `_kpti_trampoline_end`; 未声明则收窄失效.
    let src = code_only(&read(KPTI));
    assert!(
        src.contains("static _kpti_trampoline_end"),
        "kpti.rs 必须声明链接脚本符号 _kpti_trampoline_end"
    );
}

#[test]
fn test_map_kernel_pages_uses_trampoline_end_as_upper_bound() {
    // 统一装配函数必须用 `_kpti_trampoline_end` (而非 `_kernel_text_end`) 作映射上界.
    let src = code_only(&read(KPTI));
    let def = src
        .find("pub unsafe fn map_kernel_pages_in_user_pml4(")
        .expect("kpti.rs 必须定义 map_kernel_pages_in_user_pml4");
    // 取函数体 (至下一个顶层 `\n}` 之前足够长的窗口)
    let body = window(&src, def, 1600);
    let trampoline = body
        .find("_kpti_trampoline_end")
        .unwrap_or_else(|| panic!("统一装配函数必须以 _kpti_trampoline_end 为映射上界"));
    // 映射调用须在上界取值之后
    let call = body
        .find("map_text_region_in_user_pml4(")
        .expect("统一装配函数必须调用 map_text_region_in_user_pml4");
    assert!(
        trampoline < call,
        "必须先用 _kpti_trampoline_end 取值, 再传给 map_text_region_in_user_pml4"
    );
}

#[test]
fn test_map_text_region_enforces_narrowing_invariant() {
    // 收窄不变式 (KPTI-07): fail-closed 断言, 越界即停机而非静默扩大隔离缺口.
    let src = code_only(&read(KPTI));
    let def = src
        .find("pub(super) unsafe fn map_text_region_in_user_pml4(")
        .expect("kpti.rs 必须定义 map_text_region_in_user_pml4");
    let body = window(&src, def, 1200);
    assert!(
        body.contains("assert!("),
        "map_text_region_in_user_pml4 必须含收窄不变式断言"
    );
    assert!(
        body.contains("text_end_phys <= trampoline_end"),
        "收窄断言判据必须是 text_end_phys <= trampoline_end"
    );
}

#[test]
fn test_both_paths_use_unified_assembler() {
    // KPTI-10 / KPTI-08: 三条装配路径均经统一入口.
    let kpti = code_only(&read(KPTI));
    assert!(
        kpti.contains("map_kernel_pages_in_user_pml4(user_pml4_phys);"),
        "kpti_init 必须调用统一装配函数"
    );

    let vmm = code_only(&read(VMM_X86));
    assert!(
        vmm.contains("assemble_kernel_half(pml4_phys.as_u64(), kernel_pml4);"),
        "create_user_page_table 必须经 kpti::assemble_kernel_half 装配"
    );
    // 深拷贝 `clone_user_page_table` 同样是"每进程用户页表"的装配点 (经
    // `vmm_clone_user_page_table` 公开导出): 此前它自行整段复制内核高半区,
    // 在 KPTI 激活下会重新注入完整内核高半区别名 ⇒ 必须同源.
    assert!(
        vmm.contains("assemble_kernel_half(child_pml4_phys.as_u64(), kernel_pml4);"),
        "深拷贝 clone_user_page_table 必须经 kpti::assemble_kernel_half 装配"
    );

    let cow = code_only(&read(COW));
    assert!(
        cow.contains("assemble_kernel_half(child_pml4_phys.as_u64(), kernel_pml4);"),
        "COW fork 必须经 kpti::assemble_kernel_half 装配"
    );

    // 两处调用点不得再直接调用内部装配步骤 (否则映射面可能发散).
    assert!(
        !vmm.contains("map_text_region_in_user_pml4"),
        "vmm_x86_64.rs 不得直接调用 map_text_region_in_user_pml4 (须走统一入口)"
    );
    assert!(
        !vmm.contains("map_kpti_data_pages"),
        "vmm_x86_64.rs 不得直接调用 map_kpti_data_pages (须走统一入口)"
    );
    assert!(
        !cow.contains("map_text_region_in_user_pml4") && !cow.contains("map_kpti_data_pages"),
        "cow.rs 不得直接调用内部装配步骤 (须走统一入口)"
    );
}

#[test]
fn test_no_kernel_half_bulk_copy_in_per_process_paths() {
    // KPTI-08 fail-closed: 每进程用户页表路径不得再整段复制 `KERNEL_PML4[256..511]`.
    //
    // 判据取"高半区整段复制"的文本指纹 `add(256)` (复制源/目的均落在 PML4 第 256 项),
    // 而非宽松的 `copy_nonoverlapping` —— 后者在低半区数据页深拷贝等正常用途中也出现.
    let vmm = code_only(&read(VMM_X86));
    assert!(
        !vmm.contains("add(256)"),
        "vmm_x86_64.rs 不得再整段复制内核高半区 (create_user_page_table 与深拷贝 \
         clone_user_page_table 均已统一到 assemble_kernel_half)"
    );

    // COW fork: 仅允许 `#[cfg(not(target_arch = "x86_64"))]` 退化分支保留该复制;
    // 该分支之前的 x86_64 装配分支必须不含.
    let cow = code_only(&read(COW));
    let guard = cow
        .find("#[cfg(not(target_arch = \"x86_64\"))]")
        .expect("cow.rs 高半区装配须显式区分 x86_64 与非 x86_64 分支");
    assert!(
        !cow[..guard].contains("add(256)"),
        "cow.rs 的 x86_64 装配分支不得整段复制内核高半区"
    );
    assert_eq!(
        cow.lines().filter(|l| l.contains("add(256)")).count(),
        1,
        "cow.rs 只允许非 x86_64 退化分支存在一处高半区整段复制"
    );

    // 统一入口内必须保留退化分支 (KPTI 未激活时整段复制是唯一可达内核高半区的途径).
    let kpti = code_only(&read(KPTI));
    let def = kpti
        .find("pub unsafe fn assemble_kernel_half(")
        .expect("kpti.rs 必须定义 assemble_kernel_half");
    let body = window(&kpti, def, 1200);
    assert!(
        body.contains("kpti_is_active()"),
        "assemble_kernel_half 必须按 KPTI 激活与否分支"
    );
    assert!(
        body.contains("copy_nonoverlapping") && body.contains("add(256)"),
        "assemble_kernel_half 未激活分支必须保留 KERNEL_PML4[256..] 整段复制"
    );
}

#[test]
fn test_removed_apis_are_gone() {
    // KPTI-08 的删除项若被重新引入, 隔离面会被静默放大 ⇒ fail-closed 断言.
    let vmm = code_only(&read(VMM_X86));
    assert!(
        !vmm.contains("kpti_sync_pml4_entry"),
        "kpti_sync_pml4_entry 已删除 (复制 PML4 顶层指针 = 共享整棵子树)"
    );
    assert!(
        !vmm.contains("fn map_kernel_page_in_table"),
        "map_kernel_page_in_table 已删除 (唯一调用者迁至 map_rsp0_page)"
    );
    let tss = code_only(&read(TSS));
    assert!(
        !tss.contains("fn tss_get_kernel_stack"),
        "tss_get_kernel_stack 已删除 (KPTI-08 后无调用者)"
    );
}

#[test]
fn test_map_kpti_data_pages_covers_entry_dependency_face() {
    // 入口依赖面必需页: USER_CR3_SAVE / IDT / per-CPU GDT 头区 / IST+trampoline 栈顶页.
    let kpti = code_only(&read(KPTI));
    let def = kpti
        .find("pub(super) unsafe fn map_kpti_data_pages(")
        .expect("kpti.rs 必须定义 map_kpti_data_pages");
    let body = window(&kpti, def, 6000);
    for needle in [
        "USER_CR3_SAVE_ASM",
        "idt_entries_base_lma()",
        "per_cpu_gdt_head_range(cpu)",
        "ist_tops_virt(cpu)",
        "trampoline_top_virt(cpu)",
    ] {
        assert!(
            body.contains(needle),
            "map_kpti_data_pages 必须映射入口依赖面必需页: {needle}"
        );
    }
    // 时序约束: 全部地址须由静态布局推导, 不得读运行时 TSS / sidt.
    assert!(
        !body.contains("sidt") && !body.contains("get_tss_mut"),
        "map_kpti_data_pages 在 gdt_init/idt_init 之前执行, 不得读运行时 TSS / IDTR"
    );
}

#[test]
fn test_map_text_region_maps_all_three_aliases() {
    // KPTI-08 实证: LSTAR 与 IDT 门目标走 `KERNEL_BASE + LMA`, 汇编绝对寻址走 LMA,
    // 链接脚本 `_kernel_text_vma` 镜像别名需保留 ⇒ 每物理页映射 3 个别名.
    let kpti = code_only(&read(KPTI));
    let def = kpti
        .find("pub(super) unsafe fn map_text_region_in_user_pml4(")
        .expect("kpti.rs 必须定义 map_text_region_in_user_pml4");
    let body = window(&kpti, def, 2600);
    for needle in [
        "map_text_page(user_pml4, phys, phys,",
        "KERNEL_BASE + phys,",
        "LINKER_VMA_OFFSET + phys,",
    ] {
        assert!(
            body.contains(needle),
            "map_text_region_in_user_pml4 必须映射别名: {needle}"
        );
    }
}

#[test]
fn test_map_rsp0_page_is_wired_on_switch_and_enter() {
    // KPTI-08: 内核栈顶页随任务变化, 不在统一装配面内 ⇒ 必须在
    // (a) 上下文切换 (COW fork 子进程页表靠此处补齐) 与
    // (b) 用户态入口 (init 不经调度器) 两处按任务映射.
    let sched = code_only(&read(SCHED));
    let proc = code_only(&read(USER_PROC));
    assert!(
        sched.contains("map_rsp0_page(user_cr3, next_kernel_stack)"),
        "调度切换路径必须为切换目标映射内核栈顶页"
    );
    assert!(
        proc.contains("map_rsp0_page(cr3, kstack)"),
        "用户态入口路径必须为本任务映射内核栈顶页"
    );

    // 该映射不得设 USER 位 (内核栈暴露给用户态 = 提权风险):
    // 权限字面量必须是 PRESENT|WRITABLE (0x3), 而非含 USER 的 0x7.
    let kpti = code_only(&read(KPTI));
    let def = kpti
        .find("pub unsafe fn map_rsp0_page(")
        .expect("kpti.rs 必须定义 map_rsp0_page");
    let body = window(&kpti, def, 1000);
    assert!(
        body.contains("0x3"),
        "map_rsp0_page 必须用 PRESENT|WRITABLE (0x3)"
    );
    assert!(
        !body.contains("0x7"),
        "map_rsp0_page 不得设 USER 位 (0x7) —— 内核栈暴露给用户态即提权"
    );
}

#[test]
fn test_sys_fork_registers_child_in_user_proc_manager() {
    // APS-05 回归: `sys_fork` 必须为子进程创建 `UserProc` 镜像记录.
    //
    // 该注册是调度切换路径装配 per-CPU 用户 CR3 与"任务内核栈顶页"的门控
    // (`Scheduler::schedule` 以 `USER_PROC_MANAGER.get(next)` 为前置): 子进程未
    // 注册时被投送到次核, per-CPU user CR3 滞留旧值、其内核栈顶页也不在其用户
    // 页表内 ⇒ 用户态被硬件中断打断按 TSS.RSP0 压帧即 #PF (cr2=内核栈顶-8).
    // 该注册块曾在 4557cd30 重写 `sys_fork` 时被附带删除, 导致 x86_64 EL0 观测
    // 偶发未成对; fail-closed: 注册失败须回滚进程表条目.
    let src = code_only(&read(PROC_OPS));
    let start = src
        .find("pub extern \"C\" fn sys_fork()")
        .expect("proc_ops.rs 必须定义 sys_fork");
    let after = &src[start..];
    let end = after
        .find("\npub extern \"C\" fn proc_get_ppid")
        .unwrap_or(after.len());
    let body = &after[..end];

    let insert = body
        .find("PROCESS_TABLE.insert(child as *const Process as *mut Process);")
        .expect("sys_fork 必须把子进程插入进程表");
    let clone = body
        .find("user_proc_clone(parent_pid, child_pid)")
        .expect("sys_fork 必须经 user_proc_clone 为子进程注册 UserProc 镜像 (APS-05)");
    assert!(
        insert < clone,
        "UserProc 注册必须在进程表插入之后 (此时子进程页表/kstack 已就绪)"
    );
    assert!(
        body.contains("PROCESS_TABLE.remove_and_free(child_pid);"),
        "user_proc_clone 失败必须 fail-closed 回滚进程表条目"
    );
}

#[test]
fn test_linker_places_entry_code_before_trampoline_end() {
    // 链接脚本排序是 KPTI-07 的前提: 入口 stub 必须落在映射区段内.
    let ld = read(LINKER_LD);
    assert_before(&ld, "_kernel_text_start", "*(.kpti_trampoline)");
    assert_before(&ld, "*(.kpti_trampoline)", "_kpti_trampoline_end");
    assert_before(
        &ld,
        "other/build/isr.o(.text .text.*)",
        "_kpti_trampoline_end",
    );
    assert_before(&ld, "_kpti_trampoline_end", "_kernel_text_end");
    // `.trampoline` (AP 启动) 不映射进用户页表, 必须排在收窄上界之后.
    assert_before(&ld, "_kpti_trampoline_end", "*(.trampoline)");
}
