// SPDX-License-Identifier: MPL-2.0
// D4-i 丙 回归锁: UEFI(OVMF) 引导真跑腿腿的**接线**契约.
//
// 背景:
//   既有启动腿全经 SeaBIOS, 而 D4-g/D4-i 修的三处缺陷 (MB2 信息块指针硬编码 0 /
//   tag4 覆盖 tag6 内存图 / inline RSDP 被当指针读) 只在 UEFI 下暴露. 本腿
//   (`scripts/uefi_boot_test.sh`) 是唯一的 UEFI 运行读数来源, 它的判据一旦被改坏
//   (串漂移、取数失败读成"零条"、只判声明侧不判登记侧、轮询里程碑取早了把后半段
//   判据对象切掉), full 档照样绿. 本文件钉接线形态, 不重复真构建真启动.
//
//   另有一条**口径**已随 U-1 修复转档: `[THREAD]` 腿此前在 UEFI 下是已登记预存缺陷
//   (docs/plan/uefi-thread-exit-hang.md U-1/U-2), 用户裁定「登记独立工程 + 腿按
//   可达面接线」⇒ 修复前脚本**显式声明不判**并打印横幅. U-1 经 uaccess 写前预检
//   根治后, 脚本已删除横幅并把 `[THREAD] PASS` 加回硬断言, 同时新增
//   `[UACCESS] PASS` 钉住根因 (只读 TID 回写必须返回 EFAULT). 本文件转为锁新口径:
//   横幅不得复活, 两条 ASSERT 不得被摘.
//
// 验收 (六条, 缺一即红):
//   1. 判据串与**产出方**同源 (格式化串取格式前缀, 动态读数取字面前缀);
//   2. 每条判据有红分支, 取数失败不得读成"零条"/"足够";
//   3. 反向针与两侧对账都在 (firmware 声明数 == 登记数; mem 下界; CRLF 剥离);
//   4. 判据落在真 OVMF 启动产物上, 架构自检早于启动, aarch64 显式拒绝,
//      OVMF 发现单源 (--probe), 轮询里程碑取 SNAP END 而非 THREAD;
//   5. `[THREAD]`/`[UACCESS]` 都在硬断言面: 横幅残片为红, 登记册 U-1 `[x]`、U-2 `[]`;
//   6. `ci/audit.sh` full 档步骤 4 真调用本脚本, 失败走 err, 有独立取证日志,
//      固件缺失走 skip_leg 三态, 且门禁侧不另抄 OVMF 路径清单.

use std::fs;

