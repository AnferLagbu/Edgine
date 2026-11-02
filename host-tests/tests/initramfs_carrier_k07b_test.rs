// SPDX-License-Identifier: MPL-2.0
//! K-07b 批1 initramfs 载体回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-07b 批1.
//!
//! 本批将「PID 1 从文件系统来」这条链第一次接通并锁死: cpio newc 归档由
//! `scripts/make_initramfs_cpio.py` 在构建期生成 → `make` 把它作为内核镜像的依赖
//! → 内核以 `include_bytes!` 按**本架构**嵌入 → 启动末尾解析归档、逐级建父目录、
//! 落盘到 ramfs → 从 VFS 读出 `/init` → ELF 装载进 Ring 3 / EL0.
//!
//! 该链的三处形态各自都有静默失效通道, 故逐处钉死:
//!
//! 1. **构建期** —— `user` 目标原先用配方里的内联 `@cp` 落产物, 那不能成为归档
//!    规则的依赖边: 归档会静默少打几个文件而 make 依旧绿. 现全部转成 `.bin` 文件
//!    规则, 并让 `$(USER_CPIO)` 与 `$(RUST_LIB)` 都显式依赖产物.
//! 2. **装载期** —— 归档条目名前缀不统一 (`./bin/eash` / `bin/eash` / `/bin/eash`),
//!    ramfs 的 mkdir 又只建叶子; 不归一 + 不补父目录 ⇒ 嵌套条目整批静默丢失.
//! 3. **运行期** —— 解包发生在**首个用户进程建立之前**, 而 `vfs_open` 的 fd 挂在
//!    当前进程 fd 表上, 此时 `with_current_fd_table` 返 `None` ⇒ open 必失败.
//!    实测症状正是"归档解析成功但只落 1 个条目、`/init` 不存在". 落盘与读取因此
//!    必须走 `fs_create` / `Inode::write` / `Inode::read` 的无 fd 形态.
//!
//! feature 默认关闭 (内嵌 `init.bin` 仍是出货路径), 所以真跑判据不在默认门禁里,
//! 由 `ci/audit.sh` full 档的专项 feature 腿 (`scripts/initramfs_boot_test.sh`) 承担.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const CPIO_PY: &str = "scripts/make_initramfs_cpio.py";
const MAKEFILE: &str = "Makefile";
const INITRAMFS_RS: &str = "src/kernel/functions/fs/initramfs.rs";
const FS_API_RS: &str = "src/kernel/functions/fs/api.rs";
const INIT_RS: &str = "src/kernel/functions/init.rs";
const BUILD_RS: &str = "src/kernel/build.rs";
const KERNEL_CARGO: &str = "src/kernel/Cargo.toml";
const FUNC_CARGO: &str = "src/kernel/functions/Cargo.toml";
const CPIO_SH: &str = "scripts/initramfs_boot_test.sh";
const AUDIT_SH: &str = "ci/audit.sh";

/// 归档必须覆盖的用户态程序 (与 `ARCHIVE_LAYOUT` 同源).
const CARRIER_MEMBERS: &[&str] = &[
    "init.bin",
    "eash.bin",
    "proctest.bin",
    "install.bin",
    "fbterm.bin",
    "httpsrv.bin",
];

/// Makefile 里的 ELF → `.bin` 文件规则对 (左为 ELF 变量名, 右为落地产物名).
const USER_BIN_RULES: &[(&str, &str)] = &[
    ("USER_INIT_ELF", "init.bin"),
    ("USER_SHELL_ELF", "eash.bin"),
    ("USER_TEST_ELF", "proctest.bin"),
    ("USER_INSTALL_ELF", "install.bin"),
    ("USER_FBTERM_ELF", "fbterm.bin"),
    ("USER_HTTPSRV_ELF", "httpsrv.bin"),
];

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释: 锁定项是代码形态, 不是"注释里不许提旧事".
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 压缩连续空白为单空格, 使断言不受换行/缩进/续行影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_contains(hay: &str, needle: &str, why: &str) {
    assert!(
        hay.contains(needle),
        "{why}\n  期望包含: {needle}\n  实际片段: {}",
        hay.chars().take(400).collect::<String>()
    );
}

fn assert_absent(hay: &str, needle: &str, why: &str) {
    assert!(!hay.contains(needle), "{why}\n  不应出现: {needle}");
}

