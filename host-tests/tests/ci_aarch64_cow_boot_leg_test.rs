// SPDX-License-Identifier: MPL-2.0
// #108 丙 回归锁: aarch64 缺页/COW 真跑腿腿的**接线**契约.
//
// 背景:
//   本批的机器码语义 (AP[2] 极性 / 表根 / 重试链 / 射程宽窄) 由两类装备分担:
//   host 侧形状锁 `aarch64_cow_fault_wiring_test.rs` 钉源码形态, 真跑腿
//   `scripts/aarch64_cow_boot_test.sh` 钉运行读数. 后者判据一旦被改坏 (串漂移、
//   取数失败读成"零条"、只判正向不判反向), full 档读数照样绿 —— 本文件把它的
//   接线形态钉住, 不重复真构建真启动 (那太贵且已由门禁承担).
//
// 验收 (五条, 缺一即红):
//   1. 判据串与**产出方**同源: `[COW] PASS...` / `[COW] FAIL` / `[KPTI] ... denied`
//      三条都必须在 init 探针源码里逐字存在 (改名即双向脱钩, 判据空转).
//   2. 四条判据各有 err 分支, 且硬零/反向两条的读数来自 `count_of` —— 取数失败
//      (非十进制读数) 必须显式红, 不得被"计数 0"读成通过.
//   3. 双向判据都在: 正向 = 核态 EC=0x25 硬零; 反向 = EL0 EC=0x24 >=1 且 KPTI
//      收割行在. 只判正向时, 把恢复射程放宽成"任何故障都 Fixed"也能全绿.
//   4. 判据落在**真启动产物**上: `-kernel other/build/aarch64/kernel-aarch64.img`
//      + 串口日志; 架构自检 (S-6 防线) 必须早于启动; x86_64 参数显式拒绝而非静默跳过.
//   5. `ci/audit.sh` full 档步骤 4 真调用本脚本, 失败走 `err`, 且有独立取证日志.

use std::fs;

const BOOT_SCRIPT: &str = "../scripts/aarch64_cow_boot_test.sh";
const AUDIT_SCRIPT: &str = "../ci/audit.sh";
const INIT_PROBE: &str = "../src/user/init/src/main.rs";
const PAGE_FAULT: &str = "../src/kernel/privileged/mm/page_fault.rs";

/// 判据串: 与 init 探针的打印文案逐字同源 (脚本侧为字面量匹配, 无正则元字符风险).
const PASS_LINE: &str = "[COW] PASS: cow-copy+cow-reuse+stack-grow";
const FAIL_LINE: &str = "[COW] FAIL";
const KPTI_LINE: &str = "[KPTI] EL0 kernel high-half access denied";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 断言 `a` 在 `b` 之前出现 (两者都必须存在).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. 判据串与产出方同源.
#[test]
fn test_boot_markers_are_derived_from_init_probe() {
    let probe = read(INIT_PROBE);
    for marker in [PASS_LINE, FAIL_LINE, KPTI_LINE] {
        assert!(
            probe.contains(marker),
            "init 探针不再打印 {marker:?} —— 须同步改 scripts/aarch64_cow_boot_test.sh 的判据串"
        );
        assert!(
            read(BOOT_SCRIPT).contains(marker),
            "脚本判据串 {marker:?} 与产出方脱钩 (读不到即恒红/恒绿的假判据)"
        );
    }
    // 探针本身必须是 aarch64 专属 (否则第 4 条"拒绝 x86_64"的口径失效).
    assert!(
        probe.contains("#[cfg(target_arch = \"aarch64\")]\n    cow_probe();"),
        "cow_probe 调用点的架构门控被撤 ⇒ x86_64 上本腿会跑在无判据对象的路径上"
    );
}

/// 2. 四条判据各有 err 分支, 硬零/反向两条经 `count_of` 取数.
#[test]
fn test_every_judgement_has_a_red_branch() {
    let script = read(BOOT_SCRIPT);
    assert_eq!(
        script.matches("FAILED=$((FAILED + 1))").count(),
        6,
        "判红累加应恰好 6 处 (四条运行判据 + 构建失败 + 镜像架构自检失败), 实得 {} 处 \
         —— 少一处即该判据只报不红",
        script.matches("FAILED=$((FAILED + 1))").count()
    );
    for needle in [
        "if grep -qaF \"$DONE_MARKER\" \"$BOOT_LOG\"; then",
        "if grep -qaF \"$FAIL_MARKER\" \"$BOOT_LOG\"; then",
        "if ! A64_KERNEL_FAULTS=$(count_of \"SYNC! ESR=0000000096\"); then",
        "elif [ \"$A64_KERNEL_FAULTS\" -eq 0 ]; then",
        "if ! A64_EL0_FAULTS=$(count_of \"SYNC! ESR=0000000092\"); then",
        "elif [ \"$A64_EL0_FAULTS\" -ge 1 ] \\",
    ] {
        assert!(script.contains(needle), "判据形态被改动: {needle:?}");
    }
    // 取数失败 (非十进制读数) 必须显式红, 不得当成"零条".
    assert!(
        script.contains("'' | *[!0-9]*) return 1 ;;"),
        "count_of 的数字校验被撤 ⇒ grep 真错的空读数会被硬零判据当成通过"
    );
    // `-f` 是把参数当"模式文件"的选项, 内联模式配它会让判据静默失守 (本批实测踩过).
    assert!(
        !script.contains("grep -acf"),
        "脚本不得用 -f 配内联模式串 (-f 会把模式读成文件名, 计数腿静默失守)"
    );
}

