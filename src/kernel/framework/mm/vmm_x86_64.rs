//! 虚拟内存管理器 (VMM)
//!
//! 使用 4 级页表 (`x86_64`) 管理虚拟内存映射.
//! 提供:
//! - 虚实地址转换
//! - 页表创建与管理
//! - 用户空间页表
//! - 大页支持 (2MB, 1GB)
//! - 内存保护与访问控制
//!
//! ## SAFETY
//!
//! `user_tables` 的内部可变性通过 `UnsafeCell` 实现,
//! 由内部 `AtomicBool` 自旋锁 (`VMM_LOCK`) 保护.
//! 所有变更都在锁内进行, 确保 `unsafe impl Sync` 的正确性.
//!
//! ### 关键不变式 (所有 `unsafe` 块都依赖):
//!
//! 1. **`KERNEL_PML4`**: 在 `init()` 中写入一次, 之后只读 (Release/Acquire).
//! 2. **`VMM_LOCK`**: 所有页表修改与 `UserPageTable` 变更都串行化.
//! 3. **`PhysAddr` → `VirtAddr`**: `phys_to_virt(pa) = pa + KERNEL_BASE` 是合法内核 VA,
//!    因为内核在 `KERNEL_BASE` 处恒等映射所有物理内存.
//! 4. **PMM 分配**: 返回的物理地址总是页对齐且合法.
//! 5. **页表指针**: 任何从 `PhysAddr::to_virt()` 派生的指针都指向 PMM 分配的
//!    完整 4KB 页, 所有 512 项遍历都安全.
//! 6. **存在位保护**: 将表项解引用为下一级指针前, 检查 `entry & 1 != 0`.
//! 7. **死锁防止**: `acquire_lock` 在调试构建中通过 `VMM_LOCK_RECURSIVE`
//!    对递归获取直接 panic, 在死锁发生前阻止.
//!
//! ## 锁顺序
//!
//! **`VMM_LOCK` 绝不能在持有时再去获取 `VMA_LOCK` (`MmStruct::vmas`).**
//! 这避免了 ABBA 死锁:
//!   线程 A: `VMM_LOCK` → `VMA_LOCK`
//!   线程 B: `VMA_LOCK` → `VMM_LOCK` (在 `MmStruct::remove_range` 中)
//!
//! 所有调用方遵守该规则:
//! - `user_driver.rs`: VMM 操作 (map/unmap) → 释放 `VMM_LOCK` → VMA 操作 (insert/remove)
//! - `page_fault.rs`: VMA 查找 (`find_vma`) → 释放 `VMA_LOCK` → VMM 操作 (`map_page`)
//! - `MmStruct::remove_range`: 持有 `VMA_LOCK` → 获取 `VMM_LOCK` (反向顺序安全)

use super::{
    HUGE_PAGE_1G_SIZE, HUGE_PAGE_2M_SIZE, KERNEL_BASE, PAGE_NX, PAGE_PRESENT, PAGE_SIZE, PAGE_USER,
    PAGE_WRITABLE, PageFlags, PageSize, PageTableEntry, PageTranslation, PhysAddr, VirtAddr,
    get_pmm,
};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::framework::sync::{IrqSaveFlags, IrqSpinLock, disable_interrupts, restore_interrupts};

use crate::framework::sync::OnceLock;
pub(crate) static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

static VMM_LOCK: AtomicBool = AtomicBool::new(false);

#[cfg(debug_assertions)]
static VMM_LOCK_RECURSIVE: AtomicBool = AtomicBool::new(false);

/// 本 `VMM_LOCK` 临界区是否产生过需要远程 TLB 失效的页表修改.
///
/// 持锁期间由本核在 `flush_tlb_remote` 中置位 (锁内单写者), `release_lock` 在**仍持锁**时
/// 读取并清除, 出临界区后再据其「发布新代 + 定向 IPI (`0xFD`, 含本核)」——
/// 远程失效一律移出临界区, 且代计数下**不再有 ack 等待**
/// (临界区内等对端响应会与被 `VMM_LOCK` 挡住的对端互等死锁).
static TLB_SHOOTDOWN_NEEDED: AtomicBool = AtomicBool::new(false);

/// 本临界区批次链头 (帧物理地址; 0 表尾).
///
/// **仅持 `VMM_LOCK` 期间读写** (锁内单写者): `defer_free` 头插, `release_lock`
/// 仍持锁时整条摘走. 链节点就是帧自身 (见 `frame_link_next`).
static BATCH_HEAD: AtomicU64 = AtomicU64::new(0);

/// 已发布代但尚未追平的帧链 (节点同 `BATCH_HEAD`).
///
/// 由 `IrqSpinLock` 守护 (持锁即关中断): 只有它保证排空路径在中断上下文下也不自死锁.
static PENDING_HEAD: IrqSpinLock<u64> = IrqSpinLock::new(0);

/// `PENDING_HEAD` 是否非空的廉价门控 (与 `PENDING_HEAD` **同锁更新**, 故无竞态).
///
/// 用途: 排空是热路径外机会式行为, 常态 pending 为空, 免去每次 `release_lock`
/// 都取一次 `PENDING_HEAD` 锁.
static PENDING_NONEMPTY: AtomicBool = AtomicBool::new(false);

/// 累计**已真正归还 PMM** 的延迟释放帧数 (计数粒度为帧, 非链).
///
/// 仅用于可观测性 / 测试断言, **不参与任何控制逻辑**: 若"代追平判据"出错导致帧永久
/// 滞留 pending 链, 本计数不再增长 —— 据此可直接断言"帧最终被归还", 弥补现有测试
/// 对"只泄漏不崩溃"形态无感的缺口.
static DEFERRED_FREE_RELEASED: AtomicU64 = AtomicU64::new(0);

/// 累计**已送入延迟释放链** (批次链 → pending 链) 的帧数, 粒度为帧.
///
/// 仅用于可观测性 / 测试断言, **不参与任何控制逻辑**. 与 `DEFERRED_FREE_RELEASED`
/// 配对使用, 构成"释放路径是否运行"的最小判别集:
/// - `admitted == 0` ⇒ 延迟释放路径**根本没被走到** (例如进程从未被回收 ⇒
///   `Process::drop` 未运行 ⇒ `destroy_page_table` 未被调用);
/// - `admitted > 0 && released == 0 && pending` ⇒ 帧已入链但代未追平, 滞留 pending.
/// 二者诊断方向完全不同, 不可只凭 `released == 0` 判定.
static DEFERRED_FREE_ADMITTED: AtomicU64 = AtomicU64::new(0);

/// 帧内链节点布局: `[0, 8)` = next (u64 物理地址, 0 表尾); `[8, 16)` = gen.
///
/// 取帧前 16 字节当节点: 进入延迟释放的帧已不再服务于任何用途 —— unmap 路径在
/// `defer_free` 之前已把父表项清零; destroy 路径整表正在销毁, 其残留父表项由 mm
/// 生命周期契约负责 (见 `docs/plan/cr3-lifetime-ownership.md`); 帧是 PMM 分配的
/// 页对齐 4KB 帧, 经 `phys_to_virt` 常量偏移直映射 (`mm/mod.rs:262`) 可直接写.
///
/// 残留窗口 (已登记, 见 `docs/plan/tlb-shootdown-epoch.md` §6 登记项): 被覆盖槽位
/// 对"已读过父表项"的并发硬件页表遍历可见. `gen` 左移 1 位写入使 bit0 恒为 0
/// (x86_64 页表项"不存在"位), `next` 是页对齐物理地址 (bit0..11 恒为 0) —— 故遍历
/// 读到被覆盖槽位只会得到"不存在" → 正常缺页, 不会被误当作"存在"项翻译到其它物理页.
/// 数据页帧被覆盖则属该帧已无映射的既有语义 (其内容本就要丢弃).

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
/// 调用方必须保证: `frame` 是页对齐的有效物理帧地址, 且其前 16 字节当前不被任何
/// 映射引用 (即该帧已逻辑死亡、尚未归还 PMM).
unsafe fn frame_link_set_next(frame: u64, next: u64) {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.write(next) };
}

/// 读取帧内链节点的 gen 字段 (写入时左移 1 位, 读回时右移还原).
///
/// # Safety
///
/// 调用方必须保证: `frame` 是页对齐的有效物理帧地址, 且其前 16 字节当前不被任何
/// 映射引用 (即该帧已逻辑死亡、尚未归还 PMM).
unsafe fn frame_link_gen(frame: u64) -> u64 {
    let p = PhysAddr(frame).to_virt().0 as *mut u64;
    // SAFETY: 调用方保证 frame 为页对齐有效帧, 且前 16 字节不被任何映射引用.
    unsafe { p.add(1).read() >> 1 }
}

/// 写入帧内链节点的 gen 字段 (左移 1 位写入, 使页表帧 bit0 恒为 0).
///
/// # Safety
///
/// 调用方必须保证: `frame` 是页对齐的有效物理帧地址, 且其前 16 字节当前不被任何
/// 映射引用 (即该帧已逻辑死亡、尚未归还 PMM).
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

/// 非阻塞获取 `VMM_LOCK`: 锁已被占用时立即返回 `None`, 绝不重试或自旋.
///
/// 与 `VirtualMemoryManager::acquire_lock` 不同, 本函数只走一次
/// `compare_exchange` 即返回, **不复用** acquire 的单核可重入短路 — 该短路
/// 在多核下既非跨核互斥, 也会把"他人持锁"误判为获取成功.
/// 页表只读遍历 (`count_present_user_pages`) 用它避免与并发 map/unmap 争锁:
/// 拿不到即放弃本轮, 不阻塞调用者 (OOMD 运行在 scheduler tick 中断上下文).
///
/// 成功时关中断, 并在调试构建中置 `VMM_LOCK_RECURSIVE` (与 acquire 的 debug 语义一致).
fn try_acquire_lock() -> Option<IrqSaveFlags> {
    let flags = disable_interrupts();
    if VMM_LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        // 未取得锁: 恢复进入时的中断状态, 不留副作用.
        restore_interrupts(&flags);
        return None;
    }
    #[cfg(debug_assertions)]
    {
        // 成功获取时 VMM_LOCK_RECURSIVE 必为 false; 为 true 说明锁状态不一致 (死锁)
        assert!(
            !VMM_LOCK_RECURSIVE.swap(true, Ordering::Relaxed),
            "VMM_LOCK: recursive acquisition detected (deadlock)"
        );
    }
    Some(flags)
}

const MAX_USER_PAGE_TABLES: usize = 256;

#[derive(Clone, Copy)]
struct UserPageTable {
    pml4_phys: u64,
    in_use: bool,
}

pub struct VirtualMemoryManager {
    user_tables: UnsafeCell<[UserPageTable; MAX_USER_PAGE_TABLES]>,
    user_table_count: AtomicUsize,
    total_maps: AtomicU64,
    total_unmaps: AtomicU64,
    page_faults: AtomicU64,
}

// SAFETY: VMM_LOCK serializes all writes to user_tables (via UnsafeCell).
// SAFETY: VMM_LOCK 自旋锁保护所有可变状态, 原子计数器使用 Relaxed 顺序 (锁内单写者).
unsafe impl Sync for VirtualMemoryManager {}

impl VirtualMemoryManager {
    pub const fn new() -> Self {
        Self {
            user_tables: UnsafeCell::new(
                [UserPageTable {
                    pml4_phys: 0,
                    in_use: false,
                }; MAX_USER_PAGE_TABLES],
            ),
            user_table_count: AtomicUsize::new(0),
            total_maps: AtomicU64::new(0),
            total_unmaps: AtomicU64::new(0),
            page_faults: AtomicU64::new(0),
        }
    }

