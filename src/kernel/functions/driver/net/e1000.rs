#![deny(unsafe_code)]
//! Intel E1000 网卡驱动 (functions 层, 0 unsafe).
//!
//! ## 架构分工 (§4.1 归属决策树)
//!
//! - **privileged (机制)**: `e1000_io::E1000Io` 提供 MMIO 安全代理 (封装 IoMem);
//!   `e1000::{TxRing, RxRing}` 封装 DMA 描述符环的全部裸指针操作; `privileged::pci`
//!   提供 PCI 配置空间与 BAR 枚举的安全 API; `privileged::dma` 提供一致性 DMA 内存.
//! - **functions (功能, 本文件)**: 实现 `NetDeviceOps` 安全桥 trait, 承载 E1000 初始化
//!   序列 (复位/链路探测/描述符环配置/收发), 以及 PCI 网卡探测; 经
//!   `register_net_device` 交由 privileged 泛型桥生成 extern "C" 回调接入 smoltcp.
//!
//! 本模块不含任何 unsafe: 全部 MMIO / DMA / PCI 访问均经 privileged safe API.
//!
//! ## 条件编译
//!
//! E1000 探测依赖 x86_64 的 PCI 枚举 (`privileged::pci` 仅 x86_64 提供) 与
//! `kernel_test` 下不可用的 DMA 环分配, 故真实实现整体收敛于私有内层模块
//! `e1000_impl`, 并仅在该组合下编译; 其余构建提供恒返回 `None` 的探测桩,
//! 保证调用点 (functions `net_init`) 无需条件编译.

/// E1000 真实实现 (x86_64 且非 kernel_test).
#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
mod e1000_impl {
    use alloc::boxed::Box;

    use crate::privileged::driver::DriverError;
    use crate::privileged::driver::net::dma_ring::{
        E1000_RX_BUFFER_SIZE, E1000_RX_RING_SIZE, E1000_TX_RING_SIZE,
    };
    use crate::privileged::driver::net::e1000::{RxRing, TxRing};
    use crate::privileged::driver::net::e1000_io::{
        E1000_CTRL_ASDE, E1000_CTRL_FD, E1000_CTRL_FRCDPX, E1000_CTRL_FRCSPD, E1000_CTRL_RST,
        E1000_CTRL_SLU, E1000_CTRL_SPEED_1000, E1000_ICR_LSC, E1000_ICR_RXDMT0, E1000_ICR_RXT0,
        E1000_RCTL_BAM, E1000_RCTL_BSIZE_2048, E1000_RCTL_EN, E1000_RCTL_MPE, E1000_RCTL_SBP,
        E1000_RCTL_SECRC, E1000_RCTL_UPE, E1000_TCTL_COLD_FD, E1000_TCTL_CT_FD, E1000_TCTL_EN,
        E1000_TCTL_PSP, E1000_TIMEOUT, E1000Io,
    };
    use crate::privileged::mm::PhysAddr;
    use crate::privileged::net::{NetDeviceOps, NetDeviceRegistration, register_net_device};
    use crate::privileged::pci::{
        self, BarType, CLASS_NETWORK, PCI_CMD_BUS_MASTER, PCI_CMD_MEMORY_SPACE,
    };

    // ========================================================================
    // 常量 (硬件规范值, 见 §9.3)
    // ========================================================================

    /// Intel 厂商 ID.
    const E1000_VENDOR_ID: u16 = 0x8086;

    /// BAR0 MMIO 窗口大小 (E1000 规范为 128 KiB).
    const E1000_MMIO_SIZE: usize = 128 * 1024;

    /// PCI 命令寄存器偏移 (privileged::pci 未导出该偏移, 本地定义).
    const PCI_REG_COMMAND: u8 = 0x04;

    /// 单帧最大长度 (> 标准 MTU + 以太网头, 与 RX 缓冲区同量级).
    const E1000_MAX_FRAME: usize = 2048;

    /// 链路就绪轮询上限 (等待 PHY 自动协商完成).
    const E1000_LINK_POLL: u32 = 500_000;

    /// 默认 Inter-Packet Gap (IEEE 802.3 全双工推荐值).
    const E1000_IPG_DEFAULT: u32 = 0x0060_200A;

    // ========================================================================
    // E1000 网络设备驱动
    // ========================================================================

