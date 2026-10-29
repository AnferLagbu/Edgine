//! MM 回归锁: 用户页帧归还的**单一实现** —— 地址空间销毁路径独占
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` 的抖动取证线. x86_64
//! 端到端长期存在两类失败形态: (A) init 在 fork / `destroy_page_table` 边界停住
//! 直至就绪超时, (B) 首次 TCP 回显全链成功后内核态 `#GP` (选择子荒谬) 与
//! `[FREG] Rollback ... result=-1`. 帧所有权元数据取到的决定性三元组:
//!
//! ```text
//! [PMM] Warn: double free at pfn 31759 (addr=0x7C0F000) alloc=<无记录>:0
//!       first_retire=privileged/mm/api.rs:81 this_free=privileged/mm/api.rs:81
//! ```
//!
//! 双方站点相同 ⇒ **同一释放存在两份平行实现**: proc 层按固定用户栈虚拟区间逐页
//! `pmm_free_page` (经 `extern "C"` 故站点是 `api.rs` 的包装行), 而
//! `vmm_x86_64::destroy_page_table` 本就已对每个 `PRESENT|USER` leaf 走
//! `frame_dec` 归零门 + 延迟释放. proc 层那份绕过两道防线:
//!
//! 1. **持有计数** —— fork/COW 共享页被提前还回 PMM, 兄弟进程仍在用;
//! 2. **TLB 追平** —— 他核陈旧映射即刻可命中被重分配的帧 (实测同一帧下一秒成为
//!    新进程 PML4 ⇒ 页表帧被当数据页覆写 = 形态 A; 跨核踩内核栈 = 形态 B).
//!
//! 本文件锁死"用户页帧归还只有一条路径"这一责任分离, 并覆盖同一 bug 类的四处
//! 实例 (`destroy` / `replace_user_space` / `create` 回滚 / ELF 加载回滚), 以及
//! 让三元组能点名双方的**归因穿透**约束 (Rust 侧不得再经 `extern "C"` 调 PMM).
//!
//! 删除平行归还后新暴露的一个对偶缺陷一并锁住: 装配失败的回滚原本经 `destroy`
//! 的 `is_exited()` 门, 而 `create()` 把状态置 `Ready` ⇒ 回滚是**静默空操作**
//! (页表 / 已映射用户页 / 内核栈 / 两张表条目 / PID 全泄漏). 正解是先如实标
//! `Zombie` 再走 `PROCESS_TABLE::remove_and_free` 权威单源, 登记见 §6 P-16.
//!
//! # 断言方式
//!
//! 源码文本契约: 截函数体后判形状, 只锁语义不锁排版. 判定收敛成 `no_*` /
//! `has_*` / `count_*` 谓词, 由 [`negative_control_predicates_reject_regressions`]
//! 用合成回退样本 (逐字贴回被删除的 pre-free 循环) 反向验证.

use std::fs;

const USER_PROC_RS: &str = "../src/kernel/privileged/proc/user_proc.rs";
const VMM_X86_RS: &str = "../src/kernel/privileged/mm/vmm_x86_64.rs";
const MEM_ALLOC_RS: &str = "../src/kernel/memory_allocator.rs";

// ============================================================================
// 文本读取与函数体截取
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 生产代码文本: 剥掉注释行, 并在第一个 `#[cfg(test)]` 处截断.
///
/// 本工程的动机注释逐字提到 `free_phys_page` / "用户栈物理页" 等被扫描形态,
/// 不剥注释则锁的对象会变成注释本身.
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

/// 截取以 `begin` 起始的那个函数 / 块的完整文本 (含起止行).
///
/// 结束判据取 rustfmt 的硬约束: 块末 `}` 独占一行且**与该块首行同缩进**. 顶层
/// 项在第 0 列, `impl` 内的方法在第 4 列, 嵌套块的闭合始终更深, 故该判据唯一.
/// 不用括号计数 —— 被截代码里的 klog 格式串带 `{}`, 计数会被字符串内的括号带偏.
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

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// `sig` 定义前紧邻的属性行中是否含 `attr` (向前看 6 行).
fn attr_before_sig(src: &str, sig: &str, attr: &str) -> bool {
    let prod = prod_code(src);
    let at = prod
        .find(sig)
        .unwrap_or_else(|| panic!("未找到签名: {sig}"));
    prod[..at].lines().rev().take(6).any(|l| l.trim() == attr)
}

