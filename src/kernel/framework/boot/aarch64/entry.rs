//! AArch64 启动入口 (Rust 侧)
//!
//! 从 start.S 跳转后的第一个 Rust 函数。负责:
//!   1. BSS 清零 + 栈 canary 写入
//!   2. MMU 初始化 (identity mapping + TTBR1, 必须在 UART 之前)
//!   3. PL011 UART 初始化 (使用 TTBR1 高半区地址 0xFFFF_0000_0900_0000)
//!   4. 异常向量表设置
//!   5. GICv3 初始化 (使用 TTBR1 高半区地址)
//!   6. Timer 初始化
//!   7. 跳转 kernel_init()

use crate::framework::arch::uart;
use crate::framework::dtb;

/// 引导页表 (见 `arch/aarch64/mmu.rs`) 的 Device 映射窗口上界:
/// Device 区仅覆盖 `[0, 0x4000_0000)`, 越界的设备基址一律拒绝.
const DEVICE_WINDOW_END: u64 = 0x4000_0000;
/// 引导页表 DRAM 映射窗口下界 (DRAM: `[0x4000_0000, 0x8000_0000)`).
const DRAM_WINDOW_BASE: u64 = 0x4000_0000;
/// 引导页表 DRAM 映射窗口上界.
const DRAM_WINDOW_END: u64 = 0x8000_0000;

// ============================================================================
// 启动入口
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// AArch64 启动入口。
///
/// # Safety
///
/// 仅由汇编 `start.S` 在 EL1 启动阶段调用，调用前需确保：
/// - 栈指针 (SP) 已设置
/// - BSS 段可写
/// - 运行在 EL1（内核特权级）
pub unsafe extern "C" fn entry() -> ! {
    unsafe {
        // 0. 启用 FP/SIMD (编译器会生成 NEON 指令如 movi v0.2d)
        //    CPACR_EL1.FPEN[21:20] = 0b11 → 不 trap FP/SIMD
        core::arch::asm!("mrs x0, cpacr_el1", "orr x0, x0, #(0x3 << 20)", "msr cpacr_el1, x0", out("x0") _);

        // 1. BSS 清零
        clear_bss();

        // 1.1 写入 boot 栈 canary 到 stack_bottom (栈溢出检测)
        // 与 x86_64 boot.asm trampoline64_high 对齐
        // 必须在 clear_bss 之后, 否则 canary 会被清零覆盖
        crate::framework::proc::write_boot_stack_canary();

        // 2. 初始化 MMU (identity mapping + TTBR1)
        //    必须在 UART 之前, 因为 UART 使用 TTBR1 高半区地址 (0xFFFF_0000_0900_0000),
        //    在 MMU 启用前该地址为无效物理地址, 会导致立即崩溃.
        crate::framework::arch::mmu::init();

        // 2.1 从设备树 (DTB) 探测硬件资源并覆盖默认基址
        //     须在 MMU 之后 (DTB 经高半区别名访问),
        //     须在 UART 之前 (探测到的 UART 基址须在首次输出前生效).
        let fdt_info = apply_fdt_overrides();

        // 3. 初始化 UART (使用 TTBR1 高半区地址, 依赖 MMU)
        uart::init();
        uart::puts("[BOOT] QueenX starting...");

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
        let canary_ok = crate::framework::proc::check_boot_stack_canary();
        if !canary_ok {
            uart::puts("[BOOT] FATAL: canary lost between write and kernel_init!");
            loop {}
        }

        // 4. 初始化异常向量表
        uart::puts("[BOOT] Setting up exception vectors...");
        crate::framework::arch::exception::init();

        // 5. 初始化 GICv3 (使用 TTBR1 高半区地址, 依赖 MMU)
        uart::puts("[BOOT] Initializing GICv3...");
        crate::framework::arch::gic::init();

        // 6. 初始化定时器 (仅配置, 不启用 — 稍后在 kernel_init 中启用)
        uart::puts("[BOOT] Initializing timer...");
        let (_freq, interval) = crate::framework::arch::timer::init_deferred();
        crate::framework::arch::exception::TIMER_INTERVAL_TICKS
            .store(interval, core::sync::atomic::Ordering::Relaxed);

        // 7. 跳转统一内核入口
        uart::puts("[BOOT] Booting kernel...");
        crate::kernel_init();

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
/// 解析出内存 / UART / GICv3 基址, 覆盖各子系统的默认 (QEMU virt) 基址.
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
        let dtb_mapped = crate::framework::mm::KERNEL_BASE + dtb_phys;

        // SAFETY: `dtb_mapped` 位于已映射的 DRAM 窗口; 头部前缀只需前 8 字节
        // (幻数 4 字节 + totalsize 4 字节).
        let total =
            dtb::decode_header_prefix(core::slice::from_raw_parts(dtb_mapped as *const u8, 8))?;
        // 整棵树须完整落在 DRAM 窗口内.
        if dtb_phys.checked_add(total as u64)? > DRAM_WINDOW_END {
            return None;
        }
        // SAFETY: `total` 经 `decode_header_prefix` 校验 (头部长度 ≤ total ≤ 1 MiB),
        // 且 `[dtb_mapped, dtb_mapped + total)` 已确认落在已映射的 DRAM 窗口内.
        let blob = core::slice::from_raw_parts(dtb_mapped as *const u8, total);
        let info = dtb::parse(blob)?;

        // Device 窗口仅覆盖 [0, 0x4000_0000); 越界基址一律忽略, 沿用默认值.
        if let Some(pa) = info.uart_base.filter(|pa| *pa < DEVICE_WINDOW_END) {
            uart::set_base(pa);
        }
        let dist = info.gic_dist_base.filter(|pa| *pa < DEVICE_WINDOW_END);
        let redist = info.gic_redist_base.filter(|pa| *pa < DEVICE_WINDOW_END);
        if let (Some(dist), Some(redist)) = (dist, redist) {
            crate::framework::arch::gic::set_bases(dist, redist);
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
#[expect(
    clippy::borrow_as_ptr,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
unsafe fn clear_bss() {
    unsafe {
        let bss_start = &mut __bss_start as *mut u8;
        let bss_end = &_kernel_end as *const u8 as usize;

        if bss_start as usize >= bss_end {
            return;
        }

        let size = bss_end - bss_start as usize;
        core::ptr::write_bytes(bss_start, 0, size);
    }
}
