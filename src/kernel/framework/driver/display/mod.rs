//! 显示子系统 (Display Subsystem)
//!
//! 提供完整的显示支持：
//! - **Framebuffer**: 帧缓冲驱动
//! - **VBE 原语**: 显示模式探测与帧缓冲映射
//!
//! ## 架构
//!
//! ```text
//! Display Subsystem
//! └── framebuffer.rs  # Framebuffer驱动
//! ```
//!
//! 显示控制器管理策略 (`controller.rs`) 已于 2-E 批次迁至
//! `services::driver::display::controller`; framebuffer/font 因被
//! framework 图形控制台机制 (gfx_console) 直接绑定而保留于此。

pub mod font;
pub mod framebuffer;
pub mod self_test;

// 导出Framebuffer类型
pub use framebuffer::{Color, Framebuffer, PixelFormat, Point, Rect, colors};

use super::framework;
use super::framework::{Driver, DriverError};
use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;
use crate::framework::sync::IrqSpinLock;

struct DisplayDriver;

impl Driver for DisplayDriver {
    fn name(&self) -> &'static str {
        "display"
    }
    fn device_type(&self) -> framework::DeviceType {
        framework::DeviceType::Other
    }
    fn init(&mut self) -> framework::Result<()> {
        Ok(())
    }
    fn shutdown(&mut self) -> framework::Result<()> {
        Ok(())
    }
    fn is_ready(&self) -> bool {
        true
    }
}

// ============================================================================
// 全局帧缓冲实例
// ============================================================================

/// 全局帧缓冲 — 在 `display_init()` 中初始化，之后只读访问
static GLOBAL_FRAMEBUFFER: IrqSpinLock<Option<Framebuffer>> = IrqSpinLock::new(None);

/// 帧缓冲物理地址 — 供 `sys_fb_mmap` 映射到用户空间
pub static FB_PHYS_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 帧缓冲物理大小 — 供 `sys_fb_mmap` 校验
pub static FB_PHYS_SIZE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 获取全局帧缓冲的锁守卫
///
/// 返回 `IrqSpinLockGuard` 持有锁期间访问帧缓冲。
/// 调用方通过 `.as_ref()` 获取 `Option<&Framebuffer>`。
pub fn get_framebuffer()
-> Option<crate::framework::sync::IrqSpinLockGuard<'static, Option<Framebuffer>>> {
    let guard = GLOBAL_FRAMEBUFFER.lock();
    if guard.is_some() { Some(guard) } else { None }
}

// ============================================================================
// 像素格式推断
// ============================================================================

/// 根据 Multiboot2 `FramebufferInfo` 中的 bpp 和 RGB 字段位置推断像素格式
fn infer_pixel_format(bpp: u8, red_pos: u8, green_pos: u8, blue_pos: u8) -> PixelFormat {
    match bpp {
        32 => {
            if red_pos == 16 && green_pos == 8 && blue_pos == 0 {
                PixelFormat::Argb8888
            } else {
                PixelFormat::Bgra8888
            }
        }
        24 => {
            if blue_pos == 0 {
                PixelFormat::Bgr888
            } else {
                PixelFormat::Rgb888
            }
        }
        16 => PixelFormat::Rgb565,
        _ => PixelFormat::Bgra8888,
    }
}

// ============================================================================
// PCI VGA 帧缓冲探测（QEMU -kernel 启动时的回退方案, x86_64 专用）
// ============================================================================

#[cfg(target_arch = "x86_64")]
/// Bochs VBE DISPI MMIO 寄存器偏移（相对于 BAR0）
const VBE_DISPI_MMIO_BASE: u64 = 0x500;

/// Bochs VBE DISPI 端口 I/O 地址
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_PORT_INDEX: u16 = 0x01CE;
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_PORT_DATA: u16 = 0x01CF;

/// Bochs VBE DISPI 寄存器索引
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_INDEX_ID: u16 = 0;
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_INDEX_XRES: u16 = 1;
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_INDEX_YRES: u16 = 2;
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_INDEX_BPP: u16 = 3;
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_INDEX_ENABLE: u16 = 4;

