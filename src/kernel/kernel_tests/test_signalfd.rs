//! signalfd fd 对象的内核测试用例 (对象级).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级用例; syscall 级
//! 闭环 (fd 分配 / 改掩码 / close) 归属 `functions/fs/signalfd.rs` 的 host 侧
//! `#[cfg(test)]`, 不进入本载体. 掩码不可屏蔽位过滤的观测点 (对象私有状态)
//! 同样在 host 侧单测.

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::signalfd::{SFD_IOC_SETMASK, SIGNALFD_SIGINFO_SIZE, SignalFdFile};
use crate::functions::fs::vfs_types::{KernelError, VfsFileType};
use crate::privileged::tests::{TestResult, check, runner};

fn test_signalfd_file_contract() -> TestResult {
    let file = SignalFdFile::new(0, 0);
    check!(file.stream(), "signalfd 不可定位 (lseek → ESPIPE)");
    check!(file.as_inode().is_none(), "signalfd 非 inode 承载");
    check!(
        file.file_type() == VfsFileType::File,
        "signalfd 文件类型为常规文件"
    );
    check!(
        matches!(
            file.write_at(0, &[0u8; SIGNALFD_SIGINFO_SIZE], 0),
            Err(KernelError::InvalidArgument)
        ),
        "signalfd 只读, 写入返回 EINVAL"
    );
    check!(
        matches!(
            file.read_at(0, &mut [0u8; 64], 0),
            Err(KernelError::InvalidArgument)
        ),
        "短缓冲读返回 EINVAL"
    );
    check!(
        matches!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported)
        ),
        "未知控制命令返回 EOPNOTSUPP"
    );
    TestResult::Pass
}

fn test_signalfd_file_io_control_setmask() -> TestResult {
    let file = SignalFdFile::new(0, 0);
    let mask = (1u64 << 9) | (1u64 << 8) | (1u64 << 18);
    check!(
        matches!(
            file.io_control(
                SFD_IOC_SETMASK,
                crate::functions::intcast::usize_from_u64(mask)
            ),
            Ok(0)
        ),
        "SFD_IOC_SETMASK 更新掩码返回 0"
    );
    // 无进程 pending 可消费: 读/轮询保持 EAGAIN / 空就绪
    check!(
        matches!(
            file.read_at(0, &mut [0u8; SIGNALFD_SIGINFO_SIZE], 0),
            Err(KernelError::InvalidArgument | KernelError::WouldBlock)
        ),
        "无待处理信号时读不返回数据"
    );
    check!(file.poll(PollEvents::IN).is_empty(), "无待处理信号时不可读");
    TestResult::Pass
}

/// 注册 signalfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_signalfd_tests() {
    let r = runner();
    r.register("signalfd", "file_contract", test_signalfd_file_contract);
    r.register(
        "signalfd",
        "file_io_control_setmask",
        test_signalfd_file_io_control_setmask,
    );
}
