// SPDX-License-Identifier: MPL-2.0
//! K-07b 批1 initramfs 载体回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-07b 批1.
//!
//! 本批将「PID 1 从文件系统来」这条链第一次接通并锁死: cpio newc 归档由
//! `scripts/make_initramfs_cpio.py` 在构建期生成 → `make` 把它作为用户态产物集
//! 的一员 → 引导器作 **boot-module** 交付 (x86_64 GRUB `module2` / aarch64
//! `-initrd` 经 DTB `/chosen`) → 启动末尾解析归档、逐级建父目录、
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
//! D4 严格零嵌入后内嵌 `init.bin` 回退已删除, D4-e 起 `initramfs` feature 亦退役
//! (解包无条件执行) ⇒ 本腿构建的就是出货默认镜像; 真跑判据由 `ci/audit.sh` full 档
//! 的载体腿 (`scripts/initramfs_boot_test.sh`) 承担.

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
const LIB_RS: &str = "src/kernel/lib.rs";
const MEDIUM_SH: &str = "scripts/make_boot_medium.sh";

/// 归档必须覆盖的用户态程序 (与 `ARCHIVE_LAYOUT` 同源).
///
/// P4 退役内核解释器后, exec 的 `\0asm` 派发唯一目标是 `/bin/wasmrun` (用户态
/// wasmi 运行时); 少打即该腿 ENOEXEC.
const CARRIER_MEMBERS: &[&str] = &[
    "init.bin",
    "eash.bin",
    "proctest.bin",
    "exectest.bin",
    "install.bin",
    "fbterm.bin",
    "httpsrv.bin",
    "wasmrun.bin",
];

