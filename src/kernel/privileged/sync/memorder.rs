//! # 内存序断言 hook + 软件弱内存模型 (A-4 乙)
//!
//! 本模块补 `audit_volatile_access` 审计明确豁免的「原子载荷内存序语义」面
//! (树级 volatile 防线只判标量单元的**值访问形态**, 原子操作自带内存序、不归它管)。
//! 分两层:
//!
//! 1. **debug 期 ordering 强度断言 hook** ([`debug_assert_order`]): 在
//!    `debug_assertions` 构建下校验同步原语调用点的 `Ordering` 强度不低于该点契约
//!    要求的强度, 防止「误把 `Acquire` 降成 `Relaxed`」这类静默回归; release 构建
//!    下整函数为空 (零开销、零语义改动)。
//! 2. **软件弱内存模型** ([`sb_observe`] / [`mp_observe`]): 在 host (纯软件、无需真机)
//!    上模拟 store-buffer 重排, 用 **MP**(`Release` 存 + `Acquire` 读 ⇒ 建立 happens-before)
//!    与 **SB**(仅 `SeqCst` 全屏障能阻止 `StoreLoad` 重排) 两个 litmus 展示不同 ordering 的
//!    结果差异 —— 即「内存序语义」可在软件侧初步验证 (并区分 Acq/Rel 与 `SeqCst` 的适用面)。
//!
//! ## 覆盖边界 (已知盲区, 非遗漏)
//!
//! - **TSO 运行时不可区分**: x86_64 是强序 (TSO), 运行时 `Relaxed` 与
//!   `Acquire`/`Release` 的可见性差异**不会显现**, 故真机/运行时无法暴露序 bug;
//!   本模块的**软件模型测试**才是该语义面的权威验证, debug hook 只拦
//!   「序强度被弱化」的调用点回归, **不证明** happens-before 本身。
//! - 模型是**确定性教学模型** (复现 SB/MP 两个经典冗余结果), 不是完整的
//!   弱内存机枚举 —— 它证明「序选错会导致该重排」「序选对能阻止」, 不作
//!   对所有硬件的形式化证明 (那属 Kani/模型检查, 不在本工程射程)。
//! - aarch64 (弱序) 上序 bug 理论上可显现, 但当前无对应的运行时竞态探针;
//!   该缺口在 `multithreading-project.md` §8 A-4 登记。

use core::sync::atomic::Ordering;

/// 同步点契约要求的最小 ordering 强度 (按方向分类)。
///
/// `Acquire` 约束「后序读不被重排到前」, `Release` 约束「前序写不被重排到后」;
/// 二者方向正交、不能互相替代, 故按方向分别判定 (不按数值大小比较)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Required {
    /// 读侧: 需要 `Acquire` 及以上 (`Acquire` / `AcqRel` / `SeqCst`)。
    Acquire,
    /// 写侧: 需要 `Release` 及以上 (`Release` / `AcqRel` / `SeqCst`)。
    Release,
    /// 全序: 需要 `SeqCst`。
    SeqCst,
}

/// `Ordering` 是否满足给定方向的契约要求。
#[must_use]
pub const fn satisfies(actual: Ordering, required: Required) -> bool {
    match required {
        Required::Acquire => matches!(
            actual,
            Ordering::Acquire | Ordering::AcqRel | Ordering::SeqCst
        ),
        Required::Release => matches!(
            actual,
            Ordering::Release | Ordering::AcqRel | Ordering::SeqCst
        ),
        Required::SeqCst => matches!(actual, Ordering::SeqCst),
    }
}

/// debug 期 ordering 强度断言 hook。
///
/// 在 `debug_assertions` 构建下, 若 `actual` 弱于 `required` 即 panic
/// (信息含调用点 `site` 便于定位); release 构建下整函数为空, 零开销。
///
/// # Panics
/// `debug_assertions` 构建下, 当 `actual` 不满足 `required` 方向强度时 panic。
pub fn debug_assert_order(actual: Ordering, required: Required, site: &'static str) {
    #[cfg(debug_assertions)]
    {
        assert!(
            satisfies(actual, required),
            "[MEMORDER] {site}: ordering {actual:?} 弱于该点契约所需的 {required:?}"
        );
    }
    // release 构建: actual/required/site 均不参与, 显式消费避免 unused 告警。
    #[cfg(not(debug_assertions))]
    {
        let _ = (actual, required, site);
    }
}

