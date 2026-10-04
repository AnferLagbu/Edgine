// B08-12 (DECISION-052 路线 C): DMA 流状态机消除平行实现.
//
// 本模块不再本地实现 DmaStream 模拟 (PhysPage/FrameRef 复刻已删除),
// 回归测试重写为直接验证内核 `framework::dma_buf::DmaStream` 真实实现
// (host-test feature 暴露): 经 `Frame::from_raw` 构造测试帧, 验证
// 对齐/溢出/大小校验与 ToDevice/FromDevice/Bidirectional 状态机转换.
//
// 覆盖说明: 原 mock 的"未对齐/零大小/SizeOverflow"用例被移除 — 内核中这些校验的
// 防线在 `Frame` 构造边界 (from_raw 的 debug_assert 页对齐 + size = 4K<<order
// 恒非零), host 上无法经公共 API 构造违反这些前置条件的 Frame; 而 paddr+size 溢出
// 的用例必然伴随 `phys_to_virt` (paddr + KERNEL_BASE) 算术溢出 panic, 亦不可测.
// 状态机与 SizeTooLarge 上限校验 (from_frame 内独立校验) 完整保留.

#[cfg(test)]
mod tests {
    use edgine::kernel::framework::dma_buf::{DmaDirection, DmaError, DmaStream, SyncState};
    use edgine::kernel::framework::frame::Frame;
    use edgine::kernel::framework::mm::PhysAddr;

    /// 构造测试用 Frame (host 无真实物理页).
    ///
    /// # SAFETY
    /// phys 仅为纯算术载体: `DmaStream::from_frame` 不 dereference 虚拟地址,
    /// 仅做对齐/溢出/大小校验与状态机转换; 测试帧生命周期限于测试内, 无重复持有.
    unsafe fn frame(paddr: u64, order: u8) -> Frame {
        // SAFETY: 见函数文档 (前置条件与调用点相同)
        unsafe { Frame::from_raw(PhysAddr(paddr), order) }
    }

    // ===== 验证测试 =====

