//! pidfd fd 对象的内核测试用例 (对象级).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级用例;
//! syscall 级闭环 (fd 分配 / dup / close / 引用钉住 / `send_signal`) 归属
//! `functions/proc/pidfd.rs` 的 host 侧 `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::functions::proc::pidfd::PidFdFile;
use crate::privileged::tests::{TestResult, check, runner};

fn test_pidfd_file_contract() -> TestResult {
    let file = PidFdFile::new(1234);
    check!(file.pid() == 1234, "目标 pid 原样保存");
    check!(file.stream(), "pidfd 不可定位 (ESPIPE)");
    check!(file.as_inode().is_none(), "pidfd 非 inode 承载");
    check!(file.mmap_backing().is_none(), "pidfd 不可作 mmap 后端");
    check!(
        file.inode_namespace_id().is_none(),
        "pidfd 不属于 inode 命名空间"
    );
    check!(
        file.file_type() == VfsFileType::File,
        "file_type 呈现为 File"
    );
    check!(
        file.io_control(0x1234_5678, 0) == Err(KernelError::NotSupported),
        "pidfd 对象不处理通用 ioctl"
    );
    TestResult::Pass
}

fn test_pidfd_read_write_errors() -> TestResult {
    let file = PidFdFile::new(42);
    check!(
        file.read_at(0, &mut [0u8; 8], 0) == Err(KernelError::InvalidArgument),
        "pidfd 无数据面 (read → EINVAL)"
    );
    check!(
        file.write_at(0, &[0u8; 8], 0) == Err(KernelError::InvalidArgument),
        "pidfd 无数据面 (write → EINVAL)"
    );
    // 进程表尚未初始化: 目标不存在 ⇒ 防御性判据视为已退出 (可读).
    check!(
        file.poll(PollEvents::IN) == PollEvents::IN,
        "目标不存在时 pidfd 报告可读"
    );
    TestResult::Pass
}

fn test_pidfd_downcast() -> TestResult {
    let file = PidFdFile::new(7);
    let erased: &dyn File = &file;
    let recovered = erased
        .as_any()
        .downcast_ref::<PidFdFile>()
        .expect("PidFdFile 必须可下转型");
    check!(recovered.pid() == 7, "下转型后目标 pid 一致");
    TestResult::Pass
}

/// 注册 pidfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_pidfd_tests() {
    let r = runner();
    r.register("pidfd", "file_contract", test_pidfd_file_contract);
    r.register("pidfd", "read_write_errors", test_pidfd_read_write_errors);
    r.register("pidfd", "downcast", test_pidfd_downcast);
}