const BOOT_SCRIPT: &str = "../scripts/uefi_boot_test.sh";
const AUDIT_SCRIPT: &str = "../ci/audit.sh";
const REGISTER_DOC: &str = "../docs/plan/uefi-thread-exit-hang.md";
const INIT_PROBE: &str = "../src/user/init/src/main.rs";
const ACPI: &str = "../src/kernel/privileged/arch/x86_64/acpi.rs";
const BOOT_MOD: &str = "../src/kernel/privileged/boot/mod.rs";
const PMM: &str = "../src/kernel/privileged/mm/pmm.rs";
const LIB: &str = "../src/kernel/lib.rs";
const VMM: &str = "../src/kernel/privileged/mm/vmm_x86_64.rs";
const SMP_X86: &str = "../src/kernel/privileged/arch/x86_64/smp_init.rs";
const INITRAMFS_FS: &str = "../src/kernel/functions/fs/initramfs.rs";
const FUNCTIONS_INIT: &str = "../src/kernel/functions/init.rs";
const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";
const E1000: &str = "../src/kernel/functions/driver/net/e1000.rs";
const NET_INIT: &str = "../src/kernel/privileged/net/init.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 断言 `a` 在 `b` 之前出现 (两者都必须存在).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 三元组 = (产出方文件, 产出方逐字串, 脚本侧判据串). 产出方是格式化串时取**格式
/// 前缀** (运行时值由真跑腿证), 脚本侧含 `$SMP` 等变量时取脚本里的逐字形态.
///
/// 放在模块级是为让下面的测试函数体留在 clippy `too_many_lines` 预算内.
const MARKER_PAIRS: &[(&str, &str, &str)] = &[
    (INIT_PROBE, "[TLS] PASS:", "[TLS] PASS:"),
    (INIT_PROBE, "[SIG] PASS:", "[SIG] PASS:"),
    (INIT_PROBE, "[EXEC] PASS: cwd+root", "[EXEC] PASS: cwd+root"),
    (INIT_PROBE, "[WASM] PASS:", "[WASM] PASS:"),
    (
        INIT_PROBE,
        "[KPTI] EL0 kernel high-half access denied",
        "[KPTI] EL0 kernel high-half access denied",
    ),
    (INIT_PROBE, "[THREAD] FAIL: ", "[THREAD]"),
    (
        INIT_PROBE,
        "[UACCESS] PASS: readonly clone EFAULT",
        "[UACCESS] PASS:",
    ),
    (
        ACPI,
        "[ACPI] RSDP found via Multiboot2",
        "[ACPI] RSDP found via Multiboot2",
    ),
    (
        ACPI,
        "[ACPI] RSDP found via BIOS scan",
        "[ACPI] RSDP found via BIOS scan",
    ),
    (
        ACPI,
        "[ACPI] Table scan complete: MADT={} FADT={} HPET={}",
        "[ACPI] Table scan complete: MADT=true FADT=true HPET=true",
    ),
    (
        ACPI,
        "[ACPI] FADT: PM1a_EVT=0x{:X}",
        "[ACPI] FADT: PM1a_EVT=",
    ),
    (
        ACPI,
        "[ACPI] HPET: base=0x{:X}",
        "[ACPI] HPET: base=0xFED00000",
    ),
    (
        ACPI,
        "[ACPI] MADT: LAPIC base=0x{:X}, AP count={}",
        "[ACPI] MADT: LAPIC base=0xFEE00000, AP count=$SMP",
    ),
    (
        BOOT_MOD,
        "Boot firmware reserved regions: {}",
        "Boot firmware reserved regions: ",
    ),
    (
        PMM,
        "[PMM] Early reserve '{}': base=0x{:X} size={} relocate={}",
        "[PMM] Early reserve 'firmware':",
    ),
    (
        PMM,
        "[PMM] Early relocate '{}':",
        "[PMM] Early relocate 'initrd':",
    ),
    (
        PMM,
        "[PMM] Early reserve '{}' clear of boot window: base=0x{:X} end=0x{:X} window_end=0x{:X}",
        "[PMM] Early reserve 'initrd' clear of boot window:",
    ),
    (LIB, "Boot info: mem={} MB", "Boot info: mem="),
    (VMM, "[SMP] TLB probe PASS", "[SMP] TLB probe PASS"),
    (SMP_X86, "[SMP] online CPUs: {}", "[SMP] online CPUs: $SMP"),
    (
        INITRAMFS_FS,
        "[INITRAMFS] Unpacked {} files",
        "Unpacked [0-9]\\+",
    ),
    (FUNCTIONS_INIT, "init from /init", "init from /init"),
    (SCHED, "[SCHED] SNAP END", "[SCHED] SNAP END"),
    (
        SCHED,
        "if conserve_all { \"ok\" } else { \"BAD\" }",
        "conserve=ok",
    ),
    (E1000, "e1000: 初始化完成", "e1000: 初始化完成"),
    (
        NET_INIT,
        "DHCP configured (lease applied)",
        "DHCP configured (lease applied)",
    ),
];

/// 1. 判据串与产出方同源.
///
/// 三元组 = (产出方文件, 产出方逐字串, 脚本侧判据串). 产出方是格式化串时取**格式
/// 前缀** (运行时值由真跑腿证), 脚本侧含 `$SMP` 等变量时取脚本里的逐字形态.
#[test]
fn test_uefi_markers_are_derived_from_producers() {
    let script = read(BOOT_SCRIPT);
    for (producer, producer_needle, script_needle) in MARKER_PAIRS {
        let src = read(producer);
        assert!(
            src.contains(producer_needle),
            "产出方 {producer} 不再打印 {producer_needle:?} —— 须同步改 {BOOT_SCRIPT} 的判据串"
        );
        assert!(
            script.contains(script_needle),
            "脚本判据串 {script_needle:?} 与产出方 {producer} 脱钩 (读不到即恒红/恒绿的假判据)"
        );
    }
    // 预留标签单源: 脚本里的 'firmware' / 'mb2-info' 必须与 lib.rs 调用点的标签逐字一致,
    // 否则标签改名后脚本判据静默失守 (格式串两侧都对得上, 唯独标签对不上).
    let lib = read(LIB);
    for label in ["\"firmware\"", "\"mb2-info\""] {
        assert!(
            lib.contains(&format!(
                "pmm_early_reserve(region.base, region.size, {label})"
            )) || lib.contains(&format!("pmm_early_reserve(base, size, {label})")),
            "lib.rs 的早期预留调用点标签 {label} 漂移 —— 脚本判据须同步"
        );
    }
}

