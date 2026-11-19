//! `AArch64` 启动入口 (Rust 侧)
//!
//! 从 start.S 跳转后的第一个 Rust 函数。负责:
//!   1. BSS 清零 + 栈 canary 写入
//!   2. MMU 初始化 (identity mapping + TTBR1, 必须在 UART 之前)
//!   3. PL011 UART 初始化 (使用 TTBR1 高半区地址 `0xFFFF_0000_0900_0000`)
//!   4. 异常向量表设置
//!   5. `GICv3` 初始化 (使用 TTBR1 高半区地址)
//!   6. Timer 初始化
//!   7. 跳转 `kernel_init()`

use crate::privileged::arch::uart;
use crate::privileged::dtb;

/// 引导页表 (见 `arch/aarch64/mmu.rs`) 的 Device 映射窗口上界:
/// Device 区仅覆盖 `[0, 0x4000_0000)`, 越界的设备基址一律拒绝.
const DEVICE_WINDOW_END: u64 = 0x4000_0000;
/// 引导页表 DRAM 映射窗口下界 (DRAM: `[0x4000_0000, 0x8000_0000)`).
const DRAM_WINDOW_BASE: u64 = 0x4000_0000;
/// 引导页表 DRAM 映射窗口上界.
const DRAM_WINDOW_END: u64 = 0x8000_0000;

// P-71: `kernel_init` 由 glue crate 定义 (`#[unsafe(no_mangle)] extern "C"`),
// TCB 不再是同一 crate, 故以外部符号声明完成引导链交接 (声明置于模块作用域,
// 不放在函数体内语句之后 — clippy::items_after_statements).
// SAFETY: 符号在最终链接产物中唯一, 签名与调用约定与 glue 侧定义一致.
unsafe extern "C" {
    /// glue crate 的统一内核入口.
    fn kernel_init();
}

