//! 匿名管道纳入 VFS fd 对象模型的回归测试.
//!
//! 覆盖两个层面:
//!
//! 1. **端点 key 策略运行时语义** (`functions::ipc::pipe`): 创建 / 读写往返 /
//!    方向错误 / 无写者 EOF / 无读者 EPIPE / 缓冲区满 / 槽位回收与耗尽.
//! 2. **fd 对象模型接入契约** (静态扫描): `pipe_fd.rs` 端点 Inode 与 Drop 释放、
//!    `pipe()`/`pipe2()` 路由、内核缓冲 I/O、fork 继承 / dup / CLOEXEC 复用.

use std::fs;
use std::path::Path;

use edgine::kernel::functions::ipc::pipe::{
    pipe_close_safe, pipe_create_safe, pipe_find_by_key_index, pipe_read_safe, pipe_write_safe,
};
use edgine::kernel::privileged::ipc::types::{IPC_MAX_PIPES, IpcNamespace, PIPE_BUFFER_SIZE};

/// 构造带全部槽位分配的独立命名空间 (不触碰全局 `IPC_NAMESPACE`).
fn new_namespace() -> IpcNamespace {
    let mut ns = IpcNamespace::empty();
    ns.allocate();
    ns
}

fn read_src(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

// ============================================================================
// 端点 key 策略运行时语义
// ============================================================================

#[test]
fn pipe_create_returns_distinct_keys() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 7).expect("创建管道");

    assert_ne!(rk, wk, "读端 key 与写端 key 必须不同");
    assert!(rk != 0 && wk != 0, "端点 key 不得为 0 (0 为已关闭哨兵)");
    // 同一管道的两个 key 必须解析到同一槽位
    assert_eq!(
        pipe_find_by_key_index(&ns, rk),
        pipe_find_by_key_index(&ns, wk),
        "读写端点 key 应指向同一管道槽位"
    );
    assert_eq!(
        pipe_find_by_key_index(&ns, 0),
        None,
        "key=0 不得匹配任何槽位"
    );
}

#[test]
fn pipe_write_then_read_roundtrip() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");

    let payload = b"edgine-pipe";
    assert_eq!(
        pipe_write_safe(&mut ns, wk, payload, payload.len() as u32),
        Ok(payload.len() as u32),
        "写入应返回完整字节数"
    );

    let mut buf = [0u8; 16];
    assert_eq!(
        pipe_read_safe(&mut ns, rk, &mut buf, payload.len() as u32),
        Ok(payload.len() as u32),
        "读取应返回写入的字节数"
    );
    assert_eq!(&buf[..payload.len()], payload, "往返内容必须一致");

    // 写端仍打开且缓冲区空 ⇒ 非阻塞读返回 WouldBlock (-4)
    assert_eq!(
        pipe_read_safe(&mut ns, rk, &mut buf, 1),
        Err(-4),
        "空管道且有写者时应返回 EAGAIN"
    );
}

#[test]
fn pipe_rejects_wrong_direction() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");

    let mut buf = [0u8; 4];
    assert_eq!(
        pipe_read_safe(&mut ns, wk, &mut buf, 4),
        Err(-2),
        "对写端 key 读取应返回方向错误"
    );
    assert_eq!(
        pipe_write_safe(&mut ns, rk, b"x", 1),
        Err(-2),
        "对读端 key 写入应返回方向错误"
    );
}

#[test]
fn pipe_read_eof_after_writer_close() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");

    assert_eq!(pipe_close_safe(&mut ns, wk), Ok(()), "关闭写端");
    let mut buf = [0u8; 4];
    assert_eq!(
        pipe_read_safe(&mut ns, rk, &mut buf, 4),
        Ok(0),
        "写者全部关闭后读端应观测 EOF"
    );
}