/// Bochs VBE DISPI ID 值
#[cfg(target_arch = "x86_64")]
const VBE_DISPI_ID5: u16 = 0xB0C5;

#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
struct VgaFbInfo {
    addr: u64,
    pitch: u32,
    width: u32,
    height: u32,
    bpp: u8,
}

/// 通过 Bochs DISPI 端口读取 VGA 帧缓冲分辨率
#[cfg(target_arch = "x86_64")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
fn read_bochs_disp_mode() -> Option<(u32, u32, u8)> {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        port_outw(VBE_DISPI_PORT_INDEX, VBE_DISPI_INDEX_ID);
        let id = port_inw(VBE_DISPI_PORT_DATA);
        if id < VBE_DISPI_ID5 {
            // Bochs DISPI 不存在或版本太老 (< 0xB0C5)
            return None;
        }
        port_outw(VBE_DISPI_PORT_INDEX, VBE_DISPI_INDEX_ENABLE);
        let enabled = port_inw(VBE_DISPI_PORT_DATA);
        if enabled == 0 {
            return None;
        }
        port_outw(VBE_DISPI_PORT_INDEX, VBE_DISPI_INDEX_XRES);
        let xres = u32::from(port_inw(VBE_DISPI_PORT_DATA));
        port_outw(VBE_DISPI_PORT_INDEX, VBE_DISPI_INDEX_YRES);
        let yres = u32::from(port_inw(VBE_DISPI_PORT_DATA));
        port_outw(VBE_DISPI_PORT_INDEX, VBE_DISPI_INDEX_BPP);
        let bpp = port_inw(VBE_DISPI_PORT_DATA) as u8;
        if xres == 0 || yres == 0 || bpp == 0 {
            return None;
        }
        Some((xres, yres, bpp))
    }
}

/// 通过 MMIO 读取 Bochs DISPI 寄存器 (替代 port I/O)
///
/// # Safety
///
/// - `mmio_base` 必须是有效的 VGA BAR0 映射地址
/// - 偏移计算: `VBE_DISPI_MMIO_BASE` + reg * 2 (每寄存器 2 字节间距)
#[cfg(target_arch = "x86_64")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
unsafe fn read_bochs_disp_mode_mmio(mmio_base: u64) -> Option<(u32, u32, u8)> {
    // SAFETY: 调用方保证 mmio_base 是有效的 VGA BAR0 映射,
    // 偏移在 BAR0 范围内, volatile 访问对 MMIO 寄存器是必需的.
    unsafe {
        let base = mmio_base + VBE_DISPI_MMIO_BASE;
        let read_reg = |reg: u16| -> u16 {
            core::ptr::read_volatile((base + u64::from(reg) * 2) as *const u16)
        };
        let id = read_reg(VBE_DISPI_INDEX_ID);
        if id < VBE_DISPI_ID5 {
            return None;
        }
        let enabled = read_reg(VBE_DISPI_INDEX_ENABLE);
        if enabled == 0 {
            return None;
        }
        let xres = u32::from(read_reg(VBE_DISPI_INDEX_XRES));
        let yres = u32::from(read_reg(VBE_DISPI_INDEX_YRES));
        let bpp = read_reg(VBE_DISPI_INDEX_BPP) as u8;
        if xres == 0 || yres == 0 || bpp == 0 {
            return None;
        }
        Some((xres, yres, bpp))
    }
}

