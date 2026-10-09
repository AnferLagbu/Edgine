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
//!        └── 出临界区 ──▶ net_poll_deadline_update │  (登记/撤销 hrtimer)
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
//! SIMPLIFIED: 登记截止期后 **未**据此降低 tick 侧 poll 频率（tick 仍每 tick 无条件
//!   `poll_network`）；影响面: 空闲时每个仍有定时需求的 tick 可能多跑一次 poll
//!   （`poll_at` 空闲即返 `None`，故无额外开销），低功耗/tickless 收益尚未兑现；
//!   何时需扩展: tick 降频或 LAPIC/Generic Timer one-shot 接线时，把 tick 侧
//!   `poll_network` 改为仅在本截止期到期时执行。
//!   见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §5 S-3（本项是效率与低功耗
//!   优化，**不是** e2e 抖动的解药）。

use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::irq::{SoftirqVec, raise_softirq};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_cancel, hrtimer_clock_read, hrtimer_start, is_hrtimer_ready,
};

/// 未登记状态 (`NET_POLL_DEADLINE_NS` 的哨兵值).
///
/// 纳秒时基下 0 = "自启动起第 0ns"，实际截止期恒 > 0，可作哨兵。
const DEADLINE_NONE: u64 = 0;

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

/// 用本次 poll 后 smoltcp 给出的下次截止期更新 hrtimer 登记.
///
/// `None`（或已到期）= 协议栈没有内部定时需求，撤销排队中的登记，改由 tick 侧
/// poll 与入向包驱动。调用方必须已释放 `NET_STATE`（见模块头「锁序」）。
pub(crate) fn net_poll_deadline_update(next_ns: Option<u64>) {
    // hrtimer 框架尚未就绪 (启动早期 / host 单测): 不登记, 也不读到假截止期.
    if !is_hrtimer_ready() {
        NET_POLL_DEADLINE_NS.store(DEADLINE_NONE, Ordering::Release);
        return;
    }

    // 已到期 (< = now) 与无截止期同样处理: smoltcp 的"立即 poll"请求由紧随的
    // tick 侧 poll 覆盖, 登记一个过去时刻只会让回调立刻空转.
    let now_ns = hrtimer_clock_read();
    let Some(deadline) = next_ns.filter(|d| *d > now_ns) else {
        if NET_POLL_DEADLINE_NS.swap(DEADLINE_NONE, Ordering::AcqRel) != DEADLINE_NONE {
            let timer = NET_POLL_TIMER.lock();
            hrtimer_cancel(&timer);
        }
        return;
    };

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

    /// 未来截止期 (相对 hrtimer 时钟), 取分钟级避免并行用例推高的全局 tick 造成
    /// "登记即过期" 的假失败 (与 `proc::itimer` 用例同一取向).
    const T_FUTURE: u64 = 600 * 1_000_000_000;

    /// 用例串行化锁: 本模块只有**一个全局登记位** (不像 `itimer`/`posix_timer`
    /// 有按 pid 索引的槽表可隔离), 并行用例的 `update(None)` 会清掉对方刚登记的
    /// 截止期, 断言到的就不是自己的状态. 故全部用例持本锁串行跑.
    static TEST_SERIAL: IrqSpinLock<()> = IrqSpinLock::new(());

    /// 每用例前复位登记状态, 用例间隔绝共享 `NET_POLL_DEADLINE_NS` 的残留.
    fn reset() {
        if !is_hrtimer_ready() {
            crate::privileged::timer::hrtimer_init();
        }
        net_poll_deadline_update(None);
    }

    #[test]
    fn future_deadline_is_registered() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + T_FUTURE));
        let registered = net_poll_deadline_ns().expect("未来截止期应被登记");
        assert_eq!(registered, now + T_FUTURE, "登记的截止期必须原值可读");
        assert!(
            registered > now,
            "登记的截止期必须在未来: {registered} vs {now}"
        );
        net_poll_deadline_update(None);
    }

    #[test]
    fn no_deadline_clears_registration() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + T_FUTURE));
        assert!(net_poll_deadline_ns().is_some());
        // smoltcp 空闲 (poll_at 返 None) → 撤销登记, 不留下假截止期
        net_poll_deadline_update(None);
        assert_eq!(net_poll_deadline_ns(), None, "无定时需求时不得残留登记");
    }

    #[test]
    fn past_deadline_is_not_registered() {
        let _serial = TEST_SERIAL.lock();
        reset();
        // 已到期 = "立即 poll", 由 tick 侧 poll 覆盖, 不进 hrtimer.
        // 用 `now` 的相对过去 (而非硬编码小值) 断言: host 侧时基回退到 tick 计数,
        // 绝对小值在别的用例已推高 tick 时会误判成"未来".
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now.saturating_sub(1_000_000)));
        assert_eq!(net_poll_deadline_ns(), None, "过去时刻不应登记为截止期");
        net_poll_deadline_update(Some(now));
        assert_eq!(net_poll_deadline_ns(), None, "恰好等于 now 同样不登记");
    }

    #[test]
    fn re_registration_overwrites_previous_deadline() {
        let _serial = TEST_SERIAL.lock();
        reset();
        let now = hrtimer_clock_read();
        net_poll_deadline_update(Some(now + T_FUTURE));
        net_poll_deadline_update(Some(now + 2 * T_FUTURE));
        assert_eq!(
            net_poll_deadline_ns(),
            Some(now + 2 * T_FUTURE),
            "新截止期必须覆盖旧的, 不能并存两个登记"
        );
        net_poll_deadline_update(None);
    }

    /// 撤销未登记状态时不得误报截止期 (空闲 tick 的常见路径).
    #[test]
    fn repeated_clear_is_idempotent() {
        let _serial = TEST_SERIAL.lock();
        reset();
        net_poll_deadline_update(None);
        net_poll_deadline_update(None);
        assert_eq!(net_poll_deadline_ns(), None, "重复撤销后仍应为未登记");
    }
}
