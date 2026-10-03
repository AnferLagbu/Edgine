//! AArch64 SMP 次核 (AP) 启动
//!
//! BSP 在 [`init`] 中依次经 PSCI `CPU_ON` 上电各 AP; 每个 AP 以 MMU 关 / 物理
//! 地址进入 `boot/aarch64/start.S` 的 `ap_entry_asm` stub (SMP-03), 由 BSP 预写
//! 的 [`ApBootInfo`] 槽装入 TTBR0/TTBR1/MAIR/TCR/SCTLR 与本核私有栈, 开 MMU 后
//! 跳入 [`ap_main`]。
//!
//! 握手协议: BSP 写满槽 → 按 cache line 清至 PoC → PSCI `CPU_ON` → 有界自旋等待
//! AP 置 `done`。AP 在 GIC/per-CPU 状态登记完毕后置 `done=1`。
//!
//! 启动槽符号 `_ap_boot_info`/`ap_entry_asm` 位于低半区 (VMA == PA), 距高半区内核
//! 代码超出 `adrp` 的 ±4GB 可达范围, 故二者均由链接脚本 (`link/aarch64.ld`) 暴露
//! 高半区别名 (`ap_boot_info_ptr` / `ap_entry_asm_alias`) 供本模块取址。

use alloc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 次核私有内核栈大小 (16 KiB)。
const AP_STACK_SIZE: usize = 16 * 1024;

/// BSP 等待单次 `done` 握手的自旋上界 (轮次)。
///
/// SIMPLIFIED: 以固定轮次忙等替代定时器超时; 影响面: 超时阈值随 CPU 主频浮动,
///   过短可能误判慢速 AP 离线; 何时需扩展: 引入基于 CNTPCT_EL0 的定时超时。
const DONE_TIMEOUT_LOOPS: u32 = 100_000_000;

/// 次核启动信息槽布局 (与 `boot/aarch64/start.S` 的 `ap_entry_asm` 字节偏移一一对应)。
///
/// 全部字段为 64 位, 共 10 项 (80 字节), 由编译期断言强制。
#[repr(C)]
struct ApBootInfo {
    /// `[0x00]` TTBR0_EL1 值 (BSP 预写的当前页表基址)。
    ttbr0: u64,
    /// `[0x08]` TTBR1_EL1 值。
    ttbr1: u64,
    /// `[0x10]` MAIR_EL1 值 (内存属性间接寄存器)。
    mair: u64,
    /// `[0x18]` TCR_EL1 值 (地址翻译控制寄存器)。
    tcr: u64,
    /// `[0x20]` SCTLR_EL1 值 (含 MMU/Cache 使能位)。
    sctlr: u64,
    /// `[0x28]` 本核私有栈顶 (高半区 VA)。
    stack_top: u64,
    /// `[0x30]` AP Rust 入口 (`ap_main`) 的高半区 VA。
    entry_va: u64,
    /// `[0x38]` 透传给 `ap_main` 第 1 参的 `cpu_index` (= MPIDR)。
    cpu_index: u64,
    /// `[0x40]` 握手完成标志: AP 登记上线后置 1。
    done: u64,
    /// `[0x48]` 诊断用: AP 进入 stub 时实测的异常级 (0b01=EL1 ...)。
    el: u64,
}

// 编译期断言: 槽布局必须与汇编偏移 (0x00..0x48) 严格一致。
const _: () = assert!(core::mem::size_of::<ApBootInfo>() == 80);

/// 次核私有内核栈 (16 字节对齐, 满足 AAPCS64 栈对齐要求)。
#[repr(C, align(16))]
struct ApStack {
    bytes: [u8; AP_STACK_SIZE],
}

static SMP_FULLY_INITIALIZED: AtomicBool = AtomicBool::new(false);
static AP_STARTED_COUNT: AtomicU32 = AtomicU32::new(0);

// SAFETY: C ABI 互操作; `ap_boot_info_ptr` 由链接脚本 (`link/aarch64.ld`) 定义为
// `_ap_boot_info` 槽的高半区别名 (VA = _kernel_base + PA), 运行时经 TTBR1 可达.
unsafe extern "C" {
    static ap_boot_info_ptr: ApBootInfo;
}

// SAFETY: C ABI 互操作; `ap_entry_asm_alias` 由链接脚本 (`link/aarch64.ld`) 定义为
// 低半区 `.text.boot` 中 `ap_entry_asm` stub 的高半区别名.
unsafe extern "C" {
    static ap_entry_asm_alias: u8;
}

