// SPDX-License-Identifier: MPL-2.0
// D5-b 回归锁: aarch64 EGFS 磁盘挂载真跑腿腿的**接线**契约.
//
// 背景:
//   D5 的三处修复 (virtio-blk legacy 初始化序 / 挂载调用处 NUL 终止 / 去 x86-only
//   门控) 只在**带盘真启动**下暴露: 既有启动腿一律不带盘, 恒走 memory mode, 全绿
//   证不了磁盘挂载成立. 本腿补上该判据面, 而腿自身的判据一旦被改坏 (串漂移、取数
//   失败读成"零条"、二启前重置盘使持久化判据恒真、只判正向不判负控制), full 档读数
//   照样绿 —— 本文件把接线形态钉住, 不重复真构建真启动 (那太贵且已由门禁承担).
//
//   拓扑 (D5-e 裁定, Linux 式 switch_root): 引导根恒为 ramfs/initramfs, EGFS 只挂
//   非根挂载点 /disk, 由用户态 init 切根进磁盘根. 故判据串是 `EGFS mounted at /disk`,
//   不是 `Root filesystem: EGFS`; 引导根形态另由 init.rs 的 ramfs 挂根针钉住.
//
// 验收 (八条, 缺一即红):
//   1. 判据串与**产出方**同源: /disk 挂载/memory mode/挂载失败三串在 lib.rs, 格式化
//      两串在 egfs_data.rs, 超时串在 blk.rs, EL0 串在 init.rs, 注册串在 virtio/mod.rs.
//   2. 三相判据各有红分支, 且所有硬零判据经 `count_of` 取数 (非十进制读数即红).
//   3. 持久化判据非恒真: 空白盘只在相 1 前新建一次, 相 2 复用同盘且禁重新格式化.
//   4. 负控制在: 相 3 摘盘后 memory mode 必须在, /disk 挂载行必须硬零.
//   5. 判据落在真启动产物上: boot-module 载体 + 串口日志 + 架构自检早于启动;
//      x86_64 显式拒绝而非静默跳过; 恰挂 1 块盘 (注册数判据的 "1" 才有依据).
//   6. `ci/audit.sh` full 档步骤 4 真调用本脚本, 失败走 `err`, 且有独立取证日志.
//   7. D5-g 磁盘根首启安装: lib.rs 在 /disk 挂载成功后真调用 install_disk_root,
//      该函数幂等跳过 + 解到 /disk + sync 后回读 /disk/init, 三相各有正/反判据.
//   8. D5-h 切根终态: boot_once 按相取终态标记 (只等 EL0 会在切根前收机 ⇒ 判据恒绿),
//      三相 PASS/SKIP/FAIL 各有正反判据, 标记串与用户态 init 产出方同源.

use std::fs;

const BOOT_SCRIPT: &str = "../scripts/aarch64_egfs_disk_boot_test.sh";
const AUDIT_SCRIPT: &str = "../ci/audit.sh";
const LIB: &str = "../src/kernel/lib.rs";
const EGFS_DATA: &str = "../src/kernel/functions/fs/egfs/egfs_data.rs";
const VIRTIO_BLK: &str = "../src/kernel/functions/driver/virtio/blk.rs";
const VIRTIO_MOD: &str = "../src/kernel/functions/driver/virtio/mod.rs";
const INIT: &str = "../src/kernel/functions/init.rs";

