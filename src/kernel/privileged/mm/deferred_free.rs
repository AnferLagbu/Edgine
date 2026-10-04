//! 延迟释放 (deferred free) — 跨架构共用的 TLB shootdown 收尾机制
//!
//! 页表修改批次把"已逻辑死亡"的帧挂入**批次链** (`BATCH_HEAD`), 待全部在线核的
//! 已追平 TLB 代 (`smp::tlb_gen_min_online`) 覆盖该批次的释放代后, 才真正归还 PMM;
//! 未追平的批次整条挂入 **pending 链** (`PENDING_HEAD`), 由后续调用机会式排空.
//!
//! x86_64 与 aarch64 **共用本实现** (对齐项目「内核内部并行实现必须统一为单一规范
//! 实现」约束): 两架构只是"远程失效"的手段不同 —— x86_64 需靠定向 IPI 让对端重载
//! CR3, aarch64 的 `tlbi vaae1is` 虽已硬件广播, 但仍需对端执行上下文同步 (`dsb`)
//! 并声明追平代, 页表页方可安全回收.
//!
//! ## 锁序与调用约束
//!
//! - [`defer_free`] / [`take_batch`] / [`clear_shootdown_flag`] /
//!   [`mark_remote_shootdown`] 必须在**持有 `VMM_LOCK`** 期间调用
//!   (`BATCH_HEAD` 的单写者前提).
//! - [`release_tail`] 必须在**释放 `VMM_LOCK` 且恢复中断之后**调用: 其内的
//!   `tlb_gen_publish_and_shoot` 会向对端发 IPI, 持锁等待会与被本锁挡住的对端互等死锁.
//! - `frame_link_*` 以帧前 16 字节为链节点 ⇒ **同一帧只能入链一次**.

use super::{PhysAddr, get_pmm};
use crate::privileged::sync::IrqSpinLock;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 本 `VMM_LOCK` 临界区是否产生过需要远程 TLB 失效的页表修改.
///
/// 持锁期间由本核在 [`mark_remote_shootdown`] 中置位 (锁内单写者), 释放锁前由
/// [`clear_shootdown_flag`] 读取并清除, 出临界区后据其"发布新代 + 定向 IPI"——
/// 远程失效一律移出临界区, 且代计数下**不再有 ack 等待**.
static SHOOTDOWN_NEEDED: AtomicBool = AtomicBool::new(false);

/// 本临界区批次链头 (帧物理地址; 0 表尾).
///
/// **仅持 `VMM_LOCK` 期间读写** (锁内单写者): [`defer_free`] 头插, [`take_batch`]
/// 整条摘走. 链节点就是帧自身 (见 `frame_link_next`).
static BATCH_HEAD: AtomicU64 = AtomicU64::new(0);

/// 已发布代但尚未追平的帧链 (节点同 `BATCH_HEAD`).
///
/// 由 `IrqSpinLock` 守护 (持锁即关中断): 只有它保证排空路径在中断上下文下也不自死锁.
static PENDING_HEAD: IrqSpinLock<u64> = IrqSpinLock::new(0);

/// `PENDING_HEAD` 是否非空的廉价门控 (与 `PENDING_HEAD` **同锁更新**, 故无竞态).
///
/// 用途: 排空是热路径外机会式行为, 常态 pending 为空, 免去每次释放锁都取一次锁.
static PENDING_NONEMPTY: AtomicBool = AtomicBool::new(false);

/// 累计**已真正归还 PMM** 的延迟释放帧数 (计数粒度为帧, 非链).
///
/// 仅用于可观测性 / 测试断言, **不参与任何控制逻辑**: 若"代追平判据"出错导致帧永久
/// 滞留 pending 链, 本计数不再增长 —— 据此可直接断言"帧最终被归还".
static DEFERRED_FREE_RELEASED: AtomicU64 = AtomicU64::new(0);