/// Makefile 里的 ELF → `.bin` 文件规则对 (左为 ELF 变量名, 右为落地产物名).
const USER_BIN_RULES: &[(&str, &str)] = &[
    ("USER_INIT_ELF", "init.bin"),
    ("USER_SHELL_ELF", "eash.bin"),
    ("USER_TEST_ELF", "proctest.bin"),
    ("USER_EXECTEST_ELF", "exectest.bin"),
    ("USER_INSTALL_ELF", "install.bin"),
    ("USER_FBTERM_ELF", "fbterm.bin"),
    ("USER_HTTPSRV_ELF", "httpsrv.bin"),
    ("USER_WASMRUN_ELF", "wasmrun.bin"),
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

/// 去掉整行 `#` 注释 (shell / TOML 共用): 锁定项是代码形态, 不是"注释里不许提旧事".
fn code_only_hash(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
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
        6,
        "归档引用点 = 2 条 user 目标 + 2 条 $(RUST_LIB) 规则 + 归档规则自身 + iso 载体拷贝 (D4-f)"
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
        "fn normalize_entry_path(name: &[u8], prefix: &[u8], out: &mut [u8]) -> Option<usize>",
        "条目名必须先归一为目标根内绝对路径 (前缀由挂载点决定)",
    );
    assert_contains(
        &code,
        "pub fn unpack(data: &[u8], target_root: &str, pwm: u64) -> Result<usize, &'static str>",
        "解包目标必须参数化 (引导根 / 与磁盘根 /disk 共用同一份归档, D5-g)",
    );
    assert_contains(
        &code,
        "let prefix = target_root.trim_end_matches('/');",
        "前缀须剥末尾分隔符 (\"/\" → \"\"), 否则条目路径出现双斜杠或漏首段",
    );
    assert_contains(
        &code,
        "normalize_entry_path(entry.name, prefix.as_bytes(), &mut path_buf)",
        "解包必须把目标根前缀真传进归一化 (写死空前缀会让 /disk 安装落回引导根)",
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
    let prefix_reader = fn_body(&api, "pub fn read_file_prefix_kernel");
    for (name, body) in [
        ("write_file_kernel", &writer),
        ("read_file_kernel", &reader),
        ("read_file_prefix_kernel", &prefix_reader),
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
        "fs.fs_create(parent_path, name, SubjectId::new(pwm))",
        "建文件走 fs_create",
    );
    assert_contains(
        &writer,
        "inode.write(0, data, pwm)",
        "写数据走 Inode::write",
    );
    for (name, body) in [
        ("read_file_kernel", &reader),
        ("read_file_prefix_kernel", &prefix_reader),
    ] {
        assert_contains(
            body,
            "fs.fs_stat(rel_path, SubjectId::new(pwm))",
            &format!("{name} 读取前按 stat 定长"),
        );
        assert_contains(
            body,
            "inode.read(0, &mut buf, pwm)",
            &format!("{name} 读数据走 Inode::read"),
        );
    }
}

// ── 4. init 编排: 双架构都走载体, 产物按架构嵌入 ───────────────────────────

#[test]
fn test_init_reads_carrier_from_boot_module_not_embed() {
    let code = code_only(&read_src(INIT_RS));
    // D3 严格零嵌入: cpio 不再 include_bytes! 进内核镜像, 改经 boot-module 取用.
    assert_absent(
        &code,
        "other/build/x86_64/user/initramfs.cpio",
        "cpio 不得编译期嵌入 (x86_64) —— D3 零嵌入",
    );
    assert_absent(
        &code,
        "other/build/aarch64/user/initramfs.cpio",
        "cpio 不得编译期嵌入 (aarch64) —— D3 零嵌入",
    );
    assert_absent(&code, "INITRAMFS_CPIO", "不得残留 INITRAMFS_CPIO 常量");
    assert_contains(
        &code,
        "crate::privileged::boot::initrd_blob()",
        "cpio 消费者必须经 boot-module 安全读数器取数",
    );
    assert_contains(
        &code,
        "initramfs from boot module",
        "载体路径须可观测来自 boot module",
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
        "crate::functions::proc::elf::read_elf_prefix_kernel(\"/init\", 0)",
        "PID 1 的镜像必须从 VFS 读可加载前缀 (无 fd 路径)",
    );
    assert_contains(
        &flat,
        "read_elf_prefix_kernel(path, 0)",
        "PT_INTERP 也必须读可加载前缀，而不是整文件",
    );
    assert_absent(
        &code,
        "read_file_to_vec",
        "载体路径不得依赖需要 fd 的 read_file_to_vec",
    );
    assert_absent(
        &code,
        "falling back to init.bin",
        "D4 零嵌入: init 只来自 boot-module, 不得保留内嵌 init.bin 回退",
    );
    assert_eq!(
        count_needle(&code, "init from /init"),
        2,
        "两架构的成功里程碑各一条 (载体腿与 headline 腿按该串断言)"
    );
}

// ── 5. build.rs: feature 点亮时按本架构要求归档 ────────────────────────────

#[test]
fn test_build_rs_no_longer_requires_embedded_cpio() {
    let code = code_only(&read_src(BUILD_RS));
    // D3 零嵌入: cpio 走 boot-module, 内核构建不再对 cpio 产物做嵌入护栏/重编译触发.
    assert_absent(
        &code,
        "CARGO_FEATURE_INITRAMFS",
        "不再按 feature 要求嵌入 cpio",
    );
    assert_absent(&code, "cpio.display()", "不再对 cpio 发 rerun-if-changed");
}

/// D4-d: 内核构建脚本只剩 x86_64 引导码一项嵌入护栏.
///
/// 护栏与 `include_bytes!` 必须同生共死: 用户态程序已改由 cpio 交付, 若 build.rs
/// 仍按 feature 要求 `wasmrun.bin` 等产物存在, 干净 checkout 的 host 构建会被无关
/// 产物缺失打断; 反之若 stage1 护栏被顺手删掉, 全 0 占位镜像又会静默出货.
#[test]
fn test_build_rs_guards_only_x86_stage1() {
    let code = code_only(&read_src(BUILD_RS));
    for gone in [
        "wasmstub",
        "wasmrun",
        "CARGO_FEATURE_WASM_USERSPACE",
        "init.bin",
    ] {
        assert_absent(&code, gone, &format!("用户态产物护栏须已删除: {gone}"));
    }
    assert_contains(
        &code,
        "other/build/x86_64/stage1.bin",
        "x86_64 引导码仍是唯一嵌入产物, 护栏必须保留",
    );
    assert_contains(
        &code,
        "println!(\"cargo:rerun-if-changed={}\", stage1.display());",
        "rerun-if-changed 必须给绝对路径 (S-6 防线③)",
    );
}

// ── 6. initramfs feature 已退役 (D4-e) ─────────────────────────────────────

#[test]
fn test_initramfs_feature_retired() {
    // 零嵌入后解包无条件执行, 该 feature 不改任何代码路径 ⇒ 空旋钮一律删净,
    // 四份 Cargo.toml (glue / functions / privileged / edgine 壳) 都不得再声明.
    for rel in [
        KERNEL_CARGO,
        FUNC_CARGO,
        "src/kernel/privileged/Cargo.toml",
        "src/rust/Cargo.toml",
    ] {
        assert_absent(
            &code_only_hash(&read_src(rel)),
            "initramfs =",
            "{rel} 不得再声明 initramfs feature (D4-e 退役: 空门控旋钮)",
        );
    }
    // 树级发现式: 任何启动腿都不得再往 KERNEL_FEATURES 里塞 initramfs
    // (cargo 对未声明 feature 直接报错, 但 scripts 面的残留会先在这里被钉住).
    let scripts = repo_root().join("scripts");
    for entry in fs::read_dir(&scripts).expect("scripts 目录须可读") {
        let path = entry.expect("目录项须可读").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        let src = fs::read_to_string(&path).expect("脚本须可读");
        for line in code_only_hash(&src).lines() {
            if line.contains("KERNEL_FEATURES") {
                assert!(
                    !line.contains("initramfs"),
                    "{} 仍向 KERNEL_FEATURES 传已退役的 initramfs: {line}",
                    path.display()
                );
            }
        }
    }
}

// ── 7. 门禁接线: 载体腿真构建出货默认镜像 + 真启动 ─────────────────────────

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
        "make ARCH=\"$arch\" all",
        "必须真构建**出货默认**镜像 (D4-e: initramfs feature 退役, 载体链即默认启动形态)",
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
    // D4-e: 零嵌入后 K-08 理由消失 ⇒ 本腿不得再切 feature / 关默认 features.
    // 既不切换, 也就无需尾部复原; 判据改为负向钉住"跑的就是出货配置".
    let sh_code = code_only_hash(&sh);
    assert_absent(
        &sh_code,
        "KERNEL_FEATURES",
        "载体腿即出货启动形态, 切 feature 就不再覆盖默认配置 (D4-e)",
    );
    assert_absent(
        &sh_code,
        "KERNEL_NO_DEFAULT",
        "关默认 features 的历史理由是内嵌 cpio 顶破 K-08, 零嵌入后已消失 (D4-e)",
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
    // 顺序约束: 载体腿会重建默认镜像, 排在压力腿之后避免与其构建交错
    let stress = audit
        .find("thread_stress_test.sh")
        .expect("K-07a 压力腿必须在位");
    let carrier = audit
        .find("initramfs_boot_test.sh")
        .expect("载体腿必须在位");
    assert!(
        carrier > stress,
        "载体腿须排在压力腿之后 (它重建默认镜像, 不与压力腿的构建交错)"
    );
}

// ── 8. 真机介质: 归档作 boot-module 交出 (D4-f) ─────────────────────────────

/// x86_64 ISO 必须把 cpio 作 GRUB `module2` 交出, 而不是只拷进 ISO 却不声明.
///
/// 只拷不声明 = 归档在介质上但引导器不交付 ⇒ MB2 tag3 缺失, 内核侧静默走"无载体"
/// 分支; 只声明不拷 = GRUB 直接报文件不存在. 两侧必须同时在位.
#[test]
fn test_iso_delivers_cpio_as_grub_module2() {
    let mk = norm(&read_src(MAKEFILE).replace('\\', " "));
    assert_contains(
        &mk,
        "cp $(USER_CPIO) $(ISODIR)/boot/initramfs.cpio",
        "iso 目标必须把本架构归档拷进介质 (D4-f)",
    );
    assert_contains(
        &mk,
        "multiboot2 /boot/kernel.bin",
        "内核经 multiboot2 装载 (与 module2 同一引导协议)",
    );
    assert_contains(
        &mk,
        "module2 /boot/initramfs.cpio initramfs",
        "归档必须作 module2 声明, 否则引导器不交付 (MB2 tag3 缺失)",
    );
}

/// aarch64 FAT 介质: extlinux `initrd` 行 + boot.cmd `booti` 第二参数 + mcopy 落盘.
///
/// 三处缺一即载体断链: 少了 `initrd` 行, U-Boot 不填 DTB `/chosen` 的
/// linux,initrd-start/end; 少了 mcopy, 文件根本不在盘上; boot.cmd 少第二参数则
/// 手工 booti 路径不带 initrd (与 distro boot 路径行为分叉).
#[test]
fn test_aarch64_medium_delivers_cpio_via_chosen() {
    let sh = read_src(MEDIUM_SH);
    let meta = fs::metadata(repo_root().join(MEDIUM_SH)).expect("真机介质脚本须存在");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "真机介质脚本必须可执行 (用户直接按路径调用)"
    );
    assert_contains(
        &sh,
        "make ARCH=aarch64 all user",
        "aarch64 介质必须同时构建内核 Image 与用户态归档",
    );
    assert_contains(
        &sh,
        "initrd /initramfs.cpio",
        "extlinux.conf 必须有 initrd 行 (U-Boot 据此填 /chosen)",
    );
    assert_contains(
        &sh,
        "fatload mmc 0:1 ${AARCH64_INITRD_ADDR} initramfs.cpio",
        "boot.cmd 必须把归档装载到独立地址",
    );
    assert_contains(
        &sh,
        "booti ${AARCH64_LOAD_ADDR} ${AARCH64_INITRD_ADDR}",
        "booti 第二参数须为 initrd 地址 (否则手工路径不带载体)",
    );
    assert_contains(
        &sh,
        "other/build/aarch64/user/initramfs.cpio ::initramfs.cpio",
        "归档必须真落进 FAT 分区 (mcopy)",
    );
    assert_contains(
        &sh,
        "≥ 256 MiB",
        "须提示 DRAM 容量前提 (initrd 装载于 DRAM 基址 + 128 MiB)",
    );
}

