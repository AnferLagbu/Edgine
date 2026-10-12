//! 网络轮询截止期驱动 (S-3: smoltcp `Interface::poll_at` → hrtimer)
//!
//! ## 职责
//!
//! smoltcp 每次 `poll` 之后会给出一个**下次该 poll 的时刻** (`Interface::poll_at`)：
//! TCP 重传/keepalive、邻居 (ARP/NDP) 老化、DHCP 续约、SLAAC 路由发现都靠它推进。
//! 在本接线之前，这个时刻被丢弃，协议栈时限只能靠 tick 空转推进 —— 机制缺口登记于
//! `docs/plan/net-e2e-tcp-echo-flakiness.md` §3 与 §5 S-3。
//!
//! 本模块把它接进 hrtimer 框架：
//!
//! ```text
//! poll_network()  ──(NET_STATE 临界区内)──▶ iface.poll_at() → 绝对纳秒截止期
//!        │                                        │
//!        └── 出临界区 ──▶ net_poll_deadline_update │  (与入向 floor min 融合后登记 hrtimer)
//!                                   │
//!                        到期 (hardirq) ──▶ raise_softirq(NetRx)
//!                                   │
//!                        do_softirq ─┴─▶ net_rx_softirq_handler → poll_network()
//! ```
//!
//! ## 为什么到期回调只置底半部而不直接 poll
//!
//! `hrtimer_run_queues()` 在回调返回 `OneShot` 后会把定时器置回 `Inactive`
//! 并清 `queue_seq`；若回调内再次 `hrtimer_start` 自己，那次登记会被紧接着的
//! `OneShot` 收尾覆盖，截止期链条就此断裂。改为置位 `NetRx` 底半部后：
//! 1. 重登记发生在 `do_softirq` 上下文，不在自己的回调栈内；
//! 2. `poll_network` 需持 `NET_STATE`，放到中断退出前执行，不增加 hardirq 时长
//!    （与网卡入向路径共用同一底半部，见 [`super::irq`]）。
//!
//! ## 锁序
//!
//! 登记动作只发生在 `NET_STATE` 临界区**之外**（由 `poll_network` 出临界区后调用），
//! 因此不存在 `NET_STATE → HRTIMER_QUEUE` 嵌套。本模块自身的 `NET_POLL_TIMER` 锁
//! 仅在持锁期间调用 `hrtimer_start`/`hrtimer_cancel`（`HRTIMER_QUEUE` 锁），构成
//! `NET_POLL_TIMER → HRTIMER_QUEUE` 单向序；反向路径不存在 —— `hrtimer_run_queues`
//! 在释放 `HRTIMER_QUEUE` 之后才执行回调，而回调只置位原子软中断标志。
//!
//! SIMPLIFIED: E-IV 已把「入向 floor」与 smoltcp 定时截止期收敛成**截止期 min 融合**
//!   登记到 hrtimer（取更早者），但 **tick 腿仍每 tick 无条件 `poll_network`**，故本
//!   融合当前不改变实际 poll 频率（floor 粗于 1ms tick，tick 主导入向 → RX 零回归）；
//!   影响面: 低功耗/tickless 收益尚未兑现，hrtimer 只提供入向心跳的下界；何时需扩展:
//!   P-19(c) 网卡中断腿落地后，撤除 tick 侧无条件 poll、并把 [`NET_POLL_RX_FLOOR_NS`]
//!   上调（拉低入向 poll 频率）以兑现省电。
//!   见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §5 S-3 / E-IV（本项是效率与低功耗
//!   优化，**不是** e2e 抖动的解药）。

use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::irq::{SoftirqVec, raise_softirq};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_clock_read, hrtimer_start, is_hrtimer_ready,
};

/// 未登记状态 (`NET_POLL_DEADLINE_NS` 的哨兵值).
///
/// 纳秒时基下 0 = "自启动起第 0ns"，实际截止期恒 > 0，可作哨兵。
const DEADLINE_NONE: u64 = 0;

/// 入向轮询下限周期 (纳秒): 即便协议栈无内部定时需求, 也至多每 `floor` 被驱动一次,
/// 给出「入向包最坏等待时长」的上界.
///
/// E-IV 的入向腿. 取 10ms —— 粗于当前 1ms tick 周期, 故 tick 无条件 poll 在位时,
/// 实际 poll 节奏仍由 tick 主导 (本 floor 只提供一条更粗的冗余心跳), RX 零回归.
/// P-19(c) 网卡中断腿落地、tick 侧无条件 poll 退役后, 本 floor 成为入向节奏; 届时
/// 上调该值即为「拉低入向 poll 频率」以兑现低功耗.
const NET_POLL_RX_FLOOR_NS: u64 = 10 * 1_000_000;

