//! mm: IoMem 别名检测集成测试
//!
//! 追踪: I-25
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `Entry` / `AliasRegistry` (Vec 版) / `MAX_MMIO_MAPPINGS` 平行实现,
//! 以及 `#![allow(dead_code)]` (F9 违规), 改引内核真实源码:
//! - `edgine::kernel::framework::iomem::IoMem` (pub 安全代理, 内部驱动全局
//!   `ALIAS_REGISTRY` — 内核 `AliasRegistry` 为私有结构, host 经 `IoMem` 公共
//!   API 间接验证其固定数组 `[(u64,usize,&'static str); MAX_MMIO_MAPPINGS]` 语义)
//! - `edgine::kernel::framework::constants::limits::MAX_MMIO_MAPPINGS` (64)
//! - `edgine::kernel::framework::mm::PhysAddr`
//!
//! 内核 `IoMem::new` 为 `unsafe fn`, SAFETY 契约要求 phys 指向有效 MMIO 区域;
//! 本测试仅验证别名注册表算法, **从不**对返回句柄做读/写, 因此伪造 phys 地址
//! 不会被解引用, 满足 SAFETY 前提 (仅构造 + Drop 释放注册项).
//!
//! ## 与镜像的差异 (以内核为权威)
//! - 内核容量/冲突判定在 `AliasRegistry` 内 (固定数组, 无 Vec 扩容);
//!   对齐/零长/溢出校验在 `IoMem::new`.
//! - **溢出语义漂移**: 镜像用 `saturating_add` (溢出钳到 u64::MAX 仍注册成功),
//!   内核用 `checked_add` **拒绝** `phys+len` 溢出 (B03-22 修复) → 断言已同步为 Err.
//! - 无 count/capacity 访问器: 容量行为经 "注册 64 项成功 + 第 65 项失败" 验证.
//!
//! ## 因内核 host 不可测已移除
//! - `test_unregister_not_found`: 内核 `unregister` 为私有方法且 Drop 路径静默
//!   忽略缺失项, host 无公共 API 可构造该场景, 移除.
//!
//! 注: 完整验证还需 QEMU 端运行, 见 `scripts/qemu_boot_test.sh`.

use edgine::kernel::framework::constants::limits::MAX_MMIO_MAPPINGS;
use edgine::kernel::framework::iomem::IoMem;
use edgine::kernel::framework::mm::PhysAddr;

