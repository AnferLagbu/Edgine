//! MM 生命周期锁: `MmStruct` 的持有计数与**唯一释放**路径 (M5)
//!
//! M1–M4 期间 `MmStruct` 只建不放: 装载路径 `Box::into_raw` 交给 `Process::mm`,
//! 进程退出与 exec 换表都不注销, VMA 容器随进程数单调增长. M5 的收口形态是
//! 「创建者 1 / fork·clone 继承 +1 / 退出或换表 -1 / 归零回收」:
//!
//! - 计数与**页表 (cr3) 帧计数分层** —— fork 的 COW 克隆产生新 cr3, 但 VMA
//!   描述符仍与父共享同一 `MmStruct`, 故不能用 cr3 计数代替 mm 计数;
//! - 注销点必须收敛到两处: `Process::drop`（进程退出）与
//!   `UserProcManager::replace_user_space`（exec 旧 mm），否则先退出的一方会
//!   拆掉仍被 fork 子进程使用的 VMA 描述符;
//! - exec 的新 mm 是**所有权转移**（临时进程 → 当前进程），不得再 retain 一次,
//!   源槽位只清空、不 release.
//!
//! # 断言方式
//!
//! 源码文本契约（同 `user_proc_frame_release_test.rs` 的口径）: 剥注释后截函数体,
//! 只锁语义不锁排版; 负控制用合成回退样本反向验证谓词.

use std::fs;

const VMA_RS: &str = "../src/kernel/privileged/mm/vma.rs";
const PROCESS_RS: &str = "../src/kernel/privileged/proc/process.rs";
const USER_PROC_RS: &str = "../src/kernel/privileged/proc/user_proc.rs";
const PROC_OPS_RS: &str = "../src/kernel/privileged/proc/proc_ops.rs";
const CLONE_RS: &str = "../src/kernel/functions/proc/clone.rs";
const TEST_MM_RS: &str = "../src/kernel/privileged/tests/test_mm.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 生产代码文本: 剥注释行, 并在第一个 `#[cfg(test)]` 处截断.
fn prod_code(src: &str) -> String {
    src.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with("/*") || t.starts_with('*'))
        })
        .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 截取以 `begin` 起始的完整函数/块文本 (结束判据: 与首行同缩进的 `}`).
fn item_from(src: &str, begin: &str) -> String {
    let mut out = String::new();
    let mut close = String::new();
    let mut started = false;
    for line in src.lines() {
        if !started {
            if !line.contains(begin) {
                continue;
            }
            close = " ".repeat(line.len() - line.trim_start().len());
            close.push('}');
            started = true;
        }
        out.push_str(line);
        out.push('\n');
        if line == close {
            break;
        }
    }
    assert!(started, "未找到起点: {begin}");
    out
}

