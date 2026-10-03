// L1-04: aarch64 内核高半区迁移 —— 「物理地址 → 内核虚拟地址」换算单一入口契约.
//
// 迁移前 aarch64 内核恒等映射在低半区 (`KERNEL_BASE = 0`), 换算在形式上"什么都不做",
// 因而散落出多处彼此独立的实现: `pmm.rs` 的裸 `phys + KERNEL_BASE`、`kpti_aarch64.rs`
// 与 `vmm_aarch64.rs` 里各自私有的 `phys_to_virt` / `virt_to_phys` 副本、以及
// `iomem.rs` 中按架构分派的 `HIGH_ALIAS_BASE + phys`. 内核迁至高半区
// (`KERNEL_BASE = 0xFFFF_0000_0000_0000`) 后, 这些副本要么与唯一入口等价、要么
// 因基数分裂而失效, 故收敛为唯一入口 `framework::mm::{phys_to_virt, virt_to_phys}`
// (别名基数唯一来源 = `mm::KERNEL_BASE`, 独立常量 `HIGH_ALIAS_BASE` 已删除).
//
// 本文件锁定该收敛的装配面, 防副本回潮. 数值正确性由 QEMU 启动 (PMM 初始化 +
// `.vectors`/`KPTI_GLOBALS` 别名映射) 承担, 不在本文件重复.

use std::fs;
use std::path::PathBuf;

