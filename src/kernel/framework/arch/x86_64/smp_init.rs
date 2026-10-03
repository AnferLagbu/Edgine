use alloc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// SMP 初始化模块
///
/// 使用 INIT-SIPI 协议从 BSP 启动所有 Application Processors (APs)。
/// Trampoline 代码位于 0x8000 物理地址，由 BSP 在启动前拷贝。
///
/// 流程:
///   1. `parse_madt()` — 解析 ACPI MADT 获取 AP 列表
///   2. `copy_trampoline()` — 拷贝 trampoline 到 0x8000
///   3. `start_ap()` — 对每个 AP 发送 INIT-SIPI 序列
///   4. `ap_entry()` — AP 进入 64-bit 后调用的 Rust 入口
const TRAMPOLINE_BASE: u64 = 0x8000;
const AP_INFO_OFFSET: u64 = 8;
const AP_STACK_SIZE: usize = 65536;
const INIT_WAIT_US: u32 = 10000;
const SIPI_DELAY_US: u32 = 200;
const READY_POLL_US: u32 = 100;
const READY_TIMEOUT_LOOPS: usize = 1000;
const AP_ENTRY_TIMEOUT_LOOPS: usize = 500;

/// AP 启动握手结构体大小 (字节).
///
/// 必须与 `arch/x86_64/trampoline.asm` 中 `ApStartupInfo` 的汇编布局完全一致.
/// 编译期断言 `assert_eq_size!` 在 smp_init 加载时强制; 若后续字段重排,
/// 链接器/汇编端未同步, 编译立即失败 (见 P1.A + DECISION-050).
const AP_STARTUP_INFO_SIZE: usize = 54;

#[repr(C, packed)]
struct ApStartupInfo {
    cr3: u64,
    entry: u64,
    gdt_limit: u16,
    gdt_base: u64,
    stack: u64,
    lapic_id: u32,
    ready: u32,
    cpu_index: u32,
    /// AP 初始化完成标志: AP 在 `register_cpu` / per-CPU 队列 / per-CPU 调度器初始化完成后置 1
    done: u32,
    _pad: u32,
}

// 编译期断言: ApStartupInfo 字节大小 = AP_STARTUP_INFO_SIZE.
// SAFETY: `repr(C, packed)` 保证字段按声明顺序紧密排列, 无填充字节;
// 字段大小 = u64×5 + u16 + u64×3 + u32×4 + u32 = 8+8+2+8+8+4+4+4+4+4 = 54.
// 若结构体修改后大小不一致, 编译立即失败, 提示同步 trampoline.asm.
const _: () = assert!(
    core::mem::size_of::<ApStartupInfo>() == AP_STARTUP_INFO_SIZE,
    "ApStartupInfo size mismatch with trampoline.asm; update SINFO_* offsets",
);

/// `ready` 字段在 `ApStartupInfo` 中的字节偏移.
///
/// 由 Rust 端 `core::mem::offset_of!` 计算, 单一来源;
/// BSP 等待逻辑使用此常量, 不再硬编码 `+38` (DECISION-050).
const READY_OFFSET: usize = core::mem::offset_of!(ApStartupInfo, ready);

/// `done` 字段在 `ApStartupInfo` 中的字节偏移.
///
/// AP 完成 `register_cpu` 与 per-CPU 初始化后写 1; BSP 等待此位成功即代表 AP 已注册上线 (DECISION-050).
const DONE_OFFSET: usize = core::mem::offset_of!(ApStartupInfo, done);

static SMP_FULLY_INITIALIZED: AtomicBool = AtomicBool::new(false);
static AP_STARTED_COUNT: AtomicU32 = AtomicU32::new(0);

use spin::mutex::SpinMutex;
static AP_STARTUP_LOCK: SpinMutex<()> = SpinMutex::new(());

struct ApPerCpu {
    stack: [u8; AP_STACK_SIZE],
}