/// 下次轮询的绝对截止期 (纳秒, hrtimer 时基); [`DEADLINE_NONE`] = 未登记.
///
/// 兼作三件事：登记状态的去重依据、`NetStack::poll_at()` 的读出值、撤销时
/// "是否需要真的 cancel" 的判据 (未登记时不进锁)。
static NET_POLL_DEADLINE_NS: AtomicU64 = AtomicU64::new(DEADLINE_NONE);

/// 截止期定时器对象（单次触发，每次 poll 后重新登记）。
static NET_POLL_TIMER: IrqSpinLock<HrTimer> = IrqSpinLock::new(HrTimer::uninit());

/// 截止期到期回调 (hardirq 上下文, 由 `hrtimer_run_queues` 在无锁状态调用).
fn net_poll_deadline_callback(_timer: &HrTimer) -> HrTimerRestart {
    raise_softirq(SoftirqVec::NetRx);
    HrTimerRestart::OneShot
}

/// E-IV 截止期 min 融合: 求「smoltcp 定时截止期」与「入向 floor 上界」中**更早**
/// 的一个, 返回下次 poll 的绝对纳秒截止期 (恒 > `now_ns`).
///
/// - `timer_ns` = smoltcp `Interface::poll_at` 回报的定时截止期; `None` 或
///   `<= now_ns` (已到期, 由紧随的 poll 覆盖) 视为「无定时需求」→ 候选取 +∞,
///   结果落到 `now_ns + floor_ns` 上界 (入向心跳腿).
/// - `timer_ns` 若早于 floor 上界则取更早的定时值, 保证协议栈定时 (重传/老化/续约)
///   不被 floor 推迟.
/// - `now_ns + floor_ns` 饱和于 `u64::MAX`, 避免时基接近回绕时 floor 绕回过去.
///
/// 纯函数 (不触碰全局登记位), 便于 host 侧对 min 融合的各边界直接断言.
fn effective_poll_deadline(timer_ns: Option<u64>, now_ns: u64, floor_ns: u64) -> u64 {
    let floor_deadline = now_ns.saturating_add(floor_ns);
    match timer_ns {
        Some(timer) if timer > now_ns => timer.min(floor_deadline),
        _ => floor_deadline,
    }
}

/// 用本次 poll 后 smoltcp 给出的下次截止期与入向 floor 融合, 更新 hrtimer 登记.
///
/// 融合语义见 [`effective_poll_deadline`]: 无论协议栈有无定时需求, 都登记一个
/// 未来的绝对截止期 (最迟 `now + floor`), 因此不再有「空闲即撤销」的分支 —— 入向
/// 心跳即由这条 floor 腿维持, 是 tick 侧无条件 poll 退役后的接棒者. 调用方必须已
/// 释放 `NET_STATE`（见模块头「锁序」）。
pub(crate) fn net_poll_deadline_update(next_ns: Option<u64>) {
    // hrtimer 框架尚未就绪 (启动早期 / host 单测): 不登记, 也不读到假截止期.
    if !is_hrtimer_ready() {
        NET_POLL_DEADLINE_NS.store(DEADLINE_NONE, Ordering::Release);
        return;
    }

    let now_ns = hrtimer_clock_read();
    let deadline = effective_poll_deadline(next_ns, now_ns, NET_POLL_RX_FLOOR_NS);

    let mut timer = NET_POLL_TIMER.lock();
    // `init` 需要 `&mut`，故每次登记时复位 expiry/interval/state 后再入队;
    // `hrtimer_start` 自身按 `queue_seq` 摘除同一 timer 的旧条目，重复登记幂等.
    timer.init(net_poll_deadline_callback);
    NET_POLL_DEADLINE_NS.store(deadline, Ordering::Release);
    hrtimer_start(&timer, deadline);
}

/// 读出当前登记的下次轮询截止期 (纳秒).
///
/// `None` = 未登记（协议栈无定时需求，或 hrtimer 未就绪）。
pub(crate) fn net_poll_deadline_ns() -> Option<u64> {
    match NET_POLL_DEADLINE_NS.load(Ordering::Acquire) {
        DEADLINE_NONE => None,
        ns => Some(ns),
    }
}