/// 取次核启动槽的高半区可变指针。
fn boot_slot() -> *mut ApBootInfo {
    // `ap_boot_info_ptr` 是链接脚本定义的高半区别名, 始终指向 80 字节槽首.
    (&raw const ap_boot_info_ptr).cast_mut()
}

/// 取次核入口 stub (`ap_entry_asm`) 的物理地址, 供 PSCI `CPU_ON` 的 entry point 使用。
fn ap_entry_phys() -> u64 {
    // `ap_entry_asm_alias` 是链接脚本定义的高半区别名 (VA = KERNEL_BASE + PA).
    (&raw const ap_entry_asm_alias as u64).wrapping_sub(crate::framework::mm::KERNEL_BASE)
}

/// 将 `[ptr, ptr + len)` 覆盖的缓存行清理至 PoC (clean + invalidate) 并执行 `dsb sy`。
///
/// # Safety
///
/// `ptr` 必须指向有效内存, 且 `[ptr, ptr + len)` 不越过该分配边界。
unsafe fn clean_to_poc(ptr: *const u8, len: usize) {
    let start = ptr as u64;
    let end = start + len as u64;
    let line = crate::framework::mm::CACHE_LINE_SIZE;
    let mut addr = start & !(line - 1);
    while addr < end {
        // SAFETY: dc civac 是 aarch64 标准 cache 维护指令, addr 已对齐到 cache line.
        unsafe {
            core::arch::asm!("dc civac, {}", in(reg) addr);
        }
        addr += line;
    }
    // SAFETY: dsb sy 是系统级数据屏障, 确保 cache 维护在后续跨核访问前完成.
    unsafe {
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
}

/// 单核退化: 无多核拓扑或仅 1 个 CPU 时跳过 AP 启动, 直接标记 SMP 初始化完成。
fn single_core_fallback() {
    crate::klog_info!(Kernel, "[SMP] Single-core system, skipping AP startup");
    SMP_FULLY_INITIALIZED.store(true, Ordering::Release);
}

/// 启动全部次核 (BSP 侧入口)。
///
/// 由 `interrupt_late_init()` 在本核中断子系统就绪后调用; 无多核拓扑或仅单核时
/// 退化为单核路径 (见 [`single_core_fallback`])。
#[inline(never)]
pub fn init() {
    let Some(topology) = crate::framework::dtb::cpu_topology() else {
        single_core_fallback();
        return;
    };
    if topology.count <= 1 {
        single_core_fallback();
        return;
    }

    let bsp_mpidr = u64::from(crate::arch!(cpu_id()));
    let expected = topology.count;

    for &mpidr in topology.mpidrs.iter().take(topology.count as usize) {
        if mpidr == bsp_mpidr {
            continue;
        }
        // SIMPLIFIED: 稀疏 Aff0 下 `smp::register_cpu` 内部计数器与槽索引可能不一致;
        //   影响面: 仅在 MPIDR Aff0 非连续 (超 8 核且带亲和性) 时索引错位;
        //   何时需扩展: 引入 cpu_index ↔ MPIDR 独立映射表时按映射修正.
        let _ = start_ap(mpidr);
    }

    // 在线核数以权威计数器为准 (仅 `smp::register_cpu` 会增加它), 据此判定全体上线。
    let online = crate::framework::smp::get_cpu_count();
    crate::klog_info!(Kernel, "[SMP] online CPUs: {}", online);
    AP_STARTED_COUNT.store(online.saturating_sub(1), Ordering::Release);
    if online == expected {
        SMP_FULLY_INITIALIZED.store(true, Ordering::Release);
    }
}

/// 上电单个 AP 并等待其上线登记; 成功返回 `Ok(())`, 失败返回 `Err(())`。
fn start_ap(mpidr: u64) -> Result<(), ()> {
    // 本核私有栈: 零初始化后常驻 (不释放), 供 AP 生命周期内使用。
    // SAFETY: Layout::new::<ApStack>() 的 size/align 均由类型保证非零且合法;
    //   alloc_zeroed 返回的指针须显式转回 ApStack.
    let stack = unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<ApStack>()) }
        .cast::<ApStack>();
    if stack.is_null() {
        crate::klog_warn!(Kernel, "[SMP] AP {:#x} stack alloc failed, skip", mpidr);
        return Err(());
    }
    // SAFETY: stack 非空且指向有效 ApStack 分配; 取 bytes 字段首地址作栈底.
    let stack_top =
        unsafe { (&raw const (*stack).bytes).cast::<u8>() as u64 } + AP_STACK_SIZE as u64;

    let info = ApBootInfo {
        ttbr0: super::mmu::read_ttbr0(),
        ttbr1: super::mmu::read_ttbr1(),
        mair: super::mmu::read_mair(),
        tcr: super::mmu::read_tcr(),
        sctlr: super::mmu::read_sctlr(),
        stack_top,
        entry_va: ap_main as *const () as u64,
        cpu_index: mpidr,
        done: 0,
        el: 0,
    };

    let slot = boot_slot();
    // SAFETY: slot 指向 `.bootbss` 中 80 字节对齐的启动槽, 布局与 ApBootInfo 一致;
    //   整体写入后必须清至 PoC, 以确保 MMU 关闭的次核读取到主存而非陈旧 cache.
    unsafe {
        core::ptr::write_volatile(slot, info);
        clean_to_poc(slot.cast::<u8>(), core::mem::size_of::<ApBootInfo>());
    }

    if let Err(err) = super::psci::cpu_on(mpidr, ap_entry_phys(), mpidr) {
        crate::klog_err!(Kernel, "[SMP] AP {:#x} CPU_ON failed: {:?}", mpidr, err);
        return Err(());
    }

    let before = crate::framework::smp::get_cpu_count();

    // 有界自旋等待 AP 置 done; 读用 volatile, 随后 Acquire 屏障配对 AP 的 Release.
    let mut spin: u32 = 0;
    while unsafe { core::ptr::read_volatile(&raw const (*slot).done) } == 0 {
        if spin >= DONE_TIMEOUT_LOOPS {
            let el = unsafe { core::ptr::read_volatile(&raw const (*slot).el) };
            crate::klog_err!(
                Kernel,
                "[SMP] AP {:#x} did not come online within timeout (AP EL={})",
                mpidr,
                el
            );
            return Err(());
        }
        core::hint::spin_loop();
        spin += 1;
    }
    core::sync::atomic::fence(Ordering::Acquire);

    // AP 已应答但未登记 (如 stub 因异常级不符进入 `ap_bad_el` 占位分支):
    // 以权威计数器判定, 未增加即视为离线, 并回报其记录的 EL 级供 E7 诊断.
    if crate::framework::smp::get_cpu_count() == before {
        let el = unsafe { core::ptr::read_volatile(&raw const (*slot).el) };
        crate::klog_err!(
            Kernel,
            "[SMP] AP {:#x} responded but did not register (AP EL={})",
            mpidr,
            el
        );
        return Err(());
    }

    Ok(())
}

