// SPDX-License-Identifier: MPL-2.0
// 回归测试: 退出进程的 `USER_PROC_MANAGER` 镜像记录回收契约 (ISSUE-RT-006).
//
// 背景:
//   `USER_PROC_MANAGER.processes` 是 `Process` 的 FFI 镜像 (BTreeMap<pid,
//   NonNull<UserProcess>>). 镜像 destroy 负责在权威 `Process` 析构前移除镜像
//   记录并归还镜像自身的堆分配. 镜像字段是**非拥有句柄**: cr3 与内核栈的
//   所有权都在权威 `Process` 上, 用户栈物理页随 `destroy_user_page_table` 的
//   USER leaf 单源归还. 该路径此前在生产路径无调用者 ⇒ 进程退出后镜像记录
//   与栈页永不回收 (每 fork 泄漏一条).
//
// 根因与接线 (本轮):
//   全部权威 `Process` 销毁都收敛到 `ProcessTable::remove_and_free` /
//   `dec_ref_and_maybe_free` 的"引用归零即释放"分支 (scheduler 周期僵尸回收 /
//   exit 孤儿回收 / wait4 / fork 回滚均经此). 回收必须在该分支、`Box::from_raw`
//   **之前** 完成 —— 镜像先于权威对象消失 (INV-USER-PROC #2), 否则镜像的
//   `process` 句柄在 PCB 释放后成为悬空指针.
//
// A-8 (cr3 单点减计):
//   `destroy` 旧实现与 `Process::drop` 对同一 cr3 各调一次 `frame_dec`, 两次
//   调用之间该帧可被其他核重新分配 —— 第二次递减把新持有者的计数抹到 0 并
//   误销毁其在用页表. 现收敛为 `Process::drop` 唯一减计点, 镜像 destroy 不再
//   触碰 cr3; exec 旧表由 `replace_user_space` 显式注销 (权威 Process 存活且
//   其 cr3 字段即将被新值覆盖, 它的 `drop` 只看到新表).
//
// A-9 (镜像分配归还):
//   镜像 destroy 摘表后与 `alloc_user_process` 配对调用 `free_user_process`.
//   exec 交接后的临时进程与装配失败回滚同走 `discard_unrun` 单源 (写 Zombie →
//   摘镜像并归还分配 → PCB 释放 → PID 回收), 不再有"只摘索引不归还"的分离路径.
//
// 验收 (fail-closed, 文本指纹):
//   1. `remove_and_free` 在 `Box::from_raw` 前调用 `USER_PROC_MANAGER.destroy_by_pid`
//   2. `dec_ref_and_maybe_free` (延迟释放分支) 同样回收
//   3. `UserProcManager::destroy` **不再**自己拆页表 / 归还任何物理页:
//      用户帧由地址空间拆除的 `destroy_user_page_table` 单源承担, cr3 减计由
//      `Process::drop` 单点承担 (见 `user_proc_frame_release_test.rs`).
//      本用例改锁这一契约 —— 旧形态"先还栈页再拆页表"看似解决了 cr3 翻译依赖,
//      实为同一释放的第二份平行实现, 绕过持有计数门与 TLB 追平延迟释放, 实测
//      表现为 `[PMM] Warn: double free` 与启动挂死 / 内核态 #GP;
//      登记见 docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-15.
//   4. `destroy_by_pid` 取句柄与 `destroy` 的锁分离 (避免 `if let` scrutinee 的
//      guard 存活到 then 块造成同锁自锁死)
//   5. 镜像 destroy 与 `alloc_user_process` 配对释放 (A-9); exec 临时进程走
//      `discard_unrun` 单源, `detach_by_pid` 的"只摘索引"形态已删除.

use std::fs;

const PROCESS: &str = "../src/kernel/privileged/proc/process.rs";
const USER_PROC: &str = "../src/kernel/privileged/proc/user_proc.rs";
const PROC_OPS: &str = "../src/kernel/privileged/proc/proc_ops.rs";

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

/// 从 `start` 起取最多 `len` 字节的窗口, 尾部对齐 UTF-8 字符边界
/// (源码含中文注释, 直接按字节切片会 panic).
fn window(src: &str, start: usize, len: usize) -> &str {
    let mut end = (start + len).min(src.len());
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    &src[start..end]
}

fn body_of<'a>(src: &'a str, def: &str, len: usize) -> &'a str {
    let at = src.find(def).unwrap_or_else(|| panic!("未找到定义: {def}"));
    window(src, at, len)
}