// ── 9. 载体物理区: 早期预留 + 搬移 + 建堆次序 (D4-h) ────────────────────────

/// `boot_memory_init` 的四步次序必须钉死: 登记 → 位图(含搬移) → 载体发布 → kmalloc.
///
/// 任一步换序都有一条静默失效通道:
/// - 登记晚于位图 ⇒ 交付区不被置位, buddy 把它当空闲页分出去;
/// - 载体发布早于位图 ⇒ 发布的是搬移**前**的原址, 消费者读到已被堆覆写的字节;
/// - kmalloc 早于位图 ⇒ 堆首 `HeapHeader` 直接写在归档上 (实测 GRUB 落点正在堆窗内).
#[test]
fn test_boot_memory_init_orders_reserve_bitmap_publish_heap() {
    let body = norm(&fn_body(
        &code_only(&read_src(LIB_RS)),
        "fn boot_memory_init()",
    ));
    let reserve = body
        .find("pmm_early_reserve_relocatable(")
        .expect("initrd 必须登记为可搬移早期预留区");
    let bitmap = body
        .find("pmm_init_bitmap(reserved_after_kernel)")
        .expect("位图初始化必须在位");
    let publish = body
        .find("set_initrd_carrier(region.base, region.size)")
        .expect("载体必须按早期预留表的最终地址发布");
    let kmalloc = body
        .find("get_kmalloc_mut().init(heap_start, KMALLOC_HEAP_SIZE)")
        .expect("kmalloc 初始化必须在位");
    assert!(
        reserve < bitmap,
        "早期预留登记必须先于位图建立 (否则交付区不被置位)"
    );
    assert!(
        bitmap < publish,
        "载体发布必须后于位图 (搬移在位图建立期完成, 早发布即发布原址)"
    );
    assert!(
        publish < kmalloc,
        "kmalloc 必须最后建堆 (堆首 HeapHeader 会覆写落在堆窗内的归档)"
    );
    assert_contains(
        &body,
        "crate::privileged::dtb::dtb_region()",
        "aarch64 的 DTB blob 自身也须登记 (只预留不搬移)",
    );
    assert_contains(
        &body,
        "pmm_early_reserve(base, size, \"dtb\")",
        "DTB 走不可搬移登记 (PMM 存在前已解析完并持久化)",
    );
}

/// 事后补预留路径必须已删除: 它只能"发现"冲突而消解不了冲突.
///
/// `reserve_range` 对已置位页返回 `Err(overlap)` —— 读起来像保护生效, 实际载荷仍与
/// 堆窗共址. 保留该调用等于把假警报当防线, 故在载体编排里一律不得再出现.
#[test]
fn test_boot_memory_init_has_no_post_hoc_reserve() {
    let body = fn_body(&code_only(&read_src(LIB_RS)), "fn boot_memory_init()");
    assert_absent(
        &body,
        "reserve_range(",
        "交付区预留的唯一置位点是早期预留表 (D4-h), 不得保留事后补预留",
    );
    assert_absent(
        &body,
        "initrd reserve failed",
        "事后补预留的失败日志须随调用一并删除 (否则残留假警报口径)",
    );
}