#[test]
fn pipe_write_epipe_after_reader_close() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");

    assert_eq!(pipe_close_safe(&mut ns, rk), Ok(()), "关闭读端");
    assert_eq!(
        pipe_write_safe(&mut ns, wk, b"x", 1),
        Err(-3),
        "读者全部关闭后写入应返回 EPIPE"
    );
}

#[test]
fn pipe_full_buffer_rejects_or_partially_writes() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");

    let fill = vec![0xABu8; PIPE_BUFFER_SIZE];
    assert_eq!(
        pipe_write_safe(&mut ns, wk, &fill, PIPE_BUFFER_SIZE as u32),
        Ok(PIPE_BUFFER_SIZE as u32),
        "恰好填满缓冲区应成功"
    );
    assert_eq!(
        pipe_write_safe(&mut ns, wk, b"z", 1),
        Err(-4),
        "缓冲区已满且一个字节都写不下时应返回 EAGAIN"
    );

    let mut drained = vec![0u8; PIPE_BUFFER_SIZE];
    assert_eq!(
        pipe_read_safe(&mut ns, rk, &mut drained, PIPE_BUFFER_SIZE as u32),
        Ok(PIPE_BUFFER_SIZE as u32),
        "应能读回全部已写数据"
    );
    assert!(drained.iter().all(|&b| b == 0xAB), "读回内容必须一致");
}

#[test]
fn pipe_close_both_reclaims_slot_and_keys() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    let (rk, wk) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("创建管道");
    let idx = pipe_find_by_key_index(&ns, rk).expect("槽位存在");

    assert_eq!(pipe_close_safe(&mut ns, rk), Ok(()));
    assert_eq!(pipe_close_safe(&mut ns, wk), Ok(()));

    assert_eq!(ns.pipes[idx].id, 0, "两端关闭后槽位 id 应清零");
    assert_eq!(ns.pipes[idx].read_key, 0, "读端点 key 应清零");
    assert_eq!(ns.pipes[idx].write_key, 0, "写端点 key 应清零");
    assert_eq!(
        pipe_find_by_key_index(&ns, rk),
        None,
        "已回收槽位不得再被旧 key 命中"
    );

    // 槽位可复用 (key 由递增 id 派生, 不复用旧值)
    let (rk2, wk2) = pipe_create_safe(&mut ns, &mut next_id, 1).expect("槽位应可复用");
    assert_ne!(rk2, rk, "复用槽位应产生新 key");
    assert_ne!(wk2, wk, "复用槽位应产生新 key");
}

#[test]
fn pipe_slot_exhaustion_returns_error() {
    let mut ns = new_namespace();
    let mut next_id = 1u32;
    for _ in 0..IPC_MAX_PIPES {
        pipe_create_safe(&mut ns, &mut next_id, 1).expect("前 N 个创建应成功");
    }
    assert_eq!(
        pipe_create_safe(&mut ns, &mut next_id, 1),
        Err(-1),
        "槽位耗尽应返回错误"
    );
}

// ============================================================================
// fd 对象模型接入契约 (静态扫描)
// ============================================================================

#[test]
fn pipe_endpoint_is_vfs_open_file_with_drop_release() {
    let src = read_src("src/kernel/functions/fs/pipe_fd.rs");

    assert!(
        src.contains("impl Inode for PipeEndpointInode"),
        "管道端点必须实现 Inode 以接入 VFS fd 对象模型"
    );
    assert!(
        src.contains("impl Drop for PipeEndpointInode")
            && src.contains("pipe_close_safe(")
            && src.contains("self.key"),
        "端点 Drop 必须释放 key (OpenFile 引用计数归零才触发 ⇒ fork/dup 共享语义)"
    );
    assert!(
        src.contains(".pipe_create("),
        "create_pair 必须经 functions 层 IPC 门面创建端点 key"
    );
    assert!(
        src.contains("OpenFile::new_pipe"),
        "create_pair 必须经 OpenFile::new_pipe 标记管道端点"
    );
    assert!(
        src.contains("with_current_fd_table")
            && src.contains("alloc_fd(read_handle")
            && src.contains("alloc_fd(write_handle"),
        "create_pair 必须在 per-process FdTable 中分配两个 fd"
    );
    assert!(
        src.contains("KernelError::IllegalSeek"),
        "管道 seek 必须返回 ESPIPE (IllegalSeek)"
    );
}