/// 取 `def` 到 `next` 之间的窗口: 用于需要对整个函数体做"不得出现"断言的场景,
/// 避免固定长度窗口越界到下一个函数造成误判.
fn body_until<'a>(src: &'a str, def: &str, next: &str) -> &'a str {
    let at = src.find(def).unwrap_or_else(|| panic!("未找到定义: {def}"));
    let rest = &src[at..];
    let end = rest
        .find(next)
        .unwrap_or_else(|| panic!("未找到下一个定义: {next}"));
    &rest[..end]
}

#[test]
fn test_remove_and_free_reclaims_user_proc_mirror() {
    // 权威销毁的唯一入口分支: 必须在 Process 析构前回收镜像.
    let src = code_only(&read(PROCESS));
    let body = body_of(&src, "pub fn remove_and_free(&self, pid: Pid) {", 1400);
    let reclaim = body
        .find("USER_PROC_MANAGER.destroy_by_pid(pid)")
        .unwrap_or_else(|| panic!("remove_and_free 必须回收 USER_PROC_MANAGER 镜像记录"));
    let drop_box = body
        .find("Box::from_raw")
        .expect("remove_and_free 必须仍以 Box::from_raw 释放 Process");
    assert!(
        reclaim < drop_box,
        "镜像回收必须早于 Process 析构 (Box::from_raw): 镜像先于权威对象消失 \
         (INV-USER-PROC #2), 否则镜像的 process 句柄悬空"
    );
}

#[test]
fn test_dec_ref_and_maybe_free_reclaims_user_proc_mirror() {
    // 引用计数 > 1 时实际释放推迟到本分支, 同样必须回收镜像.
    let src = code_only(&read(PROCESS));
    let body = body_of(
        &src,
        "pub fn dec_ref_and_maybe_free(&self, pid: Pid) {",
        1400,
    );
    let reclaim = body
        .find("USER_PROC_MANAGER.destroy_by_pid(pid)")
        .unwrap_or_else(|| panic!("dec_ref_and_maybe_free 必须回收 USER_PROC_MANAGER 镜像记录"));
    let drop_box = body
        .find("Box::from_raw")
        .expect("dec_ref_and_maybe_free 必须仍以 Box::from_raw 释放 Process");
    assert!(
        reclaim < drop_box,
        "延迟释放分支的镜像回收必须早于 Process 析构 (Box::from_raw)"
    );
}

/// A-8 谓词: 镜像 destroy 是否触碰 cr3 (触碰即为与 `Process::drop` 的重复减计).
fn mirror_destroy_touches_cr3(body: &str) -> bool {
    body.contains("frame_dec(") || body.contains("destroy_user_page_table(")
}

/// A-9 谓词: 摘表与释放配对, 且摘表在前.
fn mirror_destroy_frees_allocation(body: &str) -> bool {
    match (
        body.find(".remove(&pid)"),
        body.find("raw::free_user_process("),
    ) {
        (Some(remove), Some(free)) => remove < free,
        _ => false,
    }
}

#[test]
fn test_mirror_destroy_defers_cr3_and_stack_to_process_drop() {
    // A-8: cr3 减计/销毁只在 `Process::drop`; 镜像 destroy 不触碰地址空间与栈.
    // 用户帧归还单源 = 拆页表时逐 USER leaf 走归零门; 内核栈归还单源 =
    // `Process::drop` 按分配时记录的页数.
    let src = code_only(&read(USER_PROC));
    let body = body_until(
        &src,
        "fn destroy(&self, proc: NonNull<UserProcess>) {",
        "fn discard_unrun_process(",
    );
    assert!(
        !mirror_destroy_touches_cr3(body),
        "镜像 destroy 不得触碰 cr3: 与 Process::drop 争同一计数槽, 帧被重新分配后\
         第二次递减会把新持有者的计数抹到 0 并误销毁在用页表 (A-8)"
    );
    assert!(
        !body.contains("raw::free_phys_page(") && !body.contains("raw::free_phys_pages("),
        "destroy 不得再按固定栈区间归还用户栈 / 内核栈: 前者与 destroy_user_page_table \
         争同一帧 (绕过 frame_dec 门 + TLB 追平), 后者与 Process::drop 争同一组帧"
    );
}

