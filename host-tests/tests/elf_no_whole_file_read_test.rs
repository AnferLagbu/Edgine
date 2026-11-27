// SPDX-License-Identifier: MPL-2.0
//! E-4: ELF 装载读入边界的不可回退契约锁.
//!
//! 锁的是**读入形态**, 不是具体调用点行号:
//! 1. `execve` 的主映像 / wasm 运行时 / `PT_INTERP` 三类读入只走前缀 API,
//!    不得再用 `ELF_MAX_SIZE` 调 `read_file_to_vec` / `read_file_kernel`;
//! 2. `init` 的 `/init` 与 `PT_INTERP` 只走前缀 API; 唯一允许的整文件读是
//!    `install_disk_root` 对 `/disk/init` 的落盘完整性回读 (不进装载器);
//! 3. wasm 魔数探测固定 4 字节, 不得回退成 128 KiB 探测窗口.

use std::fs;
use std::path::PathBuf;

const EXEC_RS: &str = "src/kernel/functions/proc/exec.rs";
const INIT_RS: &str = "src/kernel/functions/init.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释: 契约锁只判代码形态, 不禁止注释里提旧实现.
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

/// 取 `name` 所在函数从首个 `{` 到配对 `}` 的源码 (已剥整行注释).
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

fn count_needle(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
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

#[test]
fn execve_reads_only_prefix_not_whole_file() {
    let code = code_only(&read_src(EXEC_RS));
    let exec = fn_body(
        &code,
        "pub fn execve_syscall(path: u64, argv: u64, envp: u64)",
    );
    let flat = norm(&exec);

    for banned in [
        "read_file_to_vec(",
        "read_file_kernel(",
        "read_file_prefix_kernel(",
    ] {
        assert_absent(&flat, banned, "execve 不得整文件读主映像/运行时/解释器");
    }
    assert_absent(
        &flat,
        "ELF_MAX_SIZE",
        "ELF_MAX_SIZE 只作前缀预算常量, 不得出现在 execve 的读入调用",
    );
    assert_absent(
        &flat,
        "ELF_HEAD_PROBE_MAX",
        "E-3 后 execve 不得再引用 128 KiB 探测窗口",
    );

    assert_contains(
        &flat,
        "read_elf_prefix_to_vec(path as *const u8, current_pwm)",
        "非 wasm 主映像必须走可加载前缀读",
    );
    assert_contains(
        &flat,
        "read_elf_prefix_kernel(WASM_RUNTIME_PATH, 0)",
        "wasm 运行时必须走可加载前缀读",
    );
    assert_contains(
        &flat,
        "read_elf_prefix_kernel(path, current_pwm)",
        "PT_INTERP 必须走可加载前缀读",
    );
    assert_eq!(
        count_needle(&exec, "read_elf_prefix_kernel("),
        2,
        "运行时 + PT_INTERP 两个前缀读点必须同时存在"
    );
}

#[test]
fn init_reads_prefix_and_only_disk_readback_is_whole_file() {
    let code = code_only(&read_src(INIT_RS));
    let loader = fn_body(&code, "fn load_init_from_initramfs() -> i32");
    let flat = norm(&loader);

    for banned in [
        "read_file_to_vec(",
        "read_file_kernel(",
        "read_file_prefix_kernel(",
    ] {
        assert_absent(&flat, banned, "/init 装载链不得整文件读");
    }
    assert_absent(
        &flat,
        "ELF_MAX_SIZE",
        "/init 装载链不得以 ELF_MAX_SIZE 读取",
    );
    assert_contains(
        &flat,
        "read_elf_prefix_kernel(\"/init\", 0)",
        "PID 1 主映像必须走可加载前缀读",
    );
    assert_contains(
        &flat,
        "read_elf_prefix_kernel(path, 0)",
        "PID 1 的 PT_INTERP 必须走可加载前缀读",
    );
    assert_eq!(
        count_needle(&loader, "read_elf_prefix_kernel("),
        2,
        "主映像与 PT_INTERP 两个前缀读点必须同时存在"
    );

    // `/disk/init` 回读是唯一例外: 它只判落盘完整性, 不进装载器.
    let install = fn_body(&code, "pub fn install_disk_root()");
    assert_contains(
        &install,
        "read_file_kernel(",
        "/disk/init 完整性回读保留整文件 API",
    );
    assert_eq!(
        count_needle(&code, "read_file_kernel("),
        1,
        "init.rs 只允许 install_disk_root 一处整文件回读"
    );
    assert_eq!(
        count_needle(&code, "ELF_MAX_SIZE"),
        1,
        "ELF_MAX_SIZE 只允许出现在 /disk/init 回读上限"
    );
    assert_eq!(
        count_needle(&code, "read_file_to_vec("),
        0,
        "init.rs 不得出现需要 fd 的整文件读"
    );
}

#[test]
fn wasm_magic_probe_stays_four_bytes() {
    let code = code_only(&read_src(EXEC_RS));
    let exec = fn_body(
        &code,
        "pub fn execve_syscall(path: u64, argv: u64, envp: u64)",
    );
    let flat = norm(&exec);

    assert_contains(
        &code,
        "const WASM_MAGIC_LEN: usize = 4;",
        "wasm 魔数探测长度必须钉死为 4 字节",
    );
    assert_contains(
        &flat,
        "read_file_prefix_to_vec( path as *const u8, current_pwm, WASM_MAGIC_LEN",
        "execve 首读只能按 WASM_MAGIC_LEN 读魔数",
    );
    assert_absent(
        &flat,
        "read_file_prefix_to_vec( path as *const u8, current_pwm, ELF_HEAD_PROBE_MAX",
        "不得回退为 128 KiB 魔数探测窗口",
    );
    assert_eq!(
        count_needle(&exec, "read_file_prefix_to_vec("),
        1,
        "execve 只允许一次 4 字节魔数探测读"
    );
}