// ============================================================================
// 启动入口
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// `AArch64` 启动入口。
///
/// # Safety
///
/// 仅由汇编 `start.S` 在 EL1 启动阶段调用，调用前需确保：
/// - 栈指针 (SP) 已设置
/// - BSS 段可写
/// - 运行在 EL1（内核特权级）
pub unsafe extern "C" fn entry() -> ! {
    // SAFETY: 本块是 aarch64 内核唯一的 Rust 引导入口, 全部前提由 `boot/aarch64/start.S`
    // 的 `el1_entry` 在跳入前建立 (末步 `ldr x9, =entry; br x9` —— 高半区 VMA 超出 `bl`
    // 的 ±128MB 可达范围, 故用绝对跳转):
    // ① 特权级/中断/并发: EL1h, SPSR_EL2 = 0x3C5 ⇒ DAIF 四位全屏蔽, 本块全程无中断;
    //    AP 尚未由 PSCI CPU_ON 上电 ⇒ 单核独占, 块内所有 static 写入无竞争者.
    // ② 栈: SP = `_boot_stack_top` (`.bootbss`, VMA == PA), 位于低半区且**不在**
    //    `clear_bss` 的清零区间内, 故第 1 步不会毁掉正在使用的栈.
    // ③ MMU **已启用**: start.S 4.3–4.7 已建 `_boot_l0/l1/l2` 三张引导页表 (L1[0] →
    //    512×2MB Device 块覆盖 [0, 0x4000_0000), L1[1] → PA 0x4000_0000 起的 1GB
    //    Normal 块), MAIR = 0x00FF_0000_FF44 / TCR = 0xB510_3510 与 `arch::mmu::init()`
    //    同值, TTBR0 = TTBR1 = `_boot_l0` (高半区别名的 VA[47:39] 恒 0 ⇒ 同一张 L0
    //    服务两半区). 故第 2 步 `mmu::init()` 是**同值重建**到 `.bss` 内的静态
    //    AlignedPageTable 并重装 TTBR, 不是首次启用, 中间不存在未映射窗口; 高半区
    //    `.bss` 与 UART 别名此刻已可访可写.
    // ④ x0 已不再是 DTB 指针: `_start` 首条指令即把 x0 存入 `_fdt_addr`, 故第 0 步
    //    asm 以 `out("x0") _` 声明 x0 被改写是安全的; DTB 改经 `fdt_addr_ptr` 高半区
    //    别名读取 (`apply_fdt_overrides`). 第 0 步的 CPACR_EL1.FPEN = 0b11 并非冗余:
    //    引导器直接把 CPU 交到 EL1 时 start.S 走 `beq el1_entry` 跳过 `el2_entry`,
    //    而 FPEN 只在后者里设过.
    // 次序硬约束 (逐条见下方各步注释): clear_bss 先于 write_boot_stack_canary (否则
    // canary 被清零覆盖); mmu::init 先于 UART (UART 走 TTBR1 高半区别名);
    // apply_fdt_overrides 在 MMU 之后、UART 之前; exception::init 先于 gic::init
    // (开中断前须先装 VBAR). canary 校验失败走 spin_loop 停机、gic::init 返回 Err 走
    // halt 停机, 均 fail-fast 不带病继续. 末尾 `kernel_init` 是 glue crate 的外部符号
    // (P-71), 正常路径不返回.
    unsafe {
        // 0. 启用 FP/SIMD (编译器会生成 NEON 指令如 movi v0.2d)
        //    CPACR_EL1.FPEN[21:20] = 0b11 → 不 trap FP/SIMD
        core::arch::asm!("mrs x0, cpacr_el1", "orr x0, x0, #(0x3 << 20)", "msr cpacr_el1, x0", out("x0") _);

        // 1. BSS 清零
        clear_bss();

        // 1.1 写入 boot 栈 canary 到 stack_bottom (栈溢出检测)
        // 与 x86_64 boot.asm trampoline64_high 对齐
        // 必须在 clear_bss 之后, 否则 canary 会被清零覆盖
        crate::privileged::proc::write_boot_stack_canary();

        // 2. 初始化 MMU (identity mapping + TTBR1)
        //    必须在 UART 之前, 因为 UART 使用 TTBR1 高半区地址 (0xFFFF_0000_0900_0000),
        //    在 MMU 启用前该地址为无效物理地址, 会导致立即崩溃.
        crate::privileged::arch::mmu::init();

        // 2.1 从设备树 (DTB) 探测硬件资源并覆盖默认基址
        //     须在 MMU 之后 (DTB 经高半区别名访问),
        //     须在 UART 之前 (探测到的 UART 基址须在首次输出前生效).
        let fdt_info = apply_fdt_overrides();

        // 3. 初始化 UART (使用 TTBR1 高半区地址, 依赖 MMU)
        uart::init();
        uart::puts("[BOOT] Edgine starting...");

        // 3.0 输出设备树探测结果 (UART 已可用)
        if let Some(info) = fdt_info {
            crate::klog_ffi!(
                klog_ffi_info,
                "[BOOT] DTB: mem={:#x}+{:#x} uart={:#x?} gicd={:#x?} gicr={:#x?}",
                info.memory_base,
                info.memory_size,
                info.uart_base,
                info.gic_dist_base,
                info.gic_redist_base
            );
        } else {
            uart::puts("[BOOT] DTB unavailable, using built-in QEMU virt defaults");
        }

        // 3.1 验证 canary (UART 已可用, 异常向量表尚未设置, 崩了就是真崩)
        let canary_ok = crate::privileged::proc::check_boot_stack_canary();
        if !canary_ok {
            uart::puts("[BOOT] FATAL: canary lost between write and kernel_init!");
            loop {
                core::hint::spin_loop();
            }
        }

        // 4. 初始化异常向量表
        uart::puts("[BOOT] Setting up exception vectors...");
        crate::privileged::arch::exception::init();

        // 5. 初始化 GICv3 (使用 TTBR1 高半区地址, 依赖 MMU)
        //    初始化后置条件校验失败时 fail-fast, 避免带病启动导致静默挂起.
        uart::puts("[BOOT] Initializing GICv3...");
        if let Err(reason) = crate::privileged::arch::gic::init() {
            uart::puts("[BOOT] FATAL: GICv3 init failed: ");
            uart::puts(reason);
            uart::puts("\r\n");
            loop {
                crate::arch!(halt());
            }
        }
        uart::puts("[BOOT] GICv3 ready\r\n");

        // 6. 初始化定时器 (仅配置, 不启用 — 稍后在 kernel_init 中启用)
        uart::puts("[BOOT] Initializing timer...");
        let (_freq, interval) = crate::privileged::arch::timer::init_deferred();
        crate::privileged::arch::exception::TIMER_INTERVAL_TICKS
            .store(interval, core::sync::atomic::Ordering::Relaxed);

        // 7. 跳转统一内核入口 (外部符号声明见本模块顶部)
        uart::puts("[BOOT] Booting kernel...");
        kernel_init();

        // 不应该到达这里
        loop {
            crate::arch!(halt());
        }
    }
}

