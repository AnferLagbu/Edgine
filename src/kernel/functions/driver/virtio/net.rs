#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! `VirtIO` 网络设备驱动 — functions 层 (Phase 2.1.2)
//!
//! 通过 [`crate::privileged::driver::virtio::VirtioMmioDevice`] (privileged
//! transport 机制) 提供 100% safe 的网卡初始化与配置路径。
//! `VirtQueue` 操作通过 privileged 层安全 API 完成。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 所有 MMIO 读/写通过 privileged `VirtioMmioDevice` 安全代理
//! - **请求格式**: 定义 `VirtioNetHdr` / 配置偏移, 供 privileged I/O 路径使用
//! - **初始化序列**: Reset → Ack → Driver → Feature Negotiate → `Features_OK` → Queue Setup → `Driver_OK`
//!
//! ## 与 privileged 的分工
//!
//! - **functions (本文件)**: 初始化序列, 特性协商, 配置空间读取, 请求格式定义
//! - **privileged**: `VirtioMmioDevice` (MMIO transport 机制) + `VirtQueue` 分配与 DMA 缓冲区管理
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.2 任务: VirtIO-net 网络设备迁移

use crate::privileged::driver::virtio::queue::{DmaBuffer, VirtQueue};
use crate::privileged::driver::virtio::{VIRTIO_F_VERSION_1, VIRTIO_ID_NET, VirtioMmioDevice};
use crate::slog_info;
use crate::slog_warn;

// ============================================================================
// VirtIO-net 常量
// ============================================================================

/// `VirtIO` 网络头大小 (12 字节, 旧版与 v1 相同)
pub const NET_HDR_SIZE: usize = 12;

/// 默认 RX 缓冲区大小 (字节)
pub const RX_BUFFER_SIZE: usize = 2048;

/// RX virtqueue 索引
pub const RX_QUEUE_INDEX: u16 = 0;
/// TX virtqueue 索引
pub const TX_QUEUE_INDEX: u16 = 1;

// ── Feature bits ──

/// `VIRTIO_NET_F_CSUM`: 设备处理校验和
pub const VIRTIO_NET_F_CSUM: u64 = 1 << 0;
/// `VIRTIO_NET_F_GUEST_CSUM`: 驱动提供校验和
pub const VIRTIO_NET_F_GUEST_CSUM: u64 = 1 << 1;
/// `VIRTIO_NET_F_MAC`: 设备提供 MAC 地址
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
/// `VIRTIO_NET_F_GSO`: 驱动支持 GSO
pub const VIRTIO_NET_F_GSO: u64 = 1 << 6;
/// `VIRTIO_NET_F_GUEST_TSO4`: 驱动接收 TSO4
pub const VIRTIO_NET_F_GUEST_TSO4: u64 = 1 << 7;
/// `VIRTIO_NET_F_GUEST_TSO6`: 驱动接收 TSO6
pub const VIRTIO_NET_F_GUEST_TSO6: u64 = 1 << 8;
/// `VIRTIO_NET_F_MRG_RXBUF`: 合并接收缓冲区
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
/// `VIRTIO_NET_F_STATUS`: 设备提供链路状态
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
/// `VIRTIO_NET_F_CTRL_VQ`: 设备支持控制队列
pub const VIRTIO_NET_F_CTRL_VQ: u64 = 1 << 17;

// ── 配置空间偏移 (相对于 0x100) ──

/// MAC 地址偏移 (6 字节)
pub const NET_CONFIG_MAC: usize = 0x00;
/// 链路状态偏移 (2 字节, bit 0 = link up)
pub const NET_CONFIG_STATUS: usize = 0x06;

// ── 链路状态 ──

/// 链路 UP
pub const NET_STATUS_LINK_UP: u16 = 1;

// ============================================================================
// 网络头结构体
// ============================================================================

/// `VirtIO` 网络头 (12 字节, 旧版与 v1 相同).
///
/// 每个 TX/RX 包前的固定头.
/// `num_buffers` 仅在协商 `VIRTIO_NET_F_MRG_RXBUF` 时有意义.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtioNetHdr {
    /// 特性标志
    pub flags: u8,
    /// GSO 类型
    pub gso_type: u8,
    /// 头部长度
    pub hdr_len: u16,
    /// GSO 大小
    pub gso_size: u16,
    /// 校验和起始偏移
    pub csum_start: u16,
    /// 校验和偏移
    pub csum_offset: u16,
    /// 合并缓冲区数 (`MRG_RXBUF`)
    pub num_buffers: u16,
}

