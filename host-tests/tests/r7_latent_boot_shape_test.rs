// SPDX-License-Identifier: MPL-2.0
//! R7 回归: latent 不可达与引导面六项 (⑰ ⑳ ㉓ ㉕ ㊱ ㊲) 的源码形状锁.
//!
//! 六项都落在启动/固件/中断配置面, host 侧无法真实执行 (真硬件 MMIO / GIC 帧 /
//! PMM 预留 / 直射区), 故本文件钉住修复后的源码形状; 任一项回退成缺陷形态即转红:
//!
//! - ⑳ `remap_pic` 开头读旧掩码后 `let _ = (m, s)` 丢弃
//! - ㉓ `verify_post_conditions` 回读 `GICR_BASE` / `GICR_SGI_BASE` 两个静态别名
//! - ⑰ `acpi_reboot` 只取 GAS 低 32 位、忽略 AccessSize/BitWidth、低半区裸写
//! - ㉕ `parse_multiboot2` 不校验 `tag_size >= 16` / `tag_end <= end`
//! - ㊱ `load_segment` 把物理地址当 VA 且不做目标区间校验
//! - ㊲ `parse_system_table` 把物理地址当 VA 且不做对齐/直射区/固件预留校验

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免说明文字被当作代码形态.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 截取以 `header` 开头的函数体 (按大括号配对).
fn function_body<'a>(src: &'a str, header: &str) -> &'a str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("未找到函数 {header}"));
    let open = start
        + src[start..]
            .find('{')
            .unwrap_or_else(|| panic!("{header} 缺少函数体"));
    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("{header} 函数体大括号不配对");
}

#[test]
fn remap_pic_does_not_read_discarded_old_masks() {
    let src = code_only(&read_src("src/kernel/privileged/idt/table.rs"));
    let body = function_body(&src, "unsafe fn remap_pic()");
    assert!(
        !body.contains("port_inb"),
        "⑳: remap_pic 不应读旧掩码 (末尾 0xFF 全量覆盖写, 旧值无消费点)"
    );
    assert!(
        !body.contains("let _ = (m, s)"),
        "⑳: 丢弃旧掩码的形态必须消失"
    );
    assert!(
        body.contains("port_outb(0x21, 0xFF)") && body.contains("port_outb(0xA1, 0xFF)"),
        "⑳: 末尾仍须把两片掩码置 0xFF"
    );
}

#[test]
fn gic_verify_reads_affinity_located_frame() {
    let src = code_only(&read_src("src/kernel/privileged/arch/aarch64/gic.rs"));
    let body = function_body(
        &src,
        "unsafe fn verify_post_conditions() -> Result<(), &'static str>",
    );
    assert!(
        body.contains("redist_frames()"),
        "㉓: 回读必须用按亲和定位的帧"
    );
    assert!(
        !body.contains("GICR_BASE") && !body.contains("gicr_sgi_read"),
        "㉓: 不得回退到启动核的静态帧别名"
    );
    assert!(
        body.contains("redist_frames()?"),
        "㉓: 帧定位失败必须显式 Err, 不退回别名"
    );
}

#[test]
fn acpi_reset_register_consumes_full_gas() {
    let src = code_only(&read_src("src/kernel/privileged/arch/x86_64/acpi.rs"));
    assert!(
        src.contains("fn decode_reset_register(gas: [u8; 12], value: u8) -> ResetTarget"),
        "⑰: GAS 解码必须独立成可测纯函数"
    );
    assert!(
        src.contains("u64::from_le_bytes"),
        "⑰: 必须读取完整 64 位 GAS 地址"
    );
    assert!(
        src.contains("DIRECT_MAP_LIMIT"),
        "⑰: 必须校验地址落在直射区"
    );
    let body = function_body(&src, "pub fn acpi_reboot() -> !");
    assert!(
        body.contains("decode_reset_register"),
        "⑰: acpi_reboot 必须走解码结果"
    );
    assert!(
        body.contains("phys_to_virt"),
        "⑰: MMIO 写必须经内核直射别名"
    );
    assert!(
        !body.contains("reset_reg[4]"),
        "⑰: 只取低 32 位的旧形态必须消失"
    );
}

#[test]
fn multiboot2_tags_have_complete_bounds_checks() {
    let src = code_only(&read_src("src/kernel/privileged/boot/mod.rs"));
    let body = function_body(&src, "fn parse_multiboot2(ptr: *const u8)");
    assert!(
        body.contains("tag_size < 8"),
        "㉕: 标签头最小长度须显式校验"
    );
    assert!(
        body.contains("tag_end > end"),
        "㉕: 整条标签必须落在信息块内"
    );
    assert!(
        body.contains("tag_size >= 16"),
        "㉕: tag 3/4/6 载荷最小长度须校验"
    );
    assert!(
        body.contains("entry_size >= 24"),
        "㉕: 内存图条目最小长度须校验 (防 entry_size=0 挂死)"
    );
    assert!(
        body.contains("tag_size >= 38"),
        "㉕: framebuffer 载荷读取上限须先校验"
    );
}

#[test]
fn kexec_load_segment_uses_pa_to_direct_map_and_validates() {
    let src = code_only(&read_src("src/kernel/privileged/driver/kexec.rs"));
    let body = function_body(
        &src,
        "pub fn load_segment(&self, seg_type: KexecSegType, dst_addr: u64, src_data: &[u8]) -> bool",
    );
    assert!(
        body.contains("validate_segment_destination"),
        "㊱: 目标区间必须经统一校验"
    );
    assert!(
        body.contains("phys_to_virt(dst_addr)"),
        "㊱: 物理地址必须经直射别名访问"
    );
    assert!(
        !body.contains("dst_addr as *mut u8"),
        "㊱: 把物理地址直接当 VA 的旧形态必须消失"
    );
    assert!(
        src.contains("fn kernel_slice_phys(ptr: u64, len: usize)"),
        "㊱: 源物理区间换算须独立成可测函数"
    );
    assert!(
        src.contains("fn validate_segment_destination("),
        "㊱: 目标校验须独立成可测函数"
    );
}

#[test]
fn uefi_system_table_validates_address_and_mapping() {
    let src = code_only(&read_src("src/kernel/privileged/driver/uefi.rs"));
    let body = function_body(&src, "fn parse_system_table(&self, addr: u64)");
    assert!(
        body.contains("system_table_addr_problem(addr)"),
        "㊲: 地址门 (对齐 + 直射区) 必须先执行"
    );
    assert!(
        body.contains("firmware_region_contains"),
        "㊲: 必须校验区间归属固件预留区"
    );
    assert!(
        body.contains("phys_to_virt(addr)"),
        "㊲: 物理地址必须经直射别名访问"
    );
    assert!(
        src.contains("fn region_covers(base: u64, size: u64, addr: u64, len: u64) -> bool"),
        "㊲: 区间覆盖判据须独立成可测函数"
    );
}

#[test]
fn r7_markers_are_cleared() {
    for rel in [
        "src/kernel/privileged/arch/x86_64/acpi.rs",
        "src/kernel/privileged/boot/mod.rs",
        "src/kernel/privileged/driver/kexec.rs",
        "src/kernel/privileged/driver/uefi.rs",
        "src/kernel/privileged/idt/table.rs",
        "src/kernel/privileged/arch/aarch64/gic.rs",
    ] {
        let raw = read_src(rel);
        assert!(
            !raw.contains("已登记缺口") && !raw.contains("登记不改") && !raw.contains("已知局限"),
            "{rel} 仍留有本批修复前的缺口登记注释"
        );
    }
}
