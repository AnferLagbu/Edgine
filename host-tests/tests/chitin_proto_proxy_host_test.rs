//! Chitin 协议族与用户态驱动安全代理 host 专项测试 (MIG-004)
//!
//! ## 背景
//!
//! MIG-004 在 `services/chitin` 下补齐 `proto.rs` (block/net/input 协议族) 与
//! `user_driver.rs` (用户态驱动) 的安全代理。此前生产代码直接经
//! `framework::chitin::register_block_device` 等深路径调用, 绕开 services 出口。
//! 本文件对该层代理补运行验证, 补齐 MIG-007 后仍缺的 proto_*/user_driver 缺口。
//!
//! ## 覆盖范围
//!
//! - proto_net: `find_net_device` 返回的 `NetDevice` 句柄 (mac/send/try_receive/
//!   handle_irq), 经 framework `NetOps` 桥往返到设备
//! - proto_input: `input_read` / `input_has_data` 统一入口
//! - proto_block: `unregister_block` 墓碑语义 (首次成功 / 重复 / 越界)
//! - user_driver: `UserDriverError::{from_code,to_errno}` framework 私有码 →
//!   强类型 → POSIX errno 全分支映射
//! - 退化路径: 空注册表时 net/input 入口的安全返回
//!
//! ## 隔离说明
//!
//! `CHITIN_DEVICES` 是进程内全局单例, 同一测试二进制内的 `#[test]` 默认并行执行。
//! 故本文件所有用例统一持 `REGISTRY_LOCK` 串行执行, 并以 `clear_registry()` 开场
//! (口径同 framework 侧 `CHITIN_TEST_LOCK` 与 [chitin_registry_io_host_test.rs])。

use std::sync::Mutex;

use queenx::kernel::framework::chitin::user_driver as fw;
use queenx::kernel::framework::chitin::{
    CHITIN_DEVICES, ChitinOps, ChitinProto, InputOps, chitin_register, chitin_register_with_ops,
};
use queenx::kernel::framework::net::{NetDeviceOps, register_net_device};
use queenx::kernel::framework::syscall::Errno;
use queenx::kernel::services::chitin::{
    UserDriverError, find_net_device, input_has_data, input_read, unregister_block,
};

/// 注册表类用例的进程内串行锁 (见文件头「隔离说明」)。
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// 清空全局注册表, 保证每个用例从空表出发、下标可预期。
fn clear_registry() {
    CHITIN_DEVICES.lock().clear();
}

// ============================================================================
// 网络设备桩 (经 framework NetOps 安全桥接入)
// ============================================================================

/// 网络设备桩: 经 [`register_net_device`] 桥产出 `NetOps` 指针表。
struct MockNet {
    mac: [u8; 6],
    rx: Vec<Vec<u8>>,
}

impl NetDeviceOps for MockNet {
    fn send(&mut self, data: &[u8]) -> i32 {
        // 空帧由 framework 桥守卫拦截 (-1); 非空帧报告成功。
        if data.is_empty() { -1 } else { 0 }
    }

    fn try_receive(&mut self, buf: &mut [u8]) -> i32 {
        match self.rx.pop() {
            Some(pkt) if pkt.len() <= buf.len() => {
                buf[..pkt.len()].copy_from_slice(&pkt);
                pkt.len() as i32
            }
            _ => 0,
        }
    }

    fn get_mac(&self) -> [u8; 6] {
        self.mac
    }
}

// ============================================================================
// 输入设备桩 (InputOps 无泛型桥, 手工 extern "C" 指针表)
// ============================================================================

/// 输入桩读回的固定字符。
static INPUT_CHAR: u8 = b'Q';

extern "C" fn mock_input_read(_driver_data: *mut u8) -> *const u8 {
    &INPUT_CHAR as *const u8
}

extern "C" fn mock_input_has_data(_driver_data: *mut u8) -> bool {
    true
}

extern "C" fn mock_input_irq(_driver_data: *mut u8) {}

static INPUT_OPS: InputOps = InputOps {
    read_char: mock_input_read,
    has_char: mock_input_has_data,
    handle_irq: mock_input_irq,
};

// ============================================================================
// proto_net
// ============================================================================