/// 2. 每条判据有红分支, 取数失败不得被读成"通过".
#[test]
fn test_every_judgement_has_a_red_branch() {
    let script = read(BOOT_SCRIPT);
    assert_eq!(
        script.matches("FAILED=$((FAILED + 1))").count(),
        12,
        "判红累加应恰好 12 处 (need/forbid 各 1 + 解包取数/不足 2 + firmware 取数/为零/\
         登记取数/不对账 4 + mem 取数/低于下界 2 + 能力行 1 + D4-h clear 分支 1), 实得 {} 处",
        script.matches("FAILED=$((FAILED + 1))").count()
    );
    assert_eq!(
        script.matches("\nneed \"").count(),
        21,
        "正向判据应恰好 21 条 (D4-h 改 relocate/clear 二选一), 实得 {} 条 —— 少一条即该判据被静默删除",
        script.matches("\nneed \"").count()
    );
    assert_eq!(
        script.matches("\nforbid \"").count(),
        4,
        "反向判据应恰好 4 条, 实得 {} 条",
        script.matches("\nforbid \"").count()
    );
    // need/forbid 两个 helper 自身必须带红分支 (否则 22+4 条判据全是只报不红).
    assert_before(&script, "need() {", "forbid() {");
    let need_body = &script[script.find("need() {").unwrap()..script.find("forbid() {").unwrap()];
    assert!(
        need_body.contains("FAILED=$((FAILED + 1))"),
        "need helper 缺红分支 ⇒ 全部正向判据只报不红"
    );
    let forbid_body =
        &script[script.find("forbid() {").unwrap()..script.find("count_of() {").unwrap()];
    assert!(
        forbid_body.contains("FAILED=$((FAILED + 1))"),
        "forbid helper 缺红分支 ⇒ 全部反向判据只报不红"
    );
    // 取数失败 (非十进制读数) 必须显式红, 不得当成"零条"/"足够".
    assert!(
        script.contains("'' | *[!0-9]*) return 1 ;;"),
        "count_of 的数字校验被撤 ⇒ grep 真错的空读数会被对账判据当成通过"
    );
    assert_eq!(
        script.matches("'' | *[!0-9]*)").count(),
        4,
        "count_of 与三处数值判据 (解包条目数 / firmware 声明数 / mem_size) 都须有非数字红分支, 实得 {} 处",
        script.matches("'' | *[!0-9]*)").count()
    );
    // `-f` 是把参数当"模式文件"的选项, 内联模式配它会让判据静默失守 (COW 腿实测踩过).
    assert!(
        !script.contains("grep -acf"),
        "脚本不得用 -f 配内联模式串 (-f 会把模式读成文件名, 计数腿静默失守)"
    );
    assert!(
        script.contains("grep -caF"),
        "日志含 NUL, 计数腿必须带 -a (否则按二进制处理时匹配计数为 0)"
    );
    assert!(
        script.contains("UEFI_BOOT_TIMEOUT"),
        "里程碑轮询超时被删 ⇒ 启动挂死时本腿会无限等 (而非判红)"
    );
}

/// 3. 反向针与两侧对账都在.
#[test]
fn test_reverse_needles_and_two_sided_reconciliation() {
    let script = read(BOOT_SCRIPT);
    for needle in [
        "forbid \"[ACPI] RSDP found via BIOS scan\"",
        "forbid \"[EXEC] FAIL\"",
        "forbid \"[WASM] FAIL\"",
        "forbid \"conserve=BAD\"",
    ] {
        assert!(script.contains(needle), "反向针被撤: {needle:?}");
    }
    // firmware 预留必须**两侧对账**: 只判声明侧时"声明了但没登记进 PMM"照样绿,
    // 只判登记侧时"登记了 0 区"也照样绿.
    assert!(
        script.contains("[ \"$FW_RESERVED\" -ne \"$FW_DECLARED\" ]"),
        "firmware 声明数与登记数的对账判据被撤 (D4-i 乙 的核心判据)"
    );
    // 下界兼作 UEFI/BIOS 判别器: SeaBIOS e820 实测 7 区 / OVMF 实测 13-14 区, 取 10.
    // 撤掉它 ⇒ pflash 未生效退化成 BIOS 启动时, 本腿会把 BIOS 读数当 UEFI 验证 (假绿).
    assert!(
        script.contains("MIN_FW_REGIONS=10"),
        "firmware 区数下界被改/被撤 ⇒ UEFI/BIOS 判别器失效 (0 区与 BIOS 7 区都会绿)"
    );
    assert!(
        script.contains("[ \"$FW_DECLARED\" -lt \"$MIN_FW_REGIONS\" ]"),
        "firmware 区数下界的红分支被撤 ⇒ UEFI 内存图未被解析或退化成 BIOS 也能绿"
    );
    // mem 下界: tag4 在 OVMF 下只报 ~8 MB, 修复前它覆盖 tag6 的 511 MB.
    assert!(
        script.contains("[ \"$MEM_MB\" -lt 400 ]"),
        "内存图权威判据 (D4-g) 的下界被撤 ⇒ tag4 覆盖 tag6 的回归不会红"
    );
    // 串口行以 CRLF 收尾: 取数前不剥 \r 会把"读到了"误报成"测不出" (首轮实测踩过).
    assert_eq!(
        script.matches("tr -d '\\000\\r'").count(),
        2,
        "两处整行取数 (firmware 声明 / mem_size) 都须剥 NUL 与 CR, 实得 {} 处 —— \
         U-1 修复后 THREAD 对账行已删, 计数随之收敛",
        script.matches("tr -d '\\000\\r'").count()
    );
}