    pub fn init(&self) {
        // SAFETY: read_cr3() reads the CR3 control register — safe at any time
        let cr3 = unsafe { self.read_cr3() };

        KERNEL_PML4.store(cr3, Ordering::Release);

        super::api::kernel_pml4.store(cr3, Ordering::Release);

        // P1 C7: KPTI 实际页表隔离 — 分配 USER_PML4 并逐页装配入口依赖面
        // (KPTI-08 起不再复制内核高半区; 未激活时退化为整段复制)。
        // 完整功能需要汇编 entry/exit trampoline, 见 kpti.rs 模块顶部文档
        if !super::kpti::kpti_is_active()
            && crate::framework::config::KernelCapabilities::detect().kpti
        {
            // SAFETY: KERNEL_PML4 已初始化, PMM 可用, KPTI 全局状态在 init 独占
            unsafe {
                super::kpti::kpti_init(cr3);
            }
        }
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    /// 映射单个 4KB 页到内核页表 (`KERNEL_PML4`), 并累加映射计数.
    ///
    /// # Errors
    /// 当 VMM 未初始化 (`KERNEL_PML4` 为空) 时返回 `Err("VMM not initialized")`;
    /// 当中间页表 (PDPT/PD/PT) 分配失败时返回 `Err("Failed to allocate PDPT")` 等错误.
    pub fn map_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        let _flags = self.acquire_lock();

        let result = self.map_page_internal(virt, phys, flags);

        if result.is_ok() {
            self.total_maps.fetch_add(1, Ordering::Relaxed);
        }

        self.release_lock(&_flags);
        result
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    /// 映射大页 (2MB/1GB, 或退回 4KB) 到内核页表, 并累加映射计数.
    ///
    /// # Errors
    /// 当 `virt`/`phys` 未按 `size_type` 对齐时返回 `Err("Address not aligned for huge page")`;
    /// 其余错误同 `map_page` (VMM 未初始化或中间页表分配失败);
    /// 2MB/1GB 映射在对应目录条目已被拆分为页表时也会返回 `Err`.
    pub fn map_huge_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        size_type: PageSize,
    ) -> Result<(), &'static str> {
        if !size_type.is_aligned(virt.0) || !size_type.is_aligned(phys.0) {
            return Err("Address not aligned for huge page");
        }

        let _flags = self.acquire_lock();

        let mut flags = flags;
        flags.insert(PageFlags::HUGE_PAGE);

        let result = match size_type {
            PageSize::Size2M => self.map_2mb_page(virt, phys, flags),
            PageSize::Size1G => self.map_1gb_page(virt, phys, flags),
            PageSize::Size4K => self.map_page_internal(virt, phys, flags),
        };

        if result.is_ok() {
            self.total_maps.fetch_add(1, Ordering::Relaxed);
        }

        self.release_lock(&_flags);
        result
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    pub fn unmap_page(&self, virt: VirtAddr) {
        let _flags = self.acquire_lock();

        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            self.release_lock(&_flags);
            return;
        }

