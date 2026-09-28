//! 显示控制器抽象层 (Display Controller Abstraction)
//!
//! 提供统一的显示控制器接口：
//! - **多显示器支持**: 管理多个显示设备
//! - **显示模式管理**: 分辨率和刷新率切换
//! - **热插拔支持**: 显示器动态连接
//! - **显示输出路由**: 控制输出到哪个显示器
//!
//! 原始实装: `kernel::framework::driver::display::controller` (0 unsafe,
//! 纯管理策略), 2-E 批次按「VBE 原语留框架, 管理迁出」处方迁至 services。
//! framebuffer/font 等被 framework 图形控制台机制 (gfx_console) 直接绑定的
//! 原语仍留 framework。

use crate::framework::driver::{
    DeviceInfo, DeviceType, Driver, DriverError, DriverResult as Result, Framebuffer, PixelFormat,
};
use alloc::vec::Vec;

// ============================================================================
// 显示输出类型
// ============================================================================

/// 显示输出类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayOutput {
    /// VGA输出
    Vga,
    /// HDMI输出
    Hdmi,
    /// `DisplayPort输出`
    DisplayPort,
    /// DVI输出
    Dvi,
    /// 内置LCD
    Internal,
    /// 虚拟显示
    Virtual,
}

// ============================================================================
// 显示模式
// ============================================================================

/// 显示模式信息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    /// 宽度 (像素)
    pub width: u32,
    /// 高度 (像素)
    pub height: u32,
    /// 刷新率 (Hz)
    pub refresh_rate: u32,
    /// 像素格式
    pub pixel_format: PixelFormat,
    /// 是否首选模式
    pub preferred: bool,
}

impl DisplayMode {
    pub fn new(width: u32, height: u32, refresh_rate: u32, pixel_format: PixelFormat) -> Self {
        Self {
            width,
            height,
            refresh_rate,
            pixel_format,
            preferred: false,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 计算像素时钟 (kHz)
    pub fn pixel_clock_khz(&self) -> u64 {
        // 简化计算，实际需要考虑消隐时间
        let total_pixels = u64::from(self.width) * u64::from(self.height);
        total_pixels * u64::from(self.refresh_rate) / 1000
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 计算带宽 (MB/s)
    pub fn bandwidth_mbps(&self) -> u64 {
        self.pixel_clock_khz() * self.pixel_format.bytes_per_pixel() as u64 / 1000
    }
}

// ============================================================================
// 显示器信息
// ============================================================================

/// 显示器信息
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// 显示器ID
    pub id: u32,
    /// 显示器名称
    pub name: [u8; 32],
    /// 输出类型
    pub output: DisplayOutput,
    /// 是否已连接
    pub connected: bool,
    /// 是否启用
    pub enabled: bool,
    /// 当前显示模式
    pub current_mode: Option<DisplayMode>,
    /// 支持的显示模式列表
    pub supported_modes: Vec<DisplayMode>,
    /// 首选显示模式
    pub preferred_mode: Option<DisplayMode>,
    /// 物理宽度 (mm)
    pub physical_width: u32,
    /// 物理高度 (mm)
    pub physical_height: u32,
    /// 显示器索引
    pub index: usize,
}

impl MonitorInfo {
    pub fn new(id: u32, output: DisplayOutput) -> Self {
        Self {
            id,
            name: [0; 32],
            output,
            connected: false,
            enabled: false,
            current_mode: None,
            supported_modes: Vec::new(),
            preferred_mode: None,
            physical_width: 0,
            physical_height: 0,
            index: 0,
        }
    }

    /// 设置名称
    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(31);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name[len] = 0;
    }

    /// 获取名称
    pub fn get_name(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..end]).unwrap_or("Unknown")
    }
}

// ============================================================================
// 显示控制器 Trait
// ============================================================================

/// 显示控制器接口
pub trait DisplayController: Driver {
    /// 获取输出类型
    fn output_type(&self) -> DisplayOutput;

    /// 检测显示器连接
    /// # Errors
    /// 显示器检测失败时返回 Err。
    fn detect(&mut self) -> Result<bool>;

    /// 获取显示器信息
    fn get_monitor_info(&self) -> Option<&MonitorInfo>;

    /// 获取支持的显示模式
    fn get_supported_modes(&self) -> Vec<DisplayMode>;