/// 判据串: 与内核打印文案逐字同源 (脚本侧为字面量匹配, 无正则元字符风险).
const DISK_MOUNT: &str = "EGFS mounted at /disk (disk)";
const DISK_MOUNT_PREFIX: &str = "EGFS mounted at /disk";
const ROOT_MEM: &str = "EGFS: running in memory mode (no disk)";
const MOUNT_FAIL: &str = "EGFS mount failed";
const FORMAT_DONE: &str = "[EGFS] FORMAT: Complete";
const FORMAT_FIRST: &str = "formatting first drive";
const BLK_TIMEOUT: &str = "virtio-blk: 请求超时";
const BLK_REGISTERED: &str = "virtio-blk: 1 device(s) registered";
const EL0: &str = "Entering EL0";
/// D5-g 磁盘根首启安装判据串 (产出方 = functions/init.rs 的 `install_disk_root`).
const DISK_ROOT_INSTALLED: &str = "disk root installed:";
const DISK_INIT_READBACK: &str = "/disk/init read back";
const DISK_ROOT_INCOMPLETE: &str = "disk root install incomplete";
const DISK_ROOT_SKIP: &str = "/disk/init present (skip disk-root install)";
/// 引导根形态针 (D5-e): 挂 "/" 的必须是 ramfs, EGFS 不得抢占引导根.
const BOOT_ROOT_RAMFS: &str = "vfs_mount_safe(\"/\", \"ramfs\")";
/// D5-h 切根终态串 (产出方 = 用户态 init 的 `switch_root_probe`, 非内核).
const USER_INIT: &str = "../src/user/init/src/main.rs";
const SWROOT_PASS: &str = "[SWROOT] PASS: pivot to /disk";
const SWROOT_SKIP: &str = "[SWROOT] SKIP: no disk root";
const SWROOT_FAIL: &str = "[SWROOT] FAIL";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 断言 `a` 在 `b` 之前出现 (两者都必须存在).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. 判据串与产出方同源 (改名即双向脱钩, 判据空转).
#[test]
fn test_boot_markers_are_derived_from_kernel_producers() {
    let script = read(BOOT_SCRIPT);
    for (marker, producer, path) in [
        (DISK_MOUNT, LIB, "lib.rs"),
        (ROOT_MEM, LIB, "lib.rs"),
        (MOUNT_FAIL, LIB, "lib.rs"),
        (FORMAT_DONE, EGFS_DATA, "egfs_data.rs"),
        (FORMAT_FIRST, EGFS_DATA, "egfs_data.rs"),
        (BLK_TIMEOUT, VIRTIO_BLK, "blk.rs"),
        (EL0, INIT, "init.rs"),
        (DISK_ROOT_INSTALLED, INIT, "init.rs"),
        (DISK_INIT_READBACK, INIT, "init.rs"),
        (DISK_ROOT_INCOMPLETE, INIT, "init.rs"),
        (DISK_ROOT_SKIP, INIT, "init.rs"),
    ] {
        assert!(
            read(producer).contains(marker),
            "{path} 不再打印 {marker:?} —— 须同步改 {BOOT_SCRIPT} 的判据串"
        );
        assert!(
            script.contains(marker),
            "脚本判据串 {marker:?} 与产出方脱钩 (读不到即恒红/恒绿的假判据)"
        );
    }
    // 注册数是派生串 (产出方带 {} 计数), 故钉模板 + 脚本侧的 "1" 两面.
    assert!(
        read(VIRTIO_MOD).contains("device(s) registered (functions 权威)"),
        "virtio/mod.rs 的注册打印模板漂移 ⇒ 脚本的设备发现判据空转"
    );
    assert!(script.contains(BLK_REGISTERED));
}

/// 1b. /disk 挂载行的单盘/多盘两形态都在产出方, 且引导根恒为 ramfs (D5-e 裁定).
#[test]
fn test_disk_mount_line_keeps_both_forms_and_boot_root_stays_ramfs() {
    let lib = read(LIB);
    assert!(
        lib.contains("EGFS mounted at /disk ({} drives)") && lib.contains(DISK_MOUNT),
        "lib.rs 的 /disk 挂载打印只剩一种形态 ⇒ 本腿的单盘判据与真机读数会静默脱钩"
    );
    assert!(
        lib.contains("if n_drives > 1"),
        "挂载形态的驱动数据判据被改 ⇒ 挂 1 块盘时不再走 (disk) 形态"
    );
    assert!(
        lib.contains("vfs_mount_safe(\"/disk\", \"egfs\")"),
        "EGFS 挂载点改回 \"/\" 或裸指针形态 ⇒ D5-e 引导根裁定被推翻 / NUL 终止 bug 复活"
    );
    assert!(
        read(INIT).contains(BOOT_ROOT_RAMFS),
        "init.rs 不再把引导根挂成 ramfs ⇒ Linux 式 switch_root 的前提 (引导根恒为 initramfs) 失效"
    );
}

