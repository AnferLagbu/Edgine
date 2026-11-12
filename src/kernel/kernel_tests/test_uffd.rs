//! userfaultfd fd 对象的内核测试用例 (对象级).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级用例;
//! syscall 级闭环 (fd 分配 / dup / close / 非阻塞读) 归属
//! `functions/fs/uffd.rs` 的 host 侧 `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::uffd::UserFaultFdFile;
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::privileged::mm::uffd::UFFD_MSG_SIZE;
use crate::privileged::tests::{TestResult, check, runner};

fn test_uffd_file_contract() -> TestResult {
    // 槽位 15 在启动态无效 (本载体不创建实例): 此处只验证对象契约面
    let file = UserFaultFdFile::new(15);
    check!(file.idx() == 15, "槽位下标原样保存");
    check!(file.stream(), "uffd 不可定位 (ESPIPE)");
    check!(file.as_inode().is_none(), "uffd 非 inode 承载");
    check!(file.mmap_backing().is_none(), "uffd 不可作 mmap 后端");
    check!(
        file.inode_namespace_id().is_none(),
        "uffd 不属于 inode 命名空间"
    );
    check!(
        file.file_type() == VfsFileType::File,
        "file_type 呈现为 File"
    );
    check!(
        file.io_control(0x1234_5678, 0) == Err(KernelError::Other(25)),
        "未知 uffd ioctl → ENOTTY"
    );
    TestResult::Pass
}

fn test_uffd_read_errors() -> TestResult {
    let file = UserFaultFdFile::new(15);
    check!(
        file.read_at(0, &mut [0u8; UFFD_MSG_SIZE - 1], 0) == Err(KernelError::InvalidArgument),
        "短缓冲读 → EINVAL"
    );
    check!(
        file.read_at(0, &mut [0u8; UFFD_MSG_SIZE], 0) == Err(KernelError::BadFd),
        "无效槽位读 → EBADF"
    );
    check!(file.poll(PollEvents::IN).is_empty(), "无效槽位 poll 不就绪");
    check!(
        file.write_at(0, &[0u8; UFFD_MSG_SIZE], 0) == Err(KernelError::InvalidArgument),
        "uffd fd 只读 (写 → EINVAL)"
    );
    TestResult::Pass
}

fn test_uffd_downcast() -> TestResult {
    let file = UserFaultFdFile::new(7);
    let erased: &dyn File = &file;
    let recovered = erased
        .as_any()
        .downcast_ref::<UserFaultFdFile>()
        .expect("UserFaultFdFile 必须可下转型");
    check!(recovered.idx() == 7, "下转型后槽位下标一致");
    TestResult::Pass
}

/// 注册 userfaultfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_uffd_tests() {
    let r = runner();
    r.register("uffd", "file_contract", test_uffd_file_contract);
    r.register("uffd", "read_errors", test_uffd_read_errors);
    r.register("uffd", "downcast", test_uffd_downcast);
}