/// 3. 双向判据都在, 且 EC 前缀与 `page_fault.rs` 的 EC 常量同口径.
#[test]
fn test_judgements_cover_both_directions_and_ec_prefixes() {
    let script = read(BOOT_SCRIPT);
    assert!(
        script.contains("SYNC! ESR=0000000096"),
        "正向 (核态同 EL 异常硬零) 判据被撤"
    );
    assert!(
        script.contains("SYNC! ESR=0000000092"),
        "反向 (EL0 真·非法访问仍在终止面) 判据被撤 —— 只判正向时放宽射程不会红"
    );
    // 前缀 10 位 = ESR 顶字节 EC<<2|IL 的十六进制: EC=0x25 -> 0x96, EC=0x24 -> 0x92.
    let pf = read(PAGE_FAULT);
    for needle in [
        "const EC_DATA_ABORT_SAME_EL: u64 = 0x25;",
        "const EC_DATA_ABORT_LOWER_EL: u64 = 0x24;",
    ] {
        assert!(
            pf.contains(needle),
            "EC 常量表被改动 ({needle:?}) —— 脚本里的 0x96/0x92 前缀须同步"
        );
    }
    // `SYNC!` 行的语义 (#128 施工 B): 只在**未被恢复**的同步异常上打印.
    assert!(
        script.contains("AARCH64_COW_BOOT_TIMEOUT"),
        "里程碑轮询超时被删 ⇒ 启动挂死时本腿会无限等 (而非判红)"
    );
}

/// 4. 判据落在真启动产物上, 自检早于启动, x86_64 显式拒绝.
#[test]
fn test_boot_targets_real_image_and_rejects_x86_64() {
    let script = read(BOOT_SCRIPT);
    assert!(
        script.contains("IMAGE=\"other/build/aarch64/kernel-aarch64.img\""),
        "必须用真被 -kernel 加载的 .img (kernel.flat 不是 aarch64 产物)"
    );
    assert!(
        script.contains("-kernel \"$IMAGE\"") && script.contains("-serial \"file:$BOOT_LOG\""),
        "判据必须来自本架构真启动的串口日志"
    );
    assert_before(
        &script,
        "verify_image_arch.py --arch aarch64",
        "-serial \"file:$BOOT_LOG\"",
    );
    assert!(
        script.contains("grep -caF"),
        "日志含 NUL, 计数腿必须带 -a (否则按二进制处理时匹配计数为 0)"
    );
    assert_before(&script, "x86_64)", "exit 1");
    assert!(
        script.contains("本腿判据对象是 aarch64 专属探针"),
        "x86_64 必须显式拒绝 (红), 不能静默跳过 —— 静默跳过在 full 档读起来和通过一样"
    );
}

/// 5. full 档步骤 4 真调用本脚本, 失败即 err, 且有独立取证日志.
#[test]
fn test_audit_full_invokes_cow_boot_leg_with_err_on_fail() {
    let audit = read(AUDIT_SCRIPT);
    assert!(
        audit.contains("scripts/aarch64_cow_boot_test.sh\" 2>&1"),
        "ci/audit.sh 未真调用本腿脚本 (缺调用即本腿空转)"
    );
    assert_eq!(
        audit.matches("aarch64_cow_boot_test.sh").count(),
        2,
        "本腿在 audit 里应只有「注释点名脚本 + 真调用」两处, 实得 {} 处",
        audit.matches("aarch64_cow_boot_test.sh").count()
    );
    assert_before(
        &audit,
        "step \"4/6 QEMU 双架构真实启动测试\"",
        "aarch64_cow_boot_test.sh\" 2>&1",
    );
    assert!(
        audit.contains("err \"aarch64 缺页/COW 真跑腿失败"),
        "本腿失败必须走 err (audit.sh 的 err 内建 exit 1, 判红不被吞)"
    );
    assert!(
        audit.contains("audit_aarch64_cow_boot.log"),
        "本腿必须有独立取证日志 —— 偶发红要靠完整判定链归因"
    );
}
