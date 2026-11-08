// SPDX-License-Identifier: MPL-2.0
// A-16 甲 (2026-11-08) 源码形状锁: 地址空间查询的单一真源 = 当前任务的 `Process.mm`.
//
// 为何是静态锁: "按任务取 mm" 的运行期语义由内核侧用例 `mm::vma_current::follows_task`
// (真任务切换) 承担, host 跑不了裸机进程表/调度器; 此处锁住**形状**, 防止回退:
//   1. 全局 `CURRENT_MM` 槽不得复活 —— 它只在 execve 写一次, 非 per-CPU 且不随
//      fork/clone/切换更新, 曾把合法页判成"不属任何 VMA" (真机 2026-11-07 两次停机).
//   2. 访问器必须经 `process_get_current_pid` + `PROCESS_TABLE.with_process` 读/写
//      **当前任务槽位**, 且表锁只在闭包内做一次原子读写 (层级 7 不与层级 3 的
//      `MmStruct::vmas` 嵌套).
//   3. `Process` 必须携带 `mm: AtomicPtr<MmStruct>` 字段, fork 与 CLONE_VM 两条
//      继承路径都必须复制父指针 —— 缺任一条, 子任务读到 None 或旧地址空间.
//   4. EL0 缺页入口按当前任务取 mm; 核态 uaccess 的 COW 窄支路**不得**取 (它在
//      可能持有 `PROCESS_TABLE` 的内核上下文里触发, 再进表锁即自死锁).

use std::fs;

const VMA: &str = "../src/kernel/privileged/mm/vma.rs";
const PROCESS: &str = "../src/kernel/privileged/proc/process.rs";
const PROC_OPS: &str = "../src/kernel/privileged/proc/proc_ops.rs";
const CLONE: &str = "../src/kernel/functions/proc/clone.rs";
const PAGE_FAULT: &str = "../src/kernel/privileged/mm/page_fault.rs";
const ELF: &str = "../src/kernel/privileged/proc/elf/mod.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释 (`//` / `///`), 避免说明文字被当成代码读取.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 取函数体文本 (从签名后的第一个 `{` 到配平的 `}`), 用于把断言收紧到目标函数内.
fn body_of<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("缺少签名 {signature:?}"));
    let rest = &src[start..];
    let open = rest.find('{').expect("签名后应有函数体");
    let mut depth = 0usize;
    for (i, b) in rest.bytes().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[open..=i];
                }
            }
            _ => {}
        }
    }
    panic!("函数体不闭合: {signature:?}");
}

/// 1. 全局槽不得复活.
#[test]
fn test_current_mm_global_slot_removed() {
    let vma = code_only(&read(VMA));
    assert!(
        !vma.contains("CURRENT_MM"),
        "全局 CURRENT_MM 槽不得复活: 它只在 execve 写一次, 非 per-CPU 且不随 \
         fork/clone/切换更新 ⇒ VMA 与页表可能取自不同进程 (A-16 真机取证)"
    );
}

/// 2. 访问器经当前任务槽位读写 (而非静态槽).
#[test]
fn test_accessors_route_through_current_task() {
    let vma = code_only(&read(VMA));

    let get = body_of(&vma, "pub fn get_current_mm");
    assert!(
        get.contains("process_get_current_pid()"),
        "get 必须按当前任务 pid 取 mm (单一真源)"
    );
    assert!(
        get.contains(".mm.load(Ordering::Acquire)"),
        "get 必须读当前任务槽位的 mm 指针"
    );
    assert!(
        get.contains("PROCESS_TABLE"),
        "get 必须经进程表定位当前任务 (返回前释放表锁, 不与层级 3 嵌套)"
    );
    assert!(
        get.contains("pid == 0"),
        "无当前任务 (pid 0) 时必须直接返回 None, 不得进表查询"
    );

    let set = body_of(&vma, "pub fn set_current_mm");
    assert!(
        set.contains("process_get_current_pid()"),
        "set 必须写入当前任务槽位"
    );
    assert!(
        set.contains(".mm.store(ptr, Ordering::Release)"),
        "set 必须以 Release 语义发布 mm 指针"
    );

    let elf = code_only(&read(ELF));
    assert!(
        elf.contains("vma_set_current_mm"),
        "ELF 装载是 mm 的唯一挂载点, 不得绕过访问器直写 Process.mm"
    );
}

/// 3. `Process` 携带 mm 指针; fork 与 `CLONE_VM` 两条继承路径都复制父指针.
#[test]
fn test_process_field_and_both_inherit_paths() {
    let process = code_only(&read(PROCESS));
    assert!(
        process.contains("pub mm: AtomicPtr<MmStruct>"),
        "Process 必须携带 mm: AtomicPtr<MmStruct> (当前任务真源的载体)"
    );
    assert!(
        process.contains("mm: AtomicPtr::new(core::ptr::null_mut())"),
        "Process::new 必须把 mm 初始化为 null (未挂载态)"
    );

    let ops = code_only(&read(PROC_OPS));
    let fork_inherit = body_of(&ops, "fn inherit_parent_state");
    assert!(
        fork_inherit.contains("p.mm.load(Ordering::Acquire)"),
        "fork 必须读取父进程 mm"
    );
    assert!(
        fork_inherit.contains("child.mm.store(mm, Ordering::Release)"),
        "fork 必须把父 mm 指针继承给子进程 (VMA 描述符跨 fork 共享)"
    );

    let clone = code_only(&read(CLONE));
    let vm_inherit = body_of(&clone, "fn vm_child_inherit");
    assert!(
        vm_inherit.contains("p.mm.load(Ordering::Acquire)"),
        "CLONE_VM 必须读取父线程 mm (线程共享地址空间)"
    );
    assert!(
        vm_inherit.contains("child.mm.store(parent_mm, Ordering::Release)"),
        "CLONE_VM 必须把父 mm 指针继承给子线程"
    );
}

/// 4. EL0 缺页入口取当前任务; 核态 uaccess 窄支路不得取.
#[test]
fn test_fault_entry_vs_kernel_uaccess_branch() {
    let pf = code_only(&read(PAGE_FAULT));

    let el0 = body_of(&pf, "pub fn handle_user_page_fault");
    assert!(
        el0.contains("super::vma::get_current_mm()"),
        "EL0 缺页的 VMA 优先路必须按当前任务取 mm"
    );

    let kua = body_of(&pf, "pub fn break_cow_for_kernel_uaccess");
    assert!(
        !kua.contains("get_current_mm()"),
        "核态 uaccess 窄支路不得取当前任务 mm: 它在可能持有 PROCESS_TABLE 的内核 \
         上下文触发 (clone 在 with_process 闭包内写 TID), 再进表锁即自死锁; 且 COW \
         证据是 per-frame 标记, 与 VMA 无关"
    );
}