/// 累计**已送入延迟释放链** (批次链 → pending 链) 的帧数, 粒度为帧.
///
/// 仅用于可观测性 / 埋点计算, **不参与任何控制逻辑**. 与归还计数
/// ([`DEFERRED_FREE_RELEASED`]) 配对使用, 构成"释放路径是否运行"的最小判别集:
/// - `admitted == 0` ⇒ 延迟释放路径**根本没被走到**;
/// - `admitted > 0 && released == 0 && pending` ⇒ 帧已入链但代未追平, 滞留 pending.
static DEFERRED_FREE_ADMITTED: AtomicU64 = AtomicU64::new(0);

// 帧内链节点布局: `[0, 8)` = next (u64 物理地址, 0 表尾); `[8, 16)` = gen.
//
// 取帧前 16 字节当节点: 进入延迟释放的帧已不再服务于任何用途 —— unmap 路径在
// `defer_free` 之前已把父表项清零; destroy 路径整表正在销毁, 其残留父表项由 mm
// 生命周期契约负责. 帧是 PMM 分配的页对齐 4KB 帧, 经 `to_virt()` 常量偏移直映射
// 可直接写.
//
// 残留窗口 (已登记): 被覆盖槽位对"已读过父表项"的并发硬件页表遍历可见. `gen`
// 左移 1 位写入使 bit0 恒为 0 (页表项"不存在"位), `next` 是页对齐物理地址
// (bit0..11 恒为 0) —— 故遍历读到被覆盖槽位只会得到"不存在" → 正常缺页.

/// 读取帧内链节点的 next 字段 (u64 物理地址, 0 表尾).
///
/// # Safety
///
/// 调用方必须保证: `frame` 是页对齐的有效物理帧地址, 且其前 16 字节当前不被任何
/// 映射引用 (即该帧已逻辑死亡、尚未归还 PMM).
unsafe fn frame_link_next(frame: u64) -> u64 {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.read() }
}

/// 写入帧内链节点的 next 字段.
///
/// # Safety
///
/// 同 [`frame_link_next`].
unsafe fn frame_link_set_next(frame: u64, next: u64) {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.write(next) };
}

/// 读取帧内链节点的 gen 字段 (写入时左移 1 位, 读回时右移还原).
///
/// # Safety
///
/// 同 [`frame_link_next`].
unsafe fn frame_link_gen(frame: u64) -> u64 {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.add(1).read() >> 1 }
}

/// 写入帧内链节点的 gen 字段 (左移 1 位写入, 使页表帧 bit0 恒为 0).
///
/// # Safety
///
/// 同 [`frame_link_next`].
unsafe fn frame_link_set_gen(frame: u64, generation: u64) {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.add(1).write(generation << 1) };
}

/// 归还一整条帧链给 PMM.
///
/// **禁止持任何锁调用**: `free_page` 内部取 PMM 锁, 持 `PENDING_HEAD` 调用会造成
/// 锁嵌套. 每节点必须**先读 next 再释放** —— `free_page` 之后帧内容可能被他方改写.
fn free_chain(chain: u64) {
    if chain == 0 {
        return; // 空链不取 PMM: host-test 下 get_pmm 会 panic
    }
    let pmm = get_pmm();
    let mut node = chain;
    while node != 0 {
        // SAFETY: 链上节点均为已逻辑死亡的帧 (见 frame_link_next 契约).
        let next = unsafe { frame_link_next(node) };
        pmm.free_page(PhysAddr(node));
        DEFERRED_FREE_RELEASED.fetch_add(1, Ordering::Relaxed);
        node = next;
    }
}

/// 结算本临界区批次链 (`batch`, 非 0): 该批全部帧共享同一释放代 `g`.
///
/// `min >= g` ⇒ 全部在线核都已在该批修改发布之后彻底失效过 TLB ⇒ 立即归还;
/// 否则把 `g` 写入每帧节点后**整条**挂入 `PENDING_HEAD` (不丢帧, 下次排空再判).
fn settle_batch(batch: u64, g: u64, min: u64) {
    if min >= g {
        free_chain(batch);
        return;
    }
    // 锁外先写 gen 并记录链尾: 取 PENDING_HEAD 锁期间只做 O(1) 指针搬运.
    let mut tail = batch;
    loop {
        // SAFETY: 同 free_chain; 帧未归还 PMM, 内容仍可写.
        let next = unsafe { frame_link_next(tail) };
        // SAFETY: 同 free_chain.
        unsafe { frame_link_set_gen(tail, g) };
        if next == 0 {
            break;
        }
        tail = next;
    }
    let mut head = PENDING_HEAD.lock();
    // SAFETY: 同 free_chain.
    unsafe { frame_link_set_next(tail, *head) };
    *head = batch;
    PENDING_NONEMPTY.store(true, Ordering::Relaxed);
}

