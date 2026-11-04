// SPDX-License-Identifier: MPL-2.0
//! 内核裸机构建入口的 rustflags 契约测试 (A-12 丙 收口).
//!
//! 缺陷形态 (实测读数出自 §7 台账 A-12 条目): 内核镜像的 cargo 调用有两条入口且
//! cwd 不同 —— `make` 的 `$(RUST_LIB)` 配方是 `cd src/kernel && cargo build …`,
//! 而 `./ci/build.sh` 的 `build_arch()` 原为 `pushd src/rust` + `--manifest-path
//! ../kernel/Cargo.toml`. cargo 的 `.cargo/config.toml` 按 **cwd 向上** 合并, 与
//! `--manifest-path` 无关, 于是 `-C relocation-model=static` 只对后一条入口生效:
//! 同一棵树、同一 `KERNEL_FEATURES=` 编出两种机器码 (`readelf -r` 的 GOTPCREL
//! 23269 对 2008, `kernel.flat` 2517080 对 2454936). 后果不是启动失败而是判据面:
//! 门槛 5 的 QEMU 腿与门槛 1 的链接对象取决于最后一次调用的是哪条入口, FP-06 直读
//! 的 `other/build/aarch64/kernel.bin` 同样随之漂移, 而两条入口共用同一 `.a` 路径
//! ⇒ 每次切换都全量重编.
//!
//! 裁定处置 (丙): `ci/build.sh` 的内核腿改成与 Makefile 同一 cwd, 裸机 rustflags
//! 随内核 crate 自己声明 ⇒ 双入口消失. 本文件锁住四条:
//!
//! 1. 内核腿的 cwd 单源 (build.sh 里不再有 src/rust + `--manifest-path ../kernel` 形态)
//! 2. `src/kernel/.cargo/config.toml` 给三个裸机三元组都声明了 static + nostdlib
//! 3. 与 `src/rust/.cargo/config.toml` 的 `[target.*]` 段逐字对账 (两处仍是两个文件,
//!    漂移即红 —— 后者还服务 audit.sh 步骤 1/2 的 check·clippy 腿, 不能顺手删)
//! 4. 两条入口的其余构建参数同形 (同一 `--target-dir`、同一 `BUILD_STD_CFG`),
//!    且 build.sh 不另开 `RUSTFLAGS` 通道 (env 会覆盖 config 里的 rustflags)
//!
//! 实测 (同一棵树、`KERNEL_FEATURES` 空、各以 `touch src/kernel/lib.rs` 强制一次真
//! 重编后交替跑两条入口): `make` 与 `./ci/build.sh` 的 `kernel.flat` / `kernel.bin`
//! **逐字节相同** (本轮 x86_64 sha256 前缀 `5518b539…`、2454936 字节、GOTPCREL 2008;
//! aarch64 `kernel-aarch64.img` 前缀 `5369292a…`、2814856 字节), 且随后交替调用不再
//! 触发重编 (另一条入口报 `Finished ... in 0.12s`、`libkernel.a` mtime 不变) ——
//! 即判据 1+2 的效果面. 具体哈希与字节数随任意源码改动浮动 (内嵌的 `file!()` /
//! `line!()` 会跟着行号走), 本段的判据是**同批两入口相等**, 不是某个固定读数.

use std::fs;
use std::path::PathBuf;

const BUILD_SH: &str = "ci/build.sh";
const MAKEFILE: &str = "Makefile";
const KERNEL_CARGO_CONFIG: &str = "src/kernel/.cargo/config.toml";
const SHELL_CARGO_CONFIG: &str = "src/rust/.cargo/config.toml";

/// 内核镜像用到的三个裸机三元组 (与 Makefile 的 `RUST_TARGET_KERNEL` 同源).
const BARE_METAL_TRIPLES: [&str; 3] = [
    "x86_64-unknown-none",
    "aarch64-unknown-none",
    "aarch64-unknown-none-softfloat",
];

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 取一个 config 文件里 `[target.<triple>]` 段的内容 (到下一个表头为止).
///
/// 返回 `三元组 -> 该段的非空行列表`. 只比 `rustflags` 相关文本, 不比注释与空行 ——
/// 注释是给人读的口径, 漂移判据应当落在真正进 rustc 的参数上.
fn target_sections(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<String> = None;
    let mut body: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            if let Some(name) = current.take() {
                out.push((name, body.clone()));
            }
            body.clear();
            current = header
                .strip_prefix("target.")
                .map(|t| t.trim_matches('"').to_string());
            continue;
        }
        if current.is_some() && !line.is_empty() && !line.starts_with('#') {
            body.push(line.to_string());
        }
    }
    if let Some(name) = current {
        out.push((name, body));
    }
    out
}

