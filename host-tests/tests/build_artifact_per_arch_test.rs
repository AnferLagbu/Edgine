//! P-10: 内核产物按架构分目录 (build artifact per-arch isolation)
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-10.
//!
//! 背景: 内核产物 (`kernel.bin` / `kernel.flat` / `kernel-aarch64.img` /
//! `kernel.map` / 中间 `.o` / `stage1.bin` / ISO) 曾与 S-6 修复前的用户态产物
//! 一样, 两架构共用 `other/build/` 一条平路径. 后果三重:
//!   1. FP-06 审计反汇编 `other/build/kernel.bin` 求 aarch64, 在
//!      `./ci/build.sh all` 之后必然读到 x86_64 产物而 fail-closed 报假阳性;
//!   2. `ci/build.sh` 的 `all` 分支因此"仅链接主架构" ⇒ aarch64 的链接期错误
//!      在全量口径下完全不可见;
//!   3. 防跨架构误用 `boot.o` 需要三重补丁 (Makefile `arch-switch-clean` +
//!      `log/.arch` 戳记 + `qemu_boot_test.sh` 的 `sync_make_state`).
//!
//! 最隐蔽的一条: `x86_64.ld` 按**文件名**点名 `boot.o` / `isr.o`, 路径失配时
//! ld 既不报错也不警告, 对应 `.text` 静默落进 `*(.text)` 通配区 —— `_start`
//! 离开 `.boot.text`, isr 入口代码离开 KPTI 映射区段 (`_kpti_trampoline_end`
//! 之前), 直到用户态取指才 #PF.
//!
//! 本测试锁住根治后的形态: 产物路径统一由 Makefile 的 `ARCH_BUILD_DIR` 派生,
//! 平路径不再被任何构建/审计/启动脚本引用, 且跨架构补丁机制已退役.

use std::fs;
use std::path::Path;

const MAKEFILE: &str = "../Makefile";
const LD_X86: &str = "../src/kernel/privileged/link/x86_64.ld";
const BUILD_SH: &str = "../ci/build.sh";
const FP06_PY: &str = "../scripts/audit_aarch64_kernel_fp_free.py";
const BOOT_SH: &str = "../scripts/qemu_boot_test.sh";
const KERNEL_BUILD_RS: &str = "../src/kernel/build.rs";
const DISPATCH_RS: &str = "../src/kernel/privileged/syscall/dispatch.rs";

/// 曾被两架构共用的内核产物文件名 (迁移后一律带 `<arch>/` 段).
const SHARED_ARTIFACTS: &[&str] = &[
    "kernel.bin",
    "kernel.flat",
    "kernel-aarch64.img",
    "kernel.map",
    "kernel_test.bin",
    "kernel_chaos.bin",
    "stage1.bin",
    "edgine.iso",
    "edgine_test.iso",
    "edgine_chaos.iso",
    "boot.o",
    "entry.o",
    "isr.o",
    "switch.o",
    "main.o",
    "gdt_asm.o",
];

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path} failed: {e}"))
}

/// 剥掉整行注释 (`#` / `//` / `/*` / 块注释续行 `*`), 只留实际生效的指令行.
///
/// 项目的历史与动机注释大量引用旧路径 (正是本工程要锁死的对象), 它们不构成
/// 引用点, 因此扫描前必须剔除; 未剔除的注释会让本测试退化为"注释不许提旧事".
fn strip_comment_lines(src: &str) -> Vec<String> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with("//") && !l.starts_with("/*"))
        .filter(|l| !l.starts_with('*'))
        .map(String::from)
        .collect()
}

/// 从 Makefile 解析 `BUILD_DIR := <value>`, 作为产物根路径的唯一来源.
/// 断言不得自行硬编码 "other/build", 否则改根目录时测试与构建会一起撒谎.
fn makefile_build_dir() -> String {
    let mk = read(MAKEFILE);
    for raw in mk.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("BUILD_DIR :=") {
            return rest.trim().to_string();
        }
    }
    panic!("Makefile 未定义 BUILD_DIR := <path>, 产物根路径失去单一来源")
}