/// 机会式排空: 把已追平代 (`gen <= min`) 的帧归还 PMM.
///
/// 持 `PENDING_HEAD` 的时间仅为两次 O(1) 指针搬运 —— 遍历与 `free_page` 一律在锁外.
/// 摘链后本核独占该链, 他核此后插入的是另一条新链, 二者不交叉.
fn drain_pending(min: u64) {
    let chain = {
        let mut head = PENDING_HEAD.lock();
        let chain = core::mem::replace(&mut *head, 0u64);
        PENDING_NONEMPTY.store(false, Ordering::Relaxed);
        chain
    };
    if chain == 0 {
        return;
    }

    // 锁外遍历: min 追上的进 free 链, 其余进 hold 链 (记录 hold 链尾供回挂).
    let mut free_head = 0u64;
    let mut hold_head = 0u64;
    let mut hold_tail = 0u64;
    let mut node = chain;
    while node != 0 {
        // SAFETY: 链上节点均为已逻辑死亡的帧 (见 frame_link_next 契约).
        let next = unsafe { frame_link_next(node) };
        // SAFETY: 同 free_chain; 帧未归还 PMM, 内容仍可读.
        let frame_gen = unsafe { frame_link_gen(node) };
        if min >= frame_gen {
            // SAFETY: 同 free_chain.
            unsafe { frame_link_set_next(node, free_head) };
            free_head = node;
        } else {
            // SAFETY: 同 free_chain.
            unsafe { frame_link_set_next(node, hold_head) };
            if hold_head == 0 {
                hold_tail = node;
            }
            hold_head = node;
        }
        node = next;
    }

    if hold_head != 0 {
        let mut head = PENDING_HEAD.lock();
        // SAFETY: hold_tail 是本链末节点 (其 next 已在上一步写成 0), hold_head 非 0.
        unsafe { frame_link_set_next(hold_tail, *head) };
        *head = hold_head;
        PENDING_NONEMPTY.store(true, Ordering::Relaxed);
    }
    free_chain(free_head);
}

/// 把一个"已逻辑死亡"的帧挂入本临界区批次链 (头插).
///
/// **调用方须持有 `VMM_LOCK`** (见模块级锁序约束). 帧前 16 字节被复用为链节点,
/// 故同一帧只能入链一次.
///
/// SIMPLIFIED: 排空点唯一 (各架构 `release_lock` 出口的 [`release_tail`]), 不引入
/// tick / 返回用户态前的额外排空点; 影响面为未追平帧可能滞留到下一次任一核的
/// `release_lock` (不丢帧, 只推迟归还, 单核下代恒 0 立即释放); 何时需扩展: 出现
/// 长时间无 VMM 操作却需及时回收的负载时, 再补排空点.
pub(crate) fn defer_free(frame: u64) {
    // 头插: 新帧的 next 指向当前批次链头, 再更新链头.
    // SAFETY: 持 VMM_LOCK (本核是批次链唯一写者); frame 刚被解除映射/已从页表
    // 拆链, 前 16 字节不再被任何映射引用, 可复用为链节点.
    unsafe {
        let head = BATCH_HEAD.load(Ordering::Acquire);
        frame_link_set_next(frame, head);
        frame_link_set_gen(frame, 0); // 出锁前不会被读, 0 为占位
    }
    BATCH_HEAD.store(frame, Ordering::Release);
    DEFERRED_FREE_ADMITTED.fetch_add(1, Ordering::Relaxed);
}

/// 登记"本临界区需远程失效". **调用方须持有 `VMM_LOCK`**.
///
/// 仅在**替换/覆盖既有翻译**与**权限变更**时调用: 纯新建映射的 VA 此前无翻译,
/// 远程核不可能缓存条目 (若一律登记, 每次新建映射都会广播一轮 IPI —— 过度失效).
pub(crate) fn mark_remote_shootdown() {
    SHOOTDOWN_NEEDED.store(true, Ordering::Relaxed);
}

