//! I-53: 网卡探测编译时架构互斥 — 静态契约测试
//!
//! 验证 maintenance-2026-06-11.md 中 I-53 验收标准:
//!   "双架构二进制包含全部网卡驱动"
//!   "启动时按需初始化"
//!
//! 通过源码分析防止后续重构重新引入:
//!   `#[cfg(target_arch = "x86_64")]` / `#[cfg(target_arch = "aarch64")]`
//!   在 nic_probe_all 函数体内出现 (这会破坏"一个二进制可在双架构运行").
//!
//! B04-09 优化拆分 (2026-08-25): nic_probe_all 随设备探测逻辑移至
//! `src/kernel/framework/net/init/probe.rs`, 契约扫描路径同步更新.

use std::fs;
use std::path::Path;

#[test]
fn test_nic_probe_all_no_arch_mutex() {
    let probe_rs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // QueenX workspace root
        .join("src/kernel/framework/net/init/probe.rs");
    let src = fs::read_to_string(&probe_rs)
        .unwrap_or_else(|e| panic!("无法读取 {}: {}", probe_rs.display(), e));

    // 定位 nic_probe_all 函数体
    let body_start = src
        .find("fn nic_probe_all()")
        .expect("net/init/probe.rs 缺少 nic_probe_all");
    // body 范围: 直到下一个顶级 `fn ` / `static ` / `unsafe fn` / 文件末尾
    let after = &src[body_start..];
    // 找下一个 `fn ` 顶层的最早位置
    let mut body_end = after.len();
    for marker in ["\nfn ", "\nstatic ", "\nunsafe fn ", "\nasync fn "] {
        if let Some(idx) = after.find(marker)
            && idx > 0
            && idx < body_end
        {
            body_end = idx;
        }
    }
    let body = &after[..body_end];

    // 禁止的 cfg 模式
    let forbidden = [
        "cfg(target_arch = \"x86_64\")",
        "cfg(target_arch = \"aarch64\")",
        "cfg(target_arch=\"x86_64\")",
        "cfg(target_arch=\"aarch64\")",
    ];

    for pat in &forbidden {
        assert!(
            !body.contains(pat),
            "nic_probe_all 出现 {} — I-53 要求双架构二进制包含全部驱动, \
             探测顺序应在运行时决定, 不应在编译时互斥",
            pat
        );
    }

    // 同时验证关键驱动探测路径都在 nic_probe_all 中存在
    // (批次 Z ④: 网卡权威迁 services, 探测经 DECISION-K 注册契约槽位
    // net_services_driver 单向拉取, framework 不再直接调用驱动探测)
    // 阶段 3: e1000 与 virtio-net 的探测统一收敛于 services 复合探测,
    // framework nic_probe_all 不再直接调用 e1000_probe。
    assert!(
        !body.contains("e1000_probe"),
        "nic_probe_all 不应直接调用 e1000_probe (阶段 3 收敛至 services 复合探测)"
    );
    assert!(
        body.contains("net_services_driver"),
        "nic_probe_all 缺失 services 网络设备注册契约拉取 (批次 Z ④)"
    );

    // 注释 / 文档确认
    assert!(
        body.contains("I-53"),
        "nic_probe_all 应含 I-53 修复说明 (注释里)"
    );
}

#[test]
fn test_e1000_driver_no_arch_probe_mutex() {
    // e1000 探测函数 (e1000_probe) 本身不应被 cfg-gate,
    // 这样两个架构的二进制都能包含此驱动符号, 由运行时探测决定是否激活.
    // 注意: 驱动内部允许有少量架构相关代码 (e.g. DMA 物理地址转换),
    // 这与 I-53 无关 — I-53 关注的是 *探测入口* 的互斥, 不是驱动内部实现.
    let e1000 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/framework/driver/net/e1000.rs");
    let src = fs::read_to_string(&e1000)
        .unwrap_or_else(|e| panic!("无法读取 {}: {}", e1000.display(), e));

    // 仅在 e1000_probe 函数体上检查 — 函数级别 cfg-gate 才构成 I-53 互斥
    if let Some(idx) = src.find("pub fn e1000_probe") {
        // 取函数体 — 找下一个顶级 `fn ` 标记
        let after = &src[idx..];
        let mut body_end = after.len();
        for marker in ["\n    pub fn ", "\n    fn ", "\n    unsafe fn "] {
            if let Some(p) = after.find(marker)
                && p > 0
                && p < body_end
            {
                body_end = p;
            }
        }
        let body = &after[..body_end];
        let forbidden = ["cfg(target_arch", "cfg(arch"];
        for pat in &forbidden {
            assert!(
                !body.contains(pat),
                "e1000_probe 出现 {} — I-53 探测入口必须架构无关",
                pat
            );
        }
    }
}
