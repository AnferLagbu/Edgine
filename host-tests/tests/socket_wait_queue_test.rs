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
fn p5c_accept_connect_blocking_wiring() {
    // P5c (DECISION-095): accept/connect 真阻塞接线 — wait_queue 新方向 +
    // poll_network 状态迁移唤醒 + sm_accept/sm_connect 阻塞登记 + 非阻塞 -EINPROGRESS.
    let wq = read_src("src/kernel/privileged/net/wait_queue.rs");
    assert!(
        wq.contains("WAITER_ACCEPT"),
        "P5c: wait_queue 缺 WAITER_ACCEPT 方向"
    );
    assert!(
        wq.contains("WAITER_CONNECT"),
        "P5c: wait_queue 缺 WAITER_CONNECT 方向"
    );
    assert!(wq.contains("AcceptReady"), "P5c: WakeReason 缺 AcceptReady");
    assert!(wq.contains("ConnectDone"), "P5c: WakeReason 缺 ConnectDone");

    let poll = read_src("src/kernel/privileged/net/init.rs");
    assert!(
        poll.contains("collect_waiters(WAITER_ACCEPT"),
        "P5c: poll_network 未唤醒 ACCEPT 等待者"
    );
    assert!(
        poll.contains("collect_waiters(WAITER_CONNECT"),
        "P5c: poll_network 未唤醒 CONNECT 等待者"
    );

    let sm = read_src("src/kernel/privileged/net/init/sm_fi.rs");
    assert!(
        sm.contains("WAITER_ACCEPT"),
        "P5c: sm_accept 未登记 ACCEPT 等待者"
    );
    assert!(
        sm.contains("WAITER_CONNECT"),
        "P5c: sm_connect 未登记 CONNECT 等待者"
    );
    assert!(
        sm.contains("E_INPROGRESS"),
        "P5c: sm_connect 缺非阻塞 -EINPROGRESS 语义"
    );
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
        "net_add_waiter(slot, pid, WAITER_READ, dl)",
        "net_add_waiter(slot, pid, WAITER_WRITE, dl)",
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

// ============================================================================
// P5d / DECISION-096: EINTR + SO_RCVTIMEO/SNDTIMEO 超时 + MSG_DONTWAIT 接线契约
// ============================================================================

#[test]
fn p5d_wait_queue_has_deadline_and_collect_expired() {
    // 等待者槽新增绝对超时死线维度 (与方向正交), collect_expired 非破坏采集到点 pid.
    let wq = read_src("src/kernel/privileged/net/wait_queue.rs");
    assert!(
        wq.contains("waiter_deadline: [AtomicU64; WAITER_SLOTS]"),
        "P5d: wait_queue 缺 waiter_deadline 定长死线数组"
    );
    assert!(
        wq.contains("pub fn add_waiter(&self, pid: u32, want: u8, deadline_ns: u64)"),
        "P5d: add_waiter 未扩 deadline_ns 参数"
    );
    assert!(
        wq.contains("pub fn collect_expired"),
        "P5d: wait_queue 缺 collect_expired API"
    );
    // collect_expired 必须非破坏 (只 load, 不 swap), 与 collect_waiters 同一自愈根基.
    let start = wq
        .find("pub fn collect_expired")
        .expect("P5d: 缺 collect_expired");
    let end = start
        + wq[start..]
            .find("}\n    \n")
            .or_else(|| wq[start..].find("#[expect"))
            .unwrap_or(wq.len() - start);
    let body = &wq[start..end];
    assert!(
        body.contains(".load("),
        "P5d: collect_expired 应基于只读 load 采集"
    );
    assert!(
        !body.contains(".swap("),
        "P5d: collect_expired 必须非破坏 (不得 swap 清槽)"
    );
}

#[test]
fn p5d_poll_network_wires_deadline_sweep() {
    // 机制 B: poll_network 每 tick 用单调时钟扫到点死线, 与就绪唤醒共用同一
    // 出临界区 scheduler_unblock 路 (不新增第三条唤醒源).
    let poll = read_src("src/kernel/privileged/net/init.rs");
    assert!(
        poll.contains("use crate::privileged::timer::hrtimer_clock_read;"),
        "P5d: poll_network 需引入 hrtimer_clock_read 单调时钟"
    );
    assert!(
        poll.contains("let now_ns = hrtimer_clock_read();"),
        "P5d: poll_network 每 tick 采一次死线基准 now_ns"
    );
    assert!(
        poll.contains("q.collect_expired(now_ns"),
        "P5d: poll_network 未接线 collect_expired 死线扫描"
    );
}

#[test]
fn p5d_sock_timeout_per_slot_storage() {
    // per-slot 超时刻度 (ns) 存于 NetState, 经 raw accessor 读写 (调用方持 NET_STATE).
    let st = read_src("src/kernel/privileged/net/init/state.rs");
    assert!(
        st.contains("recv_timeout_ns: Vec<u64>") && st.contains("send_timeout_ns: Vec<u64>"),
        "P5d: NetState 缺 recv/send_timeout_ns per-slot 存储"
    );
    let raw = read_src("src/kernel/privileged/net/init/raw.rs");
    assert!(
        raw.contains("pub fn sock_timeout_ns(fd: usize, recv: bool) -> u64"),
        "P5d: raw 缺 sock_timeout_ns 读 accessor"
    );
    assert!(
        raw.contains("pub fn set_sock_timeout_ns(fd: usize, recv: bool, ns: u64)"),
        "P5d: raw 缺 set_sock_timeout_ns 写 accessor"
    );
}

#[test]
fn p5d_blocking_loops_eintr_timeout_wired() {
    // 统一阻塞循环三退出: ready / signal (-EINTR) / 超时 (-E_AGAIN, connect -E_INPROGRESS);
    // MSG_DONTWAIT 折叠为本次非阻塞.
    let sm = read_src("src/kernel/privileged/net/init/sm_fi.rs");
    assert!(
        sm.contains("has_deliverable_signal"),
        "P5d: 阻塞循环未检测可投递信号 (EINTR)"
    );
    assert!(
        sm.contains("return -E_INTR;"),
        "P5d: 阻塞循环缺 -EINTR 返回路径"
    );
    assert!(
        sm.contains("(flags & MSG_DONTWAIT) != 0"),
        "P5d: 缺 MSG_DONTWAIT 本次非阻塞折叠"
    );
    assert!(
        sm.contains("unsafe fn sock_block_deadline") && sm.contains("get_or_insert_with"),
        "P5d: 缺进阻塞前算绝对死线 (get_or_insert_with 复用不延长)"
    );
    // connect 超时保守异步语义: 返 -E_INPROGRESS (非 -E_AGAIN).
    let conn_start = sm.find("fn sm_connect(").expect("P5d: 缺 sm_connect");
    let conn_body = &sm[conn_start..conn_start + 4000];
    assert!(
        conn_body.contains("return -E_INPROGRESS;"),
        "P5d: sm_connect 超时未返 -E_INPROGRESS"
    );
}

#[test]
fn p5d_sockopt_timeval_abi_wired() {
    // SO_RCVTIMEO/SO_SNDTIMEO: sm 层读写 16 字节 struct timeval; syscall 层长度感知 marshalling.
    let sm = read_src("src/kernel/privileged/net/init/sm_fi.rs");
    assert!(
        sm.contains("optname == SO_RCVTIMEO || optname == SO_SNDTIMEO"),
        "P5d: sm_setsockopt/sm_getsockopt 缺 SO_RCVTIMEO/SO_SNDTIMEO 分支"
    );
    assert!(
        sm.contains("raw::set_sock_timeout_ns(slot, optname == SO_RCVTIMEO, ns)"),
        "P5d: sm_setsockopt 未将 timeval 存为 per-slot ns"
    );
    assert!(
        sm.contains("core::ptr::write_unaligned(optlen, 16);"),
        "P5d: sm_getsockopt 未回填 16 字节 timeval"
    );
    // syscall 层不再硬读/写 4 字节 u32, 改为按 valen / *optlen 全长 marshalling.
    let sc = read_src("src/kernel/privileged/net/syscall.rs");
    assert!(
        sc.contains("let len = (valen as usize).min(16);"),
        "P5d: setsockopt_syscall 未按 valen 全长 copy-in (上限 16)"
    );
    assert!(
        sc.contains("let in_len = cap.min(16);"),
        "P5d: getsockopt_syscall 未按用户 *optlen 容量回填 (上限 16)"
    );
    assert!(
        sc.contains("raw_copy_out(val_ptr, out_len, &buf)"),
        "P5d: getsockopt_syscall 未用长度感知 copy-out"
    );
}
