//! 热插拔重枚举契约 host 专项测试
//!
//! ## 背景
//!
//! 本轮接通了运行时热插拔链路: framework 侧 `HotplugManager` 新增公开
//! `dispatch` (供自行探测事件的总线驱动复用统一分发语义), 并新增
//! "重枚举先行、监听器后处理" 的时序契约; EGDF 侧新增块设备墓碑注销协议
//! (`egdf_unregister_block`), 保证移除后索引稳定 (从中间物理删除会使后续
//! `drive` 句柄错位)。
//!
//! ## 覆盖范围
//!
//! - EGDF 块设备墓碑语义: 索引稳定 / I/O 安全失败 / 幂等与边界
//! - `HotplugManager::dispatch` 分发契约: 重枚举回调先于监听器, 且
//!   `DeviceAdded` → `on_device_added`, `DeviceRemoved`/`SurpriseRemoval`
//!   → `on_device_removed`
//!
//! ## 隔离说明
//!
//! `EGDF_DEVICES` 与 `HOTPLUG_MANAGER` 均为进程内全局单例。本文件两个用例
//! 分别只触碰其中一个全局 (块设备表 / 热插拔管理器), 互不干扰, 故无需串行锁。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use edgine::kernel::framework::driver::block_device_state;
use edgine::kernel::framework::driver::hotplug::{
    BusType, DeviceLocation, HOTPLUG_MANAGER, HotplugEvent, HotplugListener, register_reenum_hook,
};
use edgine::kernel::framework::egdf::{
    BlockDevice, EGDF_DEVICES, egdf_blk_drives, egdf_blk_is_present, egdf_blk_is_removed,
    egdf_blk_read, egdf_blk_write, egdf_register_block_dev, egdf_unregister_block,
};
use edgine::kernel::framework::error::KernelError as FwError;
use edgine::kernel::services::egdf::{Proto, register};

/// 宿主块设备载体: 实现内核 `BlockDevice` 契约, 供注册表 dispatch。
struct MockBlk {
    storage: Vec<[u8; 512]>,
}

impl MockBlk {
    fn new(sectors: usize) -> Self {
        Self {
            storage: vec![[0u8; 512]; sectors],
        }
    }
}

impl BlockDevice for MockBlk {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        let idx = sector as usize;
        if idx >= self.storage.len() {
            return FwError::Io.as_i32();
        }
        buf[..512].copy_from_slice(&self.storage[idx]);
        0
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        let idx = sector as usize;
        if idx >= self.storage.len() {
            return FwError::Io.as_i32();
        }
        self.storage[idx].copy_from_slice(&buf[..512]);
        0
    }

    fn blk_is_present(&self) -> bool {
        true
    }

    fn blk_total_sectors(&self) -> u64 {
        self.storage.len() as u64
    }
}

/// EGDF 块设备墓碑注销: 索引稳定 + I/O 安全失败 + 边界。
#[test]
fn egdf_block_tombstone_keeps_index_stable() {
    EGDF_DEVICES.lock().clear();

    // 交错注册: blk0(0), char(1), blk1(2) — 中间夹一个非块设备
    let dev0: &'static mut MockBlk = Box::leak(Box::new(MockBlk::new(4)));
    let d0 = egdf_register_block_dev("hp_blk0", None, None, dev0) as u8;
    register("hp_char", Proto::Char, core::ptr::null_mut()).expect("注册字符设备失败");
    let dev1: &'static mut MockBlk = Box::leak(Box::new(MockBlk::new(4)));
    let d1 = egdf_register_block_dev("hp_blk1", None, None, dev1) as u8;
    assert_eq!((d0, d1), (0, 2), "块设备索引应与交错注册顺序一致");
    assert_eq!(
        egdf_blk_drives(),
        vec![0, 2],
        "应枚举出含中间字符设备空洞的块设备索引"
    );

    // 向 blk1 写入特征, 用于墓碑化 blk0 后验证索引未错位
    let mut payload = [0u8; 512];
    payload[0] = 0x5A;
    assert_eq!(egdf_blk_write(d1, 0, &payload), 0, "写 blk1 应成功");

    // 墓碑化 blk0: 标记移除 + 释放块设备引用 (不物理删除)
    assert!(egdf_unregister_block(d0), "首次墓碑化应成功");
    assert!(egdf_blk_is_removed(d0), "墓碑化后应报告 removed");
    assert!(!egdf_blk_is_present(d0), "墓碑化后应报告不存在");
    assert_eq!(
        block_device_state(d0),
        (false, true, 0),
        "墓碑化设备状态应为 (不存在, 移除中, 0)"
    );

    // 索引稳定性: blk1 仍是原设备, 数据可读回
    let mut rb = [0u8; 512];
    assert_eq!(egdf_blk_read(d1, 0, &mut rb), 0, "blk1 索引应保持稳定");
    assert_eq!(rb[0], 0x5A, "墓碑化 blk0 不应影响 blk1 的数据");
    assert_eq!(
        block_device_state(d1),
        (true, false, 0),
        "存活设备状态应为 (存在, 未移除, 0)"
    );

    // 墓碑化后 I/O 安全失败 (状态非 Ready → Busy), 不静默走错设备
    assert_eq!(
        egdf_blk_read(d0, 0, &mut rb),
        FwError::Busy.as_i32(),
        "墓碑化设备 I/O 应安全失败"
    );

    // 幂等与边界: 重复墓碑化 / 非块设备索引 / 越界均返回 false
    assert!(!egdf_unregister_block(d0), "重复墓碑化应返回 false");
    assert!(!egdf_unregister_block(1), "对非块设备墓碑化应返回 false");
    assert!(!egdf_unregister_block(99), "越界索引应返回 false");
    assert!(!egdf_blk_is_removed(99), "越界索引不应报告 removed");

    // 新设备追加到表尾, 不回收墓碑槽位 (索引单调递增, 保证旧句柄不指向新设备)
    let dev2: &'static mut MockBlk = Box::leak(Box::new(MockBlk::new(4)));
    let d2 = egdf_register_block_dev("hp_blk2", None, None, dev2) as u8;
    assert_eq!(d2, 3, "新设备应追加到表尾, 不覆盖墓碑槽位");

    EGDF_DEVICES.lock().clear();
}