const KERNEL_DIR: &str = "../src/kernel";
const MM_DIR: &str = "../src/kernel/framework/mm";
const MM_MOD: &str = "../src/kernel/framework/mm/mod.rs";
const IOMEM: &str = "../src/kernel/framework/iomem.rs";
const PMM: &str = "../src/kernel/framework/mm/pmm.rs";
const LIB: &str = "../src/kernel/lib.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// 压缩连续空白为单空格, 使断言不受列对齐/换行影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 递归收集目录下的全部 `.rs` 文件 (排序后返回, 断言输出稳定).
fn rs_files(dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap_or_else(|e| panic!("read_dir {}: {e}", d.display())) {
            let p = entry.expect("dir entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e.to_str() == Some("rs")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// 1. aarch64 基址取值锚点: `KERNEL_BASE` = TTBR1 窗口基址, `KERNEL_TEXT_BASE` 由其派生.
///
/// 二者是全部 PA→VA 换算的数值来源 (L1-02 裁定「A 数值 + 合一布局」), 故在此
/// fail-closed 锁定 —— 一旦回退为 `0` (低半区形态) 或脱离派生关系, 换算面即整体失真.
#[test]
fn test_aarch64_kernel_base_is_ttbr1_window_base() {
    let n = norm(&read(MM_MOD));

    assert!(
        n.contains(
            r#"#[cfg(target_arch = "aarch64")] pub const KERNEL_BASE: u64 = 0xFFFF000000000000u64;"#
        ),
        "aarch64 KERNEL_BASE 必须为 TTBR1 窗口基址 0xFFFF_0000_0000_0000 (L1-02 裁定)"
    );
    assert!(
        n.contains(
            r#"#[cfg(target_arch = "x86_64")] pub const KERNEL_BASE: u64 = 0xFFFF800000000000u64;"#
        ),
        "x86_64 KERNEL_BASE 分叉必须保留 (L1 不迁移 x86_64 形态)"
    );
    assert!(
        n.contains(
            r#"#[cfg(target_arch = "aarch64")] pub const KERNEL_TEXT_BASE: u64 = KERNEL_BASE + 0x4008_0000;"#
        ),
        "aarch64 KERNEL_TEXT_BASE 必须由 KERNEL_BASE 派生 (镜像 LMA 0x40080000 的高半区别名)"
    );
}

/// 2. `framework/mm/` 子树内 `phys_to_virt` / `virt_to_phys` 只准有 `mod.rs` 一处定义.
///
/// 私有副本是迁移前"恒等映射下换算无代价"的产物; 基数变更后副本会成为静默漂移点.
#[test]
fn test_pa_va_conversion_has_single_definition() {
    for file in rs_files(MM_DIR) {
        if file
            .file_name()
            .is_some_and(|n| n.to_str() == Some("mod.rs"))
        {
            continue;
        }
        let src = read(&file.to_string_lossy());
        for def in ["fn phys_to_virt(", "fn virt_to_phys("] {
            assert!(
                !src.contains(def),
                "{} 不得再定义 `{def}` —— PA→VA 换算唯一入口为 \
                 `framework::mm::{{phys_to_virt, virt_to_phys}}` (L1-04)",
                file.display()
            );
        }
    }
}

/// 3. `HIGH_ALIAS_BASE` 不得回归: 别名基数唯一来源为 `KERNEL_BASE`.
///
/// 迁移后二者同值, 保留两个常量即等于两处可各自漂移的数值来源.
#[test]
fn test_no_second_high_alias_base_constant() {
    for file in rs_files(KERNEL_DIR) {
        let src = read(&file.to_string_lossy());
        assert!(
            !src.contains("const HIGH_ALIAS_BASE"),
            "{} 不得再定义 `HIGH_ALIAS_BASE` —— 别名基数须取 `KERNEL_BASE` (L1-04)",
            file.display()
        );
        assert!(
            !src.contains("HIGH_ALIAS_BASE +") && !src.contains("+ HIGH_ALIAS_BASE"),
            "{} 不得再以 `HIGH_ALIAS_BASE` 做裸算术 —— 须走 `phys_to_virt` (L1-04)",
            file.display()
        );
    }
}

/// 4. `iomem.rs::mmio_virt` 不再按架构分派, 直接委托唯一入口.
///
/// 迁移后 x86_64 (高半区直接映射) 与 aarch64 (高半区别名) 同为 `PA + KERNEL_BASE`,
/// 分派已无意义; 保留分派会让 aarch64 分支重新引入独立基数.
#[test]
fn test_iomem_mmio_virt_delegates_to_single_entry() {
    let src = read(IOMEM);
    let start = src
        .find("fn mmio_virt(phys: u64) -> u64 {")
        .expect("iomem.rs 必须保留 mmio_virt 作为 MMIO PA→VA 语义点");
    let end = start + src[start..].find("\n}").expect("mmio_virt 必须闭合");
    let body = &src[start..end];

    assert!(
        !body.contains("cfg(target_arch"),
        "mmio_virt 不得按架构分派 (L1-04 收敛: 双架构同源)"
    );
    assert!(
        body.contains("phys_to_virt(phys)"),
        "mmio_virt 必须委托唯一入口 `phys_to_virt`"
    );
    assert!(
        !body.contains("HIGH_ALIAS_BASE"),
        "mmio_virt 不得引用已删除的 `HIGH_ALIAS_BASE`"
    );
}

/// 5. `pmm.rs` 元数据映射不得出现裸 `KERNEL_BASE` 算术.
///
/// PMM 的 bitmap / buddy meta / free-links / frame-counts 均落在内核直射区,
/// 换算必须同走 `phys_to_virt`, 否则基数变更时其中一处会先于其他处失真.
#[test]
fn test_pmm_metadata_mapping_uses_phys_to_virt() {
    let src = read(PMM);
    for (i, line) in src.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        assert!(
            !code.contains("+ KERNEL_BASE") && !code.contains("- KERNEL_BASE"),
            "pmm.rs:{} 出现裸 `KERNEL_BASE` 算术, 须走 `phys_to_virt` / `virt_to_phys` (L1-04): {line}",
            i + 1
        );
    }
}

/// 6. kmalloc 堆基址必须经 `KERNEL_BASE` 换算 (不得直取物理地址).
///
/// 迁移前 aarch64 的堆基址写成 `boot_info.kernel_end + 0x200000` (那时 `KERNEL_BASE = 0`,
/// 与 x86_64 分支同值). 迁移后该分叉成为残项: 堆落在低半区恒等区, 内核在
/// `TTBR0 = per-process EL1 视图` 下解引用堆对象即翻译故障 (L1-05 移除 DRAM 块后暴露,
/// `ESR=0x96000005` level-1 fault). 堆的**物理布局**不因此改变, 仅访问别名走高半区,
/// 故此处 fail-closed 锁定「每个 `heap_start` 都必须含 `KERNEL_BASE +`」.
#[test]
fn test_kmalloc_heap_base_goes_through_kernel_base() {
    let n = norm(&read(LIB));
    let mut search = n.as_str();
    let mut found = 0usize;

    while let Some(idx) = search.find("let heap_start =") {
        let tail = &search[idx..];
        let window = &tail[..tail.len().min(160)];
        assert!(
            window.contains("KERNEL_BASE + boot_info.kernel_end + 0x200000"),
            "lib.rs 的 heap_start 必须为 `KERNEL_BASE + boot_info.kernel_end + 0x200000` \
             (双架构统一, 不得直取物理地址): {window}"
        );
        found += 1;
        search = &search[idx + "let heap_start =".len()..];
    }

    assert!(
        found >= 2,
        "lib.rs 应有两处 heap_start (kernel_test 路径 + 真机路径), 实测 {found} 处"
    );
}