/// 2. 三相判据各有红分支, 硬零判据经 `count_of` 取数.
#[test]
fn test_every_judgement_has_a_red_branch() {
    let script = read(BOOT_SCRIPT);
    assert!(
        script.contains("need() {") && script.contains("forbid() {"),
        "正/反判据 helper 缺失 ⇒ 判据可能只打印不判红"
    );
    assert_before(&script, "need() {", "forbid() {");
    assert!(
        script.contains("FAILED=$((FAILED + 1))") && script.contains("if [ \"$FAILED\" -ne 0 ]"),
        "失败计数未汇总到退出码 ⇒ 判红被吞"
    );
    // 取数失败 (非十进制读数) 必须显式红, 不得被"计数 0"读成通过.
    assert!(
        script.contains("'' | *[!0-9]*) return 1 ;;") && script.contains("取数失败"),
        "硬零判据把「测不出」读成「零条」⇒ 日志不可读时恒绿"
    );
    assert!(
        script.contains("grep -caF") && script.contains("grep -qaF"),
        "日志含 NUL, 计数与匹配都必须带 -a (否则按二进制处理时读数为 0)"
    );
    for needle in [DISK_MOUNT, FORMAT_DONE, FORMAT_FIRST, BLK_REGISTERED] {
        assert!(
            script.contains(&format!("need \"$P1_LOG\" \"{needle}\"")),
            "相 1 缺正向判据 {needle:?}"
        );
    }
    assert!(
        script.contains("MILESTONE='Entering EL0'")
            && script.contains("need \"$P1_LOG\" \"$MILESTONE\""),
        "相 1 未判进 EL0 ⇒ 磁盘挂上但用户态没跑也算绿"
    );
    assert!(
        script.contains(&format!("forbid \"$P1_LOG\" \"{BLK_TIMEOUT}\"")),
        "相 1 未禁 virtio-blk 超时 ⇒ D5-PR legacy 初始化序回归无锁"
    );
    assert!(
        script.contains(&format!("forbid \"$P1_LOG\" \"{MOUNT_FAIL}\"")),
        "相 1 未禁挂载失败 ⇒ NUL 终止 bug 回归无锁"
    );
}

/// 3. 持久化判据非恒真: 空白盘只新建一次, 相 2 复用同盘并禁重新格式化.
#[test]
fn test_second_boot_reuses_the_same_disk() {
    let script = read(BOOT_SCRIPT);
    assert_eq!(
        script.matches("dd if=/dev/zero of=\"$DISK\"").count(),
        1,
        "空白盘新建必须恰一次 —— 相 2 前再重置盘会让「不得重新格式化」判据恒真"
    );
    assert_before(
        &script,
        "dd if=/dev/zero of=\"$DISK\"",
        "boot_once \"$P1_LOG\"",
    );
    assert_before(&script, "boot_once \"$P1_LOG\"", "boot_once \"$P2_LOG\"");
    assert!(
        script.contains(&format!("forbid \"$P2_LOG\" \"{FORMAT_FIRST}\"")),
        "相 2 未禁重新格式化 ⇒ uberblock 写盘丢失与写盘成功读数一样"
    );
    assert!(
        script.contains(&format!("need \"$P2_LOG\" \"{DISK_MOUNT}\"")),
        "相 2 未判 /disk 仍挂上 ⇒ 二启挂载失败会被漏判"
    );
}

/// 4. 负控制在: 相 3 摘盘后 memory mode 在且 /disk 挂载行硬零.
#[test]
fn test_no_disk_negative_control_is_present() {
    let script = read(BOOT_SCRIPT);
    assert_before(&script, "boot_once \"$P2_LOG\"", "boot_once \"$P3_LOG\"");
    // 钉整行而非「裸调用 + 换行」: D5-h 给 boot_once 加了 <终态标记> 位, 旧针已脱钩;
    // 整行形态同时钉住「不带盘」与「相 3 等的是 SKIP 不是 PASS」两面.
    assert!(
        script.contains("boot_once \"$P3_LOG\" \"$SWROOT_SKIP\"\n"),
        "相 3 必须不带盘启动且以 SKIP 为终态 (追加盘参数或改等 PASS 即负控制失效)"
    );
    assert!(
        script.contains(&format!("need \"$P3_LOG\" \"{ROOT_MEM}\"")),
        "相 3 未判 memory mode ⇒ 摘盘负控制不成立"
    );
    assert!(
        script.contains(&format!("forbid \"$P3_LOG\" \"{DISK_MOUNT_PREFIX}\"")),
        "相 3 未禁 /disk 挂载行 ⇒ 磁盘分支被无条件接管也能全绿"
    );
}

