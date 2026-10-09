//! S-3 回归锁: smoltcp `poll_at` → hrtimer 周期驱动接线
//!
//! 背景 (`docs/plan/net-e2e-tcp-echo-flakiness.md` §3 / §5 S-3):
//! 接线前 `SmoltcpNetStack::poll_at()` 恒返 `None`，smoltcp 每次 poll 后给出的
//! "下次该 poll 的时刻" 被直接丢弃，TCP 重传 / 邻居老化 / DHCP 续约 / SLAAC 全靠
//! tick 空转推进 —— 属机制缺口。**注意**：S-3 是效率与低功耗优化，不是 e2e 抖动
//! 的解药（抖动根因已由 DECISION-099 单时基模型根治，见同文件 §2）。
//!
//! 本测试锁接线的四个语义形状，不锁内部实现细节：
//! 1. `poll_stack` 把 `Interface::poll_at` 的截止期换算成纳秒回报上游（不再丢弃）；
//! 2. 登记动作发生在 `NET_STATE` 临界区**之外**（禁 `NET_STATE → HRTIMER_QUEUE` 嵌套）；
//! 3. 到期回调只置 `NetRx` 底半部，不自登记（hrtimer 的 `OneShot` 收尾会覆盖回调内
//!    的 `hrtimer_start`，导致截止期链条断裂）；
//! 4. functions 层的 `poll_at()` 委托真实登记值，而非恒 `None` 桩；
//! 5. 全程不引入死代码豁免（硬规则 F9）。
//!
//! 断言在"去空白"后的文本上进行，只锁语义形状，不受 rustfmt 换行影响。

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

/// 去掉全部空白, 使断言不受换行与缩排影响。
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

const PRIV_STACK: &str = "src/kernel/privileged/net/smoltcp_impl.rs";
const POLL_NETWORK: &str = "src/kernel/privileged/net/init.rs";
const POLL_TIMER: &str = "src/kernel/privileged/net/init/poll_timer.rs";
const FUNC_STACK: &str = "src/kernel/functions/net/smoltcp_impl.rs";
const FUNC_STUB: &str = "src/kernel/functions/net/mod.rs";

#[test]
fn s3_poll_deadline_reported_not_dropped() {
    let src = read_src(PRIV_STACK);
    assert!(
        src.contains(
            "pub fn next_poll_deadline_ns(&mut self, sockets: &SocketSet<'_>) -> Option<u64>"
        ),
        "S-3: 缺 next_poll_deadline_ns —— poll_at 的截止期没有换算出口"
    );
    let flat = squash(&src);
    assert!(
        flat.contains("self.iface.poll_at(smoltcp_now(),sockets)"),
        "S-3: 未把 smoltcp_now() 传给 Interface::poll_at (截止期失去时基原点)"
    );
    // µs → ns 换算必须夹紧负值: smoltcp 的 Instant 允许表示"已过"的时刻。
    assert!(
        flat.contains(".map(|deadline|deadline.micros().max(0)asu64*1_000)"),
        "S-3: 截止期未按 µs→ns 换算 (或丢了非负夹紧)"
    );
    assert!(
        src.contains(") -> Option<u64> {") && flat.contains("stack.poll(nic,sockets);"),
        "S-3: poll_stack 未同时执行 poll 并回报截止期"
    );
}

#[test]
fn s3_registration_happens_outside_net_state() {
    let src = read_src(POLL_NETWORK);
    let flat = squash(&src);

    let collect = flat
        .find("next_poll_ns=crate::privileged::net::poll_stack(nic,stack,sockets)")
        .expect("S-3: 临界区内未采集 poll_stack 回报的截止期");
    let register = flat
        .find("poll_timer::net_poll_deadline_update(next_poll_ns)")
        .expect("S-3: 未把采集到的截止期登记到 hrtimer");
    assert!(
        collect < register,
        "S-3: 登记必须发生在采集之后 (采集在临界区内, 登记在临界区外)"
    );

    // 临界区的收尾大括号 (rustfmt 稳定形态: 与 `unsafe {{` 同级的 4 空格缩进).
    let guard_at = src
        .find("let _guard = match NET_STATE.try_lock()")
        .expect("S-3: poll_network 不再以 try_lock 进入 NET_STATE 临界区");
    let block_end = src[guard_at..]
        .find("\n    }\n")
        .map(|off| guard_at + off)
        .expect("S-3: 找不到 NET_STATE 临界区的收尾括号");
    let register_raw = src
        .find("poll_timer::net_poll_deadline_update(next_poll_ns)")
        .expect("S-3: 缺截止期登记调用点");
    assert!(
        register_raw > block_end,
        "S-3: 登记调用被移进 NET_STATE 临界区 ⇒ 引入 NET_STATE → HRTIMER_QUEUE 嵌套锁序 (违反 F8)"
    );
}

#[test]
fn s3_expiry_callback_defers_to_softirq() {
    let src = read_src(POLL_TIMER);
    let start = src
        .find("fn net_poll_deadline_callback")
        .expect("S-3: 缺截止期到期回调");
    let body = &src[start..start + 400];
    let flat = squash(body);
    assert!(
        flat.contains("raise_softirq(SoftirqVec::NetRx)"),
        "S-3: 到期回调未置 NetRx 底半部 (改回硬中断里直接 poll 会增加 hardirq 时长)"
    );
    // 回调内自登记会被 OneShot 收尾覆盖, 截止期链条就此断裂 —— 必须留在回调外.
    assert!(
        !flat.contains("hrtimer_start("),
        "S-3: 回调内自我重登记会被 hrtimer 的 OneShot 收尾覆盖, 链条断裂"
    );
    assert!(
        flat.contains("HrTimerRestart::OneShot"),
        "S-3: 回调返回值不再是单次触发 (与重登记被覆盖的前提一致)"
    );
}

#[test]
fn s3_functions_poll_at_delegates_to_real_value() {
    let src = read_src(FUNC_STACK);
    let start = src
        .find("fn poll_at(&self) -> Option<u64>")
        .expect("S-3: functions 层 poll_at 签名变更");
    let body = squash(&src[start..start + 400]);
    assert!(
        body.contains("fw_init::smoltcp_net_stack_poll_at_ms()"),
        "S-3: poll_at 仍是不读真实登记值的桩 (恒 None)"
    );

    // kernel_test 桩必须同名存在, 否则 host 侧构建链接不到.
    let stub = read_src(FUNC_STUB);
    assert!(
        stub.contains("smoltcp_net_stack_poll_at_ms"),
        "S-3: functions/net/mod.rs 缺 kernel_test 侧同名桩"
    );
}

#[test]
fn s3_no_dead_code_escape_hatch() {
    // 硬规则 F9: 不得为绕开未使用告警而添加死代码豁免.
    for rel in [PRIV_STACK, POLL_NETWORK, POLL_TIMER, FUNC_STACK] {
        let src = read_src(rel);
        assert!(
            !src.contains("#[allow(dead_code)]") && !src.contains("#[allow(unused"),
            "S-3: {rel} 引入死代码豁免, 违反 §5 F9"
        );
    }
}
