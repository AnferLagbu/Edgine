//! debug 模块公共 re-export

pub use super::ftrace::{
    EVENT_SIZE, FTRACE, FTRACE_BUF_CAP, FtraceState, MAX_TRACE_POINTS, TraceEvent, fnv1a_32,
};
pub use super::kgdb::{
    KgdbRegs, KgdbSerial, kgdb_active, kgdb_breakpoint, kgdb_handle_exception, kgdb_loop,
    kgdb_serial_ready, kgdb_set_serial, kgdb_try_getc, kgdb_write_str,
};
pub use super::ringbuf::{DEFAULT_RING_CAPACITY, RingBuffer};

/// 初始化 debug 子系统
pub fn debug_init() {
    super::ftrace::ftrace_init();
}

/// 启用 ftrace 全局开关
pub fn ftrace_enable() {
    FTRACE.enable();
}

/// 禁用 ftrace 全局开关
pub fn ftrace_disable() {
    FTRACE.disable();
}

/// 查询 ftrace 启用状态
pub fn ftrace_is_enabled() -> bool {
    FTRACE.is_enabled()
}

/// 累计事件计数
pub fn ftrace_event_count() -> u64 {
    FTRACE.event_count()
}

/// 累计溢出计数
pub fn ftrace_overflow_count() -> u64 {
    FTRACE.overflow_count()
}

/// 弹出一条事件 (None = 空)
pub fn ftrace_pop_event() -> Option<TraceEvent> {
    FTRACE.pop()
}

/// 注册一个跟踪点 (按 name hash 查重, 返回是否成功)
pub fn ftrace_register_point(name_hash: u32) -> bool {
    FTRACE.register_point(name_hash)
}

/// KGDB 主动断点入口 (panic 路径调用)
pub fn kgdb_break_now() {
    let mut regs = KgdbRegs::default();
    kgdb_breakpoint(&mut regs);
}

/// 请求虚拟机以给定状态退出 (ISA-debug-exit), 随后停机永不返回.
///
/// x86_64: 向 `0xf4` 端口写入 `0x10` (成功) / `0x11` (失败), QEMU 以
/// `(exit_code << 1) | 1` 退出; 非 x86_64 目标忽略 `success` 并进入停机循环.
///
/// 原属 `framework::tests`; B4 内存过配治理将内核测试框架移出 release 后,
/// 本函数迁至 `debug` 作为启动失败路径与测试运行器共用的终止原语.
pub fn qemu_exit(success: bool) -> ! {
    #[cfg(target_arch = "x86_64")]
    {
        let exit_code = if success { 0x10 } else { 0x11 };
        // SAFETY: 端口 0xf4 为 QEMU isa-debug-exit 设备端口; 写字节不访问内存,
        // `options(nomem, nostack)` 与之一致; ring 0 上下文执行 `out` 合法.
        unsafe {
            use core::arch::asm;
            asm!(
                "out dx, al",
                in("dx") 0xf4u16,
                in("al") exit_code as u8,
                options(nomem, nostack)
            );
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = success;
    }
    loop {
        crate::arch!(halt());
    }
}
