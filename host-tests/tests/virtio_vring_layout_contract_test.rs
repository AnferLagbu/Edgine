//! P-31 aarch64 virtio-net TX 超时 — vring 布局与 MMIO 寄存器契约
//!
//! 追踪: docs/plan/aarch64-virtio-net-tx-rootcause.md
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! QEMU virt 上 virtio-net 以 legacy 传输 (Version=1) 呈现, 设备端无 QueueNum
//! 协商、按 realize 固定 num=1024 推算三段布局 (avail@desc+0x4000 /
//! used@desc+0x5000 页对齐). 旧驱动按编译期 VQ_SIZE=32 布置 → 设备读到分配区外
//! 恒 0 的 avail.idx → 永不回写 used → 首包即 TX 超时. 连带修正 modern 路径
//! 接线 bug (QueueNum 0x038 从未写入; QueueReady 0x044 必须用规范偏移).
//!
//! ## 防护两层
//!
//! 1. **纯逻辑直引**: `vring_layout` 与内核生产路径同源 (同一 const fn),
//!    host 侧直接断言 legacy/modern 双形态偏移数值;
//! 2. **源码文本契约**: MMIO 寄存器偏移与接线序 host 无法运行 (需真实设备),
//!    以源码文本固化, 防回归.

use edgine::kernel::privileged::driver::virtio::queue::vring_layout;
use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine 工作区根目录
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

const QUEUE_RS: &str = "src/kernel/privileged/driver/virtio/queue.rs";
const MOD_RS: &str = "src/kernel/privileged/driver/virtio/mod.rs";
const NET_RS: &str = "src/kernel/functions/driver/virtio/net.rs";

// ── 第一层: vring_layout 纯逻辑 (与内核同源) ─────────────────────────

#[test]
fn legacy_1024_layout_matches_qemu_device_inference() {
    // P-31 根因锚点: 驱动布局必须与 QEMU legacy 设备端推算逐字节一致
    let (avail_off, used_off, total) = vring_layout(true, 1024);
    assert_eq!(avail_off, 0x4000, "legacy avail 必须紧跟 16*num 描述符表");
    assert_eq!(used_off, 0x5000, "legacy used 必须 4096 对齐推算");
    assert_eq!(used_off % 4096, 0);
    assert!(
        total >= used_off + 4 + 8 * 1024 + 4,
        "total 必须容纳 used 全环"
    );
}

#[test]
fn modern_32_layout_uses_dword_align() {
    let (avail_off, used_off, total) = vring_layout(false, 32);
    assert_eq!(avail_off, 512);
    assert_eq!(used_off, 584, "modern used 仅按 4 对齐 (规范 Split Ring)");
    assert_eq!(total, 4096);
}

// ── 第二层: 源码文本契约 ─────────────────────────────────────────────

#[test]
fn queue_new_takes_runtime_num() {
    let src = read(QUEUE_RS);
    assert!(
        src.contains("pub fn new(legacy: bool, num: u16) -> Option<Self>"),
        "VirtQueue::new 必须按运行时 (legacy, num) 布置布局 (P-31: 编译期定长即错位)"
    );
    assert!(
        !src.contains("queue_size"),
        "旧编译期 queue_size 字段必须移除, 环尺寸唯一来源为 num"
    );
}

#[test]
fn mmio_queue_ready_offset_is_0x044() {
    let src = read(MOD_RS);
    assert!(
        src.contains("const QUEUE_READY: usize = 0x044;"),
        "virtio-mmio 规范 modern QueueReady 在 0x044 (legacy QueuePFN 在 0x040); \
         P-31 二次取证: 误写 0x040 被 modern 设备当 legacy 寄存器拒绝, 队列永不 enabled"
    );
    assert!(
        !src.contains("QUEUE_READY: usize = 0x040"),
        "0x040 归 legacy QueuePFN 专用, modern Ready 不得回潮此偏移"
    );
}