// ============================================================================
// 谓词
// ============================================================================

/// 体内不得出现**单页**直接归还 (`free_phys_pages` 的多页形态不受该 needle 匹配).
fn no_single_page_release(body: &str) -> bool {
    !body.contains("free_phys_page(")
}

/// 体内不得出现"按用户栈虚拟区间翻译再归还"的形态 (旧 pre-free 循环的骨架).
fn no_stack_range_pre_free(body: &str) -> bool {
    !(body.contains("virt_to_phys(") && body.contains("free_phys_page"))
}

/// cr3 归还必须先过 `frame_dec` 归零门, 归零才销毁地址空间.
fn cr3_release_is_gated(body: &str) -> bool {
    body.contains(".frame_dec(") && body.contains("destroy_user_page_table(")
}

/// `create` 回滚只归还未映射的 guard 尾页 (偏移 USER_STACK_SIZE 之后).
fn create_rollback_releases_only_unmapped_guard(body: &str) -> bool {
    count(body, "free_phys_page(") == 1 && body.contains("stack_pages as u64 + USER_STACK_SIZE")
}

/// 单一实现: 销毁路径对每个 `PRESENT|USER` leaf 走计数门 + 延迟释放.
fn has_single_leaf_release_in_destroy(vmm_body: &str) -> bool {
    vmm_body.contains("is_present() && pte.is_user()")
        && vmm_body.contains(".frame_dec(")
        && vmm_body.contains("release_frame_locked(")
}

/// 归因穿透: `extern "C"` 声明块内不得再出现 PMM 入口 (站点会被吞到 api.rs).
fn extern_block_is_pmm_free(extern_block: &str) -> bool {
    !extern_block.contains("pmm_")
}

/// 归因穿透: 文件确实改用 `mm::pmm_*_phys` 的 Rust 入口.
fn uses_phys_entry_points(src: &str) -> bool {
    src.contains("pmm_free_page_phys(") || src.contains("pmm_alloc_page_phys(")
}

/// 装配失败回滚的形状: 先标 `Zombie` (过销毁门), 再走权威单源 —— 且两者顺序固定.
///
/// 只调单源不标状态: `destroy` 的 `is_exited()` 门静默返回 ⇒ 全量泄漏 (P-16).
/// 只标状态不调单源 (或就地手抄归还): 回到平行实现 (本轮根因形状).
fn rollback_is_gated_then_single_source(body: &str) -> bool {
    match (
        body.find("ProcessState::Zombie as u32"),
        body.find("PROCESS_TABLE.remove_and_free("),
    ) {
        (Some(zombie), Some(teardown)) => zombie < teardown,
        _ => false,
    }
}

// ============================================================================
// 不变式: proc 层不再平行归还用户页帧
// ============================================================================

#[test]
fn destroy_has_no_parallel_user_stack_release() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "fn destroy(&self, proc: NonNull<UserProcess>");
    assert!(
        no_single_page_release(&body) && no_stack_range_pre_free(&body),
        "destroy() 内不得再按用户栈区间逐页归还物理页: 该职责属 destroy_page_table 单源"
    );
    assert!(
        !body.contains("load_user_stack("),
        "destroy() 不应再读取用户栈区间 (旧 pre-free 循环的判据字段)"
    );
    assert!(
        cr3_release_is_gated(&body),
        "destroy() 的 cr3 销毁必须由 frame_dec 归零门把关 (CLONE_VM / execve 共享)"
    );
}

#[test]
fn replace_user_space_has_no_parallel_user_stack_release() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "pub fn replace_user_space(");
    assert!(
        no_single_page_release(&body) && no_stack_range_pre_free(&body),
        "replace_user_space() (execve) 内不得再逐页归还旧用户栈"
    );
    assert!(
        cr3_release_is_gated(&body),
        "replace_user_space() 的旧 cr3 销毁必须由 frame_dec 归零门把关"
    );
}

#[test]
fn create_rollback_releases_only_the_unmapped_guard_page() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "pub fn create(&self, info: &UserProcInfo");
    assert!(
        create_rollback_releases_only_unmapped_guard(&body),
        "create() 的内核栈失败回滚只能归还未映射的 guard 尾页; 已映射栈页归销毁路径"
    );
}