/// 内核注册表为全局 `ALIAS_REGISTRY` (各测试并行共享), 本文件全部注册表用例
/// 合并为单个顺序测试函数, 且每个小节用独立作用域让 `IoMem` Drop 及时释放,
/// 保证容量用例 (需 64 槽全空) 不被其他小节占用干扰.
#[test]
fn alias_registry_semantics() {
    // ── 基本注册 ──
    {
        // SAFETY: 仅测试注册表算法, 返回句柄不做读写, 伪造 phys 不被解引用
        let m1 = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "dev1") };
        // SAFETY: 同上
        let m2 = unsafe { IoMem::new(PhysAddr(0x2000), 0x100, "dev2") };
        assert!(m1.is_ok());
        assert!(m2.is_ok());
    }

    // ── 4 字节对齐检查 (IoMem::new 层) ──
    {
        // SAFETY: 同前, 伪造 phys 不被解引用
        let r = unsafe { IoMem::new(PhysAddr(0x1001), 0x100, "dev") };
        assert!(r.is_err(), "phys 非 4 字节对齐必须拒绝");
    }

    // ── 零长度检查 (IoMem::new 层) ──
    {
        // SAFETY: 同前
        let r = unsafe { IoMem::new(PhysAddr(0x1000), 0, "dev") };
        assert!(r.is_err(), "零长度 MMIO 区域必须拒绝");
    }

    // ── 区间重叠: 完全相同 / 左重叠 / 右重叠 / 完全包含 ──
    {
        // SAFETY: 同前, 仅构造句柄不读写
        let m1 = unsafe { IoMem::new(PhysAddr(0x1000), 0x1000, "dev1") };
        assert!(m1.is_ok());
        // 完全相同
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0x1000), 0x1000, "dev2") }.is_err());
        // 完全包含 (内部区间)
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0x1100), 0x100, "dev3") }.is_err());
        // 到内部结尾
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0x1F00), 0x100, "dev4") }.is_err());
    }
    {
        // SAFETY: 同前
        let m1 = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "dev1") };
        assert!(m1.is_ok());
        // 左重叠 (新区间左边界在 dev1 内部)
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0x1080), 0x100, "dev2") }.is_err());
        // 右重叠 (新区间右边界覆盖 dev1 左边界)
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0x0F80), 0x100, "dev3") }.is_err());
    }

    // ── 紧邻 (touching) 不冲突 ──
    {
        // SAFETY: 同前
        let a = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "dev1") };
        // SAFETY: 同前
        let b = unsafe { IoMem::new(PhysAddr(0x1100), 0x100, "dev2") };
        // SAFETY: 同前
        let c = unsafe { IoMem::new(PhysAddr(0x1200), 0x100, "dev3") };
        assert!(
            a.is_ok() && b.is_ok() && c.is_ok(),
            "紧邻区间不重叠应可注册"
        );
    }

    // ── unregister 后重新注册 (Drop 释放) ──
    {
        let m = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "dev1") };
        assert!(m.is_ok());
        drop(m); // IoMem Drop → ALIAS_REGISTRY.unregister
        // 释放后重新注册同物理地址
        // SAFETY: 同前
        let revived = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "dev1-revived") };
        assert!(revived.is_ok(), "unregister 后应可重新注册");
        // 释放后再注册重叠区间 (模拟 test_unregister_then_register_overlap)
        drop(revived);
        // SAFETY: 同前
        let n1 = unsafe { IoMem::new(PhysAddr(0x1000), 0x200, "dev1") };
        assert!(n1.is_ok());
        // SAFETY: 同前
        let n2 = unsafe { IoMem::new(PhysAddr(0x1200), 0x200, "dev2") };
        assert!(n2.is_ok());
        drop(n1);
        // 0x1100-0x1200 与 dev2 (0x1200-0x1400) 紧邻不重叠
        // SAFETY: 同前
        let n3 = unsafe { IoMem::new(PhysAddr(0x1100), 0x100, "dev3") };
        assert!(n3.is_ok(), "释放后紧邻 dev2 的区间应可注册");
    }

    // ── 容量: 64 项满 + 第 65 项失败 ──
    {
        let mut held = Vec::new();
        for i in 0..MAX_MMIO_MAPPINGS {
            let phys = 0x100000u64 + (i as u64) * 0x1000;
            // SAFETY: 仅构造句柄不读写, 伪造 phys 不被解引用
            let m = unsafe { IoMem::new(PhysAddr(phys), 0x100, "dev") };
            assert!(m.is_ok(), "第 {} 项注册应成功", i);
            held.push(m.expect("刚确认 Ok"));
        }
        // 65 项 → 容量满
        let phys65 = 0x100000u64 + MAX_MMIO_MAPPINGS as u64 * 0x1000;
        // SAFETY: 同前
        let full = unsafe { IoMem::new(PhysAddr(phys65), 0x100, "dev") };
        assert_eq!(
            full.err(),
            Some("MMIO alias registry full"),
            "满容量后必须拒绝"
        );
        // held 出作用域 Drop → 全部 unregister
    }

    // ── 释放一半后再注册 (碎片化复用) ──
    {
        let mut held = Vec::new();
        for i in 0..MAX_MMIO_MAPPINGS {
            let phys = 0x1_0000_0000u64 + (i as u64) * 0x10_0000;
            // SAFETY: 同前
            let m = unsafe { IoMem::new(PhysAddr(phys), 0x1000, "dev") };
            assert!(m.is_ok(), "stress 注册 i={}", i);
            held.push(m.expect("刚确认 Ok"));
        }
        // 释放奇数项 (1,3,5,...,63): 逆序 swap_remove, 只移除原始奇数下标元素
        for i in (1..MAX_MMIO_MAPPINGS).step_by(2).rev() {
            drop(held.swap_remove(i));
        }
        assert_eq!(held.len(), MAX_MMIO_MAPPINGS / 2, "应保留 32 个偶数项");
        // 空出 32 槽, 重新注册 32 个新区间
        for i in 0..32 {
            let phys = 0x2_0000_0000u64 + (i as u64) * 0x10_0000;
            // SAFETY: 同前
            let m = unsafe { IoMem::new(PhysAddr(phys), 0x1000, "new-dev") };
            assert!(m.is_ok(), "复用槽位注册 i={}", i);
        }
    }

    // ── PCI BAR 模拟场景 ──
    {
        // SAFETY: 同前
        let e1000 = unsafe { IoMem::new(PhysAddr(0xFEB_C0000), 128 * 1024, "e1000-bar0") };
        // SAFETY: 同前
        let ahci = unsafe { IoMem::new(PhysAddr(0xFEB_E0000), 8 * 1024, "ahci-bar5") };
        // SAFETY: 同前
        let xhci = unsafe { IoMem::new(PhysAddr(0xFEB_F0000), 1024 * 1024, "xhci-bar0") };
        assert!(e1000.is_ok() && ahci.is_ok() && xhci.is_ok());
        // 重叠尝试: e1000 BAR0 内部
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0xFEB_C1000), 0x100, "fake-e1000") }.is_err());
        // ahci BAR5 内部
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0xFEB_E0800), 0x100, "fake-ahci") }.is_err());
        // xhci BAR0 内部
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0xFEB_F1000), 0x100, "fake-xhci") }.is_err());
        // 与 ahci-bar5 (0xFEB_E0000..0xFEB_F0000) 重叠 → Err
        // SAFETY: 同前
        assert!(unsafe { IoMem::new(PhysAddr(0xFEB_E0000), 0x100, "ahci-revived") }.is_err());
    }

    // ── 溢出: 内核 checked_add 拒绝 phys+len 溢出 (B03-22, 与镜像 saturating 漂移) ──
    {
        // phys 接近 u64::MAX, len 导致 end 溢出 u64 → IoMem::new 拒绝
        // SAFETY: 同前, 该路径在注册前即返回 Err, 无句柄产生
        let r = unsafe { IoMem::new(PhysAddr(0xFFFF_FFFF_FFFE_0000), 0x20000, "dev") };
        assert!(
            r.is_err(),
            "phys+len 溢出 u64 必须拒绝 (内核 checked_add 语义)"
        );
        // 不溢出的小区间仍可注册 (注册表当前为空)
        // SAFETY: 同前
        let ok = unsafe { IoMem::new(PhysAddr(0x1000), 0x100, "before-overflow") };
        assert!(ok.is_ok());
    }

    // ── from_platform_device: 零基址拒绝 + 合法平台设备注册 (MIG-003) ──
    {
        // 零基址: 平台设备未配置 → Err (不注册)
        assert!(
            IoMem::from_platform_device(PhysAddr(0), 0x1000, "pl011").is_err(),
            "平台设备基址为 0 必须拒绝"
        );
        // 合法平台设备基址 (aarch64 PL011 0x0900_0000): 注册成功, 句柄字段正确
        let m = IoMem::from_platform_device(PhysAddr(0x0900_0000), 0x1000, "pl011")
            .expect("合法平台设备基址应注册成功");
        assert_eq!(m.phys().as_u64(), 0x0900_0000);
        assert_eq!(m.len(), 0x1000);
        assert_eq!(m.name(), "pl011");
        // m 出作用域 Drop → unregister, 不残留占用后续用例槽位
    }
}

/// 内核 MAX_MMIO_MAPPINGS 常量 = 64 (与 iomem.rs AliasRegistry 固定数组容量一致)
#[test]
fn max_mmio_mappings_is_64() {
    assert_eq!(
        MAX_MMIO_MAPPINGS, 64,
        "内核 constants::limits::MAX_MMIO_MAPPINGS = 64"
    );
}