// ── 分发契约: 重枚举先行 + 事件变体路由 ──

/// 全局分发序号 (重枚举回调与监听器共用, 用于判定先后)
static SEQ: AtomicU32 = AtomicU32::new(0);
/// 重枚举回调被调用时记录的序号
static REENUM_SEQ: AtomicU32 = AtomicU32::new(0);
/// 监听器被调用时记录的序号
static LISTENER_SEQ: AtomicU32 = AtomicU32::new(0);
/// `on_device_added` 调用次数
static ADDED_CALLS: AtomicU32 = AtomicU32::new(0);
/// `on_device_removed` 调用次数
static REMOVED_CALLS: AtomicU32 = AtomicU32::new(0);
/// 监听器"认领"返回值
static CLAIM: AtomicBool = AtomicBool::new(false);

/// 记录调用序号与次数的探针监听器。
struct ProbeListener;

impl HotplugListener for ProbeListener {
    fn on_device_added(&self, _event: &HotplugEvent) -> bool {
        ADDED_CALLS.fetch_add(1, Ordering::SeqCst);
        LISTENER_SEQ.store(SEQ.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        CLAIM.load(Ordering::SeqCst)
    }

    fn on_device_removed(&self, _event: &HotplugEvent) {
        REMOVED_CALLS.fetch_add(1, Ordering::SeqCst);
        LISTENER_SEQ.store(SEQ.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
    }
}

/// 记录调用序号的重枚举探针回调。
fn reenum_probe(_event: &HotplugEvent) {
    REENUM_SEQ.store(SEQ.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
}

/// `dispatch` 契约: 重枚举回调先于监听器, 且事件变体正确路由。
#[test]
fn hotplug_dispatch_reenum_before_listener() {
    // 重枚举回调为进程内 OnceLock, 仅本测试二进制注册一次
    assert!(
        register_reenum_hook(reenum_probe).is_ok(),
        "reenum hook 应能注册 (本测试二进制专用)"
    );
    HOTPLUG_MANAGER.register_listener(Box::new(ProbeListener));

    let location = DeviceLocation {
        bus_type: BusType::Pcie,
        bus: 1,
        device: 0,
        function: 0,
        slot: 3,
    };

    // DeviceAdded → 重枚举先行, 再走 on_device_added
    HOTPLUG_MANAGER.dispatch(&HotplugEvent::DeviceAdded { location });
    assert_eq!(
        ADDED_CALLS.load(Ordering::SeqCst),
        1,
        "应触发一次 on_device_added"
    );
    assert_eq!(REMOVED_CALLS.load(Ordering::SeqCst), 0);
    assert!(
        REENUM_SEQ.load(Ordering::SeqCst) < LISTENER_SEQ.load(Ordering::SeqCst),
        "重枚举回调应先于监听器被调用"
    );

    // DeviceRemoved 与 SurpriseRemoval 均路由到 on_device_removed
    HOTPLUG_MANAGER.dispatch(&HotplugEvent::DeviceRemoved { location });
    HOTPLUG_MANAGER.dispatch(&HotplugEvent::SurpriseRemoval { location });
    assert_eq!(
        REMOVED_CALLS.load(Ordering::SeqCst),
        2,
        "DeviceRemoved 与 SurpriseRemoval 均应路由到 on_device_removed"
    );
    assert_eq!(
        ADDED_CALLS.load(Ordering::SeqCst),
        1,
        "不应再触发 on_device_added"
    );
}