// ============================================================================
// 设备树 (DTB) 硬件探测
// ============================================================================

// SAFETY: C ABI 互操作; `fdt_addr_ptr` 由链接脚本 (`link/aarch64.ld`) 定义为
// `.bootbss` 中 `_fdt_addr` 的高半区别名 (VA = KERNEL_BASE + PA),
// 其内容为 `start.S` 保存的设备树物理地址.
unsafe extern "C" {
    static fdt_addr_ptr: u64;
}

/// 从引导程序传入的设备树探测并覆盖硬件基址
///
/// 读取 `start.S` 保存在 `.bootbss` 中的 DTB 物理地址, 经 TTBR1 高半区别名
/// 解析出内存 / UART / `GICv3` 基址, 覆盖各子系统的默认 (QEMU virt) 基址.
/// 任一环节不满足 (无 DTB / 地址越界 / 解析失败) 均返回 `None`, 内核继续沿用
/// 内置默认值, 保证 QEMU virt 零回归.
///
/// # Safety
///
/// 调用前须完成 `mmu::init()`, 确保 `KERNEL_BASE + dtb_phys` 落在已映射的 DRAM 窗口.
unsafe fn apply_fdt_overrides() -> Option<dtb::DtbInfo> {
    unsafe {
        // SAFETY: `fdt_addr_ptr` 内容为 start.S 保存的设备树物理地址;
        // boot 阶段单核, volatile 读无数据竞争.
        let dtb_phys = core::ptr::read_volatile(&raw const fdt_addr_ptr);
        if !(DRAM_WINDOW_BASE..DRAM_WINDOW_END).contains(&dtb_phys) {
            return None;
        }
        let dtb_mapped = crate::privileged::mm::KERNEL_BASE + dtb_phys;

        // SAFETY: `dtb_mapped` 位于已映射的 DRAM 窗口; 头部前缀只需前 8 字节
        // (幻数 4 字节 + totalsize 4 字节).
        let total =
            dtb::decode_header_prefix(core::slice::from_raw_parts(dtb_mapped as *const u8, 8))?;
        // 整棵树须完整落在 DRAM 窗口内.
        if dtb_phys.checked_add(total as u64)? > DRAM_WINDOW_END {
            return None;
        }
        // 持久化 DTB blob 自身占用的物理区, 供 boot_memory_init 登记 PMM 早期预留
        // (引导器自选地址, 与内核固定布局无协商; 不登记就会被当空闲页分配出去).
        dtb::set_dtb_region(dtb_phys, total as u64);
        // SAFETY: `total` 经 `decode_header_prefix` 校验 (头部长度 ≤ total ≤ 1 MiB),
        // 且 `[dtb_mapped, dtb_mapped + total)` 已确认落在已映射的 DRAM 窗口内.
        let blob = core::slice::from_raw_parts(dtb_mapped as *const u8, total);
        let info = dtb::parse(blob)?;

        // 持久化 CPU 拓扑供 SMP 次核启动消费 (DTB 解析结果本身仅在此处局部可见).
        if let Some(topology) = info.cpus {
            dtb::set_cpu_topology(topology);
        }

        // 持久化 PSCI conduit 供后续 CPU_ON / 关机 / 重启调用分派.
        if let Some(method) = info.psci_method {
            dtb::set_psci_method(method);
        }

        // 持久化 initrd 物理区 (经 /chosen) 供 boot::init 填入 BootInfo 并 PMM 预留.
        if let (Some(start), Some(end)) = (info.initrd_start, info.initrd_end)
            && end > start
        {
            dtb::set_initrd_region(start, end - start);
        }

        // Device 窗口仅覆盖 [0, 0x4000_0000); 越界基址一律忽略, 沿用默认值.
        if let Some(pa) = info.uart_base.filter(|pa| *pa < DEVICE_WINDOW_END) {
            uart::set_base(pa);
        }
        let dist = info.gic_dist_base.filter(|pa| *pa < DEVICE_WINDOW_END);
        let redist = info.gic_redist_base.filter(|pa| *pa < DEVICE_WINDOW_END);
        if let (Some(dist), Some(redist)) = (dist, redist) {
            crate::privileged::arch::gic::set_bases(dist, redist);
        }
        Some(info)
    }
}