/// 通过 PCI 探测 VGA 设备 BAR0 获取帧缓冲信息
#[cfg(target_arch = "x86_64")]
fn probe_vga_fb_via_pci() -> Option<VgaFbInfo> {
    let devices = crate::framework::pci::find_by_class(crate::framework::pci::CLASS_DISPLAY);
    for dev in &devices {
        if dev.subclass_code != 0x00 {
            crate::klog_info!(
                Driver,
                "[DISPLAY] skipping non-VGA display dev {:04X}:{:04X} subclass {:02X}",
                dev.vendor_id,
                dev.device_id,
                dev.subclass_code
            );
            continue;
        }
        if dev.bar_count == 0 {
            continue;
        }
        let bar0 = &dev.bars[0];
        if bar0.base_addr == 0 {
            continue;
        }

        // 优先 MMIO 路径 (避免 port I/O 开销)
        let mode = if bar0.base_addr != 0 {
            // SAFETY: bar0.base_addr 是 PCI BAR0 物理地址, 已通过 PCI 枚举验证.
            // MMIO 偏移 VBE_DISPI_MMIO_BASE 在 BAR0 范围内.
            unsafe { read_bochs_disp_mode_mmio(bar0.base_addr) }
        } else {
            None
        };
        // 回退到 port I/O 路径
        let (width, height, bpp) = mode
            .or_else(read_bochs_disp_mode)
            .unwrap_or((1024, 768, 32));

        let pitch = width * (u32::from(bpp) / 8);

        crate::klog_info!(
            Driver,
            "VGA via PCI {:02X}:{:02X}.{} BAR0=0x{:X} size=0x{:X} {}x{}x{}",
            dev.bus,
            dev.device,
            dev.function,
            bar0.base_addr,
            bar0.size,
            width,
            height,
            bpp
        );

        return Some(VgaFbInfo {
            addr: bar0.base_addr,
            pitch,
            width,
            height,
            bpp,
        });
    }
    None
}

// ============================================================================
// 初始化函数
// ============================================================================