// ============================================================================
// 单元测试 (host 侧; `privileged::net::init` 在 kernel_test 下被 cfg-out)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 远未来截止期 (相对 hrtimer 时钟), 恒 > floor, 用于验证 floor 夹紧。
    const T_FUTURE: u64 = 600 * 1_000_000_000;

    /// 行为类用例的时基抖动宽容度: host 侧时钟在串行锁内几乎不走,
    /// 1s 量级足以吸收任何 `now` 采读与 `update` 内部采读之间的微小区间。
    const SLACK: u64 = 1_000_000_000;

    /// 入向 floor (与生产常量同源), 行为类用例据此断言融合后的量级。
    const FLOOR: u64 = NET_POLL_RX_FLOOR_NS;

    /// 用例串行化锁: 本模块只有**一个全局登记位** (不像 `itimer`/`posix_timer`
    /// 有按 pid 索引的槽表可隔离), 并行用例会互扰刚登记的截止期. 故全部行为
    /// 用例持本锁串行跑 (纯函数用例不触碰全局, 无需此锁).
    static TEST_SERIAL: IrqSpinLock<()> = IrqSpinLock::new(());

    /// 每用例前确保 hrtimer 就绪, 行为用例自己再登记自己的值。
    fn reset() {
        if !is_hrtimer_ready() {
            crate::privileged::timer::hrtimer_init();
        }
    }

    // ---- 纯函数: min 融合的确定性边界 (不触碰全局, 可直接断言绝对值) ----

    #[test]
    fn effective_poll_deadline_clamps_far_timer_to_floor() {
        // smoltcp 定时截止期远于 floor → 取更早的 floor 上界 (入向心跳保证).
        assert_eq!(
            effective_poll_deadline(Some(1000 + T_FUTURE), 1000, FLOOR),
            1000 + FLOOR
        );
    }

    #[test]
    fn effective_poll_deadline_honors_early_timer() {
        // 定时截止期早于 floor → 取定时值, 不被 floor 推迟.
        assert_eq!(
            effective_poll_deadline(Some(1000 + FLOOR / 2), 1000, FLOOR),
            1000 + FLOOR / 2
        );
    }

    #[test]
    fn effective_poll_deadline_none_and_past_fall_back_to_floor() {
        // 无定时需求 / 已到期 / 恰好等于 now 均落到 floor 上界.
        assert_eq!(effective_poll_deadline(None, 1000, FLOOR), 1000 + FLOOR);
        assert_eq!(
            effective_poll_deadline(Some(999), 1000, FLOOR),
            1000 + FLOOR
        );
        assert_eq!(
            effective_poll_deadline(Some(1000), 1000, FLOOR),
            1000 + FLOOR
        );
    }

    #[test]
    fn effective_poll_deadline_saturates_near_wrap() {
        // 时基接近回绕时 floor 饱和于 u64::MAX, 不绕回过去.
        assert_eq!(effective_poll_deadline(None, u64::MAX - 1, FLOOR), u64::MAX);
    }

    // ---- 行为: update 把融合后的截止期写入全局登记位, 供 poll_at 读出 ----

    #[test]
    fn update_stores_floor_clamped_future_deadline() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + T_FUTURE));
        let registered = net_poll_deadline_ns().expect("远未来定时截止期应被夹到 floor 后登记");
        assert!(
            (now + FLOOR..=now + FLOOR + SLACK).contains(&registered),
            "登记的截止期应落在 floor 量级: {registered} vs now={now}"
        );
    }

    #[test]
    fn idle_update_registers_floor_heartbeat() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        // E-IV: smoltcp 空闲 (poll_at 返 None) 不再撤销登记, 而是登记一条 floor 心跳.
        net_poll_deadline_update(None);
        let registered = net_poll_deadline_ns().expect("空闲应登记 floor 心跳而非撤销");
        assert!(
            (now + FLOOR..=now + FLOOR + SLACK).contains(&registered),
            "空闲登记的截止期应为 floor 心跳: {registered} vs now={now}"
        );
    }

    #[test]
    fn near_timer_deadline_is_wired_through() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + FLOOR / 2));
        let registered = net_poll_deadline_ns().expect("早于 floor 的定时截止期应原值登记");
        assert!(
            registered < now + FLOOR,
            "早于 floor 的定时值不应被 floor 推迟: {registered} vs floor={}",
            now + FLOOR
        );
        assert!(
            (now + FLOOR / 2..=now + FLOOR / 2 + SLACK).contains(&registered),
            "登记的截止期应落在定时值量级: {registered}"
        );
    }

    #[test]
    fn re_registration_overwrites_previous_deadline() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + FLOOR / 2));
        net_poll_deadline_update(Some(now + FLOOR / 4));
        let registered = net_poll_deadline_ns().expect("新登记应覆盖旧登记");
        assert!(
            registered < now + FLOOR / 2,
            "新的更早定时值必须覆盖旧的, 不能保留旧登记: {registered}"
        );
    }
}