fn count_needle(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// 取 `name` 所在函数从首个 `{` 到配对 `}` 的源码 (已剥注释, 故花括号只来自代码).
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(name)
        .unwrap_or_else(|| panic!("函数 {name} 不存在 (形态漂移?)"));
    let bytes: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut begun = false;
    let mut out = String::new();
    for c in &bytes {
        out.push(*c);
        match c {
            '{' => {
                depth += 1;
                begun = true;
            }
            '}' if begun => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(begun, "函数 {name} 未找到函数体");
    out
}

// ── 1. 生成器: 自测 + 归档布局 ─────────────────────────────────────────────

#[test]
fn test_cpio_generator_self_test_passes() {
    let out = Command::new("python3")
        .arg(CPIO_PY)
        .arg("--self-test")
        .current_dir(repo_root())
        .output()
        .expect("启动 python3 失败 (cpio 生成器无法执行)");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "cpio 生成器自测失败: rc={:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    assert_contains(&stdout, "自测通过", "自测应打印通过横幅");
}

#[test]
fn test_generator_layout_covers_carrier_members() {
    let py = read_src(CPIO_PY);
    assert_contains(&py, "HEADER_SIZE = 110", "newc 头固定 110 字节");
    assert_contains(&py, "TRAILER_NAME = \"TRAILER!!!\"", "归档以 TRAILER 结束");
    assert_contains(
        &py,
        "REQUIRED_SRC = \"init.bin\"",
        "init.bin 缺失必须硬失败",
    );
    for member in CARRIER_MEMBERS {
        assert_contains(&py, member, &format!("归档布局须包含 {member}"));
    }
    // 名字一律带 `./` 前缀写出 (GNU cpio 口径), 由内核侧归一 —— 两侧口径必须一致
    assert_contains(&py, "f\"./{name}\"", "生成器按 `./` 前缀写条目名");
    assert_contains(
        &py,
        "removeprefix(\"./\")",
        "自测重解析须剥去同一前缀 (与内核归一同口径)",
    );
}

// ── 2. Makefile: 归档是真依赖边, 不是配方里的内联 cp ───────────────────────

#[test]
fn test_makefile_archives_all_user_binaries() {
    let mk = read_src(MAKEFILE);
    let one = mk.replace('\\', " ");
    let one = norm(&one);
    assert_contains(
        &one,
        "USER_CPIO = $(USER_BUILD_DIR)/initramfs.cpio",
        "归档路径必须落在按架构分目录的 user 产物目录 (S-6 / P-10 口径)",
    );
    for member in CARRIER_MEMBERS {
        assert_contains(
            &one,
            &format!("$(USER_BUILD_DIR)/{member}"),
            &format!("归档规则依赖面须包含 {member}"),
        );
    }
    assert_contains(
        &one,
        "$(MK_CPIO_PY)",
        "归档规则必须以生成器脚本为依赖 (脚本改动即重打包)",
    );
    assert_contains(
        &one,
        "@python3 $(MK_CPIO_PY) --arch $(ARCH) --src $(USER_BUILD_DIR) --out $@",
        "归档配方须按本架构目录取源",
    );
}

#[test]
fn test_makefile_user_and_kernel_targets_depend_on_cpio() {
    let mk = read_src(MAKEFILE);
    // 两架构各一条 user 目标 + 两条 $(RUST_LIB) 规则, 都必须挂上归档
    assert_eq!(
        count_needle(&mk, "user: $(USER_BUILD_DIR)/init.bin"),
        2,
        "x86_64 与 aarch64 的 user 目标都要产出归档"
    );
    assert_eq!(
        count_needle(&mk, "$(USER_CPIO)"),
        5,
        "归档引用点 = 2 条 user 目标 + 2 条 $(RUST_LIB) 规则 + 归档规则自身"
    );
    // ELF → .bin 的文件规则对 (两架构各一份, 内容同形). 用内联 @cp 落产物不能成为
    // 归档规则的依赖边 —— 归档会静默少打几个文件而 make 依旧绿.
    for (elf_var, bin) in USER_BIN_RULES {
        assert_contains(
            &mk,
            &format!("$(USER_BUILD_DIR)/{bin}: $({elf_var})"),
            &format!("{elf_var} 必须以文件规则落 {bin} (供归档依赖)"),
        );
    }
}

// ── 3. 内核解包: 归一 + 补父目录 + 无 fd 落盘 ──────────────────────────────

#[test]
fn test_unpack_normalizes_names_and_creates_parents() {
    let code = code_only(&read_src(INITRAMFS_RS));
    assert_contains(
        &code,
        "fn normalize_entry_path(name: &[u8], out: &mut [u8]) -> Option<usize>",
        "条目名必须先归一为根内绝对路径",
    );
    assert_contains(
        &code,
        "b\"..\" => return None",
        "`..` 段整条拒收 (归档不得逃出根前缀)",
    );
    assert_contains(
        &code,
        "fn ensure_parent_dirs(path: &mut [u8], path_len: usize, pwm: u64)",
        "ramfs mkdir 只建叶子, 嵌套条目须逐级补父目录",
    );
    assert_contains(
        &code,
        "ensure_parent_dirs(&mut path_buf, path_len, pwm);",
        "落盘前先补父目录链",
    );
}

#[test]
fn test_unpack_and_read_are_fd_free() {
    // 无 fd 是**启动期约束**, 不是风格偏好: vfs_open 的 fd 挂在当前进程 fd 表,
    // 解包时尚无进程 ⇒ 依赖 fd 的写法一律静默失败.
    let unpack = code_only(&read_src(INITRAMFS_RS));
    for fd_call in ["vfs_open(", "vfs_write(", "vfs_close(", "vfs_read("] {
        assert_absent(&unpack, fd_call, &format!("解包不得依赖 fd: {fd_call}"));
    }
    assert_contains(
        &unpack,
        "super::api::write_file_kernel(path_str, entry.data, pwm)",
        "常规文件须走无 fd 落盘",
    );

    let api = code_only(&read_src(FS_API_RS));
    let writer = fn_body(&api, "pub fn write_file_kernel");
    let reader = fn_body(&api, "pub fn read_file_kernel");
    for (name, body) in [
        ("write_file_kernel", &writer),
        ("read_file_kernel", &reader),
    ] {
        for fd_call in ["vfs_open", "vfs_read", "vfs_write", "vfs_close"] {
            assert_absent(body, fd_call, &format!("{name} 不得经 fd 表"));
        }
        assert_contains(
            body,
            "VFS_MANAGER.resolve_mount_fs(resolved)",
            &format!("{name} 必须按挂载点解析后直取 FileSystem trait"),
        );
    }
    assert_contains(
        &writer,
        "fs.fs_create(parent_path, name, pwm)",
        "建文件走 fs_create",
    );
    assert_contains(
        &writer,
        "inode.write(0, data, pwm)",
        "写数据走 Inode::write",
    );
    assert_contains(&reader, "fs.fs_stat(rel_path, pwm)", "读取前按 stat 定长");
    assert_contains(
        &reader,
        "inode.read(0, &mut buf, pwm)",
        "读数据走 Inode::read",
    );
}

// ── 4. init 编排: 双架构都走载体, 产物按架构嵌入 ───────────────────────────

#[test]
fn test_init_embeds_carrier_per_arch() {
    let code = code_only(&read_src(INIT_RS));
    assert_contains(
        &code,
        "#[cfg(all(feature = \"initramfs\", target_os = \"none\", target_arch = \"x86_64\"))]",
        "x86_64 分支按本架构嵌入归档",
    );
    assert_contains(
        &code,
        "#[cfg(all(feature = \"initramfs\", target_os = \"none\", target_arch = \"aarch64\"))]",
        "aarch64 分支按本架构嵌入归档 (与 x86_64 同形, 不得只覆盖一架构)",
    );
    assert_contains(
        &code,
        "other/build/x86_64/user/initramfs.cpio",
        "x86_64 嵌入路径含架构段",
    );
    assert_contains(
        &code,
        "other/build/aarch64/user/initramfs.cpio",
        "aarch64 嵌入路径含架构段",
    );
    assert_contains(
        &code,
        "#[cfg(all(feature = \"initramfs\", not(target_os = \"none\")))]",
        "host 侧取空切片 (host 编译不得硬依赖裸机产物)",
    );
}

#[test]
fn test_init_loads_init_from_filesystem_on_both_arms() {
    let code = code_only(&read_src(INIT_RS));
    // read_file_kernel 的调用被 rustfmt 折成多行, 空白归一后再比对
    let flat = norm(&code);
    assert_eq!(
        count_needle(&code, "let pid = load_init_from_initramfs();"),
        2,
        "x86_64 与 aarch64 两条 boot 分支都必须先试载体"
    );
    assert_contains(
        &flat,
        "crate::functions::fs::api::read_file_kernel( \"/init\",",
        "PID 1 的镜像必须从 VFS 读出 (无 fd 路径)",
    );
    assert_contains(
        &flat,
        "\"/init\", 0, crate::functions::proc::exec::ELF_MAX_SIZE,",
        "按 ELF 尺寸上限读取 /init",
    );
    assert_absent(
        &code,
        "read_file_to_vec",
        "载体路径不得依赖需要 fd 的 read_file_to_vec",
    );
    assert_eq!(
        count_needle(&code, "falling back to init.bin"),
        2,
        "两架构都保留内嵌 init.bin 回退, 且回退点必须可观测"
    );
    assert_eq!(
        count_needle(&code, "init from /init"),
        2,
        "两架构的成功里程碑各一条 (专项 feature 腿按该串断言)"
    );
}

// ── 5. build.rs: feature 点亮时按本架构要求归档 ────────────────────────────

#[test]
fn test_build_rs_requires_carrier_when_feature_on() {
    let code = code_only(&read_src(BUILD_RS));
    assert_contains(
        &code,
        "std::env::var(\"CARGO_FEATURE_INITRAMFS\")",
        "仅 feature 点亮时要求归档 (默认构建不该被它绑住)",
    );
    assert_contains(
        &code,
        "user/initramfs.cpio",
        "检查路径与本模块 include 同源",
    );
    assert_contains(&code, "cargo:rerun-if-changed={}", "必须发出重编译触发器");
    assert_contains(
        &code,
        "cpio.display()",
        "rerun-if-changed 必须给绝对路径 (相对路径以包根为基准, 会静默失效 —— S-6 防线③)",
    );
}

// ── 6. feature 默认关闭 ────────────────────────────────────────────────────

#[test]
fn test_initramfs_feature_is_off_by_default() {
    let glue = read_src(KERNEL_CARGO);
    assert_contains(
        &glue,
        "initramfs = [\"kernel-functions/initramfs\"]",
        "glue 必须逐项转发 feature, 否则子 crate 分支静默消失 (P-71)",
    );
    let funcs = read_src(FUNC_CARGO);
    assert_contains(&funcs, "initramfs = []", "functions 侧声明同名 feature");
    for (name, src) in [("kernel", &glue), ("kernel-functions", &funcs)] {
        let default_line = src
            .lines()
            .find(|l| l.trim_start().starts_with("default ="))
            .unwrap_or_else(|| panic!("{name} 缺 default 行"));
        assert_eq!(
            default_line.split('=').nth(1).unwrap().trim(),
            "[]",
            "{name} 的 default 必须为空: 内嵌 init.bin 才是出货路径"
        );
    }
}

// ── 7. 门禁接线: 专项 feature 腿真构建 + 真启动 + 复原默认构建 ─────────────

#[test]
fn test_carrier_boot_script_asserts_milestones() {
    let sh = read_src(CPIO_SH);
    let meta = fs::metadata(repo_root().join(CPIO_SH)).expect("载体脚本须存在");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "载体脚本必须可执行 (ci/audit.sh 直接按路径调用)"
    );
    assert_contains(
        &sh,
        "make ARCH=\"$arch\" KERNEL_FEATURES=initramfs all",
        "必须真构建带 feature 的镜像",
    );
    assert_contains(
        &sh,
        "verify_image_arch.py",
        "启动前先做镜像架构自检 (S-6 防线③)",
    );
    assert_contains(&sh, "MIN_UNPACKED", "条目数下限把嵌套目录未落盘的回归钉住");
    assert_contains(&sh, "init from /init", "PID 1 必须来自文件系统装载");
    assert_contains(
        &sh,
        "THREAD\\] PASS",
        "载体加载的 init 必须真跑完线程组探针",
    );
    // 内核串口日志含 NUL (日志与探针逐字符并发写), grep 按二进制处理则不输出匹配行
    assert_contains(&sh, "grep -aq", "里程碑匹配必须用 grep -a (日志含 NUL)");
    assert_contains(
        &sh,
        "make ARCH=\"$arch\" all",
        "跑完必须复原默认构建 (否则后续门禁跑在带 feature 的镜像上)",
    );
}

#[test]
fn test_ci_audit_wires_carrier_leg_in_full_mode() {
    let audit = read_src(AUDIT_SH);
    assert_contains(
        &audit,
        "initramfs_boot_test.sh",
        "full 档必须接载体 feature 腿",
    );
    // 顺序约束: 载体腿切换过 KERNEL_FEATURES, 必须排在压力腿之后
    let stress = audit
        .find("thread_stress_test.sh")
        .expect("K-07a 压力腿必须在位");
    let carrier = audit
        .find("initramfs_boot_test.sh")
        .expect("载体腿必须在位");
    assert!(
        carrier > stress,
        "载体腿须排在压力腿之后 (它自带复原默认构建, 之后不得再跑 feature 镜像)"
    );
}