        // SAFETY: pml4_base = CR3 value, KERNEL_BASE offset produces valid kernel VA
        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // 安全门: 内核高半区防护
        // 禁止修改 PML4[256..511] (kernel high half).
        // 内核高半区翻译只由内核自身/KPTI 入口依赖面装配 (KPTI-08 后为逐页映射,
        // 不再与用户页表共享下层表页), 用户态页表修改路径无正当理由触碰该区间;
        // 触碰即破坏隔离边界或以错误语义改写内核翻译 ⇒ fail-closed 跳过.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] unmap_page: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            self.release_lock(&_flags);
            return;
        }

        // SAFETY: VMM_LOCK held. Page table walk with present-bit guards at each level.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;

            let pml4e = &*pml4.add(virt.pml4_idx());

            if !pml4e.is_present() {
                self.release_lock(&_flags);
                return;
            }

            // SAFETY: pml4e.frame() is present & valid frame; phys_to_virt gives kernel VA
            let pdpt = pml4e.frame().to_virt().0 as *mut PageTableEntry;
            let pdpte = &*pdpt.add(virt.pdpt_idx());

            if !pdpte.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pdpte.is_huge() {
                // 1GB 页: 直接清空 PDPT 项
                (*pdpt.add(virt.pdpt_idx())).set_value(0);
                // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效
                self.flush_tlb_remote(virt.0);
            } else {
                // SAFETY: pdpte.frame() valid; present && !huge → points to PD
                let pd = pdpte.frame().to_virt().0 as *mut PageTableEntry;
                let pde = &*pd.add(virt.pd_idx());

                if !pde.is_present() {
                    self.release_lock(&_flags);
                    return;
                }

                if pde.is_huge() {
                    (*pd.add(virt.pd_idx())).set_value(0);
                    // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效
                    self.flush_tlb_remote(virt.0);
                } else {
                    // SAFETY: pde.frame() valid; present && !huge → points to PT
                    let pt = pde.frame().to_virt().0 as *mut PageTableEntry;
                    (*pt.add(virt.pt_idx())).set_value(0);
                    // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效
                    self.flush_tlb_remote(virt.0);
                }
            }
        }

        self.total_unmaps.fetch_add(1, Ordering::Relaxed);
        self.release_lock(&_flags);
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    /// 修改虚拟页的保护属性 (mprotect 核心实现)
    ///
    /// 遍历四级页表找到 PTE, 修改 R/W/U/NX 位, 然后 flush TLB.
    /// 如果页不存在, 静默跳过 (mprotect 对未映射页无操作).
    pub fn protect_page(&self, virt: VirtAddr, new_flags: PageFlags) {
        let _flags = self.acquire_lock();

        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            self.release_lock(&_flags);
            return;
        }

        // SAFETY: pml4_base = CR3 value, KERNEL_BASE offset produces valid kernel VA
        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // 安全门: 内核高半区防护
        // 禁止修改 PML4[256..511] (kernel high half).
        // 内核高半区翻译只由内核自身/KPTI 入口依赖面装配, 用户态页表修改路径
        // 无正当理由触碰该区间 ⇒ fail-closed 跳过.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] protect_page: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            self.release_lock(&_flags);
            return;
        }

        // SAFETY: VMM_LOCK held. Page table walk with present-bit guards at each level.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;
            let pml4e = &*pml4.add(virt.pml4_idx());

            if !pml4e.is_present() {
                self.release_lock(&_flags);
                return;
            }

            let pdpt = pml4e.frame().to_virt().0 as *mut PageTableEntry;
            let pdpte = &*pdpt.add(virt.pdpt_idx());

            if !pdpte.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pdpte.is_huge() {
                // 1GB page: 修改 PDPT entry 的权限位
                let entry = pdpt.add(virt.pdpt_idx());
                let mut val = (*entry).value();
                // 保留物理帧地址和保留位, 仅修改权限位
                val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                (*entry).set_value(val);
                // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt.0);
                self.release_lock(&_flags);
                return;
            }

            let pd = pdpte.frame().to_virt().0 as *mut PageTableEntry;
            let pde = &*pd.add(virt.pd_idx());

            if !pde.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pde.is_huge() {
                // 2MB page: 修改 PD entry 的权限位
                let entry = pd.add(virt.pd_idx());
                let mut val = (*entry).value();
                val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                (*entry).set_value(val);
                // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt.0);
                self.release_lock(&_flags);
                return;
            }

            // 4KB page: 修改 PT entry 的权限位
            let pt = pde.frame().to_virt().0 as *mut PageTableEntry;
            let entry = pt.add(virt.pt_idx());
            let mut val = (*entry).value();
            val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
            val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
            (*entry).set_value(val);
            // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
            self.flush_tlb_remote(virt.0);
        }

        self.release_lock(&_flags);
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    /// 修改**指定页表**中虚拟页的保护属性 (mprotect 显式变体)
    ///
    /// 与 [`Self::protect_page`] 的唯一区别是目标页表由调用方显式给出 (`pml4`),
    /// 而非固定为内核表 `KERNEL_PML4`. 用户地址空间的 `mprotect` 必须走此变体:
    /// 用户数据页建在**进程用户页表**上 (见 `create_user_page_table` 的说明),
    /// 两表低半区不同源, 改内核表不会影响用户页权限.
    ///
    /// 遍历四级页表找到 PTE, 修改 R/W/U/NX 位, 然后 flush TLB.
    /// 如果页不存在, 静默跳过 (mprotect 对未映射页无操作).
    pub fn protect_page_in_table(&self, pml4: u64, virt: VirtAddr, new_flags: PageFlags) {
        if pml4 == 0 {
            return;
        }

        // 安全门: 内核高半区防护
        // 禁止修改 PML4[256..511] (kernel high half).
        // 内核高半区翻译只由内核自身/KPTI 入口依赖面装配, 进程用户页表的权限
        // 修改路径无正当理由触碰该区间 ⇒ fail-closed 跳过.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] protect_page_in_table: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            return;
        }

        let _flags = self.acquire_lock();

        // SAFETY: pml4 是进程用户页表根物理地址; phys_to_virt 给出内核 VA.
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: VMM_LOCK held. Page table walk with present-bit guards at each level.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;
            let pml4e = &*pml4.add(virt.pml4_idx());

            if !pml4e.is_present() {
                self.release_lock(&_flags);
                return;
            }

            let pdpt = pml4e.frame().to_virt().0 as *mut PageTableEntry;
            let pdpte = &*pdpt.add(virt.pdpt_idx());

            if !pdpte.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pdpte.is_huge() {
                // 1GB page: 修改 PDPT entry 的权限位
                let entry = pdpt.add(virt.pdpt_idx());
                let mut val = (*entry).value();
                // 保留物理帧地址和保留位, 仅修改权限位
                val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                (*entry).set_value(val);
                // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt.0);
                self.release_lock(&_flags);
                return;
            }

            let pd = pdpte.frame().to_virt().0 as *mut PageTableEntry;
            let pde = &*pd.add(virt.pd_idx());

            if !pde.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pde.is_huge() {
                // 2MB page: 修改 PD entry 的权限位
                let entry = pd.add(virt.pd_idx());
                let mut val = (*entry).value();
                val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
                (*entry).set_value(val);
                // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt.0);
                self.release_lock(&_flags);
                return;
            }

            // 4KB page: 修改 PT entry 的权限位
            let pt = pde.frame().to_virt().0 as *mut PageTableEntry;
            let entry = pt.add(virt.pt_idx());
            let mut val = (*entry).value();
            val &= !(PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
            val |= new_flags.bits() & (PAGE_PRESENT | PAGE_WRITABLE | PAGE_USER | PAGE_NX);
            (*entry).set_value(val);
            // 权限变更 (mprotect): 远程核可能缓存旧 R/W/U/NX 位, 必须远程失效 (S-9)
            self.flush_tlb_remote(virt.0);
        }

        self.release_lock(&_flags);
    }

    pub fn get_physical(&self, virt: VirtAddr) -> Option<PhysAddr> {
        self.get_physical_in_pml4(KERNEL_PML4.load(Ordering::Acquire), virt)
    }

    /// 在指定页表中翻译虚拟地址, 仅返回物理地址 (丢弃叶子项写位).
    ///
    /// 委托 [`Self::translate_in_pml4`], 供仅需物理地址的既有调用点使用.
    pub fn get_physical_in_pml4(&self, pml4: u64, virt: VirtAddr) -> Option<PhysAddr> {
        self.translate_in_pml4(pml4, virt).map(|t| t.phys)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 在指定页表中翻译虚拟地址, 返回物理地址与叶子项可写标志.
    ///
    /// 与 [`Self::get_physical_in_pml4`] 同一遍历逻辑, 额外透出叶子表项的写位,
    /// 供跨进程写路径判定目标页可写 (只读页拒绝写入). 未映射返回 `None`.
    pub fn translate_in_pml4(&self, pml4: u64, virt: VirtAddr) -> Option<PageTranslation> {
        if pml4 == 0 {
            return None;
        }

        // SAFETY: pml4 is a valid PML4 physical address; phys_to_virt gives kernel VA
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: 只读页表遍历. 在并发硬件页表遍历 (A/D 位) 下使用 volatile 读取保证正确性.
        unsafe {
            let pml4_raw = pml4_virt.0 as *const u64;
            let pml4e = pml4_raw.add(virt.pml4_idx()).read_volatile();
            if (pml4e & 1) == 0 {
                return None;
            }

            // SAFETY: pml4e present → frame bits point to valid PDPT
            let pdpt_virt = (pml4e & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pdpt_raw = pdpt_virt as *const u64;
            let pdpte = pdpt_raw.add(virt.pdpt_idx()).read_volatile();
            if (pdpte & 1) == 0 {
                return None;
            }

            if (pdpte & 0x80) != 0 {
                let frame = pdpte & 0x000FFFFFFFFFF000;
                let offset = virt.0 & (HUGE_PAGE_1G_SIZE - 1);
                return Some(PageTranslation {
                    phys: PhysAddr(frame + offset),
                    writable: (pdpte & PAGE_WRITABLE) != 0,
                });
            }

            // SAFETY: pdpte present && !huge → valid PD pointer
            let pd_virt = (pdpte & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pd_raw = pd_virt as *const u64;
            let pde = pd_raw.add(virt.pd_idx()).read_volatile();
            if (pde & 1) == 0 {
                return None;
            }

            if (pde & 0x80) != 0 {
                let frame = pde & 0x000FFFFFFFFFF000;
                let offset = virt.0 & (HUGE_PAGE_2M_SIZE - 1);
                return Some(PageTranslation {
                    phys: PhysAddr(frame + offset),
                    writable: (pde & PAGE_WRITABLE) != 0,
                });
            }

            // SAFETY: pde present && !huge → valid PT pointer
            let pt_virt = (pde & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pt_raw = pt_virt as *const u64;
            let pte = pt_raw.add(virt.pt_idx()).read_volatile();
            if (pte & 1) == 0 {
                return None;
            }

            let frame = pte & 0x000FFFFFFFFFF000;
            let offset = virt.0 & (PAGE_SIZE - 1);
            Some(PageTranslation {
                phys: PhysAddr(frame + offset),
                writable: (pte & PAGE_WRITABLE) != 0,
            })
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    #[expect(
        clippy::similar_names,
        reason = "similar_names: 变量名相似表达同族概念; 当前优先 expect"
    )]
    /// 读取 PTE 原始值 (用于 swap entry 检测)
    ///
    /// 返回 4KB 页的 PTE 原始值, 若页表层级不存在则返回 None.
    pub fn get_pte_value(&self, pml4: u64, virt: VirtAddr) -> Option<u64> {
        if pml4 == 0 {
            return None;
        }

        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let pml4_raw = pml4_virt.0 as *const u64;
            let pml4e = pml4_raw.add(virt.pml4_idx()).read_volatile();
            if (pml4e & 1) == 0 {
                return None;
            }

            let pdpt_virt = (pml4e & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pdpt_raw = pdpt_virt as *const u64;
            let pdpte = pdpt_raw.add(virt.pdpt_idx()).read_volatile();
            if (pdpte & 1) == 0 || (pdpte & 0x80) != 0 {
                return None;
            }

            let pd_virt = (pdpte & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pd_raw = pd_virt as *const u64;
            let pde = pd_raw.add(virt.pd_idx()).read_volatile();
            if (pde & 1) == 0 || (pde & 0x80) != 0 {
                return None;
            }

            let pt_virt = (pde & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pt_raw = pt_virt as *const u64;
            let pte = pt_raw.add(virt.pt_idx()).read_volatile();

            Some(pte)
        }
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 直接写入 PTE 原始值 (用于 swap 替换)
    ///
    /// 沿 PML4→PDPT→PD→PT 找到最终 PTE, 写入 `raw_pte` 后 TLB flush.
    /// 与 `map_page_in_table` 的区别: 接受任意 raw PTE (含 swap entry, 即将 present=0).
    /// 若任意中间层缺失 (P 位=0), 静默返回 (不创建中间页表, swap-out 不应触发缺中间页).
    pub fn set_pte_value(&self, pml4: u64, virt: VirtAddr, raw_pte: u64) {
        if pml4 == 0 {
            return;
        }

        let _flags = self.acquire_lock();
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: VMM_LOCK held; 四级页表查找 PTE 并直接写入
        unsafe {
            let pml4_raw = pml4_virt.0 as *const u64;
            let pml4e = pml4_raw.add(virt.pml4_idx()).read_volatile();
            if (pml4e & 1) == 0 {
                self.release_lock(&_flags);
                return;
            }

            let pdpt_virt = (pml4e & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pdpt_raw = pdpt_virt as *const u64;
            let pdpte = pdpt_raw.add(virt.pdpt_idx()).read_volatile();
            if (pdpte & 1) == 0 || (pdpte & 0x80) != 0 {
                self.release_lock(&_flags);
                return;
            }

            let pd_virt = (pdpte & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pd_raw = pd_virt as *const u64;
            let pde = pd_raw.add(virt.pd_idx()).read_volatile();
            if (pde & 1) == 0 || (pde & 0x80) != 0 {
                self.release_lock(&_flags);
                return;
            }

            let pt_virt = (pde & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let pt_ptr = (pt_virt as *mut u64).add(virt.pt_idx());
            pt_ptr.write_volatile(raw_pte);

            // 替换既有翻译 (swap-in/out 改写 PTE): 远程核可能缓存旧条目, 必须远程失效 (S-9)
            self.flush_tlb_remote(virt.0);
        }

        self.release_lock(&_flags);
    }

    pub fn switch_page_table(&self, pml4: u64) {
        // SAFETY: pml4 must point to a valid PML4 table; CR3 write is privileged
        unsafe {
            self.write_cr3(pml4);
        }
    }

    /// 创建新的用户进程页表: 装配内核"入口依赖面"必需页 (KPTI 激活时逐页显式映射,
    /// 未激活时退化为复制内核高半区), 低半区留待用户地址空间按需映射.
    ///
    /// # Panics
    /// 正常情况下不会 panic; 唯一存在的 unwrap 是
    /// `u64::from_le_bytes(buf[2..10].try_into().unwrap())`, 由于切片长度恒为 8 字节,
    /// 该 unwrap 实际不会触发 panic.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    pub fn create_user_page_table(&self) -> Option<u64> {
        let pmm = get_pmm();
        let pml4_phys = pmm.alloc_page()?;

        // SAFETY: pml4_phys from PMM, phys_to_virt valid; zero for clean state
        let pml4_virt = pml4_phys.to_virt();
        unsafe {
            core::ptr::write_bytes(pml4_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
        }

        let kernel_pml4 = KERNEL_PML4.load(Ordering::Acquire);

        // 装配内核映射 (KPTI-08 统一入口):
        // - KPTI 激活: 逐页装配"入口依赖面"必需内核页 (不再复制高半区整段别名);
        // - KPTI 未激活: 退化为复制 `KERNEL_PML4[256..511]` (无隔离语义不变).
        //
        // 入口代码 (`enter_user_asm` / `isr_common` / `irq_common` / `syscall_entry` /
        // KPTI 出口 stub) 在 CR3 切到本页表后仍需取指, 且切换前要访问
        // USER_CR3_SAVE / IDT / GDT 头区 / 各栈顶页 ⇒ 这些页必须在**每进程页表**
        // 中同样映射 (`kpti_init` 只覆盖共享模板). 不映射会导致 Ring 3 下首个时钟
        // 中断在 `irq_common` 写 USER_CR3_SAVE 时 #PF → Double Fault → 死锁.
        //
        // 低半部分 (PML4[0..256]) 保持全零: enter_user 在高半部分内核地址中切换
        // CR3, 不依赖低半部分映射; 用户进程的 ELF 段由加载器按需映射, 不应继承
        // 内核恒等映射.
        //
        // 每任务的内核栈顶页 (`TSS.RSP0` / 内核栈) 不在此装配: 该页随任务变化,
        // 由上下文切换路径 (`kpti::map_rsp0_page`) 在切换到该任务时追加.
        //
        // SAFETY: pml4_phys 由 PMM 分配并已清零; boot/进程创建路径, 无并发修改本页表.
        unsafe {
            crate::framework::mm::kpti::assemble_kernel_half(pml4_phys.as_u64(), kernel_pml4);
        }

        let _flags = self.acquire_lock();

        let idx = self.find_free_user_slot();
        if idx < MAX_USER_PAGE_TABLES {
            // SAFETY: VMM_LOCK held; exclusive access to user_tables via UnsafeCell
            unsafe {
                let tables = &mut *self.user_tables.get();
                tables[idx].pml4_phys = pml4_phys.as_u64();
                tables[idx].in_use = true;
            }
            self.user_table_count.fetch_add(1, Ordering::Relaxed);
        }

        self.release_lock(&_flags);

        // GDT / IDT / TSS 头区 / 各栈顶页的内核映射已由上方 `assemble_kernel_half`
        // 统一装配 (KPTI 激活时逐页; 未激活时由复制的高半区条目覆盖), 故此前紧跟
        // 其后的内联映射块 (sgdt/sidt/get_tss_base + 逐页 map_page_in_table + RSP0
        // 的 USER 映射) 已整体删除 —— 它与 `map_kpti_data_pages` 的映射面重复, 且
        // `get_tss_base` 依赖 TSS 描述符的运行时读取 (在 `create_user_page_table`
        // 早于 `gdt_init` 的时序下不可用). RSP0/内核栈顶页改由
        // `kpti::map_rsp0_page` 在上下文切换到该任务时按任务映射.

        Some(pml4_phys.as_u64())
    }

    /// 映射单个页 —— **自持 `VMM_LOCK` 变体**
    ///
    /// 语义与锁序约束见 [`Self::map_page_in_table_locked`]; 本入口只负责加/解锁,
    /// 供不持 `VMM_LOCK` 的路径使用。已持锁的路径必须直接用
    /// [`Self::map_page_in_table_locked`], 否则构成递归加锁 (`VMM_LOCK` 非重入)。
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    pub fn map_page_in_table(&self, pml4: u64, virt: VirtAddr, phys: PhysAddr, flags: PageFlags) {
        let _flags = self.acquire_lock();
        self.map_page_in_table_locked(pml4, virt, phys, flags);
        self.release_lock(&_flags);
    }

    /// 映射单个页 —— **要求调用方已持 `VMM_LOCK`**
    ///
    /// 完整 4 级页表遍历 (按需创建中间级), 写入叶项并按"是否替换既有映射"决定
    /// 本地/远程 TLB 失效。`flush_tlb_remote` 只置位"本临界区需远程失效"标志,
    /// 实际代发布与定向 IPI 在 `release_lock` 出口统一进行, 故本函数可安全用于
    /// 持锁路径 (如 COW fault 的判定-映射原子化)。
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    pub(crate) fn map_page_in_table_locked(
        &self,
        pml4: u64,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) {
        if pml4 == 0 {
            return;
        }

        // 安全门 1: 内核高半区防护
        // 禁止修改 PML4[256..511] (kernel high half).
        // 该区间在进程用户页表中只由 KPTI 入口依赖面装配 (KPTI-08 后为逐页映射,
        // 不再共享下层表页), 用户态映射路径在此处 map 会以错误的权限/语义改写
        // 装配面 ⇒ fail-closed 跳过.
        // user half (PML4[0..255]) 不在此限制内, 由 user 自己的 PDPT/PD 承载.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] map_page_in_table: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            return;
        }

        // SAFETY: pml4 is a valid PML4 address; VMM_LOCK held
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: 完整 4 级页表遍历与按需创建.
        // VMM_LOCK 串行化所有页表修改.
        unsafe {
            let pml4_ptr = pml4_virt.0 as *mut PageTableEntry;

            let (pdpt, split_pdpt) =
                self.get_or_create_table_entry(pml4_ptr.add(virt.pml4_idx()), true, 0);
            if pdpt.is_null() {
                return;
            }

            let (pd, split_pd) =
                self.get_or_create_table_entry(pdpt.add(virt.pdpt_idx()), true, HUGE_PAGE_2M_SIZE);
            if pd.is_null() {
                return;
            }

            let (pt, split_pt) =
                self.get_or_create_table_entry(pd.add(virt.pd_idx()), true, PAGE_SIZE);
            if pt.is_null() {
                crate::klog_boot_info!(
                    "[VMM] map_page_in_table: failed to get/create PT for {:#x}",
                    virt.0
                );
                return;
            }

            if flags.contains(PageFlags::USER) {
                // SAFETY: ptr.add(idx) stays within the 512-entry table.
                // 此处 pml4_idx < 256 (上方门检查保证), pdpt/PD 是 user 自己的页表,
                // 不与 kernel 共享 (低半区从不共享), 设 USER 位安全.
                (*pml4_ptr.add(virt.pml4_idx())).set_user(true);
                (*pdpt.add(virt.pdpt_idx())).set_user(true);
                (*pd.add(virt.pd_idx())).set_user(true);
            }

            let pte = &mut *pt.add(virt.pt_idx());
            let leaf_was_present = pte.is_present();
            pte.set_frame(phys);
            pte.set_flags(flags);

            // 判定"本次写入是否为替换" (叶项原本存在 / 本次拆分了巨页) ⇒ 远程失效;
            // 纯新建 (用户地址空间该 VA 首次映射, 或进程页表首次建立中间级) ⇒ 仅本核 (S-9).
            if split_pdpt || split_pd || split_pt || leaf_was_present {
                self.flush_tlb_remote(virt.0);
            } else {
                self.flush_tlb_local(virt.0);
            }
        }
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    pub fn unmap_page_in_table(&self, pml4: u64, virt: VirtAddr) {
        if pml4 == 0 {
            return;
        }

        // 安全门 1: 内核高半区防护
        // 禁止修改 PML4[256..511] (kernel high half).
        // 内核高半区在进程用户页表中的条目只由 KPTI 入口依赖面装配 (KPTI-08 后
        // 为逐页映射, 不再共享下层表页), 此处 unmap 的"递归释放空中间页表"会
        // 拆除该装配面 ⇒ fail-closed 跳过.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] unmap_page_in_table: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            return;
        }

        let _flags = self.acquire_lock();

        // SAFETY: pml4 = process CR3 value. phys_to_virt gives valid kernel VA.
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: 各级带存在位保护的只读页表遍历.
        // VMM_LOCK 串行化所有页表修改.
        unsafe {
            let pml4_tbl = pml4_virt.0 as *mut PageTableEntry;

            // SAFETY: pml4_tbl.add(idx) stays within the 4KB PML4 page
            let pml4e = &*pml4_tbl.add(virt.pml4_idx());

            if !pml4e.is_present() {
                self.release_lock(&_flags);
                return;
            }

            // SAFETY: pml4e.frame() is present & valid frame; phys_to_virt gives kernel VA
            let pdpt = pml4e.frame().to_virt().0 as *mut PageTableEntry;
            let pdpte = &*pdpt.add(virt.pdpt_idx());

            if !pdpte.is_present() {
                self.release_lock(&_flags);
                return;
            }

            if pdpte.is_huge() {
                // 1GB 页: 直接清空 PDPT 项
                (*pdpt.add(virt.pdpt_idx())).set_value(0);
                // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效
                self.flush_tlb_remote(virt.0);
            } else {
                // SAFETY: pdpte.frame() valid; present && !huge → points to PD
                let pd = pdpte.frame().to_virt().0 as *mut PageTableEntry;
                let pde = &*pd.add(virt.pd_idx());

                if !pde.is_present() {
                    self.release_lock(&_flags);
                    return;
                }

                if pde.is_huge() {
                    // 2MB 页: 直接清空 PDE 项
                    (*pd.add(virt.pd_idx())).set_value(0);
                    // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效 (S-9)
                    self.flush_tlb_remote(virt.0);
                } else {
                    // SAFETY: pde.frame() valid; present && !huge → points to PT
                    let pt = pde.frame().to_virt().0 as *mut PageTableEntry;
                    let pt_idx = virt.pt_idx();
                    // SAFETY: pt_idx 在 4KB PT 页范围内
                    let old_pte = (*pt.add(pt_idx)).value();
                    (*pt.add(pt_idx)).set_value(0);
                    // 拆除既有翻译 ⇒ 远程核可能缓存旧条目, 必须远程失效 (S-9)
                    self.flush_tlb_remote(virt.0);

                    // §8.1 规则 3: 拆除 USER leaf 即注销该映射持有的一份帧引用,
                    // 归零才延迟释放. **必须过滤 USER 位**: KPTI 入口依赖面的
                    // supervisor 页 (GDT/IDT/TSS/IST/trampoline/内核栈顶) 以
                    // PRESENT|WRITABLE (无 USER) 逐页映射进每个用户页表, 这些页的
                    // 物理帧归内核所有, 参与计数会被计入零 → 误释放内核页;
                    // 设备/MMIO 映射的 pfn 越界, frame_dec 侧 fail-closed 拒绝.
                    if old_pte & PAGE_PRESENT != 0 && old_pte & PAGE_USER != 0 {
                        let user_phys = old_pte & 0x000FFFFFFFFFF000;
                        if get_pmm().frame_dec(PhysAddr(user_phys)) {
                            super::release_frame_locked(PhysAddr(user_phys));
                        }
                    }

                    // 递归释放空的中间页表 (延迟到全部在线核 TLB 代追平后再真正释放)
                    if self.is_table_empty(pt) {
                        let pt_phys = pde.frame().as_u64();
                        (*pd.add(virt.pd_idx())).set_value(0);
                        super::release_frame_locked(PhysAddr(pt_phys));

                        if self.is_table_empty(pd) {
                            let pd_phys = pdpte.frame().as_u64();
                            (*pdpt.add(virt.pdpt_idx())).set_value(0);
                            super::release_frame_locked(PhysAddr(pd_phys));

                            if self.is_table_empty(pdpt) {
                                let pdpt_phys = pml4e.frame().as_u64();
                                (*pml4_tbl.add(virt.pml4_idx())).set_value(0);
                                super::release_frame_locked(PhysAddr(pdpt_phys));
                            }
                        }
                    }
                }
            }
        }

        self.total_unmaps.fetch_add(1, Ordering::Relaxed);
        self.release_lock(&_flags);
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    pub fn destroy_page_table(&self, pml4: u64) {
        if pml4 == 0 {
            return;
        }

        // 埋点基线: 与函数末尾的差值为本次调用真正送入延迟释放链的帧数.
        let admitted_before = DEFERRED_FREE_ADMITTED.load(Ordering::Relaxed);

        let _flags = self.acquire_lock();

        // SAFETY: pml4 valid; VMM_LOCK held
        let pml4_virt = PhysAddr(pml4).to_virt();

        // SAFETY: 遍历 4 级释放页表.
        // 仅用户空间项 (0..255); 内核项由内核自身/KPTI 入口依赖面装配, 本函数不动.
        //
        // 帧持有计数处理 (现由 PMM 计数面承载, 原 COW_REFS 已删除, 契约见
        // docs/plan/cr3-lifetime-ownership.md §8.1 文档的计数规则):
        // fork 时 clone_user_page_table_cow_inner 对每个被共享的物理页 frame_inc,
        // 每个 USER leaf 持有其帧一份引用。拆除整表即逐 leaf frame_dec, 归零才
        // defer_free。若子进程立即 exit 且未触发 COW fault, 该帧就此归还而不泄漏。
        // 锁序: VMM_LOCK 已持有, frame_dec 内部取 PMM 锁 (VMM → PMM 单向);
        // 禁止倒置 — 即禁止先取 PMM 锁再取 VMM_LOCK。
        unsafe {
            let pml4_ptr = pml4_virt.0 as *mut PageTableEntry;

            for i in 0..256usize {
                // SAFETY: pml4_ptr.add(i) within the 4KB PML4 page
                let pml4e = &*pml4_ptr.add(i);

                if pml4e.is_present() {
                    let pdpt_phys = pml4e.frame().as_u64();
                    let pdpt_virt = PhysAddr(pdpt_phys).to_virt();
                    let pdpt = pdpt_virt.0 as *mut PageTableEntry;

                    for j in 0..512usize {
                        // SAFETY: pdpt.add(j) within the 4KB PDPT page
                        let pdpte = &*pdpt.add(j);

                        if pdpte.is_present() && !pdpte.is_huge() {
                            let pd_phys = pdpte.frame().as_u64();
                            let pd_virt = PhysAddr(pd_phys).to_virt();
                            let pd = pd_virt.0 as *mut PageTableEntry;

                            for k in 0..512usize {
                                // SAFETY: pd.add(k) within the 4KB PD page
                                let pde = &*pd.add(k);

                                if pde.is_present() && !pde.is_huge() {
                                    let pt_phys = pde.frame().as_u64();
                                    let pt_virt = PhysAddr(pt_phys).to_virt();
                                    let pt = pt_virt.0 as *mut PageTableEntry;

                                    // 遍历 leaf PTE 释放用户数据物理页 (仅 4KB USER 页).
                                    //
                                    // §8.1 规则 3: 每个 USER leaf 持有其帧一份引用 ⇒ 拆除即
                                    // frame_dec, 归零才延迟释放. **必须过滤 USER 位**:
                                    // KPTI 入口依赖面的 supervisor 页 (GDT/IDT/TSS/IST/
                                    // trampoline/内核栈顶) 以 PRESENT|WRITABLE (无 USER)
                                    // 逐页映射进每个用户页表, 这些页的物理帧归内核所有,
                                    // 若参与计数会被计入零 → 误释放内核页.
                                    for l in 0..512usize {
                                        // SAFETY: pt.add(l) within the 4KB PT page
                                        let pte = &*pt.add(l);
                                        if pte.is_present() && pte.is_user() {
                                            let user_phys = pte.frame().as_u64();
                                            // 锁序: VMM_LOCK 已持有, frame_dec 内部取 PMM 锁
                                            // (VMM → PMM 单向, 与 alloc_page 路径同向).
                                            if get_pmm().frame_dec(PhysAddr(user_phys)) {
                                                // 持有计数归零: 延迟到全部在线核 TLB 代追平后再释放
                                                super::release_frame_locked(PhysAddr(user_phys));
                                            }
                                        }
                                    }

                                    super::release_frame_locked(PhysAddr(pt_phys));
                                }
                            }

                            super::release_frame_locked(PhysAddr(pd_phys));
                        }
                    }

                    super::release_frame_locked(PhysAddr(pdpt_phys));
                }
            }

            super::release_frame_locked(PhysAddr(pml4));
        }

        // SAFETY: VMM_LOCK held; only mutation is clearing user_tables slot
        let tables = unsafe { &mut *self.user_tables.get() };
        for i in 0..MAX_USER_PAGE_TABLES {
            if tables[i].pml4_phys == pml4 && tables[i].in_use {
                tables[i].in_use = false;
                tables[i].pml4_phys = 0;
                self.user_table_count.fetch_sub(1, Ordering::Relaxed);
                break;
            }
        }

        // 拆除地址空间页表即移除映射, 且上述帧已进入延迟释放队列: 若在线他核仍持有
        // 经陈旧映射缓存的 TLB 项, 帧被重分配后他核可能访问到他人占用的物理页.
        // 故必须登记"需远程失效", 由 release_lock 出临界区后先发布新代 + 定向 IPI,
        // 待全部在线核追平该代 (即均已彻底失效 TLB) 后再真正释放这些帧.
        // (本函数唯一早退在 acquire_lock 之前, 到此必已拆除映射.)
        if crate::framework::smp::is_enabled() && crate::framework::smp::get_cpu_count() > 1 {
            TLB_SHOOTDOWN_NEEDED.store(true, Ordering::Relaxed);
        }

        self.release_lock(&_flags);

        // 埋点 (见 docs/plan/tlb-shootdown-epoch.md S-10): "销毁路径是否被走到" 是释放
        // 覆盖判别的第一分位 —— 整轮日志无本行 ⇒ 进程从未被回收 (`Process::drop` 未运行),
        // 而非"帧入链但代未追平". 本行 `deferred_in_call=0` 则说明走到了但无可延迟帧.
        crate::klog_info!(
            Memory,
            "[VMM] destroy_page_table: cr3={:#X} deferred_in_call={}",
            pml4,
            DEFERRED_FREE_ADMITTED.load(Ordering::Relaxed) - admitted_before
        );
    }

    pub fn get_stats(&self) -> (u64, u64, u64) {
        (
            self.total_maps.load(Ordering::Relaxed),
            self.total_unmaps.load(Ordering::Relaxed),
            self.page_faults.load(Ordering::Relaxed),
        )
    }

    // ==================== 私有方法 ====================

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    fn map_page_internal(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        crate::klog_boot_info!("[VMM] map_page: virt={:#X} phys={:#X}", virt.0, phys.0);

        // 安全门: 禁止在 KERNEL_PML4 中修改内核高半区页表.
        // 内核高半区 (PML4[256..511]) 由 boot.asm 建立恒等映射 (1GB),
        // 后续只允许 map_page_in_table (via user PML4) 操作 user half.
        // 此门与 map_page_in_table 中的门对称, 防止 map_page/map_huge_page
        // 间接调用本函数时分裂内核大页导致 PDE 损坏.
        if virt.pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] map_page_internal: skip kernel-half virt={:#X} pml4_idx={}",
                virt.0,
                virt.pml4_idx()
            );
            return Ok(());
        }

        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return Err("VMM not initialized");
        }

        // SAFETY: KERNEL_PML4 valid; caller holds VMM_LOCK (via map_page/map_huge_page)
        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // SAFETY: Full 4-level page table walk with creation under VMM_LOCK
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;

            let (pdpt, split_pdpt) =
                self.get_or_create_table_entry(pml4.add(virt.pml4_idx()), true, 0);
            if pdpt.is_null() {
                return Err("Failed to allocate PDPT");
            }

            let (pd, split_pd) =
                self.get_or_create_table_entry(pdpt.add(virt.pdpt_idx()), true, HUGE_PAGE_2M_SIZE);
            if pd.is_null() {
                return Err("Failed to allocate PD");
            }

            let (pt, split_pt) =
                self.get_or_create_table_entry(pd.add(virt.pd_idx()), true, PAGE_SIZE);
            if pt.is_null() {
                return Err("Failed to allocate PT");
            }

            let pte = &mut *pt.add(virt.pt_idx());
            let leaf_was_present = pte.is_present();
            pte.set_frame(phys);
            pte.set_flags(flags);

            // 判定"本次写入是否为替换": 叶子项原本已存在 (覆盖), 或本次调用拆分了巨页
            // (旧巨页条目被换成下一级表指针). 二者皆否 = 纯新建 (该 VA 此前无翻译),
            // 远程核不可能缓存陈旧条目 ⇒ 只失效本核, 不登记远程需求 (S-9).
            if split_pdpt || split_pd || split_pt || leaf_was_present {
                self.flush_tlb_remote(virt.0);
            } else {
                self.flush_tlb_local(virt.0);
            }
        }

        Ok(())
    }

    fn map_2mb_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return Err("VMM not initialized");
        }

        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // SAFETY: 2MB huge page mapping at PD level. VMM_LOCK held by caller.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;

            let (pdpt, _) = self.get_or_create_table_entry(pml4.add(virt.pml4_idx()), true, 0);
            if pdpt.is_null() {
                return Err("Failed to allocate PDPT");
            }

            // 安全门: 如果 PDPT 条目是 1GB 大页且已映射, 禁止覆盖
            // (2MB 映射到已有 1GB 页的区域需拆分该 PDPTE; 拆分只改 KERNEL_PML4 的
            //  下一级表页, 与用户页表无共享关系 (KPTI-08 后 USER_PML4 只含入口
            //  依赖面逐页映射), 但拆分会改变内核自身翻译, 仍按不变更处理.)
            let pdpte = &*pdpt.add(virt.pdpt_idx());
            if pdpte.is_present() && pdpte.is_huge() {
                // 已有 1GB 大页覆盖此范围, 无需再映射 2MB
                return Ok(());
            }

            // 上方门已令 present 的 1GB PDPTE 提前返回 ⇒ 本次不会拆分巨页, 拆分标志恒 false.
            let (pd, _) =
                self.get_or_create_table_entry(pdpt.add(virt.pdpt_idx()), true, HUGE_PAGE_2M_SIZE);
            if pd.is_null() {
                return Err("Failed to allocate PD");
            }

            let pde = &mut *pd.add(virt.pd_idx());
            if pde.is_present() && !pde.is_huge() {
                return Err("PD entry already split to PT, cannot map 2MB page");
            }
            if pde.is_present() && pde.is_huge() {
                // 已有 2MB 映射, 不覆盖 (保持既有翻译不变)
                return Ok(());
            }
            pde.set_frame(phys);
            pde.set_flags(flags);

            // 到达此处的前提: 两条守卫已排除"PD 叶项 present"的两种情形, 且本次无巨页拆分
            // ⇒ 该 VA 此前不存在 2MB 翻译, 远程核不可能缓存陈旧条目 ⇒ 仅失效本核 (S-9).
            self.flush_tlb_local(virt.0);
        }

        Ok(())
    }

    fn map_1gb_page(
        &self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return Err("VMM not initialized");
        }

        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // SAFETY: 1GB huge page mapping at PDPT level. VMM_LOCK held by caller.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;

            let (pdpt, _) = self.get_or_create_table_entry(pml4.add(virt.pml4_idx()), true, 0);
            if pdpt.is_null() {
                return Err("Failed to allocate PDPT");
            }

            let pdpte = &mut *pdpt.add(virt.pdpt_idx());
            if pdpte.is_present() && !pdpte.is_huge() {
                return Err("PDPT entry already split, cannot map 1GB page");
            }
            if pdpte.is_present() && pdpte.is_huge() {
                // 已有 1GB 映射, 不覆盖
                return Ok(());
            }
            pdpte.set_frame(phys);
            pdpte.set_flags(flags);

            // 两条守卫已排除"PDPT 叶项 present"的两种情形, PML4 级又不含巨页 ⇒ 该 VA 此前
            // 不存在 1GB 翻译, 远程核不可能缓存陈旧条目 ⇒ 仅失效本核 (S-9).
            self.flush_tlb_local(virt.0);
        }

        Ok(())
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 取 (必要时创建) `entry` 指向的下一级页表, 返回 `(表指针, 本次是否拆分巨页)`.
    ///
    /// 第二个返回值供调用方判定"本次叶子写入是否为替换": 拆分把既有巨页条目换成下一级
    /// 表指针, 属**替换** (远程核可能缓存旧巨页条目 ⇒ 必须远程失效). 该事实不能从叶子
    /// 存在位隐式推得, 故由本函数显式回报 (见 docs/plan/tlb-shootdown-epoch.md S-9).
    unsafe fn get_or_create_table_entry(
        &self,
        entry: *mut PageTableEntry,
        create: bool,
        huge_step: u64,
    ) -> (*mut PageTableEntry, bool) {
        unsafe {
            // SAFETY: 调用方保证 `entry` 指向 PMM 分配的页表页内合法 PageTableEntry.
            // 解引用通过 512 项表大小做边界检查.
            let e = &*entry;

            if e.is_present() && !e.is_huge() {
                // SAFETY: Present && !huge → frame 位是合法的下一级表物理地址.
                // phys_to_virt 给出合法内核 VA.
                (e.frame().to_virt().0 as *mut PageTableEntry, false)
            } else if create {
                let pmm = get_pmm();

                pmm.alloc_page()
                    .map_or((core::ptr::null_mut(), false), |page| {
                        let page_virt = page.to_virt();
                        let pt = page_virt.0 as *mut PageTableEntry;
                        core::ptr::write_bytes(pt as *mut u8, 0, PAGE_SIZE as usize);

                        let split = e.is_huge();
                        if split {
                            // 拆分巨页: 从巨页帧填充 512 个子条目
                            // step = PAGE_SIZE → PD→PT (2MB→4KB), 新 PT 条目不需要 HUGE_PAGE
                            // step = HUGE_PAGE_2M_SIZE → PDPT→PD (1GB→2MB), 新 PD 条目需要 HUGE_PAGE
                            let huge_frame = e.frame();
                            let huge_flags = e.flags();
                            let step = if huge_step > 0 {
                                huge_step
                            } else {
                                PAGE_SIZE as u64
                            };
                            let mut new_flags =
                                (huge_flags & !PageFlags::HUGE_PAGE) | PageFlags::PRESENT;
                            if step == HUGE_PAGE_2M_SIZE {
                                // PDPT→PD 拆分: 新 PD 条目必须标记为 2MB 巨页,
                                // 否则 CPU 会将帧地址解释为 PT 指针, 导致页表遍历读取垃圾数据.
                                new_flags |= PageFlags::HUGE_PAGE;
                            }
                            crate::klog_boot_info!(
                                "[VMM] huge split: entry={:#X} frame={:#X} new_pt={:#X} step={:#X}",
                                entry as u64,
                                huge_frame.as_u64(),
                                page.as_u64(),
                                step
                            );
                            for i in 0..512 {
                                // SAFETY: pt points to a full 4KB page; add(i) stays within bounds
                                let pte = &mut *pt.add(i);
                                pte.set_frame(PhysAddr(huge_frame.as_u64() + i as u64 * step));
                                pte.set_flags(new_flags);
                            }
                        }

                        // SAFETY: `entry` 是合法 PDE/PDPTE 指针; 使用 set_value 一次性写入
                        // 新帧地址 + 标志, 避免 set_frame→set_flags 两步操作中间出现
                        // "帧=新PT, 标志=旧值(含HUGE)" 的瞬时不一致状态.
                        // 单次原子 store 保证 CPU 页表遍历器不会观察到中间态.
                        // 修复 (TRACK-INIT-RING3-PDE): 中间页表条目禁止设置 NX.
                        // PDE 的 NX 位语义是"该条目覆盖的整个区域不可执行" (2MB/1GB),
                        // 原 M9 修复 (16667750) 在此加 NX 意图防"用户态执行页表页",
                        // 但实际导致用户代码区 (0x400000 所在 PDE) 整体禁执行 →
                        // 用户态取指 #PF (e=0x15, P=1 U=1 I/D=1). 页表页的安全性由
                        // "用户页表低半区不映射页表页帧" 保证, 无需中间条目 NX.
                        let new_val = (page.as_u64() & 0x000FFFFFFFFFF000)
                            | (PageFlags::PRESENT | PageFlags::WRITABLE).bits();
                        (*entry).set_value(new_val);

                        (page_virt.0 as *mut PageTableEntry, split)
                    })
            } else {
                (core::ptr::null_mut(), false)
            }
        }
    }

    /// 将内核页表中的 2MB 巨页拆分为 4KB 页.
    ///
    /// # Errors
    /// 当 VMM 未初始化时返回 `Err("VMM not initialized")`;
    /// 当目标地址位于内核高半区 (PML4[256..511]) 时返回
    /// `Err("Cannot split kernel-half 2MB page (KPTI shared)")`;
    /// 当 PDPT/PD 不存在或 PD 条目未映射时分别返回 `Err("PDPT not present")`,
    /// `Err("PD not present")`, `Err("PD entry not present")`;
    /// 当分配新的页表页失败时返回 `Err("Failed to allocate PT")`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn split_2mb_page(&self, virt: u64) -> Result<(), &'static str> {
        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return Err("VMM not initialized");
        }

        // 安全门: 内核高半区防护
        // 禁止拆分内核高半区的 2MB 巨页 (PML4[256..511]).
        // 该区间由 boot.asm 建立的 1GB 恒等映射与内核自身装配承载, 拆分它会改变
        // 内核自身翻译; 且用户页表**不**继承该巨页映射 (KPTI-08 后入口依赖面为
        // 逐页映射), 无任何调用方需要拆分内核巨页 ⇒ fail-closed 拒绝.
        if VirtAddr(virt).pml4_idx() >= 256 {
            crate::klog_boot_info!(
                "[VMM] split_2mb_page: skip kernel-half virt={:#X} pml4_idx={}",
                virt,
                VirtAddr(virt).pml4_idx()
            );
            return Err("Cannot split kernel-half 2MB page (KPTI shared)");
        }

        let _flags = self.acquire_lock();

        let result: Result<(), &'static str> = (|| {
            let pml4_virt = PhysAddr(pml4_base).to_virt();
            let v = VirtAddr(virt);

            // SAFETY: 将 2MB 巨页拆分为 512 个 4KB 页.
            // VMM_LOCK 已持有, 所有页表修改串行化.
            unsafe {
                let pml4 = pml4_virt.0 as *mut PageTableEntry;
                // create=false: 只取现有表, 不创建也不拆分 ⇒ 拆分标志恒 false.
                let (pdpt, _) = self.get_or_create_table_entry(pml4.add(v.pml4_idx()), false, 0);
                if pdpt.is_null() {
                    return Err("PDPT not present");
                }

                let (pd, _) = self.get_or_create_table_entry(pdpt.add(v.pdpt_idx()), false, 0);
                if pd.is_null() {
                    return Err("PD not present");
                }

                let pd_entry = &mut *pd.add(v.pd_idx());
                if !pd_entry.is_present() {
                    return Err("PD entry not present");
                }
                if !pd_entry.is_huge() {
                    return Ok(());
                }

                let huge_frame = pd_entry.frame();
                let huge_flags = pd_entry.flags();

                let pmm = get_pmm();
                let pt_page = match pmm.alloc_page() {
                    Some(p) => p,
                    None => return Err("Failed to allocate PT"),
                };
                let pt = pt_page.to_virt().0 as *mut PageTableEntry;
                core::ptr::write_bytes(pt as *mut u8, 0, PAGE_SIZE as usize);

                for i in 0..512 {
                    // SAFETY: pt is a full 4KB PT page; add(i) stays in bounds
                    let pte = &mut *pt.add(i);
                    pte.set_frame(PhysAddr(huge_frame.as_u64() + i as u64 * PAGE_SIZE as u64));
                    pte.set_flags((huge_flags & !PageFlags::HUGE_PAGE) | PageFlags::PRESENT);
                    pte.set_present(true);
                }

                pd_entry.set_frame(pt_page);
                let new_flags = (huge_flags & !PageFlags::HUGE_PAGE) | PageFlags::PRESENT;
                pd_entry.set_flags(new_flags);

                // 巨页拆分: 2MB PDE 被替换为 4KB 页表, 拆分前已缓存的 2MB 条目必须远程失效 (S-9)
                self.flush_tlb_remote(virt);
            }

            Ok(())
        })();

        self.release_lock(&_flags);
        result
    }

    pub fn ensure_pml4_user(&self, virt: u64) {
        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return;
        }

        let v = VirtAddr(virt);

        // 安全门: KPTI 权限隔离
        // 禁止对内核高半区 (PML4[256..511]) 设置 USER 位.
        // 内核页表条目设 USER 位会允许用户态代码访问内核内存,
        // 破坏 Meltdown 缓解 (KPTI) 的安全边界.
        if v.pml4_idx() >= 256 {
            return;
        }

        // SAFETY: Setting USER bit on PML4 entry; KERNEL_PML4 valid, index in range
        let pml4_virt = PhysAddr(pml4_base).to_virt();
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;
            let entry = &mut *pml4.add(v.pml4_idx());
            if entry.is_present() && !entry.is_user() {
                entry.set_user(true);
                // 权限变更 (设 USER 位): 远程核可能缓存无 U 位的旧条目, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt);
            }
        }
    }

    pub fn ensure_path_user(&self, virt: u64) {
        let pml4_base = KERNEL_PML4.load(Ordering::Acquire);
        if pml4_base == 0 {
            return;
        }

        let v = VirtAddr(virt);

        // 安全门: KPTI 权限隔离
        // 禁止对内核高半区 (PML4[256..511]) 设置 USER 位.
        // 内核页表条目设 USER 位会允许用户态代码访问内核内存,
        // 破坏 Meltdown 缓解 (KPTI) 的安全边界.
        if v.pml4_idx() >= 256 {
            return;
        }

        let pml4_virt = PhysAddr(pml4_base).to_virt();

        // SAFETY: 遍历 PML4 → PDPT → PD, 各级设 USER 位.
        // 各级都有存在位保护. 索引由 VA 位计算.
        unsafe {
            let pml4 = pml4_virt.0 as *mut PageTableEntry;

            let pml4e = &mut *pml4.add(v.pml4_idx());
            if !pml4e.is_present() {
                return;
            }
            pml4e.set_user(true);

            let pdpt = pml4e.frame().to_virt().0 as *mut PageTableEntry;
            let pdpte = &mut *pdpt.add(v.pdpt_idx());
            if !pdpte.is_present() {
                return;
            }
            pdpte.set_user(true);

            if pdpte.is_huge() {
                // 权限变更 (设 USER 位): 远程核可能缓存无 U 位的旧条目, 必须远程失效 (S-9)
                self.flush_tlb_remote(virt);
                return;
            }

            let pd = pdpte.frame().to_virt().0 as *mut PageTableEntry;
            let pde = &mut *pd.add(v.pd_idx());
            if !pde.is_present() {
                return;
            }
            pde.set_user(true);
        }

        // 权限变更 (设 USER 位): 远程核可能缓存无 U 位的旧条目, 必须远程失效 (S-9)
        self.flush_tlb_remote(virt);
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    pub fn clone_user_page_table(&self, parent_pml4: u64) -> Option<u64> {
        if parent_pml4 == 0 {
            return None;
        }

        let _flags = self.acquire_lock();

        let pmm = get_pmm();
        let child_pml4_phys = pmm.alloc_page()?;
        let child_pml4_base = child_pml4_phys.to_virt().0 as *mut u64;

        // SAFETY: child_pml4_phys from PMM, phys_to_virt valid
        unsafe {
            core::ptr::write_bytes(child_pml4_base, 0, PAGE_SIZE as usize);
        }

        let kernel_pml4 = KERNEL_PML4.load(Ordering::Acquire);
        // 装配内核高半区 (KPTI-08 统一入口): 与 `create_user_page_table` / COW fork
        // 同源 —— KPTI 激活时逐页装配"入口依赖面"必需内核页, 未激活时整段复制
        // `KERNEL_PML4[256..511]`. 此前本处直接整段复制, 在 KPTI 激活下会把完整
        // 内核高半区别名重新注入子进程页表 (Meltdown 隔离缺口), 故统一到同一入口.
        //
        // SAFETY: kernel_pml4 由 vmm_init 写入; child_pml4_phys 由 PMM 分配并已清零;
        // 本函数持 VMM_LOCK, 无并发修改子页表.
        unsafe {
            crate::framework::mm::kpti::assemble_kernel_half(child_pml4_phys.as_u64(), kernel_pml4);
        }

        // SAFETY: parent_pml4 is a valid user PML4; VMM_LOCK held
        let parent_pml4_virt = PhysAddr(parent_pml4).to_virt().0 as *const u64;

        for i in 0..256u16 {
            // SAFETY: i in 0..255 within PML4 page; volatile for hardware-updated bits
            let parent_pml4e = unsafe { parent_pml4_virt.add(i as usize).read_volatile() };
            if (parent_pml4e & 1) == 0 {
                continue;
            }

            let child_pdpt_phys = pmm.alloc_page()?;
            let child_pdpt = child_pdpt_phys.to_virt().0 as *mut u64;
            // SAFETY: child_pdpt from PMM, phys_to_virt valid
            unsafe {
                core::ptr::write_bytes(child_pdpt, 0, PAGE_SIZE as usize);
            }

            let mut child_pml4e = parent_pml4e;
            child_pml4e = (child_pml4e & 0xFFF) | (child_pdpt_phys.as_u64() & 0x000FFFFFFFFFF000);
            // SAFETY: child_pml4_base 是合法的 4KB PML4 页; volatile 写以保证 TLB 一致性
            unsafe {
                child_pml4_base.add(i as usize).write_volatile(child_pml4e);
            }

            // SAFETY: parent_pml4e present → frame bits point to valid PDPT
            let parent_pdpt_virt = (parent_pml4e & 0x000FFFFFFFFFF000) + KERNEL_BASE;
            let parent_pdpt = parent_pdpt_virt as *const u64;

            for j in 0..512u16 {
                // SAFETY: j in 0..511 within PDPT page; volatile read
                let parent_pdpte = unsafe { parent_pdpt.add(j as usize).read_volatile() };
                if (parent_pdpte & 1) == 0 {
                    continue;
                }
                if (parent_pdpte & 0x80) != 0 {
                    continue;
                }

                let child_pd_phys = pmm.alloc_page()?;
                let child_pd = child_pd_phys.to_virt().0 as *mut u64;
                // SAFETY: child_pd from PMM
                unsafe {
                    core::ptr::write_bytes(child_pd, 0, PAGE_SIZE as usize);
                }

                let mut child_pdpte_v = parent_pdpte;
                child_pdpte_v =
                    (child_pdpte_v & 0xFFF) | (child_pd_phys.as_u64() & 0x000FFFFFFFFFF000);
                // SAFETY: child_pdpt valid; volatile write
                unsafe {
                    child_pdpt.add(j as usize).write_volatile(child_pdpte_v);
                }

                // SAFETY: parent_pdpte present → valid PD pointer
                let parent_pd_virt = (parent_pdpte & 0x000FFFFFFFFFF000) + KERNEL_BASE;
                let parent_pd = parent_pd_virt as *const u64;

                for k in 0..512u16 {
                    // SAFETY: k in 0..511 within PD page; volatile read
                    let parent_pde = unsafe { parent_pd.add(k as usize).read_volatile() };
                    if (parent_pde & 1) == 0 {
                        continue;
                    }

                    if (parent_pde & 0x80) != 0 {
                        // 深拷贝 2MB 巨页
                        let huge_phys = pmm.alloc_pages(512)?;
                        let huge_virt = PhysAddr(huge_phys.as_u64()).to_virt().0;
                        // SAFETY: parent_huge is valid 2MB kernel VA
                        let parent_huge = (parent_pde & 0x000FFFFFFFFFF000) + KERNEL_BASE;
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                parent_huge as *const u8,
                                huge_virt as *mut u8,
                                2 * 1024 * 1024,
                            );
                        }
                        let mut child_pde_v = parent_pde;
                        child_pde_v =
                            (child_pde_v & 0xFFF) | (huge_phys.as_u64() & 0x000FFFFFFFFFF000);
                        // SAFETY: child_pd valid; volatile write
                        unsafe {
                            child_pd.add(k as usize).write_volatile(child_pde_v);
                        }
                        continue;
                    }

                    let child_pt_phys = pmm.alloc_page()?;
                    let child_pt = child_pt_phys.to_virt().0 as *mut u64;
                    // SAFETY: child_pt from PMM
                    unsafe {
                        core::ptr::write_bytes(child_pt, 0, PAGE_SIZE as usize);
                    }

                    let mut child_pde_v = parent_pde;
                    child_pde_v =
                        (child_pde_v & 0xFFF) | (child_pt_phys.as_u64() & 0x000FFFFFFFFFF000);
                    // SAFETY: child_pd valid; volatile write
                    unsafe {
                        child_pd.add(k as usize).write_volatile(child_pde_v);
                    }

                    // SAFETY: parent_pde present && !huge → valid PT pointer
                    let parent_pt_virt = (parent_pde & 0x000FFFFFFFFFF000) + KERNEL_BASE;
                    let parent_pt = parent_pt_virt as *const u64;

                    for l in 0..512u16 {
                        // SAFETY: l in 0..511 within PT page; volatile read
                        let parent_pte = unsafe { parent_pt.add(l as usize).read_volatile() };
                        if (parent_pte & 1) == 0 {
                            continue;
                        }

                        let child_page_phys = pmm.alloc_page()?;
                        let child_page_virt = PhysAddr(child_page_phys.as_u64()).to_virt().0;
                        // SAFETY: parent_page_virt is valid kernel VA from PTE
                        let parent_page_virt = (parent_pte & 0x000FFFFFFFFFF000) + KERNEL_BASE;

                        // SAFETY: Both addresses are valid 4KB kernel VAs
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                parent_page_virt as *const u8,
                                child_page_virt as *mut u8,
                                PAGE_SIZE as usize,
                            );
                        }

                        let mut child_pte_v = parent_pte;
                        child_pte_v =
                            (child_pte_v & 0xFFF) | (child_page_phys.as_u64() & 0x000FFFFFFFFFF000);
                        // SAFETY: child_pt valid; volatile write
                        unsafe {
                            child_pt.add(l as usize).write_volatile(child_pte_v);
                        }
                    }
                }
            }
        }

        self.release_lock(&_flags);
        Some(child_pml4_phys.as_u64())
    }

    fn find_free_user_slot(&self) -> usize {
        // SAFETY: Read-only access to user_tables via UnsafeCell under VMM_LOCK.
        let tables = unsafe { &*self.user_tables.get() };
        for i in 0..MAX_USER_PAGE_TABLES {
            if !tables[i].in_use {
                return i;
            }
        }
        MAX_USER_PAGE_TABLES
    }

    #[inline(always)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    /// 获取 VMM 锁 (关中断 + CAS 自旋).
    ///
    /// 本锁是**非重入**自旋锁: 调用方不得在持锁期间进入任何会再次获取 `VMM_LOCK`
    /// 的路径. 经核实全部 21 处临界区 (x86_64 13 + aarch64 7 + cow.rs 1) 区内都不访问
    /// 用户地址、也不嵌套调用任何会再次取 `VMM_LOCK` 的公有方法, 故不存在同核递归路径.
    ///
    /// 此前存在的"单核可重入短路" (`if VMM_LOCK.load(..) { return flags; }`) 已移除:
    /// 该短路在多核下既非跨核互斥 (他核持锁时本核被误判为获取成功而直接进临界区),
    /// 其配套的 `release_lock` 又会无条件 `store(false)` 释放他人持有的锁; 运行时插桩
    /// 亦证实该短路在单核全测试套/单核完整启动/2 核启动下命中均为 0, 无保留必要.
    ///
    /// # Panics
    /// 在 `debug_assertions` 构建下, 若检测到 `VMM_LOCK` 被递归获取 (死锁), 触发 `assert!`
    /// panic, 错误信息为 "`VMM_LOCK`: recursive acquisition detected (deadlock)".
    pub fn acquire_lock(&self) -> IrqSaveFlags {
        let flags = disable_interrupts();
        while VMM_LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        #[cfg(debug_assertions)]
        {
            // 不可恢复: VMM_LOCK 递归获取意味着死锁, 继续执行只会挂起系统
            assert!(
                !VMM_LOCK_RECURSIVE.swap(true, Ordering::Relaxed),
                "VMM_LOCK: recursive acquisition detected (deadlock)"
            );
        }
        flags
    }

    #[inline(always)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是既有 API 约定 (改传值会波及调用点); 当前优先 expect"
    )]
    pub fn release_lock(&self, flags: &IrqSaveFlags) {
        // 仍持锁时读取并清除"本临界区需远程失效"标志: VMM_LOCK 是全局锁, 此刻本核
        // 是唯一写者, 故读-清不会漏掉本临界区自身的置位.
        let shootdown_needed = TLB_SHOOTDOWN_NEEDED.swap(false, Ordering::Relaxed);

        // 仍持锁时整条摘走本临界区批次链 (链头置 0). 摘走必须在同一临界区内完成:
        // 先释放锁再摘会漏摘他核进入临界区后新增的帧.
        let batch = BATCH_HEAD.swap(0, Ordering::AcqRel);

        #[cfg(debug_assertions)]
        {
            VMM_LOCK_RECURSIVE.store(false, Ordering::Relaxed);
        }
        VMM_LOCK.store(false, Ordering::Release);
        restore_interrupts(flags);

        // 远程失效 = 发布新代 + 定向 IPI (含本核), **不等待** —— 代计数下无需 ack,
        // 等待一律发生在锁外且中断开启 (锁内等 ack 会与被本锁挡住的对端互等死锁).
        // 单核 / SMP 未启用时 `flush_tlb_remote` 本就不置位, 取当前代 (恒 0) 即可立即释放.
        let smp_active =
            crate::framework::smp::is_enabled() && crate::framework::smp::get_cpu_count() > 1;
        let g = if shootdown_needed && smp_active {
            crate::framework::smp::tlb_gen_publish_and_shoot()
        } else {
            crate::framework::smp::tlb_gen_now()
        };

        // 无在线核时 `tlb_gen_min_online` 返回 u64::MAX ⇒ 恒 `>= g` ⇒ 立即释放:
        // 该情形只出现在 SMP 未初始化 (单核, 无远程 TLB 缓存), 与 HEAD 行为等价.
        let min = crate::framework::smp::tlb_gen_min_online();

        // 计数基线: 仅用于判断"本次 release_lock 是否发生了归还".
        let before = DEFERRED_FREE_RELEASED.load(Ordering::Relaxed);

        // 先排空历史 pending (其代更老, 更可能已追平), 再结算本批 —— 反序会让刚挂入的整条批次链被立即摘下又挂回 (两次 O(n) 无效往返).
        if PENDING_NONEMPTY.load(Ordering::Relaxed) {
            drain_pending(min);
        }
        if batch != 0 {
            settle_batch(batch, g, min);
        }

        // SIMPLIFIED: 只统计"释放帧总数"这一聚合量, 不区分批次/来源 (本批结算 vs 历史
        // pending 排空); 影响面为排查时无法从日志区分是哪条路径释放的; 何时需扩展:
        // 需要定位滞留来源时, 改为分路径计数.
        //
        // 埋点输出 (见 docs/plan/tlb-shootdown-epoch.md S-10): 本临界区**入链了帧**时也必须
        // 打印 —— "入链了却一帧未归还" 与 "从未入链" 是两个完全不同的诊断, 只打归还数会把
        // 前者静默掩盖 (`admitted` 的基线无法像 `released` 那样在函数内取到: 入链发生在
        // `release_lock` 之前的同一临界区内, 故用 `batch != 0` 判"本临界区有帧入链").
        // `pending` / `gen_g` / `min` 三者共同给出"是否因代未追平而滞留".
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

    #[inline(always)]
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    unsafe fn read_cr3(&self) -> u64 {
        // SAFETY: Reading CR3 is always safe; returns current page table base
        crate::arch!(read_page_table_base())
    }

    #[inline(always)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    // SAFETY: val 必须指向有效 PML4 页表; 调用方保证页表分配后保持有效.
    unsafe fn write_cr3(&self, val: u64) {
        // SAFETY: val must point to a valid PML4 table; caller guarantees this
        crate::arch!(write_page_table_base(val));
    }

    #[inline(always)]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 本核页级 TLB 失效, **不**登记远程失效需求.
    ///
    /// 仅用于"目标 VA 此前不存在翻译"的**纯新建**映射: x86 不缓存"不存在"的翻译,
    /// 远程核不可能持有该 VA 的陈旧条目, 远程失效无对象 (若一律登记, 每次新建映射都会
    /// 向全部在线核发一轮 IPI —— 过度失效, 见 docs/plan/tlb-shootdown-epoch.md S-9).
    fn flush_tlb_local(&self, addr: u64) {
        crate::arch!(tlb_flush_page(addr as usize));
    }

    #[inline(always)]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 本核页级 TLB 失效 + 登记"本临界区需远程失效".
    ///
    /// 用于**替换/覆盖既有翻译** (拆除、重映射、巨页拆分、swap 替换) 与**权限变更**
    /// (mprotect / 设 USER 位): 远程核可能已缓存旧条目, 不失效即读到陈旧翻译.
    fn flush_tlb_remote(&self, addr: u64) {
        crate::arch!(tlb_flush_page(addr as usize));

        // SIMPLIFIED: 远程失效粒度取全量 `tlb_flush_all` (对端重载 CR3) 而非页级地址载荷;
        // 影响面为 shootdown 触发时远程核全量 TLB 失效 (性能开销, 非正确性问题);
        // 何时需扩展: shootdown 成为热路径或页级失效收益显现时, 引入地址载荷
        // (需同时解决载荷复制/溢出回退/并发写者规避).
        if crate::framework::smp::is_enabled() && crate::framework::smp::get_cpu_count() > 1 {
            // 本函数在 VMM_LOCK 临界区内被调用: 此处只登记"本临界区需远程失效",
            // 真正的"发布代 + 定向 IPI"由 release_lock 出临界区后执行
            // (临界区内等对端响应会与被本锁挡住的对端互等死锁).
            TLB_SHOOTDOWN_NEEDED.store(true, Ordering::Relaxed);
        }
    }

    /// 记录一个待释放物理帧: 头插本临界区批次链, 延后到"全部在线核已追平该批代数"
    /// 之后才真正归还 PMM.
    ///
    /// 必须在持 `VMM_LOCK` 期间调用 (链头由该锁串行化, 锁内单写者). 容量无上限:
    /// 溢出帧除"记住"别无出路 —— 批次代只能在 `release_lock` 处发布, 锁内等待会与
    /// 被本锁挡住的核互等死锁.
    ///
    /// `pub(crate)`: `mm::cow` 的 COW 复制分支也须走本入口 (帧计入零但远端核 TLB
    /// 未追平, 见 docs/plan/tlb-shootdown-epoch.md §7.1).
    ///
    /// SIMPLIFIED: 排空点唯一 (`release_lock` 出口), 不引入 tick / 返回用户态前的
    /// 额外排空点; 影响面为未追平帧可能滞留到下一次任一核的 `release_lock`
    /// (不丢帧, 只推迟归还, 与 HEAD 的单核行为等价 —— 单核下代恒 0, 立即释放);
    /// 何时需扩展: 出现长时间无 VMM 操作却需及时回收的负载时, 再补排空点.
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub(crate) fn defer_free(&self, frame: u64) {
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

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn is_table_empty(&self, table: *mut PageTableEntry) -> bool {
        for i in 0..512usize {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                if (*table.add(i)).is_present() {
                    return false;
                }
            }
        }
        true
    }
}