/// 4. 判据落在真 OVMF 启动产物上, 自检早于启动, aarch64 显式拒绝, 发现单源.
#[test]
fn test_boot_targets_real_ovmf_and_rejects_aarch64() {
    let script = read(BOOT_SCRIPT);
    assert!(
        script.contains("-drive \"if=pflash,format=raw,readonly=on,file=$OVMF_CODE\""),
        "CODE 必须以只读 pflash 交付 (固件本体不可被 guest 改写)"
    );
    assert!(
        script.contains("-drive \"if=pflash,format=raw,file=$VARS_RUN\""),
        "VARS 必须以**可写私有拷贝**交付 (发行版文件是 root 只读, 直挂会启动失败)"
    );
    assert!(
        script.contains("cp \"$OVMF_VARS\" \"$VARS_RUN\""),
        "每轮私有 VARS 拷贝被撤 ⇒ 要么写不了 EFI 变量, 要么污染发行版固件文件"
    );
    assert!(
        script.contains("boot_module_carrier_args x86_64")
            && script.contains("-serial \"file:$BOOT_LOG\""),
        "判据必须来自真启动的串口日志 (D4: 经 boot-module 载体 -cdrom GRUB ISO)"
    );
    assert_before(
        &script,
        "verify_image_arch.py --arch x86_64",
        "-serial \"file:$BOOT_LOG\"",
    );
    // aarch64 显式拒绝 (红), 不静默跳过. 判据落在该 case 分支自身: 脚本里别处
    // (--probe 失败 / 构建失败) 也有 exit 1, 全局先后次序证不了"拒绝"这件事.
    let reject_at = script
        .find("    aarch64)")
        .unwrap_or_else(|| panic!("aarch64 拒绝分支未找到"));
    let reject_branch = &script[reject_at..];
    let reject_branch = &reject_branch[..reject_branch.find(";;").unwrap()];
    assert!(
        reject_branch.contains("本腿判据对象是 x86_64 UEFI(OVMF) 引导路径")
            && reject_branch.contains("exit 1"),
        "aarch64 必须显式拒绝 (红) —— 静默跳过在 full 档读起来和通过一样"
    );
    // OVMF 发现单源: --probe 与真跑共用 ovmf_discover, 不各抄一份候选清单.
    assert_eq!(
        script.matches("ovmf_discover").count(),
        3,
        "ovmf_discover 应恰有「定义 + --probe 调用 + 真跑调用」三处, 实得 {} 处",
        script.matches("ovmf_discover").count()
    );
    assert!(
        script.contains("--probe)"),
        "--probe 模式被删 ⇒ ci/audit.sh 的三态判定失去单源依据"
    );
    // 轮询里程碑必须取 SNAP END: THREAD 在 ~6 s 出结果而 EXEC/WASM/SCHED 在 ~10 s
    // 才打印, 以 THREAD 收尾会把后半段判据对象整段切掉 (归因轮实测踩过).
    assert!(
        script.contains("DONE_MARKER='[SCHED] SNAP END'"),
        "轮询里程碑被改成更早的行 ⇒ 后半段判据对象被切掉, 得到缺行即红的假红"
    );
    assert!(
        !script.contains("grep -qaF \"$DONE_MARKER\" \"$BOOT_LOG\" || grep -qaF \"[THREAD]"),
        "不得把 THREAD 行加进提前收尾条件 (同上: 会切掉 EXEC/WASM/SCHED)"
    );
}

