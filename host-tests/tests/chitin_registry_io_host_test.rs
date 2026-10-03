//! Chitin 设备注册表与块设备 IO host 专项测试 (MIG-007)
//!
//! ## 背景
//!
//! `services/chitin/mod.rs` 是对 `framework/chitin` 的安全代理 (强类型
//! `DeviceId`/`Proto`/`DeviceState` + `Result<_, ChitinError>`), 但 host-tests
//! 侧此前只有静态契约检查 ([i43_block_bridge_test.rs]), 缺少对其注册表语义与
//! 块设备 IO dispatch 的专项运行验证。本文件补齐该缺口。
//!
//! ## 覆盖范围
//!
//! - 注册表: `register` / `find_by_name` / `find_by_proto` / `list` / `count` /
//!   `set_state` / `unregister`
//! - 块设备 IO: `blk_read` / `blk_write` 成功 round trip 与错误路径 (缓冲区过小 /
//!   drive 越界), 及 `blk_is_present` / `blk_total_sectors` / `blk_count`
//!
//! ## 隔离说明
//!
//! `CHITIN_DEVICES` 是进程内全局单例, 同一测试二进制内的 `#[test]` 默认并行执行,
//! 若不加约束会互相清表并错位 drive 下标。故本文件所有用例统一持 `REGISTRY_LOCK`
//! 串行执行, 并以 `clear_registry()` 开场 (口径同 framework 侧 `CHITIN_TEST_LOCK`)。

use std::sync::Mutex;

use queenx::kernel::framework::chitin::{BlockDevice, CHITIN_DEVICES, chitin_register_block_dev};
use queenx::kernel::framework::error::KernelError as FwError;
use queenx::kernel::services::chitin::{
    ChitinError, DeviceState, Proto, blk_count, blk_is_present, blk_read, blk_total_sectors,
    blk_write, count, find_by_name, find_by_proto, list, register, set_state, unregister,
};

/// 注册表类用例的进程内串行锁 (见文件头「隔离说明」)。
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// 清空全局注册表, 保证每个用例从空表出发、drive 下标可预期。
fn clear_registry() {
    CHITIN_DEVICES.lock().clear();
}

/// 宿主块设备载体: 实现内核 `BlockDevice` 契约, 供注册表 dispatch。
struct MockBlk {
    storage: Vec<[u8; 512]>,
}