#[test]
fn test_process_drop_is_the_single_cr3_decrement_point() {
    let src = code_only(&read(PROCESS));
    let body = body_until(&src, "impl Drop for Process {", "pub struct ProcessTable {");
    assert!(
        body.contains(".frame_dec(") && body.contains("vmm_destroy_page_table("),
        "Process::drop 必须保留 cr3 归零门 + 销毁: 销毁路径的唯一减计点 (A-8)"
    );
}

#[test]
fn test_mirror_destroy_frees_user_process_allocation() {
    // A-9: 摘表与释放配对; 释放以摘表成功为门 (并发重复 destroy 不得二次释放).
    let src = code_only(&read(USER_PROC));
    let body = body_until(
        &src,
        "fn destroy(&self, proc: NonNull<UserProcess>) {",
        "fn discard_unrun_process(",
    );
    assert!(
        mirror_destroy_frees_allocation(body),
        "destroy 必须与 alloc_user_process 配对归还镜像分配 (A-9), 且先摘表再释放"
    );
    assert!(
        body.contains("if removed"),
        "释放必须以摘表成功为门: 并发重复 destroy 不得二次释放"
    );
}

#[test]
fn test_exec_temp_process_uses_single_discard_path() {
    // exec 交接后的临时进程与三个失败回滚点同走 `discard_unrun` 单源;
    // 旧 detach_by_pid 只摘索引不归还镜像分配 (A-9 同形泄漏).
    let src = code_only(&read(PROC_OPS));
    assert!(
        !src.contains("detach_by_pid"),
        "detach_by_pid 的\"只摘索引不归还\"形态必须由 discard_unrun 取代"
    );
    assert_eq!(
        src.matches("USER_PROC_MANAGER.discard_unrun(").count(),
        4,
        "exec 三个失败点 + 交接成功点都必须走 discard_unrun 单源"
    );
}

#[test]
fn test_destroy_by_pid_releases_lock_before_destroy() {
    // destroy() 内部会重新获取同一把 IrqSpinLock; 若取句柄与 destroy 同处
    // `if let` 分支 (scrutinee 的 guard 存活到 then 块结束) 将自锁死.
    let src = code_only(&read(USER_PROC));
    let def = "pub fn destroy_by_pid(&self, pid: u32) {";
    let body = body_of(&src, def, 900);
    assert!(
        body.contains("let proc = self.processes.lock().get(&pid).copied();"),
        "{def} 必须在独立语句中取句柄 (guard 随语句结束释放)"
    );
    assert!(
        !body.contains("if let Some(proc) = self.processes.lock()"),
        "{def} 不得在 `if let` scrutinee 中持锁取句柄 (会自锁死)"
    );
}

#[test]
fn test_kernel_process_alloc_symmetric_with_table_free() {
    // `create()` 产出的权威 `Process` 最终由 `ProcessTable::remove_and_free` /
    // `dec_ref_and_maybe_free` 的 `Box::from_raw` 归还 (全局分配器, 块首带 8 字节
    // 归还路由标签). 分配侧若退回裸 `kmalloc` (`raw::alloc_zeroed`), 归还侧会把
    // 堆上相邻字节当标签; 且 `size_of::<Process>() > PAGE_THRESHOLD` 时
    // `KernelAllocator::dealloc` 走 PMM 分支, 把 kmalloc 堆页按物理帧归还 ——
    // 该页随后可被 PMM 二次分配并清零, 活堆数据静默消失 (K-07b 批2 exec 真跑
    // 命中的 `Arc<FsContext>` 被清零即此形). 分配/归还必须同源.
    let src = code_only(&read(USER_PROC));
    let alloc = body_until(
        &src,
        "pub fn alloc_kernel_process() -> Option<*mut Process> {",
        "pub unsafe fn free_kernel_process",
    );
    assert!(
        alloc.contains("alloc::alloc::alloc(layout)"),
        "alloc_kernel_process 必须走全局分配器 (与进程表 Box::from_raw 归还同源)"
    );
    assert!(
        !alloc.contains("raw::alloc_zeroed"),
        "alloc_kernel_process 不得退回裸 kmalloc (无归还路由标签, 与 Box 归还失配)"
    );

    let free = body_until(
        &src,
        "pub unsafe fn free_kernel_process(kproc_ptr: *mut Process) {",
        "pub unsafe fn free_user_process",
    );
    assert!(
        free.contains("alloc::alloc::dealloc("),
        "free_kernel_process 必须与 alloc_kernel_process 同走全局分配器"
    );
    assert!(
        !free.contains("mm::kfree("),
        "free_kernel_process 不得退回裸 kfree (与全局分配器块布局失配)"
    );
}
