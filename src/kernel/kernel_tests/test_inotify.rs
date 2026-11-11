//! inotify fd 对象的内核测试用例 (对象级).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级用例; syscall 级
//! 闭环 (fd 分配 / dup / close / watch) 归属 `functions/fs/inotify.rs` 的 host 侧
//! `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::inotify::{InotifyEvent, InotifyFile};
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::privileged::tests::{TestResult, check, runner};

fn test_inotify_file_contract() -> TestResult {
    // 槽位 0 在启动态无效 (本载体不创建实例): 此处只验证对象契约面
    let file = InotifyFile::new(0);
    check!(file.idx() == 0, "槽位下标原样保存");
    check!(file.stream(), "inotify 不可定位 (ESPIPE)");
    check!(file.as_inode().is_none(), "inotify 非 inode 承载");
    check!(file.mmap_backing().is_none(), "inotify 不可作 mmap 后端");
    check!(
        file.inode_namespace_id().is_none(),
        "inotify 不属于 inode 命名空间"
    );
    check!(
        file.file_type() == VfsFileType::File,
        "file_type 呈现为 File"
    );
    check!(
        file.io_control(0x1234_5678, 0) == Err(KernelError::NotSupported),
        "inotify 对象不处理通用 ioctl"
    );
    check!(
        file.write_at(0, &[0u8; InotifyEvent::FULL_SIZE], 0) == Err(KernelError::InvalidArgument),
        "inotify fd 只读 (写 → EINVAL)"
    );
    TestResult::Pass
}

fn test_inotify_read_errors() -> TestResult {
    let file = InotifyFile::new(0);
    let mut buf = [0u8; InotifyEvent::FULL_SIZE];
    check!(
        file.read_at(0, &mut [0u8; InotifyEvent::FIXED_SIZE], 0)
            == Err(KernelError::InvalidArgument),
        "短缓冲读 → EINVAL"
    );
    check!(
        file.read_at(0, &mut buf, 0) == Err(KernelError::BadFd),
        "无效槽位读 → EBADF"
    );
    check!(file.poll(PollEvents::IN).is_empty(), "无效槽位 poll 不就绪");
    TestResult::Pass
}

fn test_inotify_downcast() -> TestResult {
    let file = InotifyFile::new(7);
    let erased: &dyn File = &file;
    let recovered = erased
        .as_any()
        .downcast_ref::<InotifyFile>()
        .expect("InotifyFile 必须可下转型");
    check!(recovered.idx() == 7, "下转型后槽位下标一致");
    TestResult::Pass
}

/// 注册 inotify 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_inotify_tests() {
    let r = runner();
    r.register("inotify", "file_contract", test_inotify_file_contract);
    r.register("inotify", "read_errors", test_inotify_read_errors);
    r.register("inotify", "downcast", test_inotify_downcast);
}