impl MockBlk {
    fn new(sectors: usize) -> Self {
        // 首 2 字节写入扇区号低/高位, 便于读回区分扇区。
        Self {
            storage: (0..sectors)
                .map(|i| {
                    let mut s = [0u8; 512];
                    s[0] = (i & 0xFF) as u8;
                    s[1] = ((i >> 8) & 0xFF) as u8;
                    s
                })
                .collect(),
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

/// 注册表生命周期: 注册 → 查找 → 列举 → 改状态 → 注销。
#[test]
fn chitin_registry_lifecycle() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    // 注册两个设备 (driver_data 传空指针: 仅验证注册表语义, 无堆内存需释放)。
    let id_blk =
        register("mock_blk0", Proto::Block, core::ptr::null_mut()).expect("注册块设备失败");
    let id_char =
        register("mock_char0", Proto::Char, core::ptr::null_mut()).expect("注册字符设备失败");
    assert_ne!(id_blk.raw(), id_char.raw(), "两次注册应分配不同 DeviceId");
    assert_eq!(count(), 2, "注册后设备总数应为 2");

    // 按名称查找 (返回注册表下标, 非 DeviceId)。
    assert!(
        find_by_name("mock_blk0").is_some(),
        "应能按名称找到已注册设备"
    );
    assert!(
        find_by_name("does_not_exist").is_none(),
        "未注册名称应返回 None"
    );

    // 按协议查找。
    assert!(
        find_by_proto(Proto::Block).is_some(),
        "应能按协议找到块设备"
    );
    assert!(
        find_by_proto(Proto::Net).is_none(),
        "无网络设备时应返回 None"
    );

    // 列举: 名称与协议应与注册一致。
    let infos = list();
    assert_eq!(infos.len(), 2);
    let blk_info = infos
        .iter()
        .find(|i| i.id == id_blk)
        .expect("list 应含块设备");
    assert_eq!(blk_info.name, "mock_blk0");
    assert_eq!(blk_info.proto, Proto::Block);
    assert_eq!(blk_info.state, DeviceState::Ready, "注册后状态应为 Ready");

    // 改状态: 经 list 回查确认。
    set_state(id_blk, DeviceState::Failed);
    let after = list();
    let blk_after = after
        .iter()
        .find(|i| i.id == id_blk)
        .expect("list 应含块设备");
    assert_eq!(blk_after.state, DeviceState::Failed, "set_state 应生效");

    // 注销: 首次成功, 再次返回 false。
    assert!(unregister(id_blk), "首次注销应成功");
    assert_eq!(count(), 1, "注销后应剩 1 个设备");
    assert!(!unregister(id_blk), "重复注销应返回 false");

    clear_registry();
}

/// 块设备 IO dispatch: 成功 round trip + 错误路径 + 元信息查询。
#[test]
fn chitin_block_io_dispatch() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    // 经 framework 注册带 trait 引用的块设备; 表为空, 故其 drive 下标为 0。
    // `Box::leak` 与 `chitin_register_block_dev` 的 `&'static mut` 契约一致, 测试进程内一次性泄漏。
    let dev: &'static mut MockBlk = Box::leak(Box::new(MockBlk::new(8)));
    let drive = chitin_register_block_dev("mock_blk_io", None, None, dev) as u8;
    assert_eq!(drive, 0, "空表注册后 drive 下标应为 0");

    // 元信息: 计数 / 存在性 / 总扇区数。
    assert_eq!(blk_count(), 1, "应统计到 1 个块设备");
    assert!(blk_is_present(drive), "build mock 应报告存在");
    assert_eq!(blk_total_sectors(drive), 8);

    // 成功读: 扇区 0 首 2 字节为扇区号 0。
    let mut buf = [0u8; 512];
    blk_read(drive, 0, &mut buf).expect("读扇区 0 应成功");
    assert_eq!(buf[0], 0, "扇区 0 低位标记");

    // 成功写 + 读回: 扇区 3 写入特征串, 再读回应一致。
    let mut payload = [0u8; 512];
    payload[0] = 0xAB;
    payload[1] = 0xCD;
    blk_write(drive, 3, &payload).expect("写扇区 3 应成功");
    let mut readback = [0u8; 512];
    blk_read(drive, 3, &mut readback).expect("读扇区 3 应成功");
    assert_eq!(&readback[..2], &[0xAB, 0xCD], "写入内容应可读回");

    // 错误路径 1: 缓冲区小于 512 字节 → framework 返回 InvalidArgument。
    let mut small = [0u8; 128];
    assert_eq!(
        blk_read(drive, 0, &mut small),
        Err(ChitinError::Kernel(FwError::InvalidArgument)),
        "缓冲区过小应返回 InvalidArgument"
    );

    // 错误路径 2: drive 下标越界 → framework 返回 Io (-5), services 映射为 Fault。
    assert_eq!(
        blk_read(99, 0, &mut buf),
        Err(ChitinError::Kernel(FwError::Fault)),
        "越界 drive 应返回错误"
    );
    assert!(!blk_is_present(99), "越界 drive 应报告不存在");
    assert_eq!(blk_total_sectors(99), 0, "越界 drive 总扇区数应为 0");

    // 错误路径 3: 非块设备的 drive 下标 (注册字符设备占据下标 1) → 同样报错。
    let _ = register("mock_char_io", Proto::Char, core::ptr::null_mut()).expect("注册字符设备失败");
    assert_eq!(
        blk_write(1, 0, &payload),
        Err(ChitinError::Kernel(FwError::Fault)),
        "对非块设备 drive 写入应返回错误"
    );

    clear_registry();
}
