//! 批次 Z ④: NetOps 安全桥契约测试
//!
//! 验证 framework `net_device_ops` 桥 (src/kernel/framework/net/net_device_ops.rs):
//! 1. `register_net_device::<T>` 产出可用 `NetOps` 指针表 (Box::leak + 泛型桥)
//! 2. extern "C" 回调正确转发到 `T: NetDeviceOps` 方法 (send/try_receive/get_mac/handle_irq)
//! 3. send null/len 守卫 (审核 P2-3): 空指针/零长 → -1
//! 4. `net_register_services_driver` set-once 语义 (DECISION-K 注册契约槽)
//!
//! 直接引用内核真实源码 (host-test feature), mock 设备仅为本测试构造。

use edgine::kernel::framework::net::{
    NetDeviceOps, NetDeviceRegistration, net_register_services_driver, register_net_device,
};

/// 桥契约测试用 mock 设备 (记录调用与数据往返)。
struct MockNetDevice {
    mac: [u8; 6],
    sent: Vec<Vec<u8>>,
    rx_queue: Vec<Vec<u8>>,
}

impl NetDeviceOps for MockNetDevice {
    fn send(&mut self, data: &[u8]) -> i32 {
        if data.is_empty() {
            return -1;
        }
        self.sent.push(data.to_vec());
        0
    }

    fn try_receive(&mut self, buf: &mut [u8]) -> i32 {
        match self.rx_queue.pop() {
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

#[test]
fn test_bridge_roundtrip_send_recv_mac() {
    let dev = MockNetDevice {
        mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        sent: Vec::new(),
        rx_queue: vec![vec![0xDE, 0xAD, 0xBE, 0xEF]],
    };
    let reg: NetDeviceRegistration = register_net_device(Box::new(dev));
    assert_eq!(reg.mac, [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let driver_data = reg.driver_data.cast::<u8>();

    // send: 经 NetOps 指针表往返到设备
    let frame = [0xFFu8; 6];
    assert_eq!(reg.ops.send(driver_data, &frame), 0);

    // try_receive: 设备队列出包 → 调用方缓冲区
    let mut buf = [0u8; 64];
    let n = reg.ops.try_receive(driver_data, &mut buf);
    assert_eq!(n, 4);
    assert_eq!(&buf[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);

    // get_mac: 指针表读取与注册时一致 (P2-4 对齐保证路径)
    let mut mac = [0u8; 6];
    reg.ops.get_mac(driver_data, &mut mac);
    assert_eq!(mac, reg.mac);

    // handle_irq: 默认空实现恒可调用 (不 panic)
    reg.ops.handle_irq(driver_data);
}

#[test]
fn test_bridge_send_guards() {
    let dev = MockNetDevice {
        mac: [0; 6],
        sent: Vec::new(),
        rx_queue: Vec::new(),
    };
    let reg = register_net_device(Box::new(dev));
    let driver_data = reg.driver_data.cast::<u8>();

    // null driver_data → -1 (net_send_impl / net_recv_impl 守卫, 审核 P2-3)
    let frame = [0u8; 4];
    assert_eq!(reg.ops.send(core::ptr::null_mut(), &frame), -1);
    let mut buf = [0u8; 4];
    assert_eq!(reg.ops.try_receive(core::ptr::null_mut(), &mut buf), -1);

    // len == 0 → -1 (send 零长守卫, 防御 from_raw_parts 边界)
    assert_eq!(reg.ops.send(driver_data, &[]), -1);
}

#[test]
fn test_services_driver_slot_set_once() {
    // DECISION-K 注册契约槽: 首次注册 Ok, 重复注册 Err (OnceLock set-once)。
    // 本测试是本二进制内唯一注册方, 避免并行测试竞争。
    fn probe_no_device() -> Option<NetDeviceRegistration> {
        None
    }
    assert!(net_register_services_driver(probe_no_device).is_ok());
    assert!(net_register_services_driver(probe_no_device).is_err());
}