/// `find_net_device` 句柄往返: MAC 读回 + send / try_receive / handle_irq。
#[test]
fn proto_net_proxy_roundtrip() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    let mac = [0x52, 0x54, 0x00, 0x11, 0x22, 0x33];
    let reg = register_net_device(Box::new(MockNet {
        mac,
        rx: vec![vec![0xDE, 0xAD, 0xBE, 0xEF]],
    }));
    let driver_data = reg.driver_data.cast::<u8>();
    let _id = chitin_register_with_ops(
        "mock_net0",
        ChitinProto::Net,
        None,
        None,
        driver_data,
        ChitinOps::Net(reg.ops),
    );

    let dev = find_net_device().expect("应找到就绪网络设备");
    assert_eq!(dev.mac(), mac, "MAC 应经 NetOps 桥读回");

    // send: 非空帧成功, 空帧被 framework 桥守卫拒绝。
    assert_eq!(dev.send(&[0xFF; 6]), 0, "send 非空帧应成功");
    assert_eq!(dev.send(&[]), -1, "send 空帧应失败 (桥守卫)");

    // try_receive: 预置队列出包 → 调用方切片; 队列空后返回 0。
    let mut buf = [0u8; 64];
    assert_eq!(dev.try_receive(&mut buf), 4, "应接收 4 字节");
    assert_eq!(&buf[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(dev.try_receive(&mut buf), 0, "队列空后应返回 0");

    // handle_irq: 轮询模式默认空实现, 恒可调用 (不 panic)。
    dev.handle_irq();

    clear_registry();
}

// ============================================================================
// proto_input
// ============================================================================

/// `input_read` / `input_has_data` 统一入口往返。
#[test]
fn proto_input_proxy_roundtrip() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    let _id = chitin_register_with_ops(
        "mock_input0",
        ChitinProto::Input,
        None,
        None,
        core::ptr::null_mut(),
        ChitinOps::Input(&INPUT_OPS),
    );

    assert!(input_has_data(), "就绪输入设备应报告有数据");
    assert_eq!(input_read(), Some(b'Q'), "应读回 mock 字符");

    clear_registry();
}

// ============================================================================
// proto_block
// ============================================================================

/// `unregister_block` 墓碑语义: 首次成功 / 重复 false / 越界 false。
#[test]
fn proto_block_unregister_tombstone() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    // 空表注册 → 条目下标 0; 墓碑化只看 proto + state, 无需 block_dev。
    let _id = chitin_register(
        "mock_blk0",
        ChitinProto::Block,
        None,
        None,
        core::ptr::null_mut(),
    );

    assert!(unregister_block(0), "首次墓碑化应成功");
    assert!(!unregister_block(0), "重复墓碑化应返回 false");
    assert!(!unregister_block(9), "越界下标应返回 false");

    clear_registry();
}

// ============================================================================
// 退化路径
// ============================================================================

/// 空注册表时各入口的安全返回 (无设备 / 无数据 / 注销失败)。
#[test]
fn proxy_degraded_empty_registry() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();

    assert!(find_net_device().is_none(), "空表应无可查找网络设备");
    assert_eq!(input_read(), None, "空表应无输入字符");
    assert!(!input_has_data(), "空表应报告无输入数据");
    assert!(!unregister_block(0), "空表注销块设备应返回 false");

    clear_registry();
}

// ============================================================================
// user_driver 错误映射
// ============================================================================

/// framework 私有码 → `UserDriverError` → POSIX errno 全分支映射。
#[test]
fn user_driver_error_mapping() {
    // 私有码 → 强类型 (6 项已知 + 2 项未知)。
    assert_eq!(
        UserDriverError::from_code(fw::ERR_NOT_FOUND),
        UserDriverError::NotFound
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_NOT_AUTHORIZED),
        UserDriverError::NotAuthorized
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_INVALID_STATE),
        UserDriverError::InvalidState
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_NO_MMIO),
        UserDriverError::NoMmio
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_PID_MISMATCH),
        UserDriverError::PidMismatch
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_OOM),
        UserDriverError::Oom
    );
    assert_eq!(
        UserDriverError::from_code(-99),
        UserDriverError::Unknown(-99),
        "未知负码应落入 Unknown"
    );
    assert_eq!(
        UserDriverError::from_code(fw::ERR_OK),
        UserDriverError::Unknown(0),
        "ERR_OK (成功码) 非错误, 应落入 Unknown"
    );

    // 强类型 → POSIX errno (全分支)。
    assert_eq!(UserDriverError::NotFound.to_errno(), Errno::ENOENT);
    assert_eq!(UserDriverError::NotAuthorized.to_errno(), Errno::EPERM);
    assert_eq!(UserDriverError::PidMismatch.to_errno(), Errno::EPERM);
    assert_eq!(UserDriverError::InvalidState.to_errno(), Errno::EBUSY);
    assert_eq!(UserDriverError::NoMmio.to_errno(), Errno::EINVAL);
    assert_eq!(UserDriverError::Oom.to_errno(), Errno::ENOMEM);
    assert_eq!(
        UserDriverError::Unknown(-99).to_errno(),
        Errno::EINVAL,
        "未知错误应回退 EINVAL (POSIX 约定)"
    );
}
