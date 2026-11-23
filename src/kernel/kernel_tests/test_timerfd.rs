//! timerfd fd 对象的内核测试用例 (对象级 + per-context 机制).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级与 context
//! 机制用例; syscall 级闭环 (fd 分配 / dup / close / settime) 归属
//! `functions/fs/timerfd.rs` 的 host 侧 `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::timerfd::{TFD_EXPIRY_SIZE, TimerFdContext, TimerFdFile};
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::privileged::syscall::TakeExpiry;
use crate::privileged::tests::{TestResult, check, runner};

fn test_timerfd_file_contract() -> TestResult {
    let file = TimerFdFile::new(TimerFdContext::create());
    check!(file.stream(), "timerfd 不可定位 (ESPIPE)");
    check!(file.as_inode().is_none(), "timerfd 非 inode 承载");
    check!(file.mmap_backing().is_none(), "timerfd 不可作 mmap 后端");
    check!(
        file.inode_namespace_id().is_none(),
        "timerfd 不属于 inode 命名空间"
    );
    check!(
        file.file_type() == VfsFileType::File,
        "file_type 呈现为 File"
    );
    check!(
        file.io_control(0x1234_5678, 0) == Err(KernelError::NotSupported),
        "timerfd 对象不处理通用 ioctl"
    );
    check!(
        file.write_at(0, &[0u8; TFD_EXPIRY_SIZE], 0) == Err(KernelError::InvalidArgument),
        "timerfd fd 只读 (写 → EINVAL)"
    );
    TestResult::Pass
}

fn test_timerfd_read_errors() -> TestResult {
    let file = TimerFdFile::new(TimerFdContext::create());
    let mut buf = [0u8; TFD_EXPIRY_SIZE];
    check!(
        file.read_at(0, &mut [0u8; TFD_EXPIRY_SIZE - 1], 0) == Err(KernelError::InvalidArgument),
        "短缓冲读 → EINVAL"
    );
    check!(
        file.read_at(0, &mut buf, 0) == Err(KernelError::WouldBlock),
        "无 tick 读 → EAGAIN"
    );
    check!(file.poll(PollEvents::IN).is_empty(), "无 tick poll 不就绪");
    TestResult::Pass
}

fn test_timerfd_downcast() -> TestResult {
    let file = TimerFdFile::new(TimerFdContext::create());
    let erased: &dyn File = &file;
    check!(
        erased.as_any().downcast_ref::<TimerFdFile>().is_some(),
        "TimerFdFile 必须可下转型"
    );
    TestResult::Pass
}

fn test_timerfd_context_lifecycle() -> TestResult {
    let ctx = TimerFdContext::create();
    check!(!ctx.poll_ready(), "新 context 无到期事件");
    check!(ctx.get_time() == (0, 0), "新 context 读数全零");
    check!(
        matches!(ctx.set_time(1_000_000_000, 5_000_000, false), Ok((0, 0))),
        "首次设时返回旧值全零"
    );
    check!(ctx.get_time().0 == 5_000_000, "周期值应可读回");
    let old = ctx.set_time(0, 0, false);
    check!(matches!(old, Ok((5_000_000, _))), "disarm 返回旧周期");
    check!(
        matches!(ctx.take_expiry(), TakeExpiry::Empty),
        "未到期 context 无到期计数"
    );
    ctx.release();
    check!(!ctx.poll_ready(), "release 后无到期事件");
    check!(ctx.get_time() == (0, 0), "release 后读数归零");
    TestResult::Pass
}

/// 注册 timerfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_timerfd_tests() {
    let r = runner();
    r.register("timerfd", "file_contract", test_timerfd_file_contract);
    r.register("timerfd", "read_errors", test_timerfd_read_errors);
    r.register("timerfd", "downcast", test_timerfd_downcast);
    r.register(
        "timerfd",
        "context_lifecycle",
        test_timerfd_context_lifecycle,
    );
}