    /// E1000 网卡驱动.
    ///
    /// 持有 privileged 的 MMIO 安全代理与两个 DMA 描述符环; 生命周期由
    /// `register_net_device` 的 `Box::into_raw` 转移至内核存续.
    struct E1000NetDriver {
        /// MMIO 安全访问器 (privileged).
        io: E1000Io,
        /// 网卡 MAC 地址.
        mac: [u8; 6],
        /// TX 描述符环 (初始化后存在).
        tx_ring: Option<TxRing>,
        /// RX 描述符环 (初始化后存在).
        rx_ring: Option<RxRing>,
    }

    impl E1000NetDriver {
        /// 构造驱动实例 (尚未初始化硬件).
        fn new(io: E1000Io, mac: [u8; 6]) -> Self {
            Self {
                io,
                mac,
                tx_ring: None,
                rx_ring: None,
            }
        }

        /// 复位网卡并等待链路就绪.
        ///
        /// 写 CTRL.RST 触发全局复位, 轮询复位完成; 随后配置自动协商 / 速度 / 双工,
        /// 再轮询链路状态. 复位未自清与链路未就绪均不视为致命错误 (QEMU 等环境
        /// 可能不上报), 记警告后继续初始化.
        fn reset_and_detect_link(&self) {
            let ctrl = self.io.ctrl();
            self.io.set_ctrl(ctrl | E1000_CTRL_RST);

            // 全局复位位 (CTRL.RST) 通常自清; 有界轮询等待其归零. 若超时未清
            // (部分实现需软件显式清位), 记警告并继续 (对齐 Linux e1000_reset_hw
            // 的容错处理); 下方写 CTRL 时已用 `& !E1000_CTRL_RST` 主动清除该位.
            let mut reset_wait = 0u32;
            while reset_wait < E1000_TIMEOUT {
                if self.io.ctrl() & E1000_CTRL_RST == 0 {
                    break;
                }
                reset_wait += 1;
                core::hint::spin_loop();
            }
            if self.io.ctrl() & E1000_CTRL_RST != 0 {
                crate::slog_warn!(Driver, "e1000: CTRL.RST 未在超时内自清, 主动清位并继续");
            }

            // 复位后先屏蔽全部中断, 避免复位残留事件误触发.
            self.io.irq_disable_all();

            let new_ctrl = (self.io.ctrl() & !E1000_CTRL_RST)
                | E1000_CTRL_SLU
                | E1000_CTRL_ASDE
                | E1000_CTRL_FRCSPD
                | E1000_CTRL_SPEED_1000
                | E1000_CTRL_FRCDPX
                | E1000_CTRL_FD;
            self.io.set_ctrl(new_ctrl);

            let mut link_wait = 0u32;
            while link_wait < E1000_LINK_POLL {
                if self.io.link_is_up() {
                    let (speed, duplex) = self.io.link_status();
                    crate::slog_info!(Driver, "e1000: 链路就绪 speed={} duplex={}", speed, duplex);
                    return;
                }
                link_wait += 1;
                core::hint::spin_loop();
            }
            crate::slog_warn!(Driver, "e1000: 链路未就绪, 继续初始化");
        }

        /// 分配并配置收发描述符环 (写入环基址 / 长度 / 指针寄存器).
        fn setup_descriptor_rings(&mut self) -> Result<(), DriverError> {
            let Some(tx_ring) = TxRing::alloc(E1000_TX_RING_SIZE) else {
                return Err(DriverError::HardwareError);
            };
            self.io.set_tx_base(tx_ring.phys_addr());
            self.io.set_tx_len(tx_ring.len_bytes() as u32);
            self.io.set_tx_head_raw(0);
            self.io.set_tx_tail(0);
            self.tx_ring = Some(tx_ring);

            let Some(rx_ring) = RxRing::alloc(E1000_RX_RING_SIZE, E1000_RX_BUFFER_SIZE) else {
                return Err(DriverError::HardwareError);
            };
            self.io.set_rx_base(rx_ring.phys_addr());
            self.io.set_rx_len(rx_ring.len_bytes() as u32);
            self.io.set_rx_head_raw(0);
            // 硬件持有 [head, tail] 之外的描述符; 初始 tail 指向最后一个槽位.
            self.io.set_rx_tail((E1000_RX_RING_SIZE - 1) as u32);
            self.rx_ring = Some(rx_ring);

            Ok(())
        }

