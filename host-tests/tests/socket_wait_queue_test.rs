//! P2-I-41: Socket WaitQueue 基础设施 host-test
//!
//! 验证:
//! 1. wait_queue.rs 模块存在并导出关键类型
//! 2. SocketWaitQueue 行为契约 (mark_waiting / try_wake / is_pending)
//! 3. SocketWaitQueueTable 边界 (按 `MAX_SM_FD` 定长 + 越界返回 None)
//! 4. poll_network 末尾调用 try_wake (静态契约)
//! 5. 单元测试 (wait_queue.rs 内 #[cfg(test)]) 数量
//! 6. 与框架/服务边界: SOCKET_WAIT_QUEUES 是 privileged 内 static (不在 functions 暴露)

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

#[test]
fn wait_queue_module_exists() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    assert!(
        src.contains("pub struct SocketWaitQueue")
            && src.contains("pub struct SocketWaitQueueTable"),
        "P2-I-41: wait_queue.rs 必须定义 SocketWaitQueue + SocketWaitQueueTable"
    );
}

#[test]
fn socket_wait_queue_exposes_required_api() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    let required = [
        "pub const fn new()",
        "pub fn mark_waiting",
        "pub fn try_wake",
        "pub fn is_pending",
        "pub fn wake_count",
        "pub fn last_reason",
    ];
    for sig in required {
        assert!(
            src.contains(sig),
            "P2-I-41: SocketWaitQueue 缺少 API `{sig}`"
        );
    }
}

#[test]
fn wake_reason_distinguishes_three_states() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    let variants = ["Readable", "Writable", "Closed"];
    for v in variants {
        assert!(src.contains(v), "P2-I-41: WakeReason 缺少变体 {v}");
    }
}

#[test]
fn socket_wait_queue_table_sized_by_max_sm_fd() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    // D10 / DECISION-092: 等待表按 `MAX_SM_FD` 定长 (修复原固定 16 导致
    // slots 16..255 唤醒静默失效的潜伏 bug); 不得回退为硬编码小常量.
    assert!(
        src.contains("queues: [SocketWaitQueue; MAX_SM_FD]"),
        "P5a/D10: SocketWaitQueueTable 必须按 MAX_SM_FD 定长, 与 FdPlan::SMOLTCP 对齐"
    );
    assert!(
        src.contains(
            "const MAX_SM_FD: usize = crate::privileged::proc::FdPlan::SMOLTCP.capacity as usize"
        ),
        "P5a/D10: MAX_SM_FD 须与 FdPlan::SMOLTCP.capacity 同源 (免 init↔wait_queue 循环依赖)"
    );
    assert!(
        !src.contains("[SocketWaitQueue; 16]"),
        "P5a/D10: 等待表不得回退为固定 16 项"
    );
    // get() 改为切片自动判界 (越界 → None), 不再硬编 `if fd < 16`.
    assert!(
        src.contains("self.queues.get(fd)"),
        "P5a/D10: get() 应基于切片自动判界"
    );
}

#[test]
fn global_instance_exported() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    assert!(
        src.contains("pub static SOCKET_WAIT_QUEUES"),
        "P2-I-41: 必须暴露全局表 SOCKET_WAIT_QUEUES"
    );
}

#[test]
fn poll_network_invokes_try_wake() {
    let src = read_src("src/kernel/privileged/net/init.rs");
    let marker = "pub unsafe fn poll_network()";
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("P2-I-41: 找不到 {marker}"));
    let body = &src[start..];
    assert!(
        body.contains("SOCKET_WAIT_QUEUES.get(fd)"),
        "P2-I-41: poll_network 必须遍历 SOCKET_WAIT_QUEUES.get(fd)"
    );
    assert!(
        body.contains("q.try_wake(reason)"),
        "P2-I-41: poll_network 必须调用 q.try_wake(reason)"
    );
    assert!(
        body.contains("MAX_SM_FD"),
        "P2-I-41: poll_network 必须按 MAX_SM_FD 遍历 fd"
    );
}

#[test]
fn poll_network_uses_try_wake_not_blocking() {
    let src = read_src("src/kernel/privileged/net/init.rs");
    // 强调 ISR 端用 try_wake (不阻塞), syscall 端才能用阻塞 wake
    let marker = "pub unsafe fn poll_network()";
    let start = src.find(marker).expect("missing poll_network");
    // 截取整个函数体 (rustfmt 后行变宽 + 属性换行, 2500 不够)
    let body_end = src[start..]
        .find("\npub fn ")
        .or_else(|| src[start..].find("\npub unsafe fn ").map(|i| i + 1))
        .unwrap_or(src.len());
    let body = &src[start..start + body_end.min(10000)];
    assert!(
        body.contains("q.try_wake("),
        "P2-I-41: poll_network 必须使用 try_wake (非阻塞) 而不是 blocking wake"
    );
    assert!(
        !body.contains("q.wake_one(") && !body.contains("q.wake_all("),
        "P2-I-41: poll_network 不应使用阻塞 wake_one/wake_all"
    );
}

#[test]
fn wait_queue_module_registered_in_net_mod() {
    let src = read_src("src/kernel/privileged/net/mod.rs");
    assert!(
        src.contains("pub mod wait_queue"),
        "P2-I-41: net/mod.rs 必须 pub mod wait_queue"
    );
}

#[test]
fn wait_queue_uses_irqspinlock_not_spin() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    assert!(
        src.contains("use crate::privileged::sync::irq_spinlock::IrqSpinLock as Mutex")
            || src.contains("use crate::privileged::sync::IrqSpinLock as Mutex"),
        "P2-I-41: wait_queue 必须使用 IrqSpinLock (关中断), 与框架同步原语保持一致"
    );
}

