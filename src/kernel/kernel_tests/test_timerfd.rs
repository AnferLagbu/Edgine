//! timerfd fd 对象的内核测试用例 (对象级 + 槽位机制).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级与槽位机制用例;
//! syscall 级闭环 (fd 分配 / dup / close / settime) 归属
//! `functions/fs/timerfd.rs` 的 host 侧 `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::timerfd::{TFD_EXPIRY_SIZE, TimerFdFile};
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::privileged::syscall::timerfd::{
    TakeExpiry, alloc_slot, free_slot, get_time, poll_ready, set_time, take_expiry,
};
use crate::privileged::tests::{TestResult, check, runner};

fn test_timerfd_file_contract() -> TestResult {
    // 槽位 15 在启动态无效 (本载体不创建实例): 此处只验证对象契约面
    let file = TimerFdFile::new(15);
    check!(file.idx() == 15, "槽位下标原样保存");
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
    let file = TimerFdFile::new(15);
    let mut buf = [0u8; TFD_EXPIRY_SIZE];
    check!(
        file.read_at(0, &mut [0u8; TFD_EXPIRY_SIZE - 1], 0) == Err(KernelError::InvalidArgument),
        "短缓冲读 → EINVAL"
    );
    check!(
        file.read_at(0, &mut buf, 0) == Err(KernelError::BadFd),
        "无效槽位读 → EBADF"
    );
    check!(file.poll(PollEvents::IN).is_empty(), "无效槽位 poll 不就绪");
    TestResult::Pass
}

fn test_timerfd_downcast() -> TestResult {
    let file = TimerFdFile::new(7);
    let erased: &dyn File = &file;
    let recovered = erased
        .as_any()
        .downcast_ref::<TimerFdFile>()
        .expect("TimerFdFile 必须可下转型");
    check!(recovered.idx() == 7, "下转型后槽位下标一致");
    TestResult::Pass
}

fn test_timerfd_slot_lifecycle() -> TestResult {
    let idx = alloc_slot().expect("分配槽位");
    check!(poll_ready(idx) == Some(false), "新槽位无到期事件");
    check!(get_time(idx) == Some((0, 0)), "新槽位读数全零");
    check!(
        matches!(set_time(idx, 1_000_000_000, 5_000_000, false), Some((0, 0))),
        "首次设时返回旧值全零"
    );
    check!(
        get_time(idx).is_some_and(|(interval, _)| interval == 5_000_000),
        "周期值应可读回"
    );
    let old = set_time(idx, 0, 0, false);
    check!(matches!(old, Some((5_000_000, _))), "disarm 返回旧周期");
    check!(
        matches!(take_expiry(idx), TakeExpiry::Empty),
        "未到期槽位无到期计数"
    );
    free_slot(idx);
    check!(poll_ready(idx).is_none(), "释放后槽位无效");
    check!(get_time(idx).is_none(), "释放后读数不可用");
    TestResult::Pass
}

/// 注册 timerfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_timerfd_tests() {
    let r = runner();
    r.register("timerfd", "file_contract", test_timerfd_file_contract);
    r.register("timerfd", "read_errors", test_timerfd_read_errors);
    r.register("timerfd", "downcast", test_timerfd_downcast);
    r.register("timerfd", "slot_lifecycle", test_timerfd_slot_lifecycle);
}
