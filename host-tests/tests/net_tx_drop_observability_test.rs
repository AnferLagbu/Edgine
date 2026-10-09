//! S-2 回归锁: smoltcp 设备桥的 TX 掉帧可观测性
//!
//! 背景 (`docs/plan/net-e2e-tcp-echo-flakiness.md` §3 / S-2):
//! `EGDFTxToken::consume` 原先直接丢弃 `NetOps::send` 的 `i32` 返回值, 驱动侧一旦
//! 返 -1, 帧就静默消失, smoltcp 仍认为"已发出", 事后无从判断是否发生过掉帧 —— 这
//! 正是排查 TCP 入向抖动时最先要排除、却排不掉的一环。
//!
//! 本测试锁四件事:
//! 1. 返回值被消费 (进 `if` 与 0 比较), 旧的"裸调用丢返回值"形态不再复现;
//! 2. 失败即累加 `TX_DROPPED` 计数;
//! 3. 计数值回流到上报内容, 且上报限频 (步长常量参与判定);
//! 4. 不引入死代码豁免 (硬规则 F9)。
//!
//! 断言一律在"去空白"后的文本上做, 只锁语义形状, 不受 rustfmt 换行影响。

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

const SMOLTCP_BRIDGE: &str = "src/kernel/privileged/net/smoltcp_impl.rs";

/// 去掉全部空白, 使断言不受换行与缩排影响。
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `TxToken for EGDFTxToken` 的 impl 体 (从 impl 头到文件尾, 该 impl 是最后一个).
fn tx_token_impl_body(src: &str) -> &str {
    let start = src
        .find("impl TxToken for EGDFTxToken")
        .expect("smoltcp_impl: 设备桥的 TxToken impl 不存在");
    &src[start..]
}

#[test]
fn s2_send_return_value_is_consumed() {
    let src = read_src(SMOLTCP_BRIDGE);
    let flat = squash(tx_token_impl_body(&src));
    assert!(
        flat.contains("ifself.ops.send("),
        "S-2: consume 未把 ops.send 的返回值当条件 —— 回到静默掉帧的旧形态"
    );
    assert!(
        flat.contains(")<0{"),
        "S-2: ops.send 的返回值未与 0 比较 (负值即驱动侧失败)"
    );
}

#[test]
fn s2_drop_counter_wired() {
    let src = read_src(SMOLTCP_BRIDGE);
    assert!(
        src.contains("static TX_DROPPED: AtomicU64 = AtomicU64::new(0);"),
        "S-2: 缺 TX_DROPPED 掉帧计数 (AtomicU64: tick/中断上下文免锁)"
    );
    let flat = squash(tx_token_impl_body(&src));
    assert!(
        flat.contains("TX_DROPPED.fetch_add(1,Ordering::Relaxed)+1"),
        "S-2: 发送失败路径未累加 TX_DROPPED"
    );
    // 计数必须被读回参与判据与上报, 否则只是没人看的自增计数器。
    assert!(
        flat.contains("lettotal=") && flat.contains("total==1||"),
        "S-2: TX_DROPPED 的累计值未回流到判据/上报内容"
    );
}

#[test]
fn s2_report_is_rate_limited() {
    let src = read_src(SMOLTCP_BRIDGE);
    assert!(
        src.contains("const TX_DROP_LOG_STRIDE: u64 = 32;"),
        "S-2: 缺限频步长常量 (首次必报 + 其后每 N 次一条)"
    );
    let flat = squash(tx_token_impl_body(&src));
    assert!(
        flat.contains("total.is_multiple_of(TX_DROP_LOG_STRIDE)"),
        "S-2: 掉帧上报未限频 —— 持续掉帧会退化成日志风暴"
    );
    // 走 Net 类日志出口, 与其他 net 诊断同口径 (不引入裸 printk).
    assert!(
        flat.contains("slog_warn!(Net,"),
        "S-2: 掉帧未经 slog_warn!(Net, ..) 上报"
    );
}

#[test]
fn s2_no_dead_code_escape_hatch() {
    // 硬规则 F9: 不得为绕开未使用告警而添加死代码豁免.
    let src = read_src(SMOLTCP_BRIDGE);
    assert!(
        !src.contains("#[allow(dead_code)]") && !src.contains("#[allow(unused"),
        "S-2: smoltcp_impl 引入死代码豁免, 违反 §5 F9"
    );
}
