//! P-3 回归锁: 内核时间戳统一取 tick 时基，禁止硬编码 TSC 频率
//!
//! 背景 (`docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-3):
//! 该条把"有效 tick 频率约 1400/s 对名义 1000Hz"记为待核的定时器异常。插桩实测
//! （宿主墙钟作独立时基，3 轮）证伪了它：tick 路径健康，实测 **975–982 tick/s**；
//! 1400 这个数字来自 `klog::format_ts` 把裸 TSC 按"假设 ~3GHz"折算成日志前缀的
//! "秒"（`tsc / 3000`），而 QEMU 实测 TSC = 2095.9 MHz ⇒ 前缀的秒比真实时间慢
//! 3000/2095.9 = **1.431 倍**，于是 `1000 × 1.431 ≈ 1431 "tick/s"`。
//! 一句话：异常不在 PIT/tick，而在**展示层的假时基**。
//!
//! 本测试锁三件事：
//! 1. klog 前缀改读 tick 派生 uptime，`tsc / 3000` 形态不再复现；
//! 2. klog 不再保留只服务于假时基的 `rdtsc()` 私有包装（硬规则 F9 死代码零容忍）；
//! 3. `cpu::tsc_nominal_hz` 的 CPUID Leaf 0x15 分支按"ECX 单位已是 Hz"换算
//!    （旧实现再乘 1e6，把结果放大 10⁶ 倍），且该换算有可测的自由函数与单测。
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

const KLOG: &str = "src/kernel/privileged/klog/mod.rs";
const CPU: &str = "src/kernel/privileged/cpu/mod.rs";

#[test]
fn p3_klog_prefix_uses_tick_uptime() {
    let src = read_src(KLOG);
    let flat = squash(&src);
    assert!(
        flat.contains("lettotal_us=ticks_to_us(get_ticks())"),
        "P-3: 日志前缀未取 tick 派生 uptime (与 hrtimer/sleep 时基背离)"
    );
    assert!(
        flat.contains("usecrate::privileged::timer::{get_ticks,ticks_to_us};"),
        "P-3: klog 应经 timer 顶层公共 API 取时基, 而非自己造"
    );
    assert!(
        src.contains("fn format_ts(buf: &mut [u8; 32], total_us: u64)"),
        "P-3: format_ts 入参不再是 uptime 微秒数"
    );
}

#[test]
fn p3_klog_has_no_hardcoded_tsc_frequency() {
    let src = read_src(KLOG);
    let flat = squash(&src);
    // 假时基的可执行形态: 那一行除法赋值本身 (历史注释里提到旧写法不算复现).
    assert!(
        !flat.contains("lettotal_us=tsc/3000;"),
        "P-3: 回到按裸 TSC 除以 3000 折算的假时基 (P-3 误判的直接来源)"
    );
    assert!(
        !src.contains("假设 ~3GHz TSC"),
        "P-3: 仍残留硬编码 TSC 频率的折算分支"
    );
    // rdtsc 包装只服务于旧时基, 留着即死代码 (§5 F9).
    assert!(
        !src.contains("fn rdtsc()"),
        "P-3: klog 保留无人调用的 rdtsc() 包装, 违反 §5 F9"
    );
}

#[test]
fn p3_cpuid_leaf15_result_is_hertz() {
    let src = read_src(CPU);
    let flat = squash(&src);
    assert!(
        src.contains("fn tsc_hz_from_cpuid_leaf15(eax: u32, ebx: u32, ecx: u32) -> Option<u64>"),
        "P-3: Leaf 0x15 换算未独立成可测函数"
    );
    assert!(
        flat.contains("ifletSome(hz)=tsc_hz_from_cpuid_leaf15(eax,ebx,ecx)"),
        "P-3: 标称频率出口不再走 Leaf 0x15 换算"
    );
    // Intel SDM: ECX 已是 Hz, 再乘 1e6 会把频率放大 10⁶ 倍.
    assert!(
        !flat.contains("estimated*1_000_000"),
        "P-3: Leaf 0x15 结果被当 MHz 再乘 1e6 (Hz 单位误解)"
    );
    assert!(
        src.contains("fn test_tsc_hz_from_cpuid_leaf15()"),
        "P-3: Leaf 0x15 的 Hz 单位换算缺回归单测"
    );
}

#[test]
fn p3_no_dead_code_escape_hatch() {
    // 硬规则 F9: 不得为绕开未使用告警而添加死代码豁免.
    for rel in [KLOG, CPU] {
        let src = read_src(rel);
        assert!(
            !src.contains("#[allow(dead_code)]"),
            "P-3: {rel} 引入死代码豁免, 违反 §5 F9"
        );
    }
}