/// AP Rust 入口 (`ap_entry_asm` 经 TTBR1 跳入, `cpu_index` = MPIDR)。
///
/// 依次完成本核 GIC/per-CPU 状态初始化、登记上线、接管本核 idle, 最后置 `done`
/// 并进入本核 idle 调度循环 (装载本核定时器 + `schedule()` 循环)。任一前置步骤
/// 失败则放弃上线 (不置 `done`, 由 BSP 判定离线)。
extern "C" fn ap_main(cpu_index: u64) -> ! {
    let idx = cpu_index as u32;

    // 0. 使能本核 FP/SIMD 访问 (CPACR_EL1.FPEN[21:20] = 0b11)。
    //
    // BSP 在 `boot/aarch64/start.S` 的 EL2→EL1 路径完成该设置, 而 AP stub
    // (`ap_entry_asm`) 直接装载 BSP 的 MMU 配置后跳入本函数, 未复制此步骤 ⇒
    // CPACR_EL1 保持复位值 FPEN=0, EL1 执行 FP/SIMD 指令即触发 FP/ASIMD trap
    // (ESR_EL1.EC=0x07)。`context_switch_asm` 每次切换都要保存/恢复 V0-V31,
    // 故 AP 首次上下文切换即陷入同步异常, 被异常处理循环挂死 (核永久离线)。
    // SAFETY: CPACR_EL1 为 per-PE 系统寄存器; 本调用仅置 FPEN 位使本核不 trap
    // FP/SIMD, 不改动其它位, 且在本核 EL1 执行, 无跨核副作用。
    unsafe {
        core::arch::asm!(
            "mrs {t}, cpacr_el1",
            "orr {t}, {t}, #(0x3 << 20)",
            "msr cpacr_el1, {t}",
            t = out(reg) _,
            options(nomem, nostack)
        );
    }

    // 1. 本核 GICv3 重分发器/CPU 接口初始化 + 定时器 PPI 使能。
    // SAFETY: 仅在 AP 本核调用; 本核 MMU 已在 stub 中启用, GIC MMIO 已由 BSP 建立映射.
    if let Err(e) = unsafe { super::gic::init_per_cpu(idx) } {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} gic init_per_cpu failed: {}",
            idx,
            e
        );
        loop {
            crate::arch!(halt());
        }
    }

    // 2. 在本核装载异常向量表 (VBAR_EL1 为 per-CPU 寄存器), 此刻仍屏蔽中断。
    // SAFETY: 仅在 AP 本核调用; 向量表已就绪, VBAR_EL1 写入无跨核副作用.
    unsafe {
        super::exception::init_vectors();
    }

    // 3. 一次性构造本核全部 per-CPU 状态 (CpuQueue / 调度器 / RCU / softirq, 均幂等)。
    if !crate::framework::proc::init_cpu_queue(idx, 0)
        || !crate::framework::proc::init_per_cpu_sched(idx)
        || !crate::framework::sync::rcu_alloc_cpu(idx)
        || !crate::framework::irq::softirq_alloc_cpu(idx)
    {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} per-CPU state alloc failed, abort bring-up",
            idx
        );
        loop {
            crate::arch!(halt());
        }
    }

    // 4. 登记本核上线。
    crate::framework::smp::register_cpu(idx);

    // 4.1 绑定本核 KPTI 状态槽地址到 TPIDR_EL1: EL0 入口/出口汇编据此按核寻址
    //     (切表前无空闲 GPR 现算 MPIDR 索引, 故索引在上电路径一次算好)。
    crate::framework::mm::kpti::kpti_bind_cpu(idx);

    // 5. 建立本核调度身份: 创建 (或复用) 本核 idle 并置 `current = idle`。
    //    此后本核 `schedule()` 才会真正执行上下文切换 (BSP 的 idle 不属于本核);
    //    失败则放弃上线, 不置 done, 由 BSP 判定离线。
    if crate::framework::proc::SCHEDULER
        .adopt_cpu_idle(idx)
        .is_none()
    {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} idle adopt failed, abort bring-up",
            idx
        );
        loop {
            crate::arch!(halt());
        }
    }

    // 6. 后置自检: 登记必须生效, 否则不上报 done。
    if !crate::framework::smp::is_cpu_online(idx) {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} online self-check failed, abort bring-up",
            idx
        );
        loop {
            crate::arch!(halt());
        }
    }

    // 7. 置 done=1 通知 BSP; Release 屏障确保此前 per-CPU 状态对本核外可见。
    core::sync::atomic::fence(Ordering::Release);
    let slot = boot_slot();
    // SAFETY: slot 有效期贯穿内核生命周期; done 位于槽内 0x40 偏移, 8 字节对齐.
    unsafe {
        core::ptr::write_volatile(&raw mut (*slot).done, 1u64);
    }

    // 8. 装载本核通用定时器并进入本核 idle 调度循环。
    //
    // CNTP_* 为 per-PE 系统寄存器: 此处在 AP 本核装载周期定时器 (BSP 的装载不
    // 影响本核), 使空闲 AP 被周期唤醒去调 `schedule()` 做拉式负载均衡。
    let interval = super::exception::TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);
    super::timer::start_interval(interval);

    loop {
        // 每轮: 关中断 → `schedule()` (本地选任务 / 负载均衡, 必要时上下文切换到
        // 任务) → 以 `msr daifclr, #2; wfi` 融合体原子地开中断并停机。
        //
        // 融合是必需的: ①`schedule()` 的中断恢复判据 (`saved_flags & 0x200`) 取
        // 的是 x86_64 IF 位语义, 对 aarch64 DAIF (bit9=D) 恒为假 ⇒ 关中断后调用它
        // 不会自行开中断, 必须在此显式开; ②若开中断与 `wfi` 分离, 二者之间到达的
        // 中断会在 `wfi` 前被处理, 随后的 `wfi` 可能错过唤醒 (对齐 `idle_entry`
        // 的 `sti; hlt` 融合手法)。
        crate::arch!(interrupt_disable());
        let _ = crate::framework::proc::SCHEDULER.schedule();
        // SAFETY: wfi 在 EL1 合法; msr daifclr 为立即数指令, 无内存/栈副作用.
        unsafe {
            core::arch::asm!("msr daifclr, #2; wfi", options(nomem, nostack));
        }
    }
}

// SAFETY: FFI 导出函数, 通过 C ABI 与外部代码互操作.
#[unsafe(no_mangle)]
pub extern "C" fn smp_init_bsp() {
    init();
}

// SAFETY: FFI 导出函数, 通过 C ABI 与外部代码互操作.
#[unsafe(no_mangle)]
pub extern "C" fn smp_ready() -> bool {
    SMP_FULLY_INITIALIZED.load(Ordering::Acquire)
}

// SAFETY: FFI 导出函数, 通过 C ABI 与外部代码互操作.
#[unsafe(no_mangle)]
pub extern "C" fn smp_get_ap_count() -> u32 {
    AP_STARTED_COUNT.load(Ordering::Acquire)
}