static GLOBAL_VMM: OnceLock<VirtualMemoryManager> = OnceLock::new();

pub fn vmm_init() {
    GLOBAL_VMM.get_or_init(|slot| {
        let vmm = VirtualMemoryManager::new();
        vmm.init();
        slot.write(vmm);
    });
}

pub fn get_vmm() -> &'static VirtualMemoryManager {
    GLOBAL_VMM.get_or_panic("VMM")
}

/// 返回 `GLOBAL_VMM` `OnceLock` 的内部状态机原始值 (仅用于诊断).
///
/// 返回值: 0=未初始化, 1=初始化中, 2=已完成.
/// 与 `get_vmm()` 不同, 本函数不会 panic, 可在 VMM 初始化前安全调用.
pub fn vmm_debug_state() -> u8 {
    GLOBAL_VMM.debug_state()
}

pub fn get_kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Acquire)
}

pub fn get_current_pml4() -> u64 {
    let cr3 = crate::arch!(read_page_table_base());
    if cr3 != 0 {
        cr3
    } else {
        KERNEL_PML4.load(Ordering::Acquire)
    }
}

/// 运行期跨核 TLB 失效探针 (S-13) —— 引导期一次性自检.
///
/// **判别力来源**: 不比对 shootdown 计数, 而是要求每个**远程核**在 `0xFD` 接收
/// 路径内 ([`crate::framework::smp::tlb_probe_report`]) 读本探针的专用探测页
/// (`TLB_PROBE_VA`) 并按 (代, 观测字节) 报告. 因此能确定性检出三类注入:
/// - `flush_tlb_remote` 不置位 (无远程失效登记 ⇒ 无 IPI) ⇒ 远程核永不报告 ⇒ FAIL;
/// - 定向 IPI 未送达 ⇒ 同上 ⇒ FAIL;
/// - `tlb_flush_all` 退化为空操作 ⇒ 远程核报告陈旧字节 (帧 A = `0xAA`) ⇒ FAIL.
///
/// 见 `docs/plan/tlb-shootdown-epoch.md` §5 注入 3 / 注入 4.
///
/// **不设 GLOBAL 位**: `tlb_flush_all` 的实现是重载 CR3, 而重载 CR3 **不失效
/// GLOBAL 页** —— 探测页一旦带 GLOBAL 位, 远程核会跨 flush 保留陈旧翻译,
/// 探针随即失去判别力 (字节比对恒为旧值).
///
/// 返回 `true` = 通过 (含单核 / SMP 未启用下的跳过); `false` = 判别性失败.
pub fn tlb_probe_selftest() -> bool {
    use crate::framework::smp;

    // 单核 / SMP 未启用: 无远程核可观测, 探针无判别对象 ⇒ 跳过 (不构成失败).
    if !smp::is_enabled() || smp::get_cpu_count() <= 1 {
        crate::klog_boot_info!("[SMP] TLB probe SKIP (single core / SMP disabled)");
        return true;
    }

    let Some(frame_a) = get_pmm().alloc_page() else {
        crate::klog_boot_info!("[SMP] TLB probe FAIL (alloc frame A)");
        return false;
    };
    let Some(frame_b) = get_pmm().alloc_page() else {
        get_pmm().free_page(frame_a);
        crate::klog_boot_info!("[SMP] TLB probe FAIL (alloc frame B)");
        return false;
    };

    // 播种两个数据帧: A = 0xAA, B = 0xBB (经内核直接映射 VA 写入).
    // SAFETY: frame_a / frame_b 由 PMM 分配, 各为完整 4KB 页; `to_virt` 给出合法
    // 内核直接映射 VA; 探针尚未装备 (`tlb_probe_arm` 未调用), 接收侧不访问该页,
    // 无并发读者.
    unsafe {
        core::ptr::write_volatile(frame_a.to_virt().0 as *mut u8, 0xAA);
        core::ptr::write_volatile(frame_b.to_virt().0 as *mut u8, 0xBB);
    }

    let probe_va = VirtAddr(crate::framework::mm::TLB_PROBE_VA);
    let vmm = get_vmm();
    let flags = PageFlags::PRESENT | PageFlags::WRITABLE;

    // 建映射到帧 A. 该 VA 此前无翻译 ⇒ `map_page_internal` 走"纯新建"分支 (仅本核
    // 失效, 不登记远程), 故下面显式发布一次 shootdown 作为 seq=1 的触发源.
    if vmm.map_page(probe_va, frame_a, flags).is_err() {
        get_pmm().free_page(frame_a);
        get_pmm().free_page(frame_b);
        crate::klog_boot_info!("[SMP] TLB probe FAIL (map frame A)");
        return false;
    }

    // seq=1: 装备探针 → 显式发布代 + 广播 0xFD → 远程核应在接收路径读到帧 A (0xAA).
    smp::tlb_probe_arm(1);
    smp::tlb_gen_publish_and_shoot();
    let miss1 = smp::tlb_probe_wait_remotes(1, 0xAA);

    // seq=2: **先装备, 再重映射**到帧 B —— 叶子已存在 ⇒ `map_page_internal` 必经
    // `flush_tlb_remote` (登记远程失效), 由 `map_page` 出临界区时发布代 + 广播 IPI.
    // 此为注入 3 的检出点: 若 `flush_tlb_remote` 不置位则无 IPI, 远程核永不报告
    // seq=2. 装备必须先于重映射 (IPI 在重映射之后的 release_lock 内才发出).
    smp::tlb_probe_arm(2);
    let remap_ok = vmm.map_page(probe_va, frame_b, flags).is_ok();
    let miss2 = smp::tlb_probe_wait_remotes(2, 0xBB);

    // 先收起探针再拆除映射: 拆除本身会广播 IPI, 若此刻仍装备, 接收侧将去读一个
    // 正被清零的页 (缺页). 收起后接收侧对探测页零访问.
    smp::tlb_probe_disarm();

    // 完整回收 (零残留): `unmap_page_in_table` 清叶子 + 远程失效 + 递归释放变空的
    // PT/PD/PDPT 三个表页 + 清零 `KERNEL_PML4[255]`. 叶子非 USER ⇒ 其回收路径不做
    // frame_dec, 两个数据帧须自行归还.
    vmm.unmap_page_in_table(get_kernel_pml4(), probe_va);
    super::release_frame(frame_a);
    super::release_frame(frame_b);

    if miss1 == 0 && miss2 == 0 && remap_ok {
        crate::klog_boot_info!(
            "[SMP] TLB probe PASS (remotes observed seq1=0xAA, seq2=0xBB; cpu_count={})",
            smp::get_cpu_count()
        );
        true
    } else {
        crate::klog_boot_info!(
            "[SMP] TLB probe FAIL (miss1={} miss2={} remap_ok={} cpu_count={})",
            miss1,
            miss2,
            remap_ok,
            smp::get_cpu_count()
        );
        false
    }
}

