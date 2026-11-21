//! x86_64 SMP 初始化 — BSP 经 INIT-SIPI 序列启动所有 Application Processor (AP)。

#![deny(missing_docs)]

use alloc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// SMP 初始化模块
///
/// 使用 INIT-SIPI 协议从 BSP 启动所有 Application Processors (APs)。
/// Trampoline 代码位于 0x8000 物理地址，由 BSP 在启动前拷贝。
///
/// 流程:
///   1. `parse_all_tables()` — 单次 SDT 遍历: 解析 MADT 获取 AP 列表, 并一并填充 FADT/HPET/DMAR
///   2. `copy_trampoline()` — 拷贝 trampoline 到 0x8000
///   3. `start_ap()` — 对每个 AP 发送 INIT-SIPI 序列
///   4. `ap_entry()` — AP 进入 64-bit 后调用的 Rust 入口
const TRAMPOLINE_BASE: u64 = 0x8000;
const AP_INFO_OFFSET: u64 = 8;
const AP_STACK_SIZE: usize = 65536;
const INIT_WAIT_US: u32 = 10000;
const SIPI_DELAY_US: u32 = 200;
const READY_POLL_US: u32 = 100;
/// AP 就绪等待的墙钟预算 (1000 × 100 µs)
const READY_TIMEOUT_MS: u64 = 100;
/// AP 就绪等待的迭代回退上限 (时基未建立时使用)
const READY_TIMEOUT_LOOPS: u32 = 1000;
/// AP 入口完成等待的墙钟预算 (500 × 100 µs)
const AP_ENTRY_TIMEOUT_MS: u64 = 50;
/// AP 入口完成等待的迭代回退上限 (时基未建立时使用)
const AP_ENTRY_TIMEOUT_LOOPS: u32 = 500;

