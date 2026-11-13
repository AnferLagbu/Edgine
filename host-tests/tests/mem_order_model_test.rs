// SPDX-License-Identifier: MPL-2.0
//! A-4 乙 — 内存序语义的软件侧验证 (弱内存模型 litmus + ordering 强度断言 hook).
//!
//! 现实硬件强序 (x86_64 TSO) 时, `Relaxed` 与 `Acquire`/`Release` 的可见性差异在运行时
//! 不显现 ⇒ 真机/运行时无法暴露序 bug。故本文件以**纯软件弱内存模型**
//! ([`sb_observe`] / [`mp_observe`]) 为权威判据, 并校验 [`satisfies`] /
//! [`debug_assert_order`] hook 的强度归类正确。真实线程用例仅作冒烟 (TSO 下恒过,
//! 不能作序 bug 判据)。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use edgine::kernel::privileged::sync::memorder::{Required, mp_observe, satisfies, sb_observe};
// hook 用例仅在 debug_assertions 下编译 (release clippy 腿会摘除该用例), 导入同掩避免 unused.
#[cfg(debug_assertions)]
use edgine::kernel::privileged::sync::memorder::debug_assert_order;

/// 弱内存模型复现两个经典 litmus 结果 —— 这是「序选错会出错、序选对能阻止」的软件判别点。
#[test]
fn model_reproduces_classic_outcomes() {
    // SB: StoreLoad 重排 ⇒ 非 SeqCst 可达 (0,0); SeqCst 全屏障 ⇒ (0,0) 不可达。
    assert_eq!(
        sb_observe(false),
        (0, 0),
        "非 SeqCst 下 SB 冗余 (0,0) 应可达"
    );
    assert_eq!(sb_observe(true), (1, 1), "SeqCst 下 SB (0,0) 应不可达");
    // MP: Relaxed 下可读到 flag=1 但 data 陈旧; Release/Acquire 建立 HB ⇒ 一致。
    assert_eq!(mp_observe(false), (1, 0), "Relaxed 下 MP 陈旧读应可达");
    assert_eq!(mp_observe(true), (1, 1), "Release/Acquire 下 MP 应一致");
}

/// 强度归类按方向正交: `Acquire` 不能顶 `Release`, 反之亦然; `SeqCst` 最严。
#[test]
fn ordering_strength_is_directional() {
    assert!(satisfies(Ordering::Acquire, Required::Acquire));
    assert!(satisfies(Ordering::AcqRel, Required::Acquire));
    assert!(satisfies(Ordering::SeqCst, Required::Acquire));
    assert!(!satisfies(Ordering::Relaxed, Required::Acquire));
    assert!(!satisfies(Ordering::Acquire, Required::Release));
    assert!(!satisfies(Ordering::Release, Required::Acquire));
    assert!(satisfies(Ordering::Release, Required::Release));
    assert!(satisfies(Ordering::SeqCst, Required::SeqCst));
    assert!(!satisfies(Ordering::AcqRel, Required::SeqCst));
}

/// debug 期 hook: 弱于契约要求的 ordering 应 panic (拦「序被静默弱化」); 达标不 panic。
#[cfg(debug_assertions)]
#[test]
fn debug_hook_panics_on_weakened_ordering() {
    let r = std::panic::catch_unwind(|| {
        debug_assert_order(Ordering::Relaxed, Required::Acquire, "test.site");
    });
    assert!(r.is_err(), "弱于契约要求的 ordering 应被 debug hook 拦下");
    debug_assert_order(Ordering::SeqCst, Required::Release, "test.site");
}

/// 真实线程 MP 冒烟: producer 存 data 再 `Release` 存 flag; consumer `Acquire` 读 flag 后读 data。
///
/// **判别力边界**(文件头已申明): x86_64 TSO 禁 `StoreStore` / `LoadLoad` 重排, 故 `Relaxed` 下
/// 本用例亦恒过, **不**声称能证 `Relaxed→Acquire/Release` 的必要性; 弱序语义判别由上面的
/// 软件模型用例承担。
#[test]
fn real_thread_mp_smoke() {
    for _ in 0..200 {
        let data = Arc::new(AtomicUsize::new(0));
        let flag = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let (d_prod, f_prod, b_prod) = (data.clone(), flag.clone(), barrier.clone());
        let producer = thread::spawn(move || {
            let _ = b_prod.wait();
            d_prod.store(1, Ordering::Relaxed);
            f_prod.store(1, Ordering::Release);
        });
        let (d_cons, f_cons, b_cons) = (data.clone(), flag.clone(), barrier.clone());
        let consumer = thread::spawn(move || {
            let _ = b_cons.wait();
            while f_cons.load(Ordering::Acquire) == 0 {
                core::hint::spin_loop();
            }
            assert_eq!(
                d_cons.load(Ordering::Relaxed),
                1,
                "读到 flag=1 后 data 必为 1"
            );
        });
        producer.join().expect("producer 线程");
        consumer.join().expect("consumer 线程");
    }
}