/// 统计进程页表中已映射的用户页数 (4 KiB 粒度, RSS 近似) — **非阻塞**.
///
/// 只读遍历 PML4 用户半区 (`0..256`). 遍历前经 `try_acquire_lock` **非阻塞**
/// 获取 `VMM_LOCK`: 锁被并发 map/unmap 占用时**立即返回 `None`, 不做任何遍历**,
/// 调用方应跳过本轮 (不得将 `None` 当作 0 参与比较). 持锁遍历期间并发
/// `unmap_page_in_table` 无法递归释放变空的中间页表 (`get_pmm().free_page`),
/// 消除踩野指针的竞态 (供 OOMD 在内存紧急时挑选占用最大的进程).
///
/// # Arguments
/// * `cr3` — 进程页表根物理地址 (`Process::cr3`); 0 表示无用户页表.
///
/// # Returns
/// * `Some(页数)` — 成功持锁遍历所得 (大页按其覆盖的 4 KiB 页数折算);
///   `cr3 == 0` 时返回 `Some(0)`.
/// * `None` — `VMM_LOCK` 被占用, 未做遍历, 调用方应跳过本轮.
///
/// # 调用约束
/// 调用方不得在已持 `VMM_LOCK` 的情况下调用本函数.
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pdpt/pd/pt 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub fn count_present_user_pages(cr3: u64) -> Option<u64> {
    if cr3 == 0 {
        return Some(0);
    }

    // 非阻塞获取: 锁被占用则不遍历, 直接放弃本轮.
    let Some(flags) = try_acquire_lock() else {
        return None;
    };

    let vmm = get_vmm();

    let mut pages = 0u64;
    let pml4_ptr = PhysAddr(cr3).to_virt().0 as *const PageTableEntry;

    // SAFETY: cr3 为进程有效 PML4 物理地址, 经直接映射转为可读虚拟地址;
    // 仅读取 4 级页表结构不做修改; 各层索引均限制在 4 KiB 表内 (< 256 / < 512);
    // 全程持 VMM_LOCK, 中间页表不会被并发 unmap 递归释放, 指针在遍历期间有效.
    unsafe {
        for i in 0..256usize {
            let pml4e = &*pml4_ptr.add(i);
            if !pml4e.is_present() {
                continue;
            }
            let pdpt_ptr = pml4e.frame().to_virt().0 as *const PageTableEntry;

            for j in 0..512usize {
                let pdpte = &*pdpt_ptr.add(j);
                if !pdpte.is_present() {
                    continue;
                }
                if pdpte.is_huge() {
                    // PDPTE 级大页 = 1 GiB = 512 × 512 个 4 KiB 页
                    pages += 512 * 512;
                    continue;
                }
                let pd_ptr = pdpte.frame().to_virt().0 as *const PageTableEntry;

                for k in 0..512usize {
                    let pde = &*pd_ptr.add(k);
                    if !pde.is_present() {
                        continue;
                    }
                    if pde.is_huge() {
                        // PDE 级大页 = 2 MiB = 512 个 4 KiB 页
                        pages += 512;
                        continue;
                    }
                    let pt_ptr = pde.frame().to_virt().0 as *const PageTableEntry;

                    for l in 0..512usize {
                        if (&*pt_ptr.add(l)).is_present() {
                            pages += 1;
                        }
                    }
                }
            }
        }
    }

    vmm.release_lock(&flags);
    Some(pages)
}