/// AP 启动握手结构体大小 (字节).
///
/// 必须与 `arch/x86_64/trampoline.asm` 中 `ApStartupInfo` 的汇编布局完全一致.
/// 编译期断言 `assert_eq_size!` 在 `smp_init` 加载时强制; 若后续字段重排,
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
/// BSP 侧 SMP 初始化入口 (C ABI 导出)。
#[unsafe(no_mangle)]
pub extern "C" fn smp_init_bsp() {
    init();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 查询 SMP 初始化是否已全部完成 (C ABI 导出)。
#[unsafe(no_mangle)]
pub extern "C" fn smp_ready() -> bool {
    SMP_FULLY_INITIALIZED.load(Ordering::Acquire)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 返回已成功启动的 AP 数量 (C ABI 导出)。
#[unsafe(no_mangle)]
pub extern "C" fn smp_get_ap_count() -> u32 {
    AP_STARTED_COUNT.load(Ordering::Acquire)
}

/// 初始化 SMP: 解析 MADT 并启动所有 AP; 单核时直接标记完成。
#[inline(never)]
pub fn init() {
    // 单次 SDT 遍历: `parse_all_tables` 内含 MADT 解析 (LAPIC/IOAPIC/ISO),
    // 并同时填充 FADT/HPET/DMAR. 不要另调 `parse_madt` —— 它只扫 MADT,
    // 再调一次会让 AP/IOAPIC 列表重复登记 (P-77).
    // MB2 信息指针必须真传: UEFI 下 RSDP 只经 tag 14/15 内联副本交付, 传 0 会
    // 让 ACPI 整条发现面失效 (RSDP not found → 单核 → SMP 探针全跑不到).
    super::acpi::parse_all_tables(crate::privileged::boot::multiboot_info_ptr());
    // MADT 解析完成后立即初始化 IOAPIC: 设备 INTx 接线 (net 探测) 依赖
    // `gsi_to_ioapic` 的区间信息, 而区间上界只能读 IOAPICVER 回填.
    // 必须在 `ap_count <= 1` 提前返回之前执行 (单核同样需要中断路由).
    super::ioapic::init_from_acpi();

    let ap_count = super::acpi::get_ap_count();
    if ap_count <= 1 {
        // SAFETY: `klog_info` 要求 NUL 结尾有效 C 串; 实参为静态字面量, 位于 .rodata,
        // 函数同步消费.
        unsafe {
            crate::privileged::klog::klog_info(
                c"[KERN] [SMP] Single-core system, skipping AP startup".as_ptr(),
            );
        }
        SMP_FULLY_INITIALIZED.store(true, Ordering::Release);
        return;
    }

    let bsp_lapic_id = super::apic::get_id();

    // SAFETY: 只能在 BSP 启动路径、低半区恒等映射已建立且 0x8000 页未被 PMM 分配时
    // 调用; 源/目的区间与拷贝方式的依据见 `copy_trampoline` 体内 SAFETY.
    unsafe {
        copy_trampoline();
    }

    let ap_list = super::acpi::get_ap_list();
    let mut cpu_index: u32 = 1;

    for ap in ap_list.iter().flatten() {
        if ap.lapic_id == bsp_lapic_id || !ap.enabled {
            continue;
        }
        // SAFETY: `start_ap` 须由 BSP 在 `copy_trampoline` 完成后的串行启动循环中调用,
        // 且 cpu_index 为已分配槽位; 各 unsafe 面前提见该函数体内的三段说明.
        unsafe {
            start_ap(ap.lapic_id, cpu_index);
        }
        cpu_index += 1;
    }

    // 汇总在线 CPU 数; `make test-smp` 以 "[SMP] online CPUs: 2" 作为 2 核启动成功判据.
    crate::klog_info!(
        Kernel,
        "[SMP] online CPUs: {}",
        crate::privileged::smp::get_cpu_count()
    );

    AP_STARTED_COUNT.store(cpu_index, Ordering::Release);
    SMP_FULLY_INITIALIZED.store(true, Ordering::Release);

    let started = cpu_index - 1;
    if started > 0 {
        crate::privileged::klog::serial_write_bytes(b"[SMP] All APs started successfully\n");
    } else {
        crate::privileged::klog::serial_write_bytes(b"[SMP] No APs started\n");
    }
}

// SAFETY(契约): 由 BSP 在启动期单线程调用, 且此时低半区恒等映射已建立、0x8000 页
// 已由 PMM 预留; 函数内逐字节拷贝的依据见体内 SAFETY.
unsafe fn copy_trampoline() {
    // SAFETY: 源区间由链接符号 `ap_trampoline_start` / `ap_trampoline_end` 界定
    // (trampoline.asm `section .trampoline` 的首尾), size = end - start 覆盖「2 字节 jmp
    // + 6 字节填充 + 54 字节 ApStartupInfo 占位 + 全部引导代码」; 二者是同一 section 内
    // 按序放置的代码符号, 取址只读不解引用, 区间恒有效且 end >= start, 故 `src.add(i)` /
    // `dst.add(i)` 在 i ∈ [0, size) 内均不越界.
    // 目的址 TRAMPOLINE_BASE = 0x8000 是**物理地址直接当 VA 用**, 依赖 boot 期低半区恒等
    // 映射; 该页不会被 PMM 分配出去 —— `pmm::mark_reserved_pages` 把 [0, kernel_end] 的
    // 全部页置已用, 而内核 LMA 起点 0x100000 (link/x86_64.ld) ⇒ page 8 恒在预留面内.
    // 目的址必须恰为 0x8000: trampoline 以绝对地址寻址 info 槽 (`SINFO_BASE equ 0x8008`),
    // 且 SIPI vector 0x08 ⇒ AP 从 0x08 << 12 = 0x8000 取指, 二者与本常量同源.
    // 逐字节 `read_volatile` / `write_volatile`: AP 在 MMU/cache 关闭阶段以物理地址取指,
    // 普通写可能被编译器合并或重排; u8 访问无对齐面. 拷贝同时把 info 槽清回镜像里的全零
    // 初值 ⇒ 每个 AP 启动前由 `start_ap` 重填. 唯一调用方 `init()` 在 BSP 单上下文、
    // 任何 SIPI 之前调用一次.
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
// SAFETY(契约): 只能由 BSP 在 `copy_trampoline` 之后串行调用, 且 `cpu_index` 须小于
// `MAX_CPUS`; 三类 unsafe 面 (static mut 槽 / info 槽 / APIC 与端口 I/O) 的依据见体内.
unsafe fn start_ap(lapic_id: u32, cpu_index: u32) {
    // SAFETY: 本块含三类 unsafe 面, 前提各不同.
    // ① 分配与 static mut 槽: `alloc_zeroed(Layout::new::<ApPerCpu>())` 的 size/align 由
    //    类型保证, 返回 null 即放弃本 AP; 非空时 `(*per_cpu).stack.as_ptr()` 是对新分配
    //    独占内存的共享借用 (无其它别名), 栈顶 = 基址 + AP_STACK_SIZE. `AP_PER_CPU[idx]`
    //    是 `static mut` 数组, 写入前提 = idx < MAX_CPUS (首行已判) **且串行独占**:
    //    全仓唯一调用点是 `init()` 内对 MADT AP 列表的串行循环, 只由 BSP 经
    //    `Arch::secondary_cpu_bringup()` 调一次 (依据见下方 AP_STARTUP_LOCK 退役注释,
    //    回归守卫 host-tests/tests/arch_ap_startup_lock_removed_test.rs).
    // ② info 槽写: TRAMPOLINE_BASE + AP_INFO_OFFSET = 0x8008 落在 `copy_trampoline` 刚
    //    拷好的页内; `ApStartupInfo` 是 `#[repr(C, packed)]` 54 字节 (编译期 size 断言,
    //    READY_OFFSET / DONE_OFFSET 由 `offset_of!` 派生并与 trampoline.asm 的 SINFO_*
    //    互校), 10 个字段写均在槽内. packed 字段以裸指针 place 写, **不构造字段引用**
    //    (packed 字段取引用即对齐 UB). 这些是普通写, 随后 `sfence` + INIT-SIPI; sfence
    //    声明 options(nomem) ⇒ 只约束 CPU 侧 store 次序, 不是编译器屏障, 跨核可见性由
    //    x86 TSO 与 SIPI 前 INIT_WAIT_US (10 ms) 延时保证; AP 侧 MMU 关闭时以物理地址读
    //    同一页, 无需 cache 维护.
    // ③ 握手读: ready / done 两个 `*const u32` 指向槽内偏移 38 / 46, 二者**非 4 字节
    //    对齐** (0x802E / 0x8036) —— x86 在 EFLAGS.AC=0 (内核不开 alignment check) 下允许
    //    非对齐访问, 故无 #AC 面, 但这也是必须走裸指针 volatile 而非引用的原因. 跨核写入
    //    ⇒ `read_volatile` 必需, 否则自旋条件被提出循环. 等待**有界**: `Deadline::after_ms`
    //    (READY_TIMEOUT_MS=100 / AP_ENTRY_TIMEOUT_MS=50, 时基未建立时回退 1000 / 500 轮),
    //    超时即跳出并视本 AP 离线. done 由 `ap_entry` 在 register_cpu + per-CPU 状态 +
    //    adopt_cpu_idle + arm_local 全部就绪后才置 1, 故「见 done」蕴含「本核已可调度」.
    //    本函数不改动 IF (P2.C + F-12 已删显式 cli/sti).
    unsafe {
        if cpu_index as usize >= super::acpi::MAX_CPUS {
            return;
        }

        // AP 启动串行化的前置事实 (deadlock 审计 HIGH 的根治依据):
        // `start_ap` 全仓唯一调用点是本文件 `init()` 内对 MADT AP 列表的串行循环,
        // 而 `init()` 仅由 BSP 经 `Arch::secondary_cpu_bringup()` 在 `kernel_main`
        // 中调用一次; AP 走 `ap_entry`, 不进入本函数. 因此不存在第二个获取者 ——
        // 旧写法的 `AP_STARTUP_LOCK` (`spin::mutex::SpinMutex`, 不做 IRQ save)
        // 既不保护任何并发路径, 又把最长约 150ms/AP 的忙等轮询圈进持锁区间,
        // 还使 `audit_deadlock_matrix.py` 只能保守报 HIGH (无竞争者无法静态证明).
        // 回归守卫: host-tests/tests/arch_ap_startup_lock_removed_test.rs.
        // 若将来引入 CPU hotplug (多上下文并发启动 CPU), 重引的锁必须是 IRQ 安全
        // 锁 (IrqSpinLock), 且忙等轮询须移出临界区.

        // AP 的 GDT 必须在该 CPU 进入长模式 (gdt_init_ap) 之前就绪, 故在 SIPI
        // 之前按需分配; 分配失败 (PMM 无可用页) 则放弃启动本 AP.
        if !super::gdt::alloc_ap_gdt(cpu_index) {
            crate::klog_warn!(
                Boot,
                "[SMP] AP cpu_index={} GDT alloc failed, skip",
                cpu_index
            );
            return;
        }

        // AP 栈 (64KiB) 用 alloc_zeroed 直接从内核分配器取清零内存: 既避免在栈上
        // 先物化整块数组再搬运 (clippy large_stack_arrays 的指摘点), 也保留了
        // "全零初值" 的等价语义 (`ApPerCpu` 仅含 `[u8; N]` 字段).
        let layout = core::alloc::Layout::new::<ApPerCpu>();
        let per_cpu = alloc::alloc::alloc_zeroed(layout).cast::<ApPerCpu>();
        if per_cpu.is_null() {
            // 分配失败 (PMM 无可用页): 与 GDT 分配失败同样处置 — 放弃启动本 AP.
            crate::klog_warn!(
                Boot,
                "[SMP] AP cpu_index={} stack alloc failed, skip",
                cpu_index
            );
            return;
        }

        let stack_top = (*per_cpu).stack.as_ptr() as u64 + AP_STACK_SIZE as u64;
        AP_PER_CPU[cpu_index as usize] = Some(per_cpu);

        let cr3_val = crate::privileged::mm::get_kernel_pml4();
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
        let ready_ptr: *const u32 =
            (TRAMPOLINE_BASE + AP_INFO_OFFSET + READY_OFFSET as u64) as *const u32;
        let mut ready = false;
        let mut ready_deadline =
            crate::privileged::timer::Deadline::after_ms(READY_TIMEOUT_MS, READY_TIMEOUT_LOOPS);
        while !ready {
            ready = core::ptr::read_volatile(ready_ptr) != 0;
            if ready {
                break;
            }
            if ready_deadline.expired() {
                break;
            }
            timer_udelay(READY_POLL_US);
        }

        if ready {
            // AP 已就绪，等待 ap_entry 完成 per-CPU GDT+TSS 初始化
            let done_ptr: *const u32 =
                (TRAMPOLINE_BASE + AP_INFO_OFFSET + DONE_OFFSET as u64) as *const u32;
            let mut entry_deadline = crate::privileged::timer::Deadline::after_ms(
                AP_ENTRY_TIMEOUT_MS,
                AP_ENTRY_TIMEOUT_LOOPS,
            );
            while core::ptr::read_volatile(done_ptr) == 0 {
                if entry_deadline.expired() {
                    break;
                }
                timer_udelay(READY_POLL_US);
            }
        }

        // P2.C + F-12: 显式 cli / sti 已删除 — IF 位保持本函数入口时的状态 (boot
        // 路径由 BSP 单一上下文串行执行, 无与中断处理器的共享锁需要靠 cli 保护).
    }
}

// SAFETY(契约): 调用方须保证 `APIC_BASE` 已由 `apic::init` 初始化 (MMIO 帧已映射),
// 且本函数只在 BSP 的串行 AP 启动序列中调用, 不与其它核并发操作同一 ICR.
unsafe fn send_init_ipi(lapic_id: u32, assert: bool) {
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
    super::apic::apic_write(0x310, (lapic_id & 0xFF) << 24);
    let level = if assert { 1 << 14 } else { 0 };
    super::apic::apic_write(0x300, (5 << 8) | level | (1 << 15));
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
}

// SAFETY(契约): 同 `send_init_ipi` —— LAPIC 已初始化, 且只在 BSP 串行启动序列内调用;
// `vector` 须与 `copy_trampoline` 使用的 SIPI vector 一致 (本文件同源常量).
unsafe fn send_sipi(lapic_id: u32, vector: u8) {
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
    super::apic::apic_write(0x310, (lapic_id & 0xFF) << 24);
    super::apic::apic_write(0x300, (6 << 8) | u32::from(vector));
    while super::apic::apic_read(0x300) & (1 << 12) != 0 {}
}

// SAFETY(契约): 调用方须在 Ring 0 且平台具备 PC/AT 兼容的 0x80 诊断端口; 只在
// AP 启动等待路径调用, 延时精度口径见体内说明.
unsafe fn timer_udelay(us: u32) {
    // SAFETY: `out dx, al` 写 POST 诊断端口 0x80 (PC/AT 沿用的 io_delay 手法): Ring-0 下
    // 的端口写, 无内存操作数 ⇒ options(nomem, nostack) 与指令语义一致 (未声明
    // preserves_flags 属保守). 该端口是只写诊断口, 写它不驱动任何设备动作, 唯一目的是
    // 消耗一次 I/O 总线周期.
    // 延时是**近似值而非校准值**: 循环 us 次、每次一个端口写, 实际时长取决于总线/模拟器
    // 的 I/O 延迟 (真机约 1 µs/次, QEMU 下远小于 1 µs), 故只用于 INIT-SIPI 序列与握手轮询
    // 这类「数量级足够」的等待 (调用方: start_ap 的 INIT_WAIT_US / SIPI_DELAY_US /
    // READY_POLL_US), 不承担精确计时 —— 精确时基归 timer 子系统.
    unsafe {
        for _ in 0..us {
            core::arch::asm!("out dx, al", in("dx") 0x80u16, in("al") 0u8, options(nomem, nostack));
        }
    }
}

extern "C" fn ap_entry(lapic_id: u32) -> ! {
    // SAFETY: `cli` 清当前核 IF 位关闭可屏蔽中断; 无内存操作数, `nomem`/`nostack`
    // 与指令语义一致. AP 此时刚上线, 中断尚未配置, 关中断是入口的固定第一步.
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    // SAFETY: `TRAMPOLINE_BASE + AP_INFO_OFFSET` (0x8008) 指向 `copy_trampoline` 已拷入
    // 的 `ApStartupInfo`: 该结构 `repr(C, packed)` 54 字节, 编译期 size 断言与 asm
    // SINFO_* 偏移互校; 该页在低半区恒等映射内 (AP 当前 MMU 关闭, 以物理地址访问),
    // `read_volatile` 整体读取避免撕裂, 且只读.
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
    if let Some(info) = crate::privileged::cpu::get_cpu_info()
        && let Err(e) = crate::privileged::cpu::init_msr(&info.features)
    {
        crate::klog_err!(
            Boot,
            "[SMP] AP cpu_index={} init_msr failed: {}",
            cpu_index,
            e
        );
    }

    // AP 必须在本核加载 IDT 之后才能开中断: gdt_init_ap 只装 GDT+TSS,
    // 而 BSP 的 idt_init() 里的 lidt 只对 BSP 本核生效. 若 AP 在 IDTR.BASE=0
    // 状态下开中断, 首个本核 tick (向量 TICK_VECTOR) 即触发 #GP → #DF → triple fault.
    // SAFETY: `IdtManager::init()` 已在 BSP 上完成, IDT 表全局共享; 本核在
    // ap_entry 入口已 cli, 且 sti 在其后, 故加载期间中断处于屏蔽状态.
    unsafe {
        crate::privileged::idt::load_idt_on_current_cpu();
    }

    // 上线前一次性构造本核全部 per-CPU 状态 (CpuQueue / 调度器 / RCU / softirq,
    // 均幂等)。任一步分配失败即放弃本 AP 上线: 不登记 CPU、不握手 done ——
    // BSP 侧等待 done 超时后视本 AP 离线 (与上方 GDT 分配失败跳过本 AP 的语义
    // 一致); 绝不静默回退 BSP 状态造成跨 CPU 混叠。
    if !crate::privileged::proc::init_cpu_queue(cpu_index, 0)
        || !crate::privileged::proc::init_per_cpu_sched(cpu_index)
        || !crate::privileged::sync::rcu_alloc_cpu(cpu_index)
        || !crate::privileged::irq::softirq_alloc_cpu(cpu_index)
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

    crate::privileged::smp::register_cpu(lapic_id);

    // 建立本核调度身份: 创建 (或复用) 本核 idle 并置 `current = idle`, 此后本核
    // `schedule()` 才会真正执行上下文切换 (BSP 的 idle 不属于本核)。失败则放弃
    // 上线: 不置 done, 由 BSP 等待超时后判定离线 (与 per-CPU 状态分配失败一致)。
    if crate::privileged::proc::SCHEDULER
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

    // 本核 tick 设备自装配 (E-II-b): LAPIC 是每核私有寄存器, BSP 无法代写.
    // 次序硬约束: ① 必在 `register_cpu` 之后 —— `clockevent` 的每核装填状态按
    // `smp::current_cpu_index()` 寻址, 而该函数靠 `register_cpu` 填入的硬件 id 表
    // 反查索引, 未登记时恒返 0 (会误写 BSP 槽位); ② 必在 `load_idt_on_current_cpu`
    // 之后 (首个 tick 需有门); ③ 必在 done 握手之前, 使 "BSP 见 done" 蕴含
    // "本核 tick 已装填" (全局 tick 源参数由启动编排 7.6 先于本函数发布, 故
    // `arm_local` 不会因未就绪而空转).
    crate::privileged::timer::clockevent::arm_local();

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
    // → `sti; hlt` 融合体原子地开中断并停机。融合是必需的: 本核 tick 虽已由
    // `arm_local` 装填, 但唤醒还依赖 resched IPI 与其它核的负载均衡, 开中断与
    // `hlt` 分离会丢掉窗口内到达的 IPI; `sti; hlt` 是 x86 的经典原子等待序列
    // (STI 影子指令), 对齐 `idle_entry` 手法。
    loop {
        let _ = crate::arch!(interrupt_disable());
        let _ = crate::privileged::proc::Scheduler::schedule();
        // SAFETY: sti/hlt 在 ring 0 合法; 不读写内存也不修改栈, options 与之一致.
        unsafe {
            core::arch::asm!("sti; hlt", options(nomem, nostack));
        }
    }
}