/// 5. `[THREAD]` 在 U-1 修复后已回到硬断言面, 不判横幅必须删除.
#[test]
fn test_thread_leg_is_judged_after_u1_fix() {
    let script = read(BOOT_SCRIPT);
    // 修复后不得再留「显式不判」横幅 (留着会让硬断言读起来像豁免).
    for stale in [
        "已登记预存缺陷 —— 本腿**不判** [THREAD]",
        "实测 [THREAD] 行 (供对账, 不参与判定)",
        "修复后须把 THREAD 探针的 PASS 行加回硬断言面并删除本横幅",
    ] {
        assert!(
            !script.contains(stale),
            "U-1 已修复, 不判横幅残片 {stale:?} 未删 —— 门禁口径自相矛盾"
        );
    }
    // 修复项与根因回归锁都必须在硬断言面.
    assert!(
        script.contains("need \"[THREAD] PASS:\""),
        "[THREAD] PASS 未加回硬断言 —— U-1 修复后必须真判, 否则回归无从发现"
    );
    assert!(
        script.contains("need \"[UACCESS] PASS:\""),
        "[UACCESS] EFAULT 回归锁未接门禁 —— 只读 TID 回写必须由 UEFI 真跑钉住"
    );
    // 登记册: U-1 已闭, U-2 仍开放 (不因 U-1 修复自动关闭).
    // U-2 可处于 `[]` (开放) 或 `[~]` (2026-11-24 重开的有界复现中), 但不得 `[x]`.
    let doc = read(REGISTER_DOC);
    let u1_start = doc.find("## U-1").unwrap();
    let u1_body = &doc[u1_start + "## U-1".len()..];
    let u1_end = u1_body.find("\n## ").unwrap_or(u1_body.len());
    assert!(
        u1_body[..u1_end].contains("- **状态**: [x]"),
        "登记册 U-1 的状态不是 \"[x]\" —— 与门禁判据口径不一致"
    );
    let u2_start = doc.find("## U-2").unwrap();
    let u2_body = &doc[u2_start + "## U-2".len()..];
    let u2_end = u2_body.find("\n## ").unwrap_or(u2_body.len());
    let u2_block = &u2_body[..u2_end];
    assert!(
        u2_block.contains("- **状态**: []") || u2_block.contains("- **状态**: [~]"),
        "登记册 U-2 的状态不是 \"[]\"/\"[~]\" —— 不得因 U-1 修复自动关闭"
    );
    assert!(
        doc.contains("group_exit_leg") && doc.contains("reap-timeout"),
        "登记册须点名落点与签名, 否则日后无从对账是否同一缺陷"
    );
}

/// 6. full 档步骤 4 真调用本脚本, 失败即 err, 有取证日志, 固件缺失走三态.
#[test]
fn test_audit_full_invokes_uefi_leg_with_three_state() {
    let audit = read(AUDIT_SCRIPT);
    assert!(
        audit.contains("scripts/uefi_boot_test.sh\" --probe 2>&1"),
        "ci/audit.sh 未用 --probe 决定跑腿还是跳过 (三态失去单源依据)"
    );
    assert!(
        audit.contains("scripts/uefi_boot_test.sh\" x86_64 2>&1"),
        "ci/audit.sh 未真调用本腿脚本 (缺调用即本腿空转)"
    );
    assert_eq!(
        audit.matches("uefi_boot_test.sh").count(),
        3,
        "本腿在 audit 里应只有「注释点名 + --probe 调用 + 真跑调用」三处, 实得 {} 处",
        audit.matches("uefi_boot_test.sh").count()
    );
    assert_before(
        &audit,
        "step \"4/6 QEMU 双架构真实启动测试\"",
        "uefi_boot_test.sh\" --probe 2>&1",
    );
    assert!(
        audit.contains("err \"UEFI 引导真跑腿失败"),
        "本腿失败必须走 err (audit.sh 的 err 内建 exit 1, 判红不被吞)"
    );
    assert!(
        audit.contains("audit_uefi_boot.log"),
        "本腿必须有独立取证日志 —— 偶发红要靠完整判定链归因"
    );
    assert!(
        audit.contains("skip_leg \"uefi\""),
        "固件缺失必须走 skip_leg 三态 (零读数记入台账), 不得当绿也不得判红"
    );
    // 单源: 门禁侧不得另抄一份 OVMF 候选路径清单 (两份必漂移).
    assert!(
        !audit.contains("OVMF_CODE.fd"),
        "ci/audit.sh 里出现 OVMF 固件路径 ⇒ 发现逻辑被抄成两份, 单源口径失守"
    );
}
