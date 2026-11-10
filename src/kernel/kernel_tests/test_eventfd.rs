//! eventfd fd 对象的内核测试用例 (对象级).
//!
//! `kernel_test` 运行器在进程表初始化前执行, 故此处只放对象级用例; syscall 级
//! 闭环 (fd 分配 / dup / close) 归属 `functions/fs/eventfd.rs` 的 host 侧
//! `#[cfg(test)]`, 不进入本载体.

use crate::functions::fs::eventfd::EventFdFile;
use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::vfs_types::KernelError;
use crate::privileged::tests::{TestResult, check, runner};

fn test_eventfd_file_read_write() -> TestResult {
    let file = EventFdFile::new(5, false);
    let mut buf = [0u8; 8];
    check!(
        matches!(file.read_at(0, &mut buf, 0), Ok(8)),
        "eventfd 读返回 8 字节"
    );
    check!(u64::from_ne_bytes(buf) == 5, "eventfd 读值为 5");
    check!(
        matches!(file.read_at(0, &mut buf, 0), Err(KernelError::WouldBlock)),
        "计数清零后再读返回 EAGAIN"
    );

    let val = 10u64.to_ne_bytes();
    check!(
        matches!(file.write_at(0, &val, 0), Ok(8)),
        "eventfd 写返回 8 字节"
    );
    check!(
        matches!(file.read_at(0, &mut buf, 0), Ok(8)) && u64::from_ne_bytes(buf) == 10,
        "写入 10 后读回 10"
    );
    TestResult::Pass
}

fn test_eventfd_file_semaphore() -> TestResult {
    let file = EventFdFile::new(3, true);
    let mut buf = [0u8; 8];
    for expected in [1u64, 1, 1] {
        check!(
            matches!(file.read_at(0, &mut buf, 0), Ok(8)),
            "信号量模式读返回 8 字节"
        );
        check!(
            u64::from_ne_bytes(buf) == expected,
            "信号量模式每次读返回 1"
        );
    }
    check!(
        matches!(file.read_at(0, &mut buf, 0), Err(KernelError::WouldBlock)),
        "信号量计数耗尽后返回 EAGAIN"
    );
    TestResult::Pass
}

fn test_eventfd_file_poll() -> TestResult {
    let file = EventFdFile::new(0, false);
    check!(file.poll(PollEvents::IN).is_empty(), "计数为 0 时不可读");
    check!(
        file.poll(PollEvents::OUT) == PollEvents::OUT,
        "计数为 0 时可写"
    );
    let val = 1u64.to_ne_bytes();
    check!(matches!(file.write_at(0, &val, 0), Ok(8)), "写入 1");
    check!(
        file.poll(PollEvents::IN | PollEvents::OUT) == (PollEvents::IN | PollEvents::OUT),
        "计数 > 0 时可读可写"
    );
    TestResult::Pass
}

/// 注册 eventfd 对象级内核测试用例 (glue 侧 `register_upper_tests` 调用).
pub fn register_eventfd_tests() {
    let r = runner();
    r.register("eventfd", "file_read_write", test_eventfd_file_read_write);
    r.register("eventfd", "file_semaphore", test_eventfd_file_semaphore);
    r.register("eventfd", "file_poll", test_eventfd_file_poll);
}
