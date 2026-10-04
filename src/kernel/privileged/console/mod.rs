pub mod gfx_console;

use core::sync::atomic::{AtomicPtr, Ordering};
use gfx_console::GfxConsole;

// B04-08: 保留 AtomicPtr 但把 unsafe 解引用集中到单一 with_console 闭包 helper,
// 调用方仅看到 safe API. 闭包保证 null 检查 + 解引用在一处, 避免遗漏悬挂风险.
//
// # 并发约束 (B04-AUDIT-005 #5)
//
// gfx_console 的 3 个公共 API (write/panic_reclaim/panic_write) **当前仅供 BSP
// 单线程 boot 阶段调用**. 多核并发场景下 `with_console` 闭包会同时拿到两个
// `&mut GfxConsole` 引用 → Rust 别名规则 UB. 后续多核 gfx 输出需扩展:
//   1. `GfxConsole` 内部嵌入 `IrqSpinLock<GfxConsole>` 或 `Mutex<GfxConsole>`;
//   2. `with_console` 闭包改为 `&mut IrqSpinLockGuard<GfxConsole>` 参数;
//   3. 删除 `static GFX_CONSOLE_PTR` (锁自管, 不需要 AtomicPtr 间接寻址).
// 当前实现属于"显式单核约束"路径: 调用方契约 + 文档声明, 编译期通过
// `#[cfg(not(target_arch = "smp"))]` 隔离 (若启用 SMP 配置必须重构).
static GFX_CONSOLE_PTR: AtomicPtr<GfxConsole> = AtomicPtr::new(core::ptr::null_mut());

/// 初始化图形控制台 —— 绑定到已分配在静态存储中的 `GfxConsole`
///
/// # Safety
///
/// - `console` 必须来自静态存储（`Box::leak` 出品）
/// - 仅调用一次
/// - **必须在 BSP 单线程阶段调用** (AP 未启动). SMP 启动后调用需重新设计见上方并发约束.
#[expect(
    clippy::ref_as_ptr,
    reason = "console 为 &'static mut 引用, 需取其裸指针存入 AtomicPtr"
)]
pub fn gfx_console_init(console: &'static mut GfxConsole) {
    GFX_CONSOLE_PTR.store(console as *mut GfxConsole, Ordering::Release);
}

/// 安全访问 GfxConsole (闭包形式).
///
/// 集中处理 null 指针 + 解引用 unsafe, 避免每个调用方各自处理.
///
/// # 单核约束
///
/// 当前闭包在多核并发下会触发 `&mut` 别名 UB. 限制为 BSP 单线程阶段调用.
/// 详见模块顶部并发约束注释.
fn with_console<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut GfxConsole) -> R,
{
    let ptr = GFX_CONSOLE_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: 初始化时 gfx_console_init 保证指针来自 `Box::leak` 静态存储;
    //          一旦设置不再重置; 闭包 f 持有唯一借用, **当前仅在 BSP 单线程
    //          boot 阶段调用** (klog_output 串行化 + panic_reclaim 单线程 panic
    //          handler), 不与其它访问并发. SMP 启用后必须重构 (见模块顶部).
    let console = unsafe { &mut *ptr };
    Some(f(console))
}

/// 将内核日志同步到图形控制台（在 `klog_output` 中调用）
///
/// **单核约束**: 仅 BSP 单线程 boot 阶段调用. SMP 启用后需重构.
pub fn gfx_console_write(msg: &[u8]) {
    if let Ok(s) = core::str::from_utf8(msg) {
        let _ = with_console(|c| c.write_str(s));
    }
}

/// Panic 发生时接管图形控制台 — 绘制崩溃横幅并输出消息
///
/// 此函数在 panic handler 中调用，无论是否初始化都尝试接管。
/// 优先使用已存在的 `GfxConsole` 实例，如果没有则什么也不做（至少串口已输出）。
///
/// **单核约束**: panic handler 串行触发, 不与其它 with_console 调用并发.
pub fn gfx_console_panic_reclaim(msg: &str) {
    let _ = with_console(|c| c.panic_reclaim(msg));
}

/// Panic 模式下向图形控制台追加崩溃详情
///
/// 在 `gfx_console_panic_reclaim` 之后再调用此函数输出寄存器转储等信息。
///
/// **单核约束**: 见 `gfx_console_panic_reclaim`.
pub fn gfx_console_panic_write(msg: &str) {
    let _ = with_console(|c| c.panic_write(msg));
}