        /// 使能收发引擎并写入 MAC / IPG / 中断掩码.
        fn complete_init(&self) {
            self.io
                .set_tx_ctl(E1000_TCTL_EN | E1000_TCTL_PSP | E1000_TCTL_COLD_FD | E1000_TCTL_CT_FD);
            self.io.set_rx_ctl(
                E1000_RCTL_EN
                    | E1000_RCTL_SBP
                    | E1000_RCTL_UPE
                    | E1000_RCTL_MPE
                    | E1000_RCTL_BAM
                    | E1000_RCTL_SECRC
                    | E1000_RCTL_BSIZE_2048,
            );
            self.io.set_mac(self.mac);
            self.io.set_rx_tail((E1000_RX_RING_SIZE - 1) as u32);
            self.io.set_ipg(E1000_IPG_DEFAULT);
            self.io
                .irq_enable(E1000_ICR_RXT0 | E1000_ICR_RXDMT0 | E1000_ICR_LSC);
            crate::slog_info!(Driver, "e1000: 初始化完成");
        }

        /// 完整初始化序列: 复位 → 描述符环 → 使能.
        fn init(&mut self) -> Result<(), DriverError> {
            self.reset_and_detect_link();
            self.setup_descriptor_rings()?;
            self.complete_init();
            Ok(())
        }

        /// 发送一个以太网帧, 返回发送字节数.
        fn send_packet(&mut self, data: &[u8]) -> Result<usize, DriverError> {
            let total_len = data.len().min(E1000_MAX_FRAME);
            if total_len == 0 {
                return Err(DriverError::InvalidParameter);
            }
            let Some(tx_ring) = self.tx_ring.as_mut() else {
                return Err(DriverError::NotInitialized);
            };

            // 等待当前 tail 描述符完成 (硬件置 DD), 描述符环满则超时.
            let mut waited = 0u32;
            while !tx_ring.is_done(tx_ring.tail()) {
                if waited >= E1000_TIMEOUT {
                    return Err(DriverError::Timeout);
                }
                waited += 1;
                core::hint::spin_loop();
            }

            tx_ring.prepare_from_virt(data.as_ptr() as u64, total_len as u16);
            // 保证描述符写入先于 tail 更新对设备可见 (设备随后 DMA 读取).
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            tx_ring.advance_tail();
            self.io.set_tx_tail(tx_ring.tail() as u32);
            Ok(total_len)
        }

        /// 尝试接收一个以太网帧到 `buf`, 无数据返回 `None`.
        ///
        /// 循环跳过带错误的描述符 (回收后继续), 直到取到有效帧或无可用数据.
        fn receive_packet(&mut self, buf: &mut [u8]) -> Option<usize> {
            loop {
                let Some(rx_ring) = self.rx_ring.as_mut() else {
                    return None;
                };
                let tail = rx_ring.tail();
                // head == tail 表示硬件已处理完所有待收描述符.
                if tail == self.io.rx_head() as usize || !rx_ring.is_done(tail) {
                    return None;
                }

                if rx_ring.has_errors(tail) {
                    let err = rx_ring.errors(tail);
                    crate::slog_warn!(Driver, "e1000: RX 描述符错误 err=0x{:02X}", err);
                    rx_ring.clear_status(tail);
                    rx_ring.advance_tail();
                    self.io.set_rx_tail(rx_ring.tail() as u32);
                    continue;
                }

                let len = rx_ring.copy_packet(tail, buf);
                rx_ring.clear_status(tail);
                rx_ring.advance_tail();
                self.io.set_rx_tail(rx_ring.tail() as u32);
                return Some(len);
            }
        }
    }

    /// `NetDeviceOps` 安全桥实现 (privileged 泛型桥生成 extern "C" 回调).
    impl NetDeviceOps for E1000NetDriver {
        fn send(&mut self, data: &[u8]) -> i32 {
            match self.send_packet(data) {
                Ok(_) => 0,
                Err(_) => -1,
            }
        }

        fn try_receive(&mut self, buf: &mut [u8]) -> i32 {
            self.receive_packet(buf).map_or(0, |n| n as i32)
        }

        fn get_mac(&self) -> [u8; 6] {
            self.mac
        }

        /// 中断处理: 读取并回写 ICR 清除中断原因位 (write-1-to-clear)。
        ///
        /// 由 privileged 统一 ISR 在中断上下文调用; 清除中断源后底半部
        /// (`NetRx` softirq) 经 `poll_network` 完成收包。丢弃 ICR 读取值:
        /// 收包不依赖具体中断原因 (轮询式 `try_receive` 兜底)。
        fn handle_irq(&mut self) {
            let _ = self.io.irq_ack();
        }
    }

    // ========================================================================
    // MAC 地址读取
    // ========================================================================