#[test]
fn mmio_queue_num_register_defined_with_setter() {
    let src = read(MOD_RS);
    assert!(
        src.contains("const QUEUE_NUM: usize = 0x038;"),
        "modern 必须定义 QueueNum 寄存器 (0x038)"
    );
    assert!(
        src.contains("pub fn set_queue_num(&self, num: u16)"),
        "必须提供 set_queue_num 供驱动协商队列尺寸"
    );
}

#[test]
fn net_legacy_uses_full_queue_num_max() {
    let src = read(NET_RS);
    assert!(
        src.contains("crate::functions::intcast::low_u16_from_u32(device.queue_num_max())"),
        "legacy 必须全量取 QueueNumMax 与设备端固定 num 对齐 (截断即错位)"
    );
    assert!(
        src.contains("let vq_legacy = is_legacy;"),
        "队列布局形态锚定传输层 device.is_legacy(), 不得混用特性位 !has_v1"
    );
    assert!(
        !src.contains("let vq_legacy = !has_v1;"),
        "特性位判定 (旧实现) 不得回潮"
    );
}

#[test]
fn net_setup_queue_modern_writes_num_before_addrs_and_ready() {
    let src = read(NET_RS);
    let start = src
        .find("pub fn setup_queue(")
        .expect("net.rs 必须有 setup_queue");
    let body = &src[start..];
    let num_off = body
        .find("set_queue_num(num)")
        .expect("modern 分支必须写 QueueNum");
    let addrs_off = body
        .find("setup_queue_addrs(")
        .expect("modern 分支必须写地址三段");
    let ready_off = body.find("set_queue_ready()").expect("必须置 QueueReady");
    assert!(
        num_off < addrs_off && addrs_off < ready_off,
        "规范序必须为 QueueNum → 地址三段 → QueueReady=1"
    );
}

#[test]
fn net_setup_queue_legacy_follows_linux_paradigm() {
    // P-31 legacy 线根因: QEMU 10.x 仅写 PFN 不触发设备端 vring 布局刷新,
    // 必须按 Linux legacy-mmio 范式序 QueueNum → QueueAlign → QueuePFN
    let src = read(NET_RS);
    let start = src
        .find("if self.device.is_legacy() {")
        .expect("setup_queue 必须有 legacy 分支");
    let body = &src[start..start + 1200];
    let num_off = body
        .find("set_queue_num(num)")
        .expect("legacy 必须写 QueueNum");
    let align_off = body
        .find("set_queue_align(4096)")
        .expect("legacy 必须写 QueueAlign");
    let pfn_off = body
        .find("setup_queue_legacy(desc_paddr)")
        .expect("legacy 必须写 QueuePFN");
    assert!(
        num_off < align_off && align_off < pfn_off,
        "legacy 规范序必须为 QueueNum → QueueAlign → QueuePFN"
    );
    assert!(
        !body[..pfn_off].contains("self.device.set_queue_ready();"),
        "legacy 分支绝不可调用 QueueReady (0x044 被 legacy 设备拒绝)"
    );
}

#[test]
fn net_legacy_hdr_is_10_bytes() {
    // P-31 legacy 线根因: 0.9.5 无 num_buffers, 头恒 10 字节; 驱动写 12
    // 会把帧起点前移 2 字节 → 设备解析出损坏以太网头 → slirp 丢包
    let src = read(NET_RS);
    assert!(
        src.contains("pub const LEGACY_NET_HDR_SIZE: usize = 10;"),
        "必须定义 0.9.5 传统头大小 10 字节常量"
    );
    assert!(
        src.contains("hdr_size = LEGACY_NET_HDR_SIZE;"),
        "legacy 协商分支必须选 10 字节头"
    );
}

#[test]
fn net_legacy_sets_guest_page_size_before_pfn() {
    // P-31 二层根因: GuestPageSize(0x028) 复位默认 0 → 设备把 PFN 当字节地址
    let src = read(NET_RS);
    assert!(
        src.contains("device.set_guest_page_size(4096);"),
        "legacy 必须在首写 PFN 前写 GuestPageSize=4096"
    );
}