/// 读取并清除"本临界区需远程失效"标志. **调用方须持有 `VMM_LOCK`**.
///
/// 锁内单写者, 故读-清不会漏掉本临界区自身的置位.
pub(crate) fn clear_shootdown_flag() -> bool {
    SHOOTDOWN_NEEDED.swap(false, Ordering::Relaxed)
}

/// 整条摘走本临界区批次链 (链头置 0) 并返回. **调用方须持有 `VMM_LOCK`**.
///
/// 摘走必须在同一临界区内完成: 先释放锁再摘会漏摘他核进入临界区后新增的帧.
pub(crate) fn take_batch() -> u64 {
    BATCH_HEAD.swap(0, Ordering::AcqRel)
}

/// 释放锁后的收尾: 发布代并通知在线核失效, 再机会式归还已追平的帧.
///
/// **必须在释放 `VMM_LOCK` 且恢复中断之后调用** (见模块级锁序约束). `shootdown_needed`
/// 与 `batch` 由 [`clear_shootdown_flag`] / [`take_batch`] 在**持锁期间**取得.
pub(crate) fn release_tail(shootdown_needed: bool, batch: u64) {
    // 远程失效 = 发布新代 + 定向 IPI (含本核), **不等待** —— 代计数下无需 ack.
    // 单核 / SMP 未启用时不发布, 取当前代 (恒 0) 即可立即释放.
    let smp_active =
        crate::privileged::smp::is_enabled() && crate::privileged::smp::get_cpu_count() > 1;
    let g = if shootdown_needed && smp_active {
        crate::privileged::smp::tlb_gen_publish_and_shoot()
    } else {
        crate::privileged::smp::tlb_gen_now()
    };

    // 无在线核时 `tlb_gen_min_online` 返回 u64::MAX ⇒ 恒 `>= g` ⇒ 立即释放.
    let min = crate::privileged::smp::tlb_gen_min_online();

    // 计数基线: 仅用于判断"本次是否发生了归还".
    let before = DEFERRED_FREE_RELEASED.load(Ordering::Relaxed);

    // 先排空历史 pending (其代更老, 更可能已追平), 再结算本批 —— 反序会让刚挂入的
    // 整条批次链被立即摘下又挂回 (两次 O(n) 无效往返).
    if PENDING_NONEMPTY.load(Ordering::Relaxed) {
        drain_pending(min);
    }
    if batch != 0 {
        settle_batch(batch, g, min);
    }

    // SIMPLIFIED: 只统计"释放帧总数"这一聚合量, 不区分批次/来源; 影响面为排查时无法
    // 从日志区分是哪条路径释放的; 何时需扩展: 需要定位滞留来源时, 改为分路径计数.
    //
    // 埋点 (见 docs/plan/tlb-shootdown-epoch.md S-10): 本临界区**入链了帧**时也必须打印 ——
    // "入链了却一帧未归还" 与 "从未入链" 是两个完全不同的诊断, 只打归还数会把前者静默
    // 掩盖 (故用 `batch != 0` 判"本临界区有帧入链"). `pending` / `gen_g` / `min` 三者共同
    // 给出"是否因代未追平而滞留".
    let after = DEFERRED_FREE_RELEASED.load(Ordering::Relaxed);
    if after != before || batch != 0 {
        crate::klog_info!(
            Memory,
            "[VMM] deferred-free admitted_total={} released_total={} pending={} gen_g={} min={}",
            DEFERRED_FREE_ADMITTED.load(Ordering::Relaxed),
            after,
            PENDING_NONEMPTY.load(Ordering::Relaxed),
            g,
            min
        );
    }
}

/// 累计已送入延迟释放链的帧数 (仅观测).
///
/// 供销毁路径埋点计算"本次调用送入链的帧数". 归还数与 pending 状态由
/// [`release_tail`] 的日志行承载, 不再单独开访问器 (避免无调用点死代码).
pub(crate) fn admitted() -> u64 {
    DEFERRED_FREE_ADMITTED.load(Ordering::Relaxed)
}