/// 初始化显示子系统
///
/// Phase G1: 连接真实 LFB，在 QEMU GTK 窗口显示蓝色像素。
///
/// 流程:
/// 1. 首先尝试从 Multiboot2 获取帧缓冲信息（GRUB 启动时可用）
/// 2. 若不可用，回退到 PCI + Bochs DISPI 探测（QEMU -kernel 启动）
/// 3. 将物理地址映射到内核虚拟地址空间
/// 4. 推断像素格式
/// 5. 创建 Framebuffer 实例并自检
/// # Errors
/// 未找到可用帧缓冲或创建 Framebuffer 失败时返回 Err。
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[cfg_attr(
    target_arch = "aarch64",
    expect(
        clippy::manual_let_else,
        reason = "aarch64 display_init 是占位; x86_64 走 multiboot2 + pci 两条路径"
    )
)]
pub fn display_init() -> framework::Result<()> {
    crate::klog_boot_info!("[DISPLAY] display_init: probing framebuffer");

    // ── 方案 A: Multiboot2 tag 8 (GRUB boot) ──
    let fb_info = match crate::framework::boot::multiboot2_fb::get_framebuffer_info() {
        Some(info) if info.is_valid() => {
            crate::klog_boot_info!(
                "[DISPLAY] got framebuffer from Multiboot2: {}x{}x{} @ 0x{:X}",
                info.width,
                info.height,
                info.bpp,
                info.addr
            );
            Some((info.addr, info.width, info.height, info.bpp, info.pitch))
        }
        _ => {
            crate::klog_drv_warn!(
                "[DISPLAY] no Multiboot2 framebuffer tag, falling back to PCI probe"
            );
            None
        }
    };

    // ── 方案 B: PCI VGA BAR0 probing (QEMU -kernel boot, x86 only) ──
    let (fb_addr, width, height, bpp, pitch) = match fb_info {
        Some(info) => info,
        None => {
            #[cfg(target_arch = "x86_64")]
            {
                if let Some(info) = probe_vga_fb_via_pci() {
                    (info.addr, info.width, info.height, info.bpp, info.pitch)
                } else {
                    crate::klog_drv_warn!("[DISPLAY] no VGA device found via PCI");
                    return Ok(());
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                crate::klog_drv_warn!("[DISPLAY] no framebuffer info on non-x86 platform");
                return Ok(());
            }
        }
    };

    let fb_size = u64::from(pitch) * u64::from(height);

    FB_PHYS_ADDR.store(fb_addr, core::sync::atomic::Ordering::Release);
    FB_PHYS_SIZE.store(fb_size, core::sync::atomic::Ordering::Release);

    let _virt_addr = crate::framework::mm::map_framebuffer(fb_addr, fb_size);

    let format = infer_pixel_format(bpp, 16, 8, 0);

    // SAFETY: 帧缓冲物理地址来自 bootloader, 由 map_framebuffer 恒等映射
    let fb_iomem = unsafe {
        IoMem::new(PhysAddr(fb_addr), fb_size as usize, "fb")
            .map_err(|_| DriverError::HardwareError)?
    };

    GLOBAL_FRAMEBUFFER.with_mut(|opt| {
        *opt = Some(Framebuffer::new(fb_iomem, width, height, pitch, format));

        if let Some(ref mut fb) = *opt {
            let _ = fb.init();

            crate::klog_info!(
                Driver,
                "[DISPLAY] OK: {}x{}x{} @ 0x{:X}",
                width,
                height,
                bpp,
                fb_addr
            );

            let font = font::default_font();
            let failures = self_test::framebuffer_self_test(fb, font);
            if failures == 0 {
                crate::klog_info!(Driver, "[DISPLAY] self-test: ALL PASSED");
            } else {
                crate::klog_drv_warn!("[DISPLAY] self-test: {} FAILURES", failures);
            }

            let console = alloc::boxed::Box::new(
                crate::framework::console::gfx_console::GfxConsole::new(fb as *mut _, font),
            );
            crate::framework::console::gfx_console_init(alloc::boxed::Box::leak(console));
            crate::klog_info!(Driver, "[DISPLAY] GfxConsole initialized");
        }
    });

    crate::framework::egdf::egdf_register_driver(
        "vga-display",
        crate::framework::egdf::EGDFProto::Other,
        Some(fb_addr),
        None,
        alloc::boxed::Box::new(DisplayDriver),
    );

    Ok(())
}

// ============================================================================
// DECISION-K 单向注册契约: 显示控制器工厂槽 (MIG-008)
// ============================================================================

/// services 层显示控制器工厂回调槽 (DECISION-K 模式)
///
/// MIG-008 接线补齐: framework 持有本槽 + [`display_probe_controllers`] 机制,
/// services 注册无捕获工厂函数指针 (在 services 侧构造 HDMI/DP/DisplayManager
/// 并经 EGDF 注册)。未注册时 [`display_probe_controllers`] fail-quiet 跳过。
static DISPLAY_CONTROLLER_FACTORY: crate::framework::sync::OnceLock<fn()> =
    crate::framework::sync::OnceLock::new();

/// 注册 services 层显示控制器工厂回调 (services 可调用的 0 unsafe 入口)
///
/// # Errors
///
/// 回调槽已被占用 (重复注册) 时返回 `Err(已注册回调)`.
pub fn register_display_controller_factory(factory: fn()) -> core::result::Result<(), fn()> {
    DISPLAY_CONTROLLER_FACTORY.set(factory)
}

/// 单向拉取 services 注册的显示控制器工厂 (crate root lib.rs 编排调用)
///
/// MIG-008: 帧缓冲初始化 ([`display_init`]) 完成后由 crate root 调用本函数,
/// 触发 services 侧 HDMI/DP/DisplayManager 注册 (services→framework 注册槽,
/// framework→services 单向拉取, 见 DECISION-K)。
pub fn display_probe_controllers() {
    if let Some(factory) = DISPLAY_CONTROLLER_FACTORY.get() {
        factory();
    }
}

// ============================================================================
// 端口 I/O 辅助函数 (x86_64 专用)
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
unsafe fn port_outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn port_inw(port: u16) -> u16 {
    unsafe {
        let ret: u16;
        core::arch::asm!("in ax, dx", out("ax") ret, in("dx") port, options(nomem, nostack));
        ret
    }
}