#[test]
fn elf_load_rollback_has_no_per_page_release() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "pub fn load_elf_from_memory(");
    assert_eq!(
        count(&body, "free_phys_page"),
        0,
        "ELF 加载失败回滚不得逐个归还已映射页 (allocated_pages 还混有复用的既有页)"
    );
    assert!(
        !src.contains("free_phys_page_for_rollback"),
        "专供该回滚的 free_phys_page_for_rollback 已无消费者, 必须一并删除 (§5 F9)"
    );
}

// ============================================================================
// 不变式: 装配失败的回滚不是静默空操作
// ============================================================================

#[test]
fn unrun_rollback_marks_zombie_then_uses_authoritative_teardown() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "fn rollback_unrun_process(");
    assert!(
        rollback_is_gated_then_single_source(&body),
        "回滚必须先标 Zombie (否则 destroy 的 is_exited 门静默返回 = 全量泄漏), \
         再交给 PROCESS_TABLE.remove_and_free (权威销毁单源)"
    );
    assert!(
        !body.contains("free_phys_page") && !body.contains("destroy_user_page_table("),
        "回滚入口不得自己重抄归还顺序: 单源已在 remove_and_free 下游"
    );
}

#[test]
fn elf_load_rollback_routes_every_failure_site_to_single_source() {
    let src = read(USER_PROC_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "pub fn load_elf_from_memory(");
    let segments = item_from(&prod, "unsafe fn load_elf_segments(");
    // 装载失败点已收口为 `load_elf_segments` 的 `None` 返回, 由调用方唯一一处判据统一
    // 路由到回滚入口 —— 单路由 + 失败点逐个 `return None` 比逐点重复调用更强.
    assert_eq!(
        count(&body, "Self::rollback_unrun_process(proc_ref)"),
        1,
        "load_elf_from_memory 必须把装载失败统一路由到回滚入口"
    );
    assert!(
        body.contains(
            "Self::load_elf_segments(elf_data, elf_size, &header, load_bias, cr3).is_none()"
        ),
        "回滚必须挂在装载失败的判据上 (装载成功不得回滚)"
    );
    // phnum 超限 / phdr 越界 / 装配页 OOM 三个失败点逐个返回 None, 不接受部分接线.
    assert_eq!(
        count(&segments, "return None;"),
        3,
        "load_elf_segments 的三个装载失败点必须各自返回 None (逐个计数, 防漏接)"
    );
    assert!(
        !src.contains("destroy_raw"),
        "destroy_raw 的消费者全被回滚入口取代后即为零引用包裹, 必须一并删除 (§5 F9)"
    );
}

// ============================================================================
// 不变式: 销毁路径是唯一实现, 且归因可穿透
// ============================================================================

#[test]
fn destroy_page_table_is_the_single_user_leaf_release() {
    let src = read(VMM_X86_RS);
    let prod = prod_code(&src);
    let body = item_from(&prod, "pub fn destroy_page_table(");
    assert!(
        has_single_leaf_release_in_destroy(&body),
        "destroy_page_table 必须逐 USER leaf 走 frame_dec 门 + 延迟释放 (唯一实现)"
    );
}

#[test]
fn pmm_calls_are_pierced_for_attribution() {
    let proc_src = read(USER_PROC_RS);
    let heap_src = read(MEM_ALLOC_RS);
    let proc_extern = item_from(&prod_code(&proc_src), "unsafe extern \"C\" {");
    let heap_extern = item_from(&prod_code(&heap_src), "unsafe extern \"C\" {");

    assert!(
        extern_block_is_pmm_free(&proc_extern) && extern_block_is_pmm_free(&heap_extern),
        "Rust 侧不得经 extern \"C\" 调 PMM: C ABI 无 track_caller, 三元组会失去点名能力"
    );
    assert!(
        uses_phys_entry_points(&proc_src) && uses_phys_entry_points(&heap_src),
        "proc / 内核堆必须走 mm::pmm_*_phys 的 Rust 入口"
    );
    assert!(
        attr_before_sig(
            &proc_src,
            "pub fn free_phys_page(phys: *mut u8)",
            "#[track_caller]"
        ),
        "raw::free_phys_page 需 track_caller 才能把站点穿透到真实调用方"
    );
}

// ============================================================================
// 负控制: 谓词必须能在回归发生时返 false
// ============================================================================