#[test]
fn unit_tests_inside_wait_queue_module() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    let test_count = src.matches("#[test]").count();
    assert!(
        test_count >= 4,
        "P2-I-41: wait_queue.rs 内置至少 4 个 #[test] 单元测试, 实测 {test_count}"
    );
}

#[test]
fn wake_without_pending_does_not_count() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    // 行为契约: try_wake 无人等待时 wake_count 不递增
    let test_block = src
        .rsplit_once("#[cfg(test)]")
        .map(|(_, b)| b)
        .unwrap_or("");
    assert!(
        test_block.contains("fn wake_without_waiter_is_noop"),
        "P2-I-41: 必须存在 'wake_without_waiter_is_noop' 单元测试"
    );
    assert!(
        test_block.contains("fn multiple_wake_increments_count"),
        "P2-I-41: 必须存在 'multiple_wake_increments_count' 单元测试"
    );
}

// ============================================================================
// P5b / DECISION-094: 多等待者模型 + 锁序 F8 + 丢失唤醒自愈 静态契约
// ============================================================================

#[test]
fn wait_queue_exposes_multi_waiter_api() {
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    for sig in [
        "pub fn add_waiter",
        "pub fn remove_waiter",
        "pub fn collect_waiters",
    ] {
        assert!(
            src.contains(sig),
            "P5b: SocketWaitQueue 缺少多等待者 API `{sig}`"
        );
    }
    // 定长多槽 + 方向位 (取代 P5a 的单 waiter_pid).
    for marker in [
        "pub const WAITER_SLOTS",
        "pub const WAITER_READ",
        "pub const WAITER_WRITE",
        "waiter_pid: [AtomicU32; WAITER_SLOTS]",
        "waiter_want: [AtomicU8; WAITER_SLOTS]",
    ] {
        assert!(
            src.contains(marker),
            "P5b: wait_queue 缺少多等待者建模 `{marker}`"
        );
    }
}

#[test]
fn collect_waiters_is_non_destructive() {
    // 丢失唤醒自愈的根基: collect_waiters 只读 (load), 不清槽; 等待者由消费者
    // 成功返回时自行 remove_waiter 自摘. 若 collect 误用 swap 清槽会被电平重扫漏唤醒.
    let src = read_src("src/kernel/privileged/net/wait_queue.rs");
    let start = src
        .find("pub fn collect_waiters")
        .expect("P5b: 缺 collect_waiters");
    let end = src[start..]
        .find("#[expect")
        .map(|i| start + i)
        .unwrap_or(src.len());
    let body = &src[start..end];
    assert!(
        body.contains(".load("),
        "P5b: collect_waiters 应基于只读 load 采集"
    );
    assert!(
        !body.contains(".swap("),
        "P5b: collect_waiters 必须非破坏 (不得 swap 清槽), 否则丢唤醒无法自愈"
    );
}

#[test]
fn poll_network_collects_inside_lock_unblocks_outside() {
    // F8: 禁 NET_STATE → SCHEDULER 嵌套锁. poll_network 必须锁内 collect_waiters,
    // 出临界区后才 scheduler_unblock (用文本出现顺序作结构代理).
    let src = read_src("src/kernel/privileged/net/init.rs");
    let start = src
        .find("pub unsafe fn poll_network()")
        .expect("P5b: 缺 poll_network");
    let body = &src[start..];
    let collect = body
        .find("collect_waiters(")
        .expect("P5b: poll_network 必须在锁内 collect_waiters 采集待唤醒 pid");
    let unblock = body
        .find("scheduler_unblock(pid)")
        .expect("P5b: poll_network 必须调用 scheduler_unblock 唤醒等待者");
    assert!(
        collect < unblock,
        "P5b/F8: poll_network 应先锁内 collect_waiters, 出临界区后才 scheduler_unblock"
    );
    assert!(
        body.contains("use crate::privileged::proc::scheduler_unblock;"),
        "P5b: poll_network 需引入 scheduler_unblock"
    );
}

#[test]
fn recv_send_block_loop_wired() {
    // sm_recv/sm_send/sm_recvfrom/sm_sendto 均接入阻塞重试循环.
    let src = read_src("src/kernel/privileged/net/init/sm_fi.rs");
    for marker in [
        "fn net_add_waiter",
        "fn net_clear_waiter",
        "can_block",
        "net_add_waiter(slot, pid, WAITER_READ)",
        "net_add_waiter(slot, pid, WAITER_WRITE)",
        "drop(guard)",
        "scheduler_block(BlockReason::WaitingForIo)",
        "scheduler_schedule()",
    ] {
        assert!(
            src.contains(marker),
            "P5b: sm_fi 阻塞循环缺少接线 `{marker}`"
        );
    }
    // 中断/idle 路径守卫: 不可阻塞时直接返 eager, 不得登记等待者.
    assert!(
        src.contains("pid != 0 && !in_irq_context()"),
        "P5b: 阻塞前必须守卫 in_irq_context/pid!=0 (中断路径禁阻塞)"
    );
    // 消费者返回前自摘 (非破坏 collect 的配套).
    let recv_start = src
        .find("pub unsafe extern \"C\" fn sm_recv(")
        .expect("P5b: 缺 sm_recv");
    let recv_body = &src[recv_start..];
    assert!(
        recv_body.contains("net_clear_waiter(slot, pid)"),
        "P5b: sm_recv 返回前必须 net_clear_waiter 自摘等待者"
    );
}