/// 5. 判据落在真启动产物上; 架构自检早于启动; x86_64 显式拒绝; 恰挂 1 块盘.
#[test]
fn test_leg_runs_on_real_aarch64_boot_with_one_disk() {
    let script = read(BOOT_SCRIPT);
    assert!(
        script.contains("boot_module_build aarch64")
            && script.contains("boot_module_carrier_args aarch64"),
        "D4 载体: 必须构建出货默认核并经 boot-module 交付 cpio (不嵌入内核)"
    );
    assert!(
        script.contains("-serial \"file:$log\""),
        "判据必须来自本架构真启动的串口日志"
    );
    assert_before(
        &script,
        "verify_image_arch.py --arch aarch64",
        "-serial \"file:$log\"",
    );
    assert_before(&script, "x86_64)", "exit 1");
    assert!(
        script.contains("本腿判据对象是 aarch64 /disk 挂载"),
        "x86_64 必须显式拒绝 (红), 不能静默跳过 —— 静默跳过在 full 档读起来和通过一样"
    );
    assert_eq!(
        script
            .matches("-device virtio-blk-device,drive=vd0")
            .count(),
        1,
        "恰挂 1 块盘, 注册数判据的 \"1\" 才有依据"
    );
    assert!(
        script.contains("if=none,id=vd0,format=raw"),
        "盘必须以 raw 空白镜像交付 (内核自格式化, 无 host mkfs 依赖)"
    );
}

/// 6. full 档步骤 4 真调用本脚本, 失败即 err, 且有独立取证日志.
#[test]
fn test_audit_full_invokes_egfs_disk_leg_with_err_on_fail() {
    let audit = read(AUDIT_SCRIPT);
    assert!(
        audit.contains("scripts/aarch64_egfs_disk_boot_test.sh\" aarch64 2>&1"),
        "ci/audit.sh 未真调用本腿脚本 (缺调用即本腿空转)"
    );
    assert_eq!(
        audit.matches("aarch64_egfs_disk_boot_test.sh").count(),
        2,
        "本腿在 audit 里应只有「注释点名脚本 + 真调用」两处, 实得 {} 处",
        audit.matches("aarch64_egfs_disk_boot_test.sh").count()
    );
    assert_before(
        &audit,
        "step \"4/6 QEMU 双架构真实启动测试\"",
        "aarch64_egfs_disk_boot_test.sh\" aarch64 2>&1",
    );
    assert!(
        audit.contains("err \"aarch64 EGFS 磁盘挂载真跑腿失败"),
        "本腿失败必须走 err (audit.sh 的 err 内建 exit 1, 判红不被吞)"
    );
    assert!(
        audit.contains("audit_aarch64_egfs_disk_boot.log"),
        "本腿必须有独立取证日志 —— 偶发红要靠完整判定链归因"
    );
}