#[test]
fn negative_control_predicates_reject_regressions() {
    // 逐字贴回被删除的 pre-free 循环 (destroy 与 replace_user_space 各一份).
    let regressed_destroy = "\
    fn destroy(&self, proc: NonNull<UserProcess>, keep_kstack: bool) {
        let ustack = proc_ref.load_user_stack();
        if ustack != 0 {
            for i in 0..(USER_STACK_SIZE / PAGE_SIZE) {
                let svirt = stack_virt + USER_STACK_GUARD + i * PAGE_SIZE;
                let phys = raw::virt_to_phys(cr3, svirt);
                if phys != 0 {
                    raw::free_phys_page(phys as *mut u8);
                }
            }
        }
        raw::destroy_user_page_table(cr3);
    }
";
    assert!(
        !no_single_page_release(regressed_destroy) && !no_stack_range_pre_free(regressed_destroy),
        "负控制失效: pre-free 循环回贴未被识别"
    );
    assert!(
        !regressed_destroy.contains(".frame_dec("),
        "负控制失效: 样本本身就没有计数门, 却未被 cr3_release_is_gated 拒掉"
    );

    // 回滚路径退化: ELF 加载失败时逐个归还已映射页.
    let regressed_elf = "\
    pub fn load_elf_from_memory(&self) -> i32 {
        if page.is_null() {
            for pi in 0..page_count {
                raw::free_phys_page_for_rollback(allocated_pages[pi]);
            }
            return -1;
        }
        0
    }
";
    assert_ne!(
        count(regressed_elf, "free_phys_page"),
        0,
        "负控制失效: ELF 回滚逐个归还页的回退样本未被计数"
    );

    // create() 回滚退化成归还**已映射**的栈块头页 (旧形态).
    let regressed_guard = "\
    pub fn create(&self) -> Option<*mut UserProcess> {
        if kstack.is_null() {
            raw::free_phys_page(stack_pages);
            raw::destroy_user_page_table(cr3_val);
        }
        None
    }
";
    assert!(
        !create_rollback_releases_only_unmapped_guard(regressed_guard),
        "负控制失效: 回滚归还了已映射页却未被识别"
    );

    // 销毁路径失去单源实现 (USER 位过滤或延迟释放被删).
    let regressed_vmm = "\
    pub fn destroy_page_table(&self, pml4: u64) {
        for l in 0..512usize {
            let pte = &*pt.add(l);
            if pte.is_present() {
                get_pmm().free_page(pte.frame());
            }
        }
    }
";
    assert!(
        !has_single_leaf_release_in_destroy(regressed_vmm),
        "负控制失效: 未过滤 USER 位 / 未走 frame_dec 的销毁路径未被识别"
    );

    // 归因回退: 改走 extern C 入口.
    let regressed_extern = "\
unsafe extern \"C\" {
    fn pmm_alloc_page() -> *mut u8;
    fn pmm_free_page(addr: *mut u8);
    fn memset(s: *mut c_void, c: i32, n: usize) -> *mut c_void;
}
";
    assert!(
        !extern_block_is_pmm_free(regressed_extern),
        "负控制失效: extern C 里的 PMM 声明未被识别"
    );

    // 回滚缺状态标记 (P-16 的原始缺陷形状): 直接调单源, 被 is_exited 门拒掉.
    let regressed_no_state = "\
    fn rollback_unrun_process(proc_ref: &UserProcRef) {
        let pid = proc_ref.pid();
        PROCESS_TABLE.remove_and_free(pid);
    }
";
    assert!(
        !rollback_is_gated_then_single_source(regressed_no_state),
        "负控制失效: 未标 Zombie 的回滚未被识别 (那就是静默空操作)"
    );

    // 回滚就地手抄归还 (造出第四份平行实现的形状).
    let regressed_hand_rolled = "\
    fn rollback_unrun_process(proc_ref: &UserProcRef) {
        proc_ref.store_state(ProcessState::Zombie as u32);
        raw::destroy_user_page_table(proc_ref.load_cr3());
        raw::free_phys_page(proc_ref.load_user_stack() as *mut u8);
    }
";
    assert!(
        regressed_hand_rolled.contains("ProcessState::Zombie as u32")
            && !regressed_hand_rolled.contains("PROCESS_TABLE.remove_and_free("),
        "负控制失效: 样本本就没走单源, 却可能被反向断言误判为通过"
    );
    assert!(
        !rollback_is_gated_then_single_source(regressed_hand_rolled),
        "负控制失效: 就地手抄归还的回滚未被识别"
    );
}
