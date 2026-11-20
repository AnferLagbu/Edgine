// SPDX-License-Identifier: MPL-2.0
//! R3 回归: reboot 命令语义与双路径形态契约.
//!
//! 缺陷形态 (安全缺口册 R3 调研读出): `privileged::syscall::api::reboot_mechanism`
//! 把命令码写成 0=停机自旋 / 1=重启, 而用户侧 `eash reboot` 传 0、`halt` 传 1,
//! `install` 完成页也传 0 期望重启 ⇒ `reboot` 命令实际停机、`halt` 实际重启,
//! 且内核重启机制 (x86_64 IDT / aarch64 PSCI) 只在 `halt` 命令下才被触达.
//!
//! 修复形态: 命令码上收 `edgine-abi` 单一权威源, 内核分派与三处调用方全部引用
//! 同一组常量; x86_64 伪描述符改 10 字节, aarch64 转发给仓内唯一 PSCI 实现.
//! 本文件锁定"语义值 + 两侧引用形态", 防止再次各写裸数字而写反.

use std::fs;
use std::path::PathBuf;

const API: &str = "src/kernel/privileged/syscall/api.rs";
const RAW: &str = "src/kernel/privileged/syscall/mod.rs";
const EASH: &str = "src/user/eash/src/commands/system.rs";
const INSTALL_FINISH: &str = "src/user/install/src/wizard/finish.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史说明被当作代码形态.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 空白不敏感 (rustfmt 会折行, 语义仍等价).
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn reboot_cmd_values_are_user_facing_semantics() {
    assert_eq!(
        edgine_abi::reboot::REBOOT_CMD_RESTART,
        0,
        "重启命令码必须是 0 —— 用户态 eash reboot / install 完成页按 0 调用"
    );
    assert_eq!(
        edgine_abi::reboot::REBOOT_CMD_HALT,
        1,
        "停机命令码必须是 1 —— 用户态 eash halt 按 1 调用"
    );
}

#[test]
fn kernel_dispatch_maps_restart_to_real_reboot_path() {
    let src = norm(&code_only(&read_src(API)));
    let restart = "edgine_abi::reboot::REBOOT_CMD_RESTART =>";
    let halt = "edgine_abi::reboot::REBOOT_CMD_HALT =>";
    let restart_at = src
        .find(restart)
        .unwrap_or_else(|| panic!("reboot_mechanism 未按单一权威源常量分派重启命令: {src}"));
    let halt_at = src
        .find(halt)
        .unwrap_or_else(|| panic!("reboot_mechanism 未按单一权威源常量分派停机命令: {src}"));
    let restart_arm = &src[restart_at..halt_at];
    assert!(
        restart_arm.contains("super::raw::reboot_via_idt")
            && restart_arm.contains("super::raw::reboot_via_psci"),
        "重启命令分支必须落到 x86_64/aarch64 真复位路径: {restart_arm}"
    );
    let halt_arm = &src[halt_at..];
    assert!(
        halt_arm.contains("core::hint::spin_loop()"),
        "停机命令分支必须是自旋, 不得调用复位机制: {halt_arm}"
    );
    assert!(
        !restart_arm.contains("spin_loop"),
        "重启命令分支不得退化为停机自旋: {restart_arm}"
    );
}

#[test]
fn user_side_callers_use_shared_constants() {
    let eash = norm(&code_only(&read_src(EASH)));
    assert!(
        eash.contains("sys_reboot(REBOOT_CMD_RESTART)"),
        "eash reboot 必须以 REBOOT_CMD_RESTART 调用: {eash}"
    );
    assert!(
        eash.contains("sys_reboot(REBOOT_CMD_HALT)"),
        "eash halt 必须以 REBOOT_CMD_HALT 调用: {eash}"
    );
    assert!(
        !eash.contains("sys_reboot(0)") && !eash.contains("sys_reboot(1)"),
        "eash 调用方不得再写裸命令码: {eash}"
    );

    let install = norm(&code_only(&read_src(INSTALL_FINISH)));
    assert!(
        install.contains("sys::reboot(sys::REBOOT_CMD_RESTART)"),
        "install 完成页必须以 REBOOT_CMD_RESTART 调用: {install}"
    );
}

#[test]
fn reboot_paths_keep_gap_free_shapes() {
    let raw_all = read_src(RAW);
    let raw = norm(&code_only(&raw_all));
    assert!(
        raw.contains("#[repr(C, packed)] struct Idtr { limit: u16, base: u64, }"),
        "x86_64 IDTR 伪描述符必须是 packed 的 10 字节形态: {raw}"
    );
    assert!(
        raw.contains("crate::privileged::arch::aarch64::psci::system_reset()"),
        "aarch64 reboot 必须转发给仓内唯一 PSCI 实现: {raw}"
    );
    assert!(
        !raw.contains("svc #0"),
        "aarch64 reboot 不得在 syscall 层重写 SMCCC 汇编 (功能号/conduit/clobber 三错): {raw}"
    );
    assert!(
        !raw_all.contains("已登记缺口 (批6 待裁, 本批不改)"),
        "R3 修完后不得残留本批的缺口登记段"
    );
}
