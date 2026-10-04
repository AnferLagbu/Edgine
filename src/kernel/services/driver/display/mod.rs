#![deny(unsafe_code)]
//! 显示子系统 — services 层安全实现
//!
//! 提供 HDMI/DisplayPort 驱动的 safe 业务逻辑:
//! - DDC I2C bitbang 协议 (通过 IoMem 安全代理)
//! - EDID 解析 (纯数据)
//! - 视频模式管理与时序参数派生
//! - 像素时钟 PLL 配置
//!
//! 所有硬件寄存器访问通过 framework 的 IoMem 安全接口, 0 unsafe.

/// DDC (Display Data Channel) I2C bitbang 协议
pub mod ddc;

/// HDMI 控制器驱动
pub mod hdmi;

/// DisplayPort 控制器驱动 (从 framework 迁移, 0 unsafe)
pub mod dp;

/// 显示控制器管理策略 (从 framework 迁移, 0 unsafe)
pub mod controller;

// 重新导出 DisplayPort 公共类型
pub use dp::{
    AuxCommand, AuxTransaction, DpController, DpError, DpIo, Dpcd, LaneCount, LinkRate,
    REQUIRED_IOMEM_SIZE, TrainingState, assert_iomem_size_at_least,
};

/// 初始化显示控制器子系统并注册到 EGDF (MIG-008 接线补齐, DECISION-K 契约)
///
/// services 层权威实现: 本函数仅向 framework 注册"工厂回调" (经
/// [`crate::framework::driver::register_display_controller_factory`]),
/// 实际注册动作由 crate root lib.rs 调用
/// `framework::driver::display_probe_controllers()` 单向拉取触发
/// (见 DECISION-K 单向注册契约)。frame buffer 初始化仍由
/// [`crate::framework::driver::display_init`] 承担。
///
/// SIMPLIFIED: QEMU x86_64 无真实 HDMI/DP 硬件, framework 无 HDMI/DP 探测路径,
/// 此处以 fallback 构造 (`HdmiController::new` / `DpController::new`) 注册;
/// 真实硬件 MMIO 接入 (厂商 PHY/DPLL 差异, Intel/AMD/Synopsys) 待按 DECISION-K
/// 在 services 侧重立后再补探测路径.
pub fn display_init() {
    let _ = crate::framework::driver::register_display_controller_factory(enroll_controllers);
}

/// 显示控制器工厂回调 (无捕获函数指针, DECISION-K)
///
/// 由 framework [`crate::framework::driver::display_probe_controllers`]
/// 单向拉取时执行, 在 services 侧构造并注册显示设备 (0 unsafe)。
fn enroll_controllers() {
    use crate::framework::egdf::{EGDFProto, egdf_register_driver};
    use alloc::boxed::Box;
    use controller::DisplayManager;
    use dp::DpController;
    use hdmi::HdmiController;

    egdf_register_driver(
        "hdmi0",
        EGDFProto::Other,
        None,
        None,
        Box::new(HdmiController::new()),
    );
    egdf_register_driver(
        "dp0",
        EGDFProto::Other,
        None,
        None,
        Box::new(DpController::new(0)),
    );
    egdf_register_driver(
        "display-manager",
        EGDFProto::Other,
        None,
        None,
        Box::new(DisplayManager::new()),
    );
}