    /// 设置显示模式
    /// # Errors
    /// 指定的显示模式不被支持或设置失败时返回 Err。
    fn set_mode(&mut self, mode: DisplayMode) -> Result<()>;

    /// 获取当前显示模式
    fn get_current_mode(&self) -> Option<DisplayMode>;

    /// 获取Framebuffer
    fn get_framebuffer(&mut self) -> Option<&mut Framebuffer>;

    /// 刷新显示
    /// # Errors
    /// 刷新操作失败时返回 Err。
    fn flush(&mut self) -> Result<()>;

    /// 获取显示器索引
    fn monitor_index(&self) -> usize;
}

// ============================================================================
// 显示管理器
// ============================================================================

/// 显示管理器
pub struct DisplayManager {
    /// 显示器列表
    monitors: Vec<MonitorInfo>,
    /// 活动显示器索引
    active_monitor: Option<usize>,
    /// 主显示器索引
    primary_monitor: Option<usize>,
    /// 设备信息
    info: DeviceInfo,
    /// 是否已初始化
    initialized: bool,
}

impl DisplayManager {
    /// 创建新的显示管理器
    pub fn new() -> Self {
        Self {
            monitors: Vec::new(),
            active_monitor: None,
            primary_monitor: None,
            info: DeviceInfo::new("display_manager", DeviceType::Other),
            initialized: false,
        }
    }

    /// 获取设备信息
    pub fn get_info(&self) -> &DeviceInfo {
        &self.info
    }

    /// 注册显示器
    pub fn register_monitor(&mut self, mut monitor: MonitorInfo) -> usize {
        let index = self.monitors.len();
        monitor.index = index;
        self.monitors.push(monitor);

        // 如果是第一个显示器，设为主显示器
        if index == 0 {
            self.primary_monitor = Some(0);
            self.active_monitor = Some(0);
        }

        index
    }

    /// 移除显示器
    /// # Errors
    /// 指定的显示器索引超出范围时返回 Err。
    pub fn remove_monitor(&mut self, index: usize) -> Result<()> {
        if index >= self.monitors.len() {
            return Err(DriverError::InvalidParameter);
        }

        self.monitors.remove(index);

        // 更新索引
        for (i, monitor) in self.monitors.iter_mut().enumerate() {
            monitor.index = i;
        }

        // 更新活动显示器
        if self.active_monitor == Some(index) {
            self.active_monitor = if self.monitors.is_empty() {
                None
            } else {
                Some(0)
            };
        }

        Ok(())
    }

    /// 获取显示器数量
    pub fn monitor_count(&self) -> usize {
        self.monitors.len()
    }

    /// 获取显示器信息
    pub fn get_monitor(&self, index: usize) -> Option<&MonitorInfo> {
        self.monitors.get(index)
    }

    /// 获取显示器信息 (可变)
    pub fn get_monitor_mut(&mut self, index: usize) -> Option<&mut MonitorInfo> {
        self.monitors.get_mut(index)
    }

    /// 设置活动显示器
    /// # Errors
    /// 指定的显示器索引超出范围时返回 Err。
    pub fn set_active_monitor(&mut self, index: usize) -> Result<()> {
        if index >= self.monitors.len() {
            return Err(DriverError::InvalidParameter);
        }

        self.active_monitor = Some(index);
        Ok(())
    }

    /// 获取活动显示器
    pub fn get_active_monitor(&self) -> Option<&MonitorInfo> {
        self.active_monitor.and_then(|i| self.monitors.get(i))
    }

    /// 设置主显示器
    /// # Errors
    /// 指定的显示器索引超出范围时返回 Err。
    pub fn set_primary_monitor(&mut self, index: usize) -> Result<()> {
        if index >= self.monitors.len() {
            return Err(DriverError::InvalidParameter);
        }

        self.primary_monitor = Some(index);
        Ok(())
    }

    /// 获取主显示器
    pub fn get_primary_monitor(&self) -> Option<&MonitorInfo> {
        self.primary_monitor.and_then(|i| self.monitors.get(i))
    }

    /// 检测所有显示器
    pub fn detect_all(&mut self) {
        for monitor in &mut self.monitors {
            // 这里应该调用实际的检测逻辑
            // 简化实现，假设都已连接
            monitor.connected = true;
        }
    }

    /// 获取连接的显示器数量
    pub fn connected_count(&self) -> usize {
        self.monitors.iter().filter(|m| m.connected).count()
    }