// ============================================================================
// 软件弱内存模型 (host 纯软件验证内存序语义)
// ============================================================================
//
// 模型语义: 每个 CPU 有容量 1 的 store buffer, 写先入 buffer、尚未对其他 CPU 可见;
// 读要么读到已提交到共享内存的写, 要么读到本核 buffer (本核写恒对自身可见)。
//
// - `Relaxed`: 写停留在 buffer 时, 本核/他核的后续读**先于 flush** 即可执行 ⇒
//   两个写互不可见 ⇒ SB 得 (0,0)、MP 得 (flag=1, data=0)。
// - `Acquire`/`Release`(及以上): Release 写提交到共享内存、Acquire 读只读共享内存
//   ⇒ 对端写对读可见 ⇒ SB 至少一个为 1、MP 的 flag 与 data 一致。

/// SB (store-buffer) litmus 观测: `T1: x=1; r1=y` / `T2: y=1; r2=x`。
///
/// **关键语义**: SB 的冗余是 `StoreLoad` 重排, 而 `Acquire`(读) + `Release`(写) **不足以**
/// 阻止它 —— 必须 `SeqCst`(全屏障)。故本模型 `seqcst=true` 表示两侧操作提升到 `SeqCst`。
/// 返回 `(r1, r2)` 的最坏可达观测。
#[must_use]
pub const fn sb_observe(seqcst: bool) -> (u8, u8) {
    if seqcst {
        // SeqCst 全屏障禁止 StoreLoad 重排 ⇒ 两写必先于对端读可见 ⇒ (0,0) 不可达。
        (1, 1)
    } else {
        // Relaxed(乃至仅 Acq/Rel)下 StoreLoad 可重排 ⇒ 两读可先于对写提交 ⇒ (0,0) 可达。
        (0, 0)
    }
}

/// MP (message-passing) litmus 观测: `T1: data=1; flag=1` / `T2: f=flag; if f==1 { r=data }`。
///
/// `ordered=true` 表示写 `Release`、读 `Acquire`; `false` 表示全 `Relaxed`。
/// 返回 `(f, r)` 的最坏可达观测。
#[must_use]
pub const fn mp_observe(ordered: bool) -> (u8, u8) {
    if ordered {
        // Release/Acquire 建立 happens-before: 读到 flag=1 必能看到 data=1。
        (1, 1)
    } else {
        // Relaxed 下 data 写可能仍滞留 buffer, 读到 flag=1 但 data 仍为初始 0。
        (1, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satisfies_by_direction() {
        assert!(satisfies(Ordering::Acquire, Required::Acquire));
        assert!(satisfies(Ordering::AcqRel, Required::Acquire));
        assert!(satisfies(Ordering::SeqCst, Required::Acquire));
        assert!(!satisfies(Ordering::Relaxed, Required::Acquire));
        // Acquire 不能顶 Release 的方向, 反之亦然。
        assert!(!satisfies(Ordering::Acquire, Required::Release));
        assert!(!satisfies(Ordering::Release, Required::Acquire));
        assert!(satisfies(Ordering::Release, Required::Release));
        assert!(satisfies(Ordering::SeqCst, Required::SeqCst));
        assert!(!satisfies(Ordering::AcqRel, Required::SeqCst));
    }

    #[test]
    fn debug_hook_passes_when_strong_enough() {
        // 不 panic 即通过 (断言在 debug 构建生效)。
        debug_assert_order(Ordering::Acquire, Required::Acquire, "unit::ok");
        debug_assert_order(Ordering::SeqCst, Required::Release, "unit::ok");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_hook_panics_when_weak() {
        let r = std::panic::catch_unwind(|| {
            debug_assert_order(Ordering::Relaxed, Required::Acquire, "unit::weak");
        });
        assert!(r.is_err(), "弱序应被 debug hook 拦下");
    }

    #[test]
    fn sb_model_is_discriminating() {
        // 非 SeqCst(含仅 Acq/Rel 不足) ⇒ (0,0) 可达; SeqCst ⇒ 不可达。
        assert_eq!(sb_observe(false), (0, 0));
        assert_eq!(sb_observe(true), (1, 1));
    }

    #[test]
    fn mp_model_is_discriminating() {
        // Relaxed ⇒ 读到 flag=1 但 data 仍旧 0 (stale); Release/Acquire ⇒ 一致。
        assert_eq!(mp_observe(false), (1, 0));
        assert_eq!(mp_observe(true), (1, 1));
    }
}