/// 7. D5-g 磁盘根首启安装: 真接线 + 幂等 + 回读, 三相各有判据.
///
/// 只判「挂上 /disk」证不了盘上有可执行体: 用户态切根后要靠 `/disk/init`. 本针把
/// 安装动作的接线、幂等跳过、sync 后回读三处机制与三相判据一起钉住.
#[test]
fn test_disk_root_install_is_wired_idempotent_and_judged() {
    let script = read(BOOT_SCRIPT);
    let lib = read(LIB);
    let init = read(INIT);
    assert!(
        lib.contains("crate::functions::init::install_disk_root();"),
        "lib.rs 未调用 install_disk_root ⇒ 磁盘根永不安装, 切根后无可执行体"
    );
    assert_before(
        &lib,
        "vfs_mount_safe(\"/disk\", \"egfs\")",
        "crate::functions::init::install_disk_root();",
    );
    // 函数体单独切出: 否则 read_file_kernel 一类针会被相邻函数满足而恒真.
    let install = init
        .split("pub fn install_disk_root()")
        .nth(1)
        .expect("install_disk_root 定义")
        .split("// ====")
        .next()
        .expect("install_disk_root 函数体");
    assert!(
        install.contains("vfs_stat_safe(c\"/disk/init\".as_ptr().cast(), 0).is_some()"),
        "install_disk_root 失去幂等跳过 ⇒ 二启重装会覆盖用户改动"
    );
    assert!(
        install.contains("unpack(blob, \"/disk\", 0)"),
        "安装必须把归档解到 /disk (解到 \"/\" 等于没装)"
    );
    assert!(
        install.contains("vfs_sync()"),
        "安装后未 sync ⇒ 回读可能命中缓存而非盘上字节"
    );
    assert!(
        install.contains("read_file_kernel("),
        "install_disk_root 未回读 /disk/init ⇒ 写路径静默丢数据无判据"
    );
    for (phase, kind, needle) in [
        ("$P1_LOG", "need", DISK_ROOT_INSTALLED),
        ("$P1_LOG", "need", DISK_INIT_READBACK),
        ("$P1_LOG", "forbid", DISK_ROOT_INCOMPLETE),
        ("$P2_LOG", "need", DISK_ROOT_SKIP),
        ("$P2_LOG", "forbid", DISK_ROOT_INSTALLED),
        ("$P3_LOG", "forbid", DISK_ROOT_INSTALLED),
        ("$P3_LOG", "forbid", DISK_ROOT_INCOMPLETE),
    ] {
        assert!(
            script.contains(&format!("{kind} \"{phase}\" \"{needle}\"")),
            "{phase} 缺 {kind} 判据 {needle:?} (D5-g 安装面漏判)"
        );
    }
}

/// 8. D5-h 切根终态标记 + 三相三态判据 (删任一判据仍会让真跑腿空绿).
#[test]
fn test_switch_root_terminal_marker_and_three_state_judgements() {
    let script = read(BOOT_SCRIPT);
    let user_init = read(USER_INIT);
    for (var, marker) in [
        ("SWROOT_PASS", SWROOT_PASS),
        ("SWROOT_SKIP", SWROOT_SKIP),
        ("SWROOT_FAIL", SWROOT_FAIL),
    ] {
        assert!(
            user_init.contains(marker),
            "用户态 init 不再产出 {marker:?} ⇒ 切根判据与产出方脱钩"
        );
        assert!(
            script.contains(&format!("{var}='{marker}'")),
            "腿侧 {var} 定义与用户态产出串不同源 ⇒ 判据空转"
        );
    }
    // 终态标记占 boot_once 第 2 位, 其余才是 qemu 额外参数 (错位会把标记当盘参数).
    assert!(
        script.contains("local log=\"$1\" until=\"$2\"; shift 2"),
        "boot_once 未把终态标记取作第 2 位参数 ⇒ 相 3 的 SKIP 会被当成 qemu 参数"
    );
    assert!(
        script.contains("grep -qaF \"$until\" \"$log\""),
        "收机条件未等本相终态 ⇒ 只等 EL0 会在切根发生前收机, 判据恒绿"
    );
    for phase in ["$P1_LOG", "$P2_LOG"] {
        assert!(
            script.contains(&format!(
                "boot_once \"{phase}\" \"$SWROOT_PASS\" \"${{DISK_ARGS[@]}}\""
            )),
            "{phase} 必须以 PASS 为终态且带盘 (改等 EL0 或摘盘即测不到切根)"
        );
    }
    for (phase, kind, var) in [
        ("$P1_LOG", "need", "$SWROOT_PASS"),
        ("$P1_LOG", "forbid", "$SWROOT_FAIL"),
        ("$P1_LOG", "forbid", "$SWROOT_SKIP"),
        ("$P2_LOG", "need", "$SWROOT_PASS"),
        ("$P2_LOG", "forbid", "$SWROOT_FAIL"),
        ("$P2_LOG", "forbid", "$SWROOT_SKIP"),
        ("$P3_LOG", "need", "$SWROOT_SKIP"),
        ("$P3_LOG", "forbid", "$SWROOT_PASS"),
        ("$P3_LOG", "forbid", "$SWROOT_FAIL"),
    ] {
        assert!(
            script.contains(&format!("{kind} \"{phase}\" \"{var}\"")),
            "{phase} 缺 {kind} 判据 {var} (D5-h 三态判别漏判)"
        );
    }
}