    /// 从 EEPROM 读取 MAC 地址 (真实硬件).
    #[cfg(feature = "e1000-real-hw")]
    fn read_mac_address(io: &E1000Io) -> [u8; 6] {
        let mut mac = [0u8; 6];
        for i in 0..3usize {
            let word = io.eeprom_read(i as u8);
            mac[i * 2] = (word & 0xFF) as u8;
            mac[i * 2 + 1] = (word >> 8) as u8;
        }
        crate::slog_info!(
            Driver,
            "e1000: MAC {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
        mac
    }

    /// 返回固定 MAC 地址 (默认构建: QEMU/仿真环境 EEPROM 不可靠).
    #[cfg(not(feature = "e1000-real-hw"))]
    fn read_mac_address(_io: &E1000Io) -> [u8; 6] {
        [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]
    }

    // ========================================================================
    // PCI 探测与注册
    // ========================================================================

    /// 探测 E1000 网卡并构造注册数据 (functions 探测回调).
    ///
    /// 流程: 幂等 `pci::init` → 扫描 PCI 总线 → 匹配 Intel 网络类设备 →
    /// 校验 BAR0 为 MMIO → 使能 MEMORY_SPACE / BUS_MASTER → 映射 MMIO →
    /// 读 MAC → 初始化硬件 → 注册.
    pub(crate) fn e1000_net_registration() -> Option<NetDeviceRegistration> {
        let _ = pci::init();
        let devices = pci::scan_all_buses();
        let dev = devices
            .iter()
            .find(|d| d.class_code == CLASS_NETWORK && d.vendor_id == E1000_VENDOR_ID)?;

        let bar = dev.bars.first()?;
        if !matches!(bar.bar_type, BarType::Memory32 | BarType::Memory64) {
            crate::slog_warn!(Driver, "e1000: BAR0 非 MMIO 类型, 跳过");
            return None;
        }
        if bar.base_addr == 0 || bar.base_addr == 0xFFFF_FFFF {
            crate::slog_warn!(Driver, "e1000: BAR0 地址无效");
            return None;
        }

        // 使能 MMIO 空间与总线主控 (privileged pci::init 不做此配置).
        let cmd = pci::read_config_word(dev.bus, dev.device, dev.function, PCI_REG_COMMAND);
        pci::write_config_word(
            dev.bus,
            dev.device,
            dev.function,
            PCI_REG_COMMAND,
            cmd | PCI_CMD_MEMORY_SPACE | PCI_CMD_BUS_MASTER,
        );

        let Ok(io) = E1000Io::new(PhysAddr::new(bar.base_addr), E1000_MMIO_SIZE) else {
            crate::slog_warn!(Driver, "e1000: BAR0 MMIO 映射失败");
            return None;
        };

        let mac = read_mac_address(&io);
        let mut driver = E1000NetDriver::new(io, mac);
        if let Err(e) = driver.init() {
            crate::slog_err!(Driver, "e1000: 初始化失败 {:?}", e);
            return None;
        }

        crate::slog_info!(
            Driver,
            "e1000: 探测成功 MAC {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );

        // ISSUE-SRC-008: 接线中断 — 使能 MSI-X 并注册网卡 ISR。中断到达时
        // privileged 统一分发做设备侧 ack (handle_irq) 并置位 NetRx 底半部,
        // 由 poll_network 完成收包。启动临界区 IF=0, 不存在"中断先于 ISR
        // 注册"的竞态窗口。MSI-X 不可用 (如 QEMU 82540EM) 时 fail-quiet
        // 回落轮询模式, 不影响收包正确性。
        if let Some(msix) = pci::msi::msix_enable(dev, 1) {
            if let Err(e) = crate::privileged::net::net_register_msix_isr(msix.base_vector) {
                crate::slog_warn!(Driver, "e1000: MSI-X ISR 注册失败 {:?}, 回落轮询", e);
            }
        } else {
            crate::slog_warn!(Driver, "e1000: MSI-X 不可用, 回落轮询模式");
        }

        Some(register_net_device(Box::new(driver)))
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
pub(crate) use e1000_impl::e1000_net_registration;

/// E1000 探测桩 (非 x86_64 或 kernel_test 构建, 恒返回 `None`).
///
/// 保持调用点 (functions `net_init`) 无需条件编译; 该组合下无 PCI 枚举能力,
/// 故直接报告"未探测到设备".
#[cfg(all(not(target_arch = "x86_64"), not(feature = "kernel_test")))]
pub(crate) fn e1000_net_registration() -> Option<crate::privileged::net::NetDeviceRegistration> {
    None
}