    /// 获取启用的显示器数量
    pub fn enabled_count(&self) -> usize {
        self.monitors.iter().filter(|m| m.enabled).count()
    }

    /// 启用显示器
    /// # Errors
    /// 指定的显示器索引超出范围或显示器未连接时返回 Err。
    pub fn enable_monitor(&mut self, index: usize) -> Result<()> {
        let monitor = self
            .monitors
            .get_mut(index)
            .ok_or(DriverError::InvalidParameter)?;

        if !monitor.connected {
            return Err(DriverError::DeviceNotFound);
        }

        monitor.enabled = true;
        Ok(())
    }

    /// 禁用显示器
    /// # Errors
    /// 指定的显示器索引超出范围时返回 Err。
    pub fn disable_monitor(&mut self, index: usize) -> Result<()> {
        let monitor = self
            .monitors
            .get_mut(index)
            .ok_or(DriverError::InvalidParameter)?;

        monitor.enabled = false;
        Ok(())
    }

    /// 设置显示模式
    /// # Errors
    /// 指定的显示器索引超出范围或显示器未连接时返回 Err。
    pub fn set_display_mode(&mut self, index: usize, mode: DisplayMode) -> Result<()> {
        let monitor = self
            .monitors
            .get_mut(index)
            .ok_or(DriverError::InvalidParameter)?;

        if !monitor.connected {
            return Err(DriverError::DeviceNotFound);
        }

        // 检查模式是否支持
        let supported = monitor.supported_modes.iter().any(|m| {
            m.width == mode.width && m.height == mode.height && m.refresh_rate == mode.refresh_rate
        });

        if !supported {
            return Err(DriverError::UnsupportedOperation);
        }

        monitor.current_mode = Some(mode);
        Ok(())
    }

    /// 获取最佳显示模式
    pub fn get_best_mode(&self, index: usize) -> Option<DisplayMode> {
        let monitor = self.monitors.get(index)?;

        // 优先使用首选模式
        if let Some(ref mode) = monitor.preferred_mode {
            return Some(*mode);
        }

        // 否则选择最高分辨率
        monitor
            .supported_modes
            .iter()
            .max_by_key(|m| m.width * m.height)
            .copied()
    }
}

impl Driver for DisplayManager {
    fn name(&self) -> &'static str {
        "Display Manager"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Other
    }

    fn init(&mut self) -> Result<()> {
        self.detect_all();
        self.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.monitors.clear();
        self.active_monitor = None;
        self.primary_monitor = None;
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "Display Manager ready"
        } else {
            "Display Manager not initialized"
        }
    }
}

impl Default for DisplayManager {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_mode_creation() {
        let mode = DisplayMode::new(1920, 1080, 60, PixelFormat::Argb8888);
        assert_eq!(mode.width, 1920);
        assert_eq!(mode.height, 1080);
        assert_eq!(mode.refresh_rate, 60);
    }

    #[test]
    fn test_display_mode_bandwidth() {
        let mode = DisplayMode::new(1920, 1080, 60, PixelFormat::Argb8888);
        let bw = mode.bandwidth_mbps();
        assert!(bw > 0);
    }

    #[test]
    fn test_monitor_info() {
        let mut monitor = MonitorInfo::new(1, DisplayOutput::Hdmi);
        monitor.set_name("Test Monitor");

        assert_eq!(monitor.get_name(), "Test Monitor");
        assert_eq!(monitor.output, DisplayOutput::Hdmi);
        assert!(!monitor.connected);
    }

    #[test]
    fn test_display_manager() {
        let mut manager = DisplayManager::new();

        let monitor = MonitorInfo::new(1, DisplayOutput::Hdmi);
        let index = manager.register_monitor(monitor);

        assert_eq!(index, 0);
        assert_eq!(manager.monitor_count(), 1);
        assert_eq!(manager.primary_monitor, Some(0));
    }

    #[test]
    fn test_display_manager_enable() {
        let mut manager = DisplayManager::new();

        let mut monitor = MonitorInfo::new(1, DisplayOutput::Hdmi);
        monitor.connected = true;
        manager.register_monitor(monitor);

        let result = manager.enable_monitor(0);
        assert!(result.is_ok());

        let monitor = manager.get_monitor(0).unwrap();
        assert!(monitor.enabled);
    }
}