// ============================================================================
// BSS 清零
// ============================================================================

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    static mut __bss_start: u8;
    static _kernel_end: u8;
}

// SAFETY: `clear_bss` 是有效的 C ABI 函数指针; 参数列表与声明一致
unsafe fn clear_bss() {
    // SAFETY: `__bss_start` / `_kernel_end` 是 `privileged/link/aarch64.ld` 定义的链接
    // 脚本符号 (前者在 `.bss` 段起点, 后者在段尾 ALIGN(4096) 之后), 声明为 `u8` 只为
    // 取址 —— 本函数只用二者地址算区间, 从不解引用那个 u8 本身, 故无类型有效性面,
    // u8 对齐恒 1 亦无对齐面. 用 `&raw mut` / `&raw const` 而非 `&` / `&mut`: 既避免
    // 对 `static mut` 构造引用 (clippy::static_mut_refs), 也因这里只需要地址.
    // 区间语义: [__bss_start, _kernel_end) = `.bss` + `.bss.*` + COMMON + 末尾至多
    // 4095 字节的页对齐填充, 全在内核镜像的高半区 VMA 内. 调用时 start.S 的引导页表
    // 已把 PA [0x4000_0000, 0x8000_0000) 映为 Normal 可写块 (L1[1]), 高半区 VA = PA +
    // 0xFFFF_0000_0000_0000 ⇒ `write_bytes` 的目的区间已映射且可写; 该物理区也不会被
    // PMM 分配出去 (`pmm::mark_reserved_pages` 按 `kernel_end` 把镜像区整段置已用).
    // **低半区 `.bootbss` 不在区间内** (其地址远低于 `__bss_start`): 正在使用的引导栈、
    // `_fdt_addr` (DTB 指针)、`_ap_boot_info` (次核启动槽) 与三张引导页表因此存活,
    // 清零不会自毁. `bss_start >= bss_end` 提前返回使 `size` 不会下溢 (链接脚本异常时
    // 退化为空操作, 而非写一个巨大区间). 唯一调用点是 `entry()` 第 1 步, 单核引导期,
    // 无并发写者.
    unsafe {
        let bss_start = &raw mut __bss_start;
        let bss_end = &raw const _kernel_end as usize;

        if bss_start as usize >= bss_end {
            return;
        }

        let size = bss_end - bss_start as usize;
        core::ptr::write_bytes(bss_start, 0, size);
    }
}