/// 内核镜像的两条构建入口必须同 cwd —— 双入口是 A-12 的分叉源.
#[test]
fn test_kernel_bare_metal_build_entry_is_single_cwd() {
    let build_sh = read_src(BUILD_SH);
    let makefile = read_src(MAKEFILE);

    assert!(
        build_sh.contains("pushd src/kernel"),
        "{BUILD_SH} 的内核腿不再从 src/kernel 起 cargo —— cwd 一变, 加载的 \
         .cargo/config.toml 就变, 双入口分叉随之复活"
    );
    // 反向锁: 老形态一旦回来 (无论以 manifest-path 还是 pushd src/rust 的方式),
    // 内核腿就会吃到 src/rust 那份 rustflags, 与 make 入口产出不同机器码.
    for forbidden in ["--manifest-path ../kernel/Cargo.toml", "pushd src/rust"] {
        assert!(
            !build_sh.contains(forbidden),
            "{BUILD_SH} 仍含内核腿的旧形态 `{forbidden}` —— A-12 的 cwd 分叉回来了"
        );
    }
    assert!(
        makefile.contains("cd src/kernel && cargo build --release --target $(RUST_TARGET_KERNEL)"),
        "{MAKEFILE} 的 $(RUST_LIB) 配方不再是 `cd src/kernel && cargo build --release \
         --target $(RUST_TARGET_KERNEL)` —— 与本文件上面的 build.sh 判据配对, 两处共同\
         决定镜像的 cwd"
    );
}

/// 裸机 rustflags 必须随内核 crate 声明, 否则统一到 src/kernel cwd 后 static 会丢.
#[test]
fn test_kernel_cargo_config_declares_bare_metal_rustflags() {
    let text = read_src(KERNEL_CARGO_CONFIG);
    let sections = target_sections(&text);
    for triple in BARE_METAL_TRIPLES {
        let body = sections
            .iter()
            .find(|(name, _)| name == triple)
            .unwrap_or_else(|| panic!("{KERNEL_CARGO_CONFIG} 缺 [target.{triple}] 段"));
        let joined = body.1.join("\n");
        for flag in ["relocation-model=static", "link-arg=-nostdlib"] {
            assert!(
                joined.contains(flag),
                "{KERNEL_CARGO_CONFIG} 的 [target.{triple}] 不再含 `{flag}` —— \
                 内核镜像的 rustflags 由本段单源给出, 丢一项就换了一种机器码"
            );
        }
    }
}

/// 两份 config 的 `[target.*]` 段逐字对账 (两处声明并存期间的漂移防线).
#[test]
fn test_kernel_and_shell_target_rustflags_do_not_drift() {
    let kernel = target_sections(&read_src(KERNEL_CARGO_CONFIG));
    let shell = target_sections(&read_src(SHELL_CARGO_CONFIG));
    let mut diffs: Vec<String> = Vec::new();

    for triple in BARE_METAL_TRIPLES {
        let left = kernel
            .iter()
            .find(|(name, _)| name == triple)
            .map(|(_, body)| body.clone());
        let right = shell
            .iter()
            .find(|(name, _)| name == triple)
            .map(|(_, body)| body.clone());
        match (left, right) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => diffs.push(format!(
                "[target.{triple}] 两处内容不同:\n  {KERNEL_CARGO_CONFIG}: {}\n  {SHELL_CARGO_CONFIG}: {}",
                a.join(" / "),
                b.join(" / ")
            )),
            (Some(_), None) => diffs.push(format!(
                "[target.{triple}] 只在 {KERNEL_CARGO_CONFIG} 声明 —— audit.sh 步骤 1/2 \
                 从 src/rust 起 cargo, 会编出与镜像不同的一份代码"
            )),
            (None, Some(_)) => diffs.push(format!(
                "[target.{triple}] 只在 {SHELL_CARGO_CONFIG} 声明 —— 镜像入口吃不到它"
            )),
            (None, None) => diffs.push(format!("[target.{triple}] 两处都缺")),
        }
    }
    assert!(
        diffs.is_empty(),
        "裸机 rustflags 出现跨文件漂移 ({} 项):\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

/// 两条入口的其余构建参数同形, 且没有第二套 rustflags 通道.
#[test]
fn test_kernel_build_entries_share_the_same_flags_surface() {
    let build_sh = read_src(BUILD_SH);
    let makefile = read_src(MAKEFILE);

    // 同一 target 缓存目录: 两条入口写同一个 `.a`, 参数不同形就会互相踩指纹.
    // 比对 `--target-dir` 的**值**而非子串 —— `contains("--target-dir ../../other/target")`
    // 会被 `../../other/target-ci` 这类更长路径满足 (负控制实测: 换成 target-ci 后原写法仍绿).
    let tokens: Vec<&str> = build_sh.split_whitespace().collect();
    let dirs: Vec<&str> = tokens
        .windows(2)
        .filter(|pair| pair[0] == "--target-dir")
        .map(|pair| pair[1])
        .collect();
    assert!(
        !dirs.is_empty() && dirs.iter().all(|dir| *dir == "../../other/target"),
        "{BUILD_SH} 的内核腿没有显式 `--target-dir ../../other/target` (实测取到 {dirs:?}) —— \
         缺该参数即退回吃 src/kernel/.cargo/config.toml 的相对 target-dir, \
         取到别的目录则与 Makefile 的缓存不再同源"
    );
    assert!(
        build_sh.contains("\"${BUILD_STD_CFG[@]}\"") && makefile.contains("$(BUILD_STD_CFG)"),
        "两条入口不再各自显式注入 build-std —— 隐式加载 (靠 cwd 命中某份 config) 会 \
         把 E0152 类双 alloc 冲突重新引入构建面"
    );
    // env 里的 RUSTFLAGS 优先级高于 config 的 target rustflags, 是一等逃逸口.
    assert!(
        !build_sh.contains("RUSTFLAGS="),
        "{BUILD_SH} 出现了 `RUSTFLAGS=` —— env 形态会覆盖 config 里的 rustflags, \
         等于在镜像入口旁另开一条 rustflags 单源"
    );
}