    #[test]
    fn from_aligned_page_ok() {
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::ToDevice).unwrap();
        assert_eq!(d.dma_addr().as_u64(), 0x10000);
        assert_eq!(d.size(), 4096);
        assert_eq!(d.direction(), DmaDirection::ToDevice);
        assert_eq!(d.sync_state(), SyncState::CpuReady);
    }

    #[test]
    fn too_large_rejected() {
        // order 17 → 512 MiB > 256 MiB 上限
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d = DmaStream::from_frame(unsafe { frame(0x10000, 17) }, DmaDirection::ToDevice);
        assert!(matches!(d, Err(DmaError::SizeTooLarge)));
    }

    #[test]
    fn size_at_u64_max_boundary() {
        // order 17 (512 MiB) 超上限
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d = DmaStream::from_frame(unsafe { frame(0, 17) }, DmaDirection::ToDevice);
        assert!(matches!(d, Err(DmaError::SizeTooLarge)));
        // 边界: order 16 = 256 MiB = DMA_MAX_SIZE (4K 倍数), 恰好允许
        let d = DmaStream::from_frame(unsafe { frame(0x1000, 16) }, DmaDirection::ToDevice);
        assert!(d.is_ok());
    }

    // ===== 状态机测试 =====

    #[test]
    fn to_device_lifecycle() {
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::ToDevice).unwrap();
        assert_eq!(d.sync_state(), SyncState::CpuReady);
        // CPU 写完, 调 sync_for_device 进入 DeviceReady
        assert!(d.sync_for_device().is_ok());
        assert_eq!(d.sync_state(), SyncState::DeviceReady);
        // 重复调 sync_for_device 应失败 (状态机不允许)
        assert_eq!(
            d.sync_for_device().unwrap_err(),
            DmaError::InvalidStateTransition
        );
    }

    #[test]
    fn from_device_lifecycle() {
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::FromDevice).unwrap();
        // FromDevice 初始为 DeviceReady (设备已写入)
        assert_eq!(d.sync_state(), SyncState::DeviceReady);
        // CPU 想读, 调 sync_for_cpu 进入 CpuReady
        assert!(d.sync_for_cpu().is_ok());
        assert_eq!(d.sync_state(), SyncState::CpuReady);
        // 重复调应失败
        assert_eq!(
            d.sync_for_cpu().unwrap_err(),
            DmaError::InvalidStateTransition
        );
    }

    #[test]
    fn to_device_cannot_sync_for_cpu() {
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::ToDevice).unwrap();
        // ToDevice 不允许 sync_for_cpu
        assert_eq!(
            d.sync_for_cpu().unwrap_err(),
            DmaError::InvalidStateTransition
        );
    }

    #[test]
    fn from_device_cannot_sync_for_device() {
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::FromDevice).unwrap();
        // FromDevice 不允许 sync_for_device
        assert_eq!(
            d.sync_for_device().unwrap_err(),
            DmaError::InvalidStateTransition
        );
    }

    #[test]
    fn bidir_full_cycle() {
        // Bidirectional 完整周期: CpuReady → DeviceReady → CpuReady → ...
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::Bidirectional)
                .unwrap();
        assert_eq!(d.sync_state(), SyncState::CpuReady);
        assert!(d.sync_for_device().is_ok());
        assert_eq!(d.sync_state(), SyncState::DeviceReady);
        assert!(d.sync_for_cpu().is_ok());
        assert_eq!(d.sync_state(), SyncState::CpuReady);
        // 第二次
        assert!(d.sync_for_device().is_ok());
        assert!(d.sync_for_cpu().is_ok());
        // 1000 次循环
        for _ in 0..1000 {
            assert!(d.sync_for_device().is_ok());
            assert!(d.sync_for_cpu().is_ok());
        }
    }

    // ===== 生命周期测试 =====

    #[test]
    fn frame_lifecycle() {
        // 创建后 drop 释放 (没有 panic 即说明 Frame 正确 Drop)
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d =
            DmaStream::from_frame(unsafe { frame(0x10000, 0) }, DmaDirection::ToDevice).unwrap();
        drop(d);
    }

    // ===== 真实场景模拟 =====

    #[test]
    fn e1000_tx_desc_simulation() {
        // 模拟 e1000 发送描述符: 4K 对齐, ToDevice 方向
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d = DmaStream::from_frame(unsafe { frame(0xFEB_C0000, 0) }, DmaDirection::ToDevice)
            .unwrap();
        // 提交给设备
        assert!(d.sync_for_device().is_ok());
        assert_eq!(d.dma_addr().as_u64(), 0xFEB_C0000);
        // 状态机检查
        assert_eq!(d.sync_state(), SyncState::DeviceReady);
    }

    #[test]
    fn e1000_rx_desc_simulation() {
        // 模拟 e1000 接收描述符: FromDevice 方向
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let mut d =
            DmaStream::from_frame(unsafe { frame(0xFEB_C1000, 0) }, DmaDirection::FromDevice)
                .unwrap();
        // 设备 DMA 完成后, CPU 读描述符
        assert!(d.sync_for_cpu().is_ok());
        assert_eq!(d.dma_addr().as_u64(), 0xFEB_C1000);
        assert_eq!(d.sync_state(), SyncState::CpuReady);
    }

    #[test]
    fn stress_random_dmas() {
        // 1000 个随机 DMA 流的创建 + 完整同步周期
        for i in 0..1000 {
            let paddr = (i as u64) * 0x10_0000;
            let dir = match i % 3 {
                0 => DmaDirection::ToDevice,
                1 => DmaDirection::FromDevice,
                _ => DmaDirection::Bidirectional,
            };
            // SAFETY: paddr = i*0x100000 页对齐, 见 frame() 说明
            let mut d =
                DmaStream::from_frame(unsafe { frame(paddr, 0) }, dir).expect("create failed");
            // 完整同步周期
            for _ in 0..10 {
                match dir {
                    DmaDirection::ToDevice => {
                        let _ = d.sync_for_device();
                    }
                    DmaDirection::FromDevice => {
                        let _ = d.sync_for_cpu();
                    }
                    DmaDirection::Bidirectional => {
                        let _ = d.sync_for_device();
                        let _ = d.sync_for_cpu();
                    }
                }
            }
        }
    }

    #[test]
    fn large_buffer_under_limit() {
        // 边界: 恰好等于 DMA_MAX_SIZE
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d = DmaStream::from_frame(unsafe { frame(0x10000, 16) }, DmaDirection::Bidirectional);
        assert!(d.is_ok());
    }

    #[test]
    fn mid_size_buffer() {
        // 2 MiB huge page (order 9)
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d = DmaStream::from_frame(unsafe { frame(0x200000, 9) }, DmaDirection::Bidirectional)
            .unwrap();
        assert_eq!(d.size(), 2 * 1024 * 1024);
    }

    #[test]
    fn pci_bar_aligned_buffer() {
        // 模拟 PCI BAR 分配的 DMA 缓冲区: 起始地址 4K 对齐, 8KB (order 1)
        let bar_base: u64 = 0xFEB_C0000; // 4K 对齐
        // SAFETY: 页对齐物理地址, 见 frame() 说明
        let d =
            DmaStream::from_frame(unsafe { frame(bar_base, 1) }, DmaDirection::ToDevice).unwrap();
        assert_eq!(d.dma_addr().as_u64(), bar_base);
    }
}
