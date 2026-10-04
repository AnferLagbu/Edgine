use std::path::Path;

/// DECISION-H15: 构建产物缺失时直接报错, 不再生成全 0 占位符.
///
/// 原因: stage1.bin / init.bin 由 Makefile 从真实汇编/ELF 产物生成
/// (Makefile:221 由 stage1.asm 汇编, Makefile:132/177 cp 自 `USER_INIT_ELF`).
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
    // other/build/user/init.bin + other/build/stage1.bin 由 Makefile 生成, other/build/ 目录被
    // .gitignore 忽略. host 构建 (host-tests 经 edgine path 依赖触发) 的
    // CARGO_CFG_TARGET_OS 为 linux, 不应检查裸机产物 — 否则干净 checkout 直接
    // cargo test 会因产物缺失 panic, 形成未记录的隐式 make 依赖.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        // stage1.bin 是 x86_64 专属引导码 (boot/stage1.asm → nasm -f bin).
        // aarch64 引导走 boot/aarch64/start.S, 不生成也不依赖 stage1.bin.
        // 且 Makefile arch-switch-clean (Makefile:115) 跨架构切换时删除 stage1.bin,
        // aarch64 构建时缺失属正常, 不能 panic.
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        if target_arch == "x86_64" {
            require_exists(&base.join("other/build/stage1.bin"));
        }
        require_exists(&base.join("other/build/user/init.bin"));

        println!("cargo:rerun-if-changed=other/build/stage1.bin");
        println!("cargo:rerun-if-changed=other/build/user/init.bin");
    }
}