impl VirtioNetHdr {
    /// 创建空头 (无 GSO, 无校验和卸载)
    pub fn empty() -> Self {
        Self::default()
    }

    /// 头大小 (字节)
    pub const fn size() -> usize {
        core::mem::size_of::<Self>()
    }
}

// ============================================================================
// 安全驱动逻辑
// ============================================================================

/// `VirtIO` 网络设备安全驱动 (functions 层, 0 unsafe)。
///
/// 封装 `VirtIO` 网络设备的初始化序列与配置读取, 通过 privileged `VirtioMmioDevice`
/// 安全代理访问 MMIO。DMA 缓冲区管理与 `VirtQueue` 操作保留在 privileged 层。
///
/// ## 初始化流程
///
/// 1. `VirtioNetDriver::new(device)` — 验证设备 ID, 执行初始化序列
/// 2. 读取 MAC 地址与链路状态
/// 3. privileged: 分配 RX/TX `VirtQueue` 并配置
/// 4. `set_driver_ok()` — 设备进入 live 状态
pub struct VirtioNetDriver {
    /// MMIO 设备传输代理 (privileged transport 机制)
    device: VirtioMmioDevice,
    /// MAC 地址 (从配置空间读取)
    mac: [u8; 6],
    /// 链路状态 (true = up)
    link_up: bool,
    /// 设备支持的特性位
    negotiated_features: u64,
    /// 协商后的头大小: 10 (现代 `VERSION_1`) 或 12 (旧版)
    hdr_size: usize,
    /// RX virtqueue (队列 0)
    rx_vq: VirtQueue,
    /// TX virtqueue (队列 1)
    tx_vq: VirtQueue,
    /// RX 缓冲区表 (desc_idx → DmaBuffer), §6.4 迁业务 (privileged rx_buffers 等价)
    ///
    /// 与 privileged VirtioNet::rx_buffers 同构: `desc_idx == 槽位索引`
    /// (refill 按序提交描述符, 设备按序消费, 回收后重提交复用同槽缓冲区).
    rx_buffers: [Option<DmaBuffer>; 32],
}