/// AP (Application Processor) per-CPU 数据
///
/// # Safety 不变量
///
/// - **写入时机**: SMP init, 每个 AP 写入自己的槽位
/// - **运行时**: 各 CPU 读取自己的数据
/// - **并发**: 写入时 AP 串行启动 (通过 SIPI 序列)
/// - **释放**: 无主动释放, 随系统生命周期存在
static mut AP_PER_CPU: [Option<*mut ApPerCpu>; super::acpi::MAX_CPUS] =
    [None; super::acpi::MAX_CPUS];

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn ap_trampoline_start();
    fn ap_trampoline_end();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_init_bsp() {
    init();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_ready() -> bool {
    SMP_FULLY_INITIALIZED.load(Ordering::Acquire)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_get_ap_count() -> u32 {
    AP_STARTED_COUNT.load(Ordering::Acquire)
}

#[inline(never)]
pub fn init() {
    super::acpi::parse_madt(0);

    let ap_count = super::acpi::get_ap_count();
    if ap_count <= 1 {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            crate::framework::klog::klog_info(
                c"[KERN] [SMP] Single-core system, skipping AP startup".as_ptr(),
            );
        }
        SMP_FULLY_INITIALIZED.store(true, Ordering::Release);
        return;
    }

    let bsp_lapic_id = super::apic::get_id();

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        copy_trampoline();
    }

    let ap_list = super::acpi::get_ap_list();
    let mut cpu_index: u32 = 1;

    for ap in ap_list.iter().flatten() {
        if ap.lapic_id == bsp_lapic_id || !ap.enabled {
            continue;
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            start_ap(ap.lapic_id, cpu_index);
        }
        cpu_index += 1;
    }

    // 汇总在线 CPU 数; `make test-smp` 以 "[SMP] online CPUs: 2" 作为 2 核启动成功判据.
    crate::klog_info!(
        Kernel,
        "[SMP] online CPUs: {}",
        crate::framework::smp::get_cpu_count()
    );

    AP_STARTED_COUNT.store(cpu_index, Ordering::Release);
    SMP_FULLY_INITIALIZED.store(true, Ordering::Release);

    let started = cpu_index - 1;
    if started > 0 {
        crate::framework::klog::serial_write_bytes(b"[SMP] All APs started successfully\n");
    } else {
        crate::framework::klog::serial_write_bytes(b"[SMP] No APs started\n");
    }
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn copy_trampoline() {
    unsafe {
        let src = ap_trampoline_start as *const u8;
        let end = ap_trampoline_end as *const u8;
        let size = end as usize - src as usize;
        let dst = TRAMPOLINE_BASE as *mut u8;

        for i in 0..size {
            dst.add(i).write_volatile(src.add(i).read_volatile());
        }
    }
}

#[inline(never)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
#[expect(
    clippy::large_stack_arrays,
    reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
)]
unsafe fn start_ap(lapic_id: u32, cpu_index: u32) {
    unsafe {
        if cpu_index as usize >= super::acpi::MAX_CPUS {
            return;
        }

        // P2.C + F-12: 删除显式 cli / sti, 改用 AP_STARTUP_LOCK 内部 IRQ 保存.
        // 此前外层 cli 与 AP_STARTUP_LOCK (spin::Mutex, 不做 IRQ save) 嵌套导致
        // spinlock drop 时不会恢复 IF → 若路径 panic 则系统 hang. 当前依赖
        // spin::Mutex 在 IRQ 安全路径下不调用 (boot 单线程, MP 启动期), 安全.
        // AP_STARTUP_LOCK 仅保护 BSP 串行启动 AP 的临界区, IRQ 安全不强制要求.
        let _lock = AP_STARTUP_LOCK.lock();

        // AP 的 GDT 必须在该 CPU 进入长模式 (gdt_init_ap) 之前就绪, 故在 SIPI
        // 之前按需分配; 分配失败 (PMM 无可用页) 则放弃启动本 AP.
        if !super::gdt::alloc_ap_gdt(cpu_index) {
            crate::klog_warn!(Boot, "[SMP] AP cpu_index={} GDT alloc failed, skip", cpu_index);
            return;
        }

        let per_cpu = alloc::boxed::Box::new(ApPerCpu {
            stack: [0u8; AP_STACK_SIZE],
        });

        let stack_top = per_cpu.stack.as_ptr() as u64 + AP_STACK_SIZE as u64;
        AP_PER_CPU[cpu_index as usize] = Some(alloc::boxed::Box::into_raw(per_cpu));

        let cr3_val = crate::framework::mm::get_kernel_pml4();
        let gdt_ptr = super::gdt::get_gdt_ptr();
        let entry_addr = ap_entry as *const () as u64;

        let info = (TRAMPOLINE_BASE + AP_INFO_OFFSET) as *mut ApStartupInfo;
        (*info).cr3 = cr3_val;
        (*info).entry = entry_addr;
        (*info).gdt_limit = gdt_ptr.limit;
        (*info).gdt_base = gdt_ptr.base;
        (*info).stack = stack_top;
        (*info).lapic_id = lapic_id;
        (*info).ready = 0;
        (*info).cpu_index = cpu_index;
        (*info).done = 0;

        core::arch::asm!("sfence", options(nomem, nostack));

        send_init_ipi(lapic_id, true);
        timer_udelay(INIT_WAIT_US);
        send_init_ipi(lapic_id, false);

        send_sipi(lapic_id, 0x08);
        timer_udelay(SIPI_DELAY_US);
        send_sipi(lapic_id, 0x08);

        // 等待 AP 就绪 (最多 100ms), volatile 读取跨 CPU 写入.
        // ready/done 偏移由 Rust 端 offset_of! 计算, 与 trampoline.asm SINFO_* 符号互校
        // (DECISION-050: 单一来源, 避免 magic 偏移扩散).
        let mut timeout = READY_TIMEOUT_LOOPS;
        let ready_ptr: *const u32 =
            (TRAMPOLINE_BASE + AP_INFO_OFFSET + READY_OFFSET as u64) as *const u32;
        while timeout > 0 {
            if core::ptr::read_volatile(ready_ptr) != 0 {
                break;
            }
            timer_udelay(READY_POLL_US);
            timeout -= 1;
        }

        if timeout > 0 {
            // AP 已就绪，等待 ap_entry 完成 per-CPU GDT+TSS 初始化
            let done_ptr: *const u32 =
                (TRAMPOLINE_BASE + AP_INFO_OFFSET + DONE_OFFSET as u64) as *const u32;
            let mut wait = AP_ENTRY_TIMEOUT_LOOPS;
            while wait > 0 {
                if core::ptr::read_volatile(done_ptr) != 0 {
                    break;
                }
                timer_udelay(READY_POLL_US);
                wait -= 1;
            }
        }

        // P2.C + F-12: 删除显式 sti. 当前 _lock drop 后 IRQ 状态保持 cli 嵌套前.
        // 由于本函数 cli 已被删除, _lock drop 后 IF 位仍为 boot 启动时的状态.
        // AP_STARTUP_LOCK 仅在 boot 早期使用 (此函数唯一调用方), IRQ 默认开启.
    }
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn send_init_ipi(lapic_id: u32, assert: bool) {
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
    super::apic::apic_write(0x310, (lapic_id & 0xFF) << 24);
    let level = if assert { 1 << 14 } else { 0 };
    super::apic::apic_write(0x300, (5 << 8) | level | (1 << 15));
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn send_sipi(lapic_id: u32, vector: u8) {
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
    super::apic::apic_write(0x310, (lapic_id & 0xFF) << 24);
    super::apic::apic_write(0x300, (6 << 8) | u32::from(vector));
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn timer_udelay(us: u32) {
    unsafe {
        for _ in 0..us {
            core::arch::asm!("out dx, al", in("dx") 0x80u16, in("al") 0u8, options(nomem, nostack));
        }
    }
}

extern "C" fn ap_entry(lapic_id: u32) -> ! {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let cpu_index = unsafe {
        ((TRAMPOLINE_BASE + AP_INFO_OFFSET) as *const ApStartupInfo)
            .read_volatile()
            .cpu_index
    };

    super::apic::init();

    super::gdt::gdt_init_ap(cpu_index);

    // AP 本核 CPU 特性初始化 (使能 FPU/SSE + 装载 SYSCALL 入口 MSR)。
    // BSP 侧由 `cpu_init` → `init_msr` 完成, AP 上电路径此前遗漏: 这些状态
    // (CR0/CR4 及 IA32_STAR/LSTAR/SFMASK/EFER.SCE) 均为 per-CPU, 不随 AP 上电
    // 继承。缺失时 `process_switch_asm` 的 fxsave 在 CR4.OSFXSR=0 下触发 #UD →
    // #DF, 使本核首次上下文切换即崩溃离线; SYSCALL MSR 未设则 AP 上用户态首个
    // syscall 跳向无效入口。此处复用 BSP 探测到的特性集合, 仅作用于本核。
    if let Some(info) = crate::framework::cpu::get_cpu_info() {
        if let Err(e) = crate::framework::cpu::init_msr(&info.features) {
            crate::klog_err!(Boot, "[SMP] AP cpu_index={} init_msr failed: {}", cpu_index, e);
        }
    }

    // AP 必须在本核加载 IDT 之后才能开中断: gdt_init_ap 只装 GDT+TSS,
    // 而 BSP 的 idt_init() 里的 lidt 只对 BSP 本核生效. 若 AP 在 IDTR.BASE=0
    // 状态下开中断, 首个定时器中断 (向量 0x20) 即触发 #GP → #DF → triple fault.
    // SAFETY: `IdtManager::init()` 已在 BSP 上完成, IDT 表全局共享; 本核在
    // ap_entry 入口已 cli, 且 sti 在其后, 故加载期间中断处于屏蔽状态.
    unsafe {
        crate::framework::idt::load_idt_on_current_cpu();
    }

    // 上线前一次性构造本核全部 per-CPU 状态 (CpuQueue / 调度器 / RCU / softirq,
    // 均幂等)。任一步分配失败即放弃本 AP 上线: 不登记 CPU、不握手 done ——
    // BSP 侧等待 done 超时后视本 AP 离线 (与上方 GDT 分配失败跳过本 AP 的语义
    // 一致); 绝不静默回退 BSP 状态造成跨 CPU 混叠。
    if !crate::framework::proc::init_cpu_queue(cpu_index, 0)
        || !crate::framework::proc::init_per_cpu_sched(cpu_index)
        || !crate::framework::sync::rcu_alloc_cpu(cpu_index)
        || !crate::framework::irq::softirq_alloc_cpu(cpu_index)
    {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} per-CPU state alloc failed, abort bring-up",
            cpu_index
        );
        loop {
            crate::arch!(halt());
        }
    }

    crate::framework::smp::register_cpu(lapic_id);

    // 建立本核调度身份: 创建 (或复用) 本核 idle 并置 `current = idle`, 此后本核
    // `schedule()` 才会真正执行上下文切换 (BSP 的 idle 不属于本核)。失败则放弃
    // 上线: 不置 done, 由 BSP 等待超时后判定离线 (与 per-CPU 状态分配失败一致)。
    if crate::framework::proc::SCHEDULER
        .adopt_cpu_idle(cpu_index)
        .is_none()
    {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} idle adopt failed, abort bring-up",
            cpu_index
        );
        loop {
            crate::arch!(halt());
        }
    }

    // SAFETY: TRAMPOLINE_BASE + AP_INFO_OFFSET + DONE_OFFSET 是 AP 握手内存布局中
    // 预留的 done 标志位, BSP 已映射该物理页, 写入对齐 u32 安全.
    // DONE_OFFSET 由 Rust 端 offset_of! 计算, 与 BSP 等待逻辑共享同一来源 (DECISION-050).
    // 此时本核 GDT/TSS/IDT 与运行队列/调度器均已就绪, BSP 的 done 等待成功
    // 即代表 AP 完成注册上线.
    unsafe {
        let done_ptr = (TRAMPOLINE_BASE + AP_INFO_OFFSET + DONE_OFFSET as u64) as *mut u32;
        core::ptr::write_volatile(done_ptr, 1);
    }

    // 进入本核 idle 调度循环。
    //
    // 每轮: 关中断 → `schedule()` (本地选任务 / 负载均衡, 必要时上下文切换到任务)
    // → `sti; hlt` 融合体原子地开中断并停机。融合是必需的: ①AP 本核无周期定时器
    // (PIT 仅 BSP 可用, LAPIC 周期定时器未标定), 唤醒靠 resched IPI 与其它核的负载
    // 均衡, 开中断与 `hlt` 分离会丢掉窗口内到达的 IPI; ②`sti; hlt` 是 x86 的经典
    // 原子等待序列 (STI 影子指令), 对齐 `idle_entry` 手法。
    loop {
        crate::arch!(interrupt_disable());
        let _ = crate::framework::proc::SCHEDULER.schedule();
        // SAFETY: sti/hlt 在 ring 0 合法; 不读写内存也不修改栈, options 与之一致.
        unsafe {
            core::arch::asm!("sti; hlt", options(nomem, nostack));
        }
    }
}