/// 截取**最后一次**以 `signature` 起始的函数体（同名桩在前、实体在后的文件用）.
fn body_last(src: &str, signature: &str) -> String {
    let start = src
        .rfind(signature)
        .unwrap_or_else(|| panic!("未找到签名: {signature}"));
    let rest = &src[start..];
    let open = rest
        .find('{')
        .unwrap_or_else(|| panic!("签名后无函数体: {signature}"));
    let mut depth = 0usize;
    for (i, b) in rest.bytes().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return rest[open..=i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("函数体不闭合: {signature}");
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

// ============================================================================
// 谓词
// ============================================================================

/// 释放只发生在 `mm_release` 内, 且归零判据在同一个函数体内.
fn release_is_single_and_gated(body: &str) -> bool {
    body.contains("ref_count.fetch_sub") && body.contains("Box::from_raw") && {
        // `prev == 1` 才回收: 先退出者只减计数
        body.contains("prev == 1") || body.contains("if prev == 1")
    }
}

/// 进程退出注销 mm: Drop 读槽位一次、release 一次.
fn drop_releases_mm_once(body: &str) -> bool {
    count(body, "self.mm.load") == 1 && count(body, "mm_release(mm)") == 1
}

/// fork/clone 继承必须登记第二份引用, 而不是裸指针拷贝.
fn inherit_registers_reference(body: &str) -> bool {
    body.contains("mm_retain(mm)") && body.contains("self.mm.store")
}

// ============================================================================
// 锁 1: 释放形态与唯一销毁点
// ============================================================================

#[test]
fn mm_release_frees_only_on_last_reference() {
    let src = read(VMA_RS);
    let body = item_from(&prod_code(&src), "pub unsafe fn mm_release(");
    assert!(
        release_is_single_and_gated(&body),
        "mm_release 必须先 fetch_sub、仅在 prev == 1 时 Box::from_raw; \
         任何无条件释放都会拆掉 fork 子进程仍在用的 VMA 描述符"
    );
    assert!(
        !body.contains("vmm_destroy_page_table"),
        "mm 只持 VMA 元数据, 页表帧由 cr3 计数独立回收; mm_release 不得顺带销毁页表"
    );
}

#[test]
fn process_drop_is_the_only_mm_release_site() {
    let src = read(PROCESS_RS);
    let body = item_from(&prod_code(&src), "impl Drop for Process {");
    assert!(
        drop_releases_mm_once(&body),
        "Process::drop 必须读一次 mm 槽位并调用一次 mm_release: 每个持有该指针的 \
         Process 各有一份计数, 退出只注销自己那一份"
    );
}

// ============================================================================
// 锁 2: 继承 / 转移 / 装载三条所有权边
// ============================================================================

#[test]
fn fork_and_clone_register_a_second_reference() {
    let process = read(PROCESS_RS);
    let inherit = item_from(&prod_code(&process), "pub fn inherit_mm_from(");
    assert!(
        inherit_registers_reference(&inherit),
        "继承入口必须在写入子进程槽位后 mm_retain, 否则子进程退出会提前回收"
    );

    let clone = read(CLONE_RS);
    let vm_child = item_from(&prod_code(&clone), "fn vm_child_inherit(");
    assert!(
        vm_child.contains("inherit_mm_from(parent_pid)"),
        "CLONE_VM 子线程必须走统一继承入口 (functions 层不得直接碰裸指针计数)"
    );
    assert!(
        !vm_child.contains("mm.store(parent_mm"),
        "CLONE_VM 路径不得绕过继承入口直接裸拷贝 mm 指针 (会漏计数)"
    );

    let proc_ops = read(PROC_OPS_RS);
    let fork_state = item_from(&prod_code(&proc_ops), "fn inherit_parent_state(");
    assert!(
        fork_state.contains("inherit_mm_from(parent_pid)"),
        "fork 子进程必须与 clone 共用同一继承入口"
    );
}

#[test]
fn exec_releases_old_mm_and_moves_new_one_without_double_count() {
    let src = read(USER_PROC_RS);
    let body = item_from(&prod_code(&src), "pub fn replace_user_space(");
    assert!(
        body.contains("let old_mm = proc_ref.load_mm();"),
        "exec 必须在 store 新 mm 之前记住旧指针, 否则无从注销这一份引用"
    );
    assert!(
        body.contains("store_mm(new_mm)") && body.contains("mm_release(old_mm)"),
        "exec 是「旧 mm -1 / 新 mm 所有权转移」: 两者都必须出现在 replace_user_space"
    );
    assert_eq!(
        count(&body, "mm_release(old_mm)"),
        1,
        "旧 mm 的注销只能一处, 重复 release 会把 fork 共享者的计数减穿"
    );

    let proc_ops = read(PROC_OPS_RS);
    let exec = item_from(
        &prod_code(&proc_ops),
        "pub extern \"C\" fn proc_exec_replace_with_interp(",
    );
    assert!(
        exec.contains("p.mm.store(core::ptr::null_mut()"),
        "临时进程的 mm 是转移源: 必须清空槽位"
    );
    assert!(
        !exec.contains("mm_release("),
        "转移 = 不增减计数: exec 入口不得对临时进程的 mm 调 release"
    );
}

#[test]
fn loader_hands_ownership_to_process_exactly_once() {
    let src = read(USER_PROC_RS);
    let body = item_from(&prod_code(&src), "pub fn load_elf_from_memory_with_interp(");
    assert!(
        body.contains("Box::into_raw(mm)") && body.contains("store_mm("),
        "装载路径以 ref_count = 1 的创建者引用移交 Process::mm"
    );
    assert!(
        !body.contains("mm_release("),
        "装载失败时局部 Box 自行回收; 成功路径不得在装载函数里再 release"
    );
}

// ============================================================================
// 锁 3: 内核测试里的栈对象必须在 Drop 前摘挂载
// ============================================================================

#[test]
fn kernel_test_clears_stack_mm_before_process_drop() {
    let src = read(TEST_MM_RS);
    // 文件里先有 host-test 桩、后有真实现, 取后者.
    let body = body_last(&prod_code(&src), "fn test_current_mm_follows_current_task(");
    assert!(
        body.contains("p.mm") && body.contains("core::ptr::null_mut()"),
        "内核测试把栈上 MmStruct 挂进临时 Process, 拆表前必须清空槽位, \
         否则 Process::drop 会把栈指针当堆对象注销"
    );
    // 顺序: 在拆除循环内先清槽位, 后 drop 描述符 (函数内 install 失败的
    // drop 早于拆除段, 不能拿来比较).
    let teardown = &body[body
        .find("for (pid, ptr) in [(pid_a")
        .expect("拆除循环缺失")..];
    let clear = teardown.find("core::ptr::null_mut()").unwrap();
    let drop_at = teardown.find("drop_boxed_process(ptr)").unwrap();
    assert!(
        clear < drop_at,
        "清空必须发生在 drop_boxed_process 之前 (否则来不及)"
    );
}

// ============================================================================
// 负控制: 合成回退样本必须被谓词拒绝
// ============================================================================

#[test]
fn negative_control_predicates_reject_regressions() {
    let unconditional =
        "pub unsafe fn mm_release(mm: *mut MmStruct) {\n    drop(Box::from_raw(mm));\n}";
    assert!(
        !release_is_single_and_gated(unconditional),
        "无条件释放样本必须被拒绝"
    );

    let bare_copy = "fn vm_child_inherit() {\n    child.mm.store(parent_mm, Ordering::Release);\n}";
    assert!(
        !bare_copy.contains("inherit_mm_from(parent_pid)"),
        "裸拷贝样本必须被继承入口谓词拒绝"
    );

    let no_release_drop = "impl Drop for Process {\n    fn drop(&mut self) {\n        let mm = self.mm.load(Ordering::Acquire);\n    }\n}";
    assert!(
        !drop_releases_mm_once(no_release_drop),
        "只读不清的 Drop 样本必须被拒绝"
    );
}