/// 扫描一组文件, 返回仍引用 `<build_dir>/<裸产物名>` 的行.
fn find_shared_path_refs(build_dir: &str) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![
        root.join(MAKEFILE),
        root.join(LD_X86),
        root.join(BUILD_SH),
        root.join(FP06_PY),
        root.join(BOOT_SH),
        root.join(KERNEL_BUILD_RS),
        root.join(DISPATCH_RS),
    ];
    if let Ok(rd) = fs::read_dir(root.join("../scripts")) {
        for ent in rd.flatten() {
            let p = ent.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // 隐藏文件 (.tmp_*) 与 __pycache__ 不参与门禁口径.
            if name.starts_with('.') {
                continue;
            }
            if p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "sh" || e == "py")
            {
                files.push(p);
            }
        }
    }
    if let Ok(rd) = fs::read_dir(root.join("../.github/workflows")) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) == Some("yml") {
                files.push(p);
            }
        }
    }

    let mut hits = Vec::new();
    for f in files {
        let Ok(src) = fs::read_to_string(&f) else {
            continue;
        };
        let shown = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        for line in strip_comment_lines(&src) {
            for art in SHARED_ARTIFACTS {
                // 只拦"根路径直挂产物"的平形态; `<build_dir>/<arch>/...` 与
                // `<build_dir>/log` / `<build_dir>/$(ARCH)` 等合法形态不在此列.
                if line.contains(&format!("{build_dir}/{art}")) {
                    hits.push(format!("{shown}: {}", line.trim()));
                }
            }
        }
    }
    hits
}

#[test]
fn test_makefile_declares_arch_scoped_build_dir() {
    let mk = read(MAKEFILE);
    assert!(
        mk.contains("ARCH_BUILD_DIR := $(BUILD_DIR)/$(ARCH)"),
        "Makefile 必须把内核产物根定义为 $(BUILD_DIR)/$(ARCH) 的派生量 (P-10 单一来源)"
    );
    // 正例: 内核产物规则一律走 ARCH_BUILD_DIR.
    for art in ["kernel.bin", "kernel.flat", "kernel.map", "stage1.bin"] {
        assert!(
            mk.contains(&format!("$(ARCH_BUILD_DIR)/{art}")),
            "Makefile 中 {art} 未走 $(ARCH_BUILD_DIR)/ —— 会退回跨架构共用路径"
        );
    }
}

#[test]
fn test_no_shared_kernel_artifact_paths_remain() {
    let build_dir = makefile_build_dir();
    let hits = find_shared_path_refs(&build_dir);
    assert!(
        hits.is_empty(),
        "仍有文件引用两架构共用的内核产物路径 {build_dir}/<裸产物名> (P-10 未收口):\n{}",
        hits.join("\n")
    );
}

#[test]
fn test_cross_arch_patch_mechanism_retired() {
    let mk = strip_comment_lines(&read(MAKEFILE));
    let joined = mk.join("\n");
    for gone in ["arch-switch-clean", "PREVIOUS_ARCH", "ARCH_CHANGED"] {
        assert!(
            !joined.contains(gone),
            "Makefile 仍含跨架构补丁机制 {gone} —— 产物分目录后该机制无对象可补, 须退役"
        );
    }
    // 启动脚本侧的对等防线同步退役, 但产物内容自检 (S-6 防线③) 必须保留.
    let boot = strip_comment_lines(&read(BOOT_SH)).join("\n");
    assert!(
        !boot.contains("sync_make_state"),
        "qemu_boot_test.sh 仍调用 sync_make_state (跨架构戳记反查) —— 应已由 ensure_arch_artifacts 取代"
    );
    assert!(
        boot.contains("ensure_arch_artifacts"),
        "qemu_boot_test.sh 缺少 ensure_arch_artifacts: 启动前幂等重建本架构产物的入口"
    );
    assert!(
        boot.contains("verify_image_arch"),
        "qemu_boot_test.sh 丢失 verify_image_arch (S-6 防线③) —— 目录隔离取代的是路径补丁, 不是产物内容自检"
    );
}

