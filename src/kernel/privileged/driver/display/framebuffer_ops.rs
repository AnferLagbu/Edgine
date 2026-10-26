//! 帧缓冲绘图策略 trait — 策略-机制分离接口
//!
//! 落实 `docs/plan/framekernel-paradigm-enforcement.md` §6.3: 帧缓冲的
//! **绘图算法** (填充/直线/矩形/圆/alpha 混合/Wu 反走样) 属策略, 由 functions
//! 实现; 像素格式转换与 MMIO 写入 (`set_pixel`/`get_pixel`)、几何参数与滚动
//! 内存搬移属机制, 保留在 privileged.
//!
//! ## 设计
//!
//! - [`PixelCanvas`] 是绘图算法所需的**最小画布契约** (宽/高 + 像素读写),
//!   由 [`Framebuffer`](super::framebuffer::Framebuffer) 实现; functions 的
//!   算法对 `dyn PixelCanvas` 编程, 单元测试用内存 mock 画布, 无需 MMIO.
//! - [`FramebufferOps`] 是策略接口, 定义在 privileged、实现在 functions.
//! - 未注册时 [`current_framebuffer_ops`] 返回 `None`, 调用方跳过绘制
//!   (逻辑错误降级: 启动早期串口日志仍可用, 不会因策略缺失而 panic).

use super::framebuffer::{Color, Rect};

/// 绘图画布契约 — 绘图算法所需的最小像素访问面
///
/// 由 `Framebuffer` 实现 (像素格式转换 + MMIO 机制), functions 侧用内存
/// 缓冲区实现以便单元测试。
pub trait PixelCanvas {
    /// 画布宽度 (像素)
    fn width(&self) -> u32;
    /// 画布高度 (像素)
    fn height(&self) -> u32;
    /// 写入像素 (越界由实现方自行忽略)
    fn set_pixel(&mut self, x: u32, y: u32, color: Color);
    /// 读取像素 (越界返回 `None`)
    fn get_pixel(&self, x: u32, y: u32) -> Option<Color>;
}

/// 帧缓冲绘图策略接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯算法, 不接触硬件寄存器; 像素落盘由 [`PixelCanvas`] 承担.
pub trait FramebufferOps: Send + Sync {
    /// 填充整个画布
    fn fill(&self, canvas: &mut dyn PixelCanvas, color: Color);
    /// 以黑色清屏
    fn clear(&self, canvas: &mut dyn PixelCanvas);
    /// 填充矩形区域 (自动裁剪到画布内)
    fn fill_rect(&self, canvas: &mut dyn PixelCanvas, rect: Rect, color: Color);
    /// 绘制水平线
    fn draw_hline(&self, canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color);
    /// 绘制垂直线
    fn draw_vline(&self, canvas: &mut dyn PixelCanvas, x: i32, y: i32, length: u32, color: Color);
    /// 绘制直线 (Bresenham 算法)
    fn draw_line(
        &self,
        canvas: &mut dyn PixelCanvas,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Color,
    );
    /// 绘制矩形边框
    fn draw_rect(&self, canvas: &mut dyn PixelCanvas, rect: Rect, color: Color);
    /// 绘制圆形边框 (中点圆算法)
    fn draw_circle(
        &self,
        canvas: &mut dyn PixelCanvas,
        cx: i32,
        cy: i32,
        radius: u32,
        color: Color,
    );
    /// 绘制填充圆 (水平线扫描)
    fn fill_circle(
        &self,
        canvas: &mut dyn PixelCanvas,
        cx: i32,
        cy: i32,
        radius: u32,
        color: Color,
    );
    /// Alpha 混合写入单个像素 (读-改-写)
    fn blend_pixel(&self, canvas: &mut dyn PixelCanvas, x: u32, y: u32, color: Color);
    /// 绘制反走样直线 (Wu 算法)
    fn draw_line_aa(
        &self,
        canvas: &mut dyn PixelCanvas,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Color,
    );
}

/// 全局绘图策略注册表 — functions 通过 `register_framebuffer_ops` 注册
static FRAMEBUFFER_OPS: crate::privileged::sync::OnceLock<&'static dyn FramebufferOps> =
    crate::privileged::sync::OnceLock::new();

/// 注册帧缓冲绘图策略 (由 functions 启动期调用)
///
/// 只能注册一次; 重复注册返回 `Err(已注册策略)`.
///
/// # Errors
/// 策略槽已被占用时返回 `Err`, 携带已注册的旧策略.
pub fn register_framebuffer_ops(
    ops: &'static dyn FramebufferOps,
) -> Result<(), &'static dyn FramebufferOps> {
    FRAMEBUFFER_OPS.set(ops)
}

/// 获取当前注册的绘图策略 (未注册时返回 `None`)
#[inline]
pub fn current_framebuffer_ops() -> Option<&'static dyn FramebufferOps> {
    FRAMEBUFFER_OPS.get().copied()
}