impl VirtioNetDriver {
    /// 创建并初始化 `VirtIO` 网络设备驱动。
    ///
    /// 验证设备 ID 为网络设备, 执行完整初始化序列 (Reset → Ack → Driver → Feature → `Features_OK`)。
    /// 返回 `Some(VirtioNetDriver)` 表示设备就绪, 可继续配置 `VirtQueue`。
    ///
    /// # 参数
    /// - `device`: 已探测到的 `VirtIO` 设备 (`device_id` 必须为 `VIRTIO_ID_NET`)
    pub fn new(device: VirtioMmioDevice) -> Option<Self> {
        if device.device_id() != VIRTIO_ID_NET {
            slog_warn!(
                Driver,
                "virtio-net: 期望 device_id={}, 实际={}",
                VIRTIO_ID_NET,
                device.device_id()
            );
            return None;
        }

        let is_legacy = device.is_legacy();
        slog_info!(
            Driver,
            "virtio-net: 初始化 at {:#x} (version={}, {} mode)",
            device.mmio_base(),
            device.version(),
            if is_legacy { "legacy" } else { "modern" }
        );

        // Step 1: Reset
        device.reset();

        // Step 2: ACKNOWLEDGE
        device.ack();

        // Step 3: DRIVER
        device.set_driver();

        // Step 4: 特性协商
        let dev_features = device.device_features();
        slog_info!(Driver, "virtio-net: 设备特性={:#018x}", dev_features);

        let has_v1 = (dev_features & VIRTIO_F_VERSION_1) != 0;
        let negotiated_v1: bool;
        let hdr_size: usize;

        if !is_legacy || has_v1 {
            // 现代模式: 协商 VERSION_1 + MAC + STATUS
            negotiated_v1 = true;
            hdr_size = NET_HDR_SIZE;
            let feat = VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;
            device.set_driver_features(feat);
            slog_info!(
                Driver,
                "virtio-net: 协商现代特性 (VERSION_1 + MAC + STATUS)"
            );
        } else {
            // 传统模式: MAC + STATUS (如果设备支持)
            negotiated_v1 = false;
            hdr_size = NET_HDR_SIZE;
            let mut feat = VIRTIO_NET_F_MAC;
            if dev_features & VIRTIO_NET_F_STATUS != 0 {
                feat |= VIRTIO_NET_F_STATUS;
            }
            device.set_driver_features(feat);
            slog_info!(Driver, "virtio-net: 协商传统特性");
        }

        // Step 5: FEATURES_OK
        if !device.features_ok() {
            slog_warn!(Driver, "virtio-net: FEATURES_OK 被拒绝");
            return None;
        }

        // 分配 RX/TX VirtQueue
        let vq_legacy = !has_v1;
        let rx_vq = VirtQueue::new(vq_legacy)?;
        let tx_vq = VirtQueue::new(vq_legacy)?;

        // 从配置空间读取 MAC 地址
        let mut mac: [u8; 6] = [0; 6];
        for i in 0..6 {
            mac[i] = (device.read_config32(NET_CONFIG_MAC + (i & !3)) >> ((i & 3) * 8)) as u8;
        }

        slog_info!(
            Driver,
            "virtio-net: MAC={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );

        // 读取链路状态
        let link_status = device.read_config32(NET_CONFIG_STATUS) as u16;
        let link_up = (link_status & NET_STATUS_LINK_UP) != 0;
        slog_info!(
            Driver,
            "virtio-net: 链路 {}",
            if link_up { "UP" } else { "DOWN" }
        );

        let net = Self {
            device,
            mac,
            link_up,
            negotiated_features: if negotiated_v1 {
                VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS
            } else {
                VIRTIO_NET_F_MAC
            },
            hdr_size,
            rx_vq,
            tx_vq,
            rx_buffers: [const { None }; 32],
        };
        // RX 预填移至 finalize (队列 MMIO 配置 + DRIVER_OK 之后执行), 与旧
        // privileged 驱动时序一致 (避免对未配置队列 MMIO notify)
        Some(net)
    }

    /// 设置 `DRIVER_OK` (设备进入 live 状态).
    ///
    /// 必须在所有 virtqueue 配置完成后调用.
    pub fn set_driver_ok(&self) {
        self.device.set_driver_ok();
        slog_info!(Driver, "virtio-net: DRIVER_OK 已设置");
    }

    /// 完成设备初始化并进入 live (批次 Z ④: 注册前调用).
    ///
    /// 等价于 privileged `VirtioNet::new` 的收尾 (与 blk `finalize` 同构):
    /// 配置 vq0 (RX) / vq1 (TX) MMIO 寄存器 → 设置 `DRIVER_OK` → 预填
    /// RX 队列 (refill 在 `DRIVER_OK` 后执行, 与旧 privileged 驱动时序一致)。
    pub fn finalize(&mut self) {
        let _ = self.setup_queue(
            RX_QUEUE_INDEX,
            self.rx_vq.desc_paddr(),
            self.rx_vq.avail_paddr(),
            self.rx_vq.used_paddr(),
        );
        let _ = self.setup_queue(
            TX_QUEUE_INDEX,
            self.tx_vq.desc_paddr(),
            self.tx_vq.avail_paddr(),
            self.tx_vq.used_paddr(),
        );
        self.set_driver_ok();
        // 用空缓冲区预填 RX 队列 (§6.4 迁业务: privileged refill_rx 等价)
        self.refill_rx();
    }

    /// 获取 MMIO 设备引用 (用于 `VirtQueue` 配置).
    pub fn device(&self) -> &VirtioMmioDevice {
        &self.device
    }

    /// 获取 MAC 地址.
    pub fn mac(&self) -> &[u8; 6] {
        &self.mac
    }

    /// 链路是否 UP.
    pub fn link_up(&self) -> bool {
        self.link_up
    }

    /// 获取协商后的特性位.
    pub fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    /// 检查是否协商了指定特性.
    pub fn has_feature(&self, feature: u64) -> bool {
        self.negotiated_features & feature != 0
    }

    /// 获取头大小 (10 或 12 字节).
    pub fn hdr_size(&self) -> usize {
        self.hdr_size
    }

    /// 是否为传统模式 (未协商 `VERSION_1`).
    pub fn is_legacy(&self) -> bool {
        self.negotiated_features & VIRTIO_F_VERSION_1 == 0
    }

    // ── 队列配置辅助 (通过 VirtioMmioDevice MMIO) ──

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 配置指定 virtqueue 的 MMIO 寄存器.
    ///
    /// # 参数
    /// - `vq_index`: virtqueue 索引 (RX=0, TX=1)
    /// - `desc_paddr`: 描述符表物理地址
    /// - `avail_paddr`: available ring 物理地址
    /// - `used_paddr`: used ring 物理地址
    ///
    /// # 返回
    /// - `Ok(max_size)`: 队列最大尺寸
    /// - `Err(())`: 配置失败
    ///
    /// # Errors
    /// 当底层 virtio 设备队列配置失败时返回 `Err(())`.
    pub fn setup_queue(
        &self,
        vq_index: u16,
        desc_paddr: u64,
        avail_paddr: u64,
        used_paddr: u64,
    ) -> Result<u32, ()> {
        self.device.select_queue(vq_index);
        let max_size = self.device.queue_num_max();
        slog_info!(Driver, "virtio-net: vq{} max_size={}", vq_index, max_size);

        if self.is_legacy() {
            // 传统模式: 使用 PFN 接口
            self.device.setup_queue_legacy(desc_paddr);
        } else {
            // 现代模式: 使用 64 位地址
            self.device
                .setup_queue_addrs(desc_paddr, avail_paddr, used_paddr);
        }

        self.device.set_queue_ready();
        slog_info!(Driver, "virtio-net: vq{} ready", vq_index);
        Ok(max_size)
    }

    /// 通知设备: 指定队列有新描述符.
    pub fn notify(&self, vq_index: u16) {
        self.device.notify_queue(vq_index);
    }

    /// 读取并应答中断状态.
    pub fn ack_interrupt(&self) -> u32 {
        let status = self.device.interrupt_status();
        if status != 0 {
            self.device.ack_interrupt_mask(status);
        }
        status
    }

    /// 读取当前链路状态 (从配置空间实时读取).
    pub fn read_link_status(&self) -> bool {
        let status = self.device.read_config32(NET_CONFIG_STATUS) as u16;
        (status & NET_STATUS_LINK_UP) != 0
    }

    // ── 数据路径: 网络包收发 ──

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 发送网络包.
    ///
    /// `data` 包含完整以太网帧 (不含 `VirtIO` 头). 框架自动添加头.
    /// 发送完成后 DMA 缓冲区自动释放.
    ///
    /// # Errors
    /// 当 `data` 为空或长度超过 65535 时返回 `Err(())`;
    /// 当 DMA 缓冲区分配失败或设备未接受描述符时也返回 `Err(())`.
    pub fn send_packet(&mut self, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() || data.len() > 65535 {
            return Err(());
        }

        // DMA 缓冲区: VirtIO 头 + 帧数据
        let total = self.hdr_size + data.len();
        let mut dma = if let Some(b) = DmaBuffer::new(total) {
            b
        } else {
            slog_warn!(Driver, "virtio-net: TX DMA 缓冲区分配失败");
            return Err(());
        };

        // 清零 VirtIO 网络头 (safe: 通过 DmaBuffer)
        for i in 0..self.hdr_size {
            dma.write_byte(i, 0);
        }

        // 复制帧数据到 DMA 缓冲区 (紧跟在头之后)
        dma.write_slice(self.hdr_size, data);

        // 准备 TX 描述符 (设备读)
        let desc_idx = self
            .tx_vq
            .prepare_desc(dma.phys_addr(), total as u32, false);
        if desc_idx == 0xFFFF {
            slog_warn!(Driver, "virtio-net: TX 描述符耗尽");
            return Err(());
        }

        // 提交并通知设备
        self.tx_vq.submit(desc_idx);
        self.tx_vq.commit_and_kick();
        self.device.notify_queue(TX_QUEUE_INDEX);

        // 轮询等待 TX 完成
        let mut waited = 0u32;
        loop {
            if let Some((_id, _len)) = self.tx_vq.pop_used() {
                self.tx_vq.reclaim_desc(desc_idx);
                return Ok(());
            }
            if waited > 100_000 {
                self.tx_vq.reclaim_desc(desc_idx);
                slog_warn!(Driver, "virtio-net: TX 超时");
                return Err(());
            }
            waited += 1;
            core::hint::spin_loop();
        }
    }

    /// 尝试接收一个网络包.
    ///
    /// 将包数据 (不含 `VirtIO` 头) 复制到 `buf`, 返回实际拷贝的字节数.
    /// 无包可读时返回 0. 内部自动回收并重新填充 RX 描述符 (§6.4 迁业务:
    /// 完整数据路径, 维护 desc_idx → DmaBuffer 映射, 取代原"返回 0 丢弃"半成品).
    pub fn try_receive(&mut self, buf: &mut [u8]) -> usize {
        let Some((desc_idx, len)) = self.rx_vq.pop_used() else {
            return 0;
        };
        let buf_idx = desc_idx as usize;

        if buf_idx >= self.rx_buffers.len() || self.rx_buffers[buf_idx].is_none() {
            // 槽位无效: 回收描述符 (无缓冲区可重提交)
            self.rx_vq.reclaim_desc(desc_idx);
            return 0;
        }

        let data_len = len as usize;
        if data_len <= self.hdr_size || data_len > RX_BUFFER_SIZE {
            // 异常包: 回收并复用同槽缓冲区重新填充
            self.rx_vq.reclaim_desc(desc_idx);
            self.refill_single_rx(buf_idx);
            return 0;
        }

        // 从槽位 DMA 缓冲区拷贝有效载荷 (跳过 VirtIO 头)
        let payload_len = data_len - self.hdr_size;
        let copy_len = payload_len.min(buf.len());
        if let Some(dma) = &self.rx_buffers[buf_idx] {
            dma.read_slice(self.hdr_size, &mut buf[..copy_len]);
        }

        // 回收描述符并复用同槽缓冲区重新填充
        self.rx_vq.reclaim_desc(desc_idx);
        self.refill_single_rx(buf_idx);

        copy_len
    }

    /// 用空缓冲区填充 RX virtqueue (§6.4 迁业务: privileged refill_rx 等价).
    ///
    /// 为每个空槽位分配 `DmaBuffer`, 提交设备可写描述符.
    fn refill_rx(&mut self) {
        for i in 0..self.rx_buffers.len() {
            if self.rx_buffers[i].is_some() {
                continue;
            }
            let Some(dma) = DmaBuffer::new(RX_BUFFER_SIZE) else {
                slog_warn!(Driver, "virtio-net: RX 缓冲区分配失败 (slot {i})");
                return;
            };
            let desc = self
                .rx_vq
                .prepare_desc(dma.phys_addr(), RX_BUFFER_SIZE as u32, true);
            if desc == 0xFFFF {
                return;
            }
            // desc_idx 即槽位索引 (refill 按序提交, 设备按序消费)
            self.rx_buffers[desc as usize] = Some(dma);
            self.rx_vq.submit(desc);
        }
        self.rx_vq.commit_and_kick();
        self.device.notify_queue(RX_QUEUE_INDEX);
    }

    /// 回收单个 RX 描述符并复用同槽缓冲区重新填充.
    fn refill_single_rx(&mut self, buf_idx: usize) {
        if let Some(dma) = &self.rx_buffers[buf_idx] {
            let desc = self
                .rx_vq
                .prepare_desc(dma.phys_addr(), RX_BUFFER_SIZE as u32, true);
            if desc != 0xFFFF {
                self.rx_vq.submit(desc);
                self.rx_vq.commit_and_kick();
                self.device.notify_queue(RX_QUEUE_INDEX);
            }
        }
    }
}

// ============================================================================
// NetOps 安全桥 (批次 Z ④): impl trait → privileged 泛型桥接入 smoltcp
// ============================================================================

impl crate::privileged::net::NetDeviceOps for VirtioNetDriver {
    fn send(&mut self, data: &[u8]) -> i32 {
        match self.send_packet(data) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(
        clippy::cast_possible_truncation,
        reason = "RX 缓冲区上限 2048 字节 (RX_BUFFER_SIZE), usize→i32 截断不可达"
    )]
    fn try_receive(&mut self, buf: &mut [u8]) -> i32 {
        // 审核 P1-1: 显式全路径调用 — trait impl 块内 `self.try_receive(buf)`
        // 存在同名方法解析风险 (trait 方法与 inherent 方法同签名), 必须显式
        // 限定 inherent 方法, 杜绝解析到 trait 方法自身造成无限递归。
        VirtioNetDriver::try_receive(self, buf) as i32
    }

    fn get_mac(&self) -> [u8; 6] {
        *self.mac()
    }

    /// 中断处理: 读取并回写 InterruptACK 清除设备中断状态 (virtio-mmio
    /// 无独立中断掩码, 设备在 InterruptStatus != 0 时拉高 IRQ 线)。
    ///
    /// 由 privileged 统一 ISR 在中断上下文调用; 清除中断源后底半部
    /// (`NetRx` softirq) 经 `poll_network` 完成收包。丢弃返回值: 收包不
    /// 依赖具体中断原因 (轮询式 `try_receive` 兜底)。
    fn handle_irq(&mut self) {
        let _ = self.ack_interrupt();
    }
}