#[test]
fn test_linker_script_object_paths_track_build_dir() {
    // ld 的按文件名输入段选取失配时静默通过, 故 .ld 的字面路径必须与 Makefile
    // 的产物根逐字对齐. 这里从 Makefile 反推期望值, 不在测试里二次硬编码.
    let build_dir = makefile_build_dir();
    let ld = read(LD_X86);
    for obj in ["boot.o", "isr.o"] {
        let expected = format!("{build_dir}/x86_64/{obj}");
        assert!(
            ld.contains(&expected),
            "x86_64.ld 未按 {expected} 点名输入文件 —— 该 .o 的 .text 会静默落进 *(.text) 通配区"
        );
        let stale = format!("{build_dir}/{obj}(");
        assert!(
            !ld.contains(&stale),
            "x86_64.ld 仍含共用路径模式 {stale} —— ld 会去加载这个不该存在的文件, \
             与 ARCH_BUILD_DIR 下的真产物一起被链进镜像"
        );
    }
    // .boot.text 与 KPTI 区段的排序契约由 kpti_x86_user_table_test 细查, 此处只锁路径来源.
}

#[test]
fn test_linker_script_is_kernel_bin_prerequisite() {
    // 改 .ld 必须触发重链, 否则新布局与旧镜像静默共存 (ld 不报错).
    let mk = read(MAKEFILE);
    let rule = mk
        .lines()
        .find(|l| l.trim_start().starts_with("$(ARCH_BUILD_DIR)/kernel.bin:"))
        .unwrap_or_else(|| panic!("Makefile 缺少 $(ARCH_BUILD_DIR)/kernel.bin 规则"));
    assert!(
        rule.contains("$(LDSCRIPT)"),
        "kernel.bin 未依赖 $(LDSCRIPT): 改链接脚本不会重链 —— {rule}"
    );
}

#[test]
fn test_build_sh_all_links_both_architectures() {
    // 旧规避写法"只链主架构"会让 aarch64 的链接期错误在全量口径下隐身.
    let sh = read(BUILD_SH);
    let at = sh
        .find("all)")
        .unwrap_or_else(|| panic!("ci/build.sh 缺少 all) 分支"));
    let body = &sh[at..sh[at..].find(";;").map_or(sh.len(), |e| at + e)];
    assert!(
        body.contains("link_kernel \"x86_64\""),
        "ci/build.sh 的 all 分支未链接 x86_64"
    );
    assert!(
        body.contains("link_kernel \"aarch64\""),
        "ci/build.sh 的 all 分支未链接 aarch64 —— 产物已分目录, 不存在互相覆盖, \
         不链接即是链接期错误盲区"
    );
}

#[test]
fn test_fp06_audit_reads_arch_scoped_artifact() {
    let py = read(FP06_PY);
    assert!(
        py.contains("\"aarch64\", \"kernel.bin\""),
        "FP-06 审计的产物路径必须锁定 other/build/aarch64/kernel.bin"
    );
    assert!(
        !py.contains("\"build\", \"kernel.bin\""),
        "FP-06 审计仍读共用路径 kernel.bin —— `./ci/build.sh all` 后必为 x86_64 产物, 假阳性"
    );
}

#[test]
fn test_embedded_stage1_path_is_arch_scoped() {
    // stage1.bin 由 build.rs 的存在性检查 + dispatch.rs 的 include_bytes! 两处引用,
    // 必须与 Makefile 产物根同源, 否则嵌入的是另一架构的引导码或直接编译失败.
    let build_dir = makefile_build_dir();
    let expected = format!("{build_dir}/x86_64/stage1.bin");
    assert!(
        read(KERNEL_BUILD_RS).contains(&expected),
        "src/kernel/build.rs 的 stage1 检查路径未跟到 {expected}"
    );
    assert!(
        read(DISPATCH_RS).contains("../../../../other/build/x86_64/stage1.bin"),
        "dispatch.rs 的 include_bytes! 未指向 other/build/x86_64/stage1.bin"
    );
}