#[test]
fn pipe_fd_module_exported_and_kernel_io_available() {
    let fs_mod = read_src("src/kernel/functions/fs/mod.rs");
    assert!(
        fs_mod.contains("pub mod pipe_fd;"),
        "functions/fs 必须声明 pipe_fd 模块"
    );
    assert!(
        fs_mod.contains("vfs_read_kernel") && fs_mod.contains("vfs_write_kernel"),
        "内核缓冲读写的切片版必须对外导出"
    );

    let handle = read_src("src/kernel/functions/fs/handle.rs");
    assert!(
        handle.contains("pub fn vfs_read_kernel") && handle.contains("pub fn vfs_write_kernel"),
        "handle.rs 必须提供内核缓冲 I/O 入口"
    );
    assert!(
        handle.contains("UserWritePtr::checked_new") && handle.contains("UserReadPtr::checked_new"),
        "用户指针入口必须保留 privileged 安全代理校验"
    );
}

#[test]
fn pipe_syscalls_route_through_fd_object_model() {
    let io = read_src("src/kernel/functions/fs/io.rs");
    assert!(
        io.contains("pipe2_syscall(fds, 0)"),
        "pipe_syscall 必须委托 pipe2_syscall (单一创建路径)"
    );
    assert!(
        io.contains("super::pipe_fd::create_pair"),
        "pipe2_syscall 必须经 pipe_fd::create_pair 创建端点"
    );
    assert!(
        io.contains("fn fd_table_has_entry"),
        "read/write 必须按 fd 表条目判定标准流路由 (支持 dup2 重定向)"
    );
    assert!(
        io.contains("vfs_write_kernel"),
        "write_syscall 必须走内核缓冲写入"
    );
}

#[test]
fn vfs_data_paths_notify_event_not_close() {
    let handle = read_src("src/kernel/functions/fs/handle.rs");
    assert!(
        handle.contains("fd_notify::notify_fd_event(fd_idx.cast_signed())"),
        "管道读/写成功路径必须发 fd 事件通知 (epoll 重新求值就绪位)"
    );
    // 写路径不得误发 close 通知 (原预存 bug: 写入成功被 epoll 观测为 EPOLLHUP).
    let write_slice = handle
        .split("fn vfs_write_slice")
        .nth(1)
        .unwrap_or_default()
        .split("fn vfs_write_internal")
        .next()
        .unwrap_or_default();
    assert!(
        !write_slice.contains("notify_fd_close"),
        "写路径不得误发 fd 关闭通知"
    );
    assert!(
        handle.contains("fd_notify::notify_fd_close(fd_idx.cast_signed())"),
        "真正的 fd 关闭路径必须保留关闭通知"
    );

    let notify = read_src("src/kernel/privileged/fd_notify.rs");
    assert!(
        notify.contains("pub fn notify_fd_event") && notify.contains("pub fn notify_fd_close"),
        "fd 事件通知接口必须同时提供事件/关闭两个入口"
    );
}

