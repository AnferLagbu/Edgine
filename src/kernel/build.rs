//! 内核 crate 的构建脚本 — 嵌入产物的存在性护栏与重编译触发器.
//!
//! D4 严格零嵌入后内核镜像不再嵌任何用户态字节 (init / initramfs cpio / wasm 桩
//! 均由引导器作 boot-module 交付, 经 VFS 取用), 唯一仍 `include_bytes!` 的产物是
//! x86_64 专属引导码 `other/build/x86_64/stage1.bin`. 裸机 x86_64 target 下检查其
//! 存在性, 缺失即 panic (DECISION-H15: 不再生成全 0 占位符, 以免遮蔽真实产物缺失
//! 状态); 并对它发出**绝对路径**的 `cargo:rerun-if-changed` —— cargo 以包根
//! `src/kernel` 解析相对路径, 而 `include_bytes!` 以源文件为基准, 两者不同源时该
//! 指令会静默失效. host target (host-tests 触发) 与 aarch64 不做上述检查, 避免干净
//! checkout 形成隐式 `make` 依赖.

use std::path::Path;

/// DECISION-H15: 构建产物缺失时直接报错, 不再生成全 0 占位符.
///
/// 原因: stage1.bin 由 Makefile 从真实汇编产物生成 (Makefile:221 由 stage1.asm 汇编).
/// 若此处静默写入全 0 占位, 会覆盖/遮蔽真实产物或让缺失状态被掩盖,
/// 且与 Makefile 产物存在顺序冲突 (全 0 镜像被当成真实引导码).
fn require_exists(path: &Path) {
    assert!(
        path.exists(),
        "构建产物缺失: {} — 请先运行 `make` 生成该文件, 不要依赖占位符",
        path.display()
    );
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();

    // G-06 (2026-09-06): 产物存在性检查仅对裸机 target 生效.
    // other/build/x86_64/stage1.bin 由 Makefile 生成, other/build/ 目录被 .gitignore
    // 忽略. host 构建 (host-tests 经 edgine path 依赖触发) 的 CARGO_CFG_TARGET_OS 为
    // linux, 不应检查裸机产物 — 否则干净 checkout 直接 cargo test 会因产物缺失 panic,
    // 形成未记录的隐式 make 依赖.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        // stage1.bin 是 x86_64 专属引导码 (boot/stage1.asm → nasm -f bin);
        // aarch64 引导走 boot/aarch64/start.S, 不生成也不依赖 stage1.bin.
        // 它已随内核产物迁入 other/build/<arch>/ (P-10), 路径自带架构段,
        // 不存在异架构残留, 故无需 arch-switch-clean 之类的补丁机制.
        //
        // S-6 防线③附带: rerun-if-changed 必须给**绝对路径**. cargo 对相对路径以
        // 包根 (CARGO_MANIFEST_DIR = src/kernel) 为基准解析, 而 include_bytes! 以
        // 源文件为基准 —— 旧写法 `other/build/user/init.bin` 实际指向不存在的
        // `src/kernel/other/build/...`, 该指令从未生效: 嵌入产物被换后 cargo 仍判
        // Fresh, 不把新字节重新写进镜像, 构成跨架构污染静默通过的通道之一.
        if target_arch == "x86_64" {
            let stage1 = base.join("other/build/x86_64/stage1.bin");
            require_exists(&stage1);
            println!("cargo:rerun-if-changed={}", stage1.display());
        }
    }
}