#[test]
fn pipe_blocking_waits_outside_pipe_lock() {
    let pipe_src = read_src("src/kernel/functions/ipc/pipe.rs");
    assert!(
        pipe_src.contains("pub fn pipe_wait_readable")
            && pipe_src.contains("pub fn pipe_wait_writable"),
        "端点策略必须提供阻塞等待原语"
    );
    assert!(
        pipe_src.contains("scheduler_block(BlockReason::WaitingForIo)"),
        "等待必须在持 PIPE_LOCK 期间标记 Blocked (关闭丢唤醒窗口)"
    );
    assert!(
        pipe_src.contains("drop(lock)") && pipe_src.contains("scheduler_yield_ex()"),
        "置 Blocked 后必须先释放 PIPE_LOCK 再 yield"
    );

    let pipe_fd = read_src("src/kernel/functions/fs/pipe_fd.rs");
    assert!(
        pipe_fd.contains("pub fn wait_readable")
            && pipe_fd.contains("pub fn wait_writable")
            && pipe_fd.contains("pub fn pipe_endpoint_of"),
        "pipe_fd 必须向 syscall 层暴露等待门面与端点元数据"
    );

    let io = read_src("src/kernel/functions/fs/io.rs");
    assert!(
        io.contains("wait_readable(key)") && io.contains("wait_writable(key)"),
        "read/write syscall 必须在 EAGAIN 时进入阻塞等待"
    );
    assert!(
        io.contains("flags & O_NONBLOCK != 0"),
        "pipe2 必须把 O_NONBLOCK 传给 create_pair"
    );
    assert!(
        io.contains("of.set_nonblock(nonblock)"),
        "fcntl(F_SETFL) 必须能切换管道端点的非阻塞位"
    );
}

#[test]
fn fd_table_reserves_standard_streams() {
    let src = read_src("src/kernel/privileged/proc/fd_table.rs");
    assert!(
        src.contains("pub const FIRST_USER_FD: usize = 3"),
        "FdTable 必须保留 0/1/2 给标准流"
    );
    assert!(
        src.contains("for i in FIRST_USER_FD..MAX_FDS_PER_PROCESS"),
        "first-fit 分配必须从 FIRST_USER_FD 起扫描"
    );
}

#[test]
fn fork_inherits_pipe_fds_via_open_file_ref() {
    let src = read_src("src/kernel/privileged/proc/proc_ops.rs");
    assert!(
        src.contains("child.fd_table.copy_from(&p.fd_table)")
            && src.contains("inc_open_file_ref(hid)"),
        "fork 必须复制 fd 表并递增 OpenFile 引用 (管道端点继承共享)"
    );
}

#[test]
fn privileged_pipe_ffi_retired_and_strategy_clean() {
    let ipc_mod = read_src("src/kernel/privileged/ipc/mod.rs");
    assert!(
        !ipc_mod.contains("pub mod pipe;"),
        "privileged/ipc/pipe.rs 已退役, 不得再声明模块"
    );

    let strategy = read_src("src/kernel/privileged/ipc/strategy.rs");
    for method in [
        "is_pipe_fd",
        "pipe_create",
        "pipe_read",
        "pipe_write",
        "pipe_close",
    ] {
        assert!(
            !strategy.contains(&format!("fn {}(", method)),
            "IpcStrategy 不得再承载管道方法 {} (管道走 VFS fd 对象模型)",
            method
        );
    }
}

#[test]
fn sendfile_uses_kernel_buffers_and_pipe_marker() {
    let src = read_src("src/kernel/functions/fs/sendfile.rs");
    assert!(
        src.contains("super::pipe_fd::is_pipe_fd"),
        "sendfile/splice 必须经 pipe_fd 判定管道端点"
    );
    assert!(
        src.contains("vfs_read_kernel") && src.contains("vfs_write_kernel"),
        "sendfile/splice 数据面必须走内核缓冲 I/O"
    );
}

#[test]
fn init_pipe_probe_covers_create_read_write_close() {
    let src = read_src("src/user/init/src/main.rs");
    assert!(
        src.contains("pipe_create(&mut fds)"),
        "init 探针必须覆盖 pipe() 创建"
    );
    assert!(
        src.contains("fs_write(fds[1]") && src.contains("fs_read(fds[0]"),
        "init 探针必须覆盖写端写入 + 读端读取"
    );
    assert!(
        src.contains("fs_close(fds[0])") && src.contains("fs_close(fds[1])"),
        "init 探针必须覆盖两端关闭"
    );
}
