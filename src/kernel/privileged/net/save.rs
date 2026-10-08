// ============================================================================
// P2-I-44: 网络快照 (net_save / net_restore 完整实现)
// ============================================================================
//
// ## 背景
//
// `recovery_domain_register("net", 5, ...)` 在 init 末尾注册网络恢复域,
// 但原 `net_save` 函数体为空. 恢复后所有配置 (IP, gateway) 与 FD 表
// (用户 socket 句柄) 都丢失, 即便 `net_restore` 调用 `eg_net_init` 重新
// 初始化, 也只是把 NIC 重启, 不会保留任何业务状态.
//
// ## 修复方案
//
// 引入 `NetSnapshot` 结构体, 序列化 **可恢复** 的网络状态:
//   - 链路层: MAC 地址
//   - 网络层: IPv4 地址 + 前缀长度 + 默认网关 + DNS (8 字节 4 槽)
//   - FD 表:   MAX_SM_FD 个 (type, handle) 元组 (0=free)
//   - 状态:    net_ready / net_configured / init_state / sockets_initialized
//   - 校验:    magic + version
//
// `net_save` 在 NET_LOCK 持有时填充快照 (O(1) 内存拷贝).
// `net_restore` 从快照恢复: 跳过 DHCP 重配 (避免租约漂移), 重新绑定 IP/GW,
// 把 FD 表恢复到 save 时刻. **smoltcp 内部 socket 状态 (TCP 缓冲, UDP
// metadata) 因 smoltcp 不暴露 serialize API 而无法恢复, 已知限制** —
// 这部分在文档中标注.
//
// ## 线程安全
//
// 快照本身是 `static mut NET_SNAPSHOT`, 写入路径 (`net_save`) 与读取
// 路径 (`net_restore`) 通过 NET_LOCK 串行化, 互斥访问安全.
//
// ## 与 Framekernel 安全契约
//
// 快照不含敏感凭证 (无密码无 key), 仅网络配置 + FD 索引.
use crate::privileged::sync::IrqSpinLock as Mutex;

/// 快照魔数 (`"ANXS"` 0x584E414C  小端)
pub const NET_SNAPSHOT_MAGIC: u32 = 0x584E_4153;
/// 快照版本
pub const NET_SNAPSHOT_VERSION: u32 = 1;
/// 快照 FD 表长度: 与 Smoltcp 段 FD 上限 (`FdPlan::SMOLTCP.capacity`) **单一来源**
/// 同源, 与 `net/init.rs` 的 `MAX_SM_FD` 恒等 (二者皆从 `FdRange::capacity` 派生).
/// `net_save` / `net_restore` 按 `0..MAX_SM_FD` 读写本数组, 若本长度短于
/// `MAX_SM_FD` 会越界 panic (TD-02 曾将 `MAX_SM_FD` 重基为 256 而此处滞留 16 即
/// 为潜伏崩溃); 故此处不得硬编码, 必须复用 `FdPlan` 单一来源.
/// 不引用 `net::init::MAX_SM_FD` 别名, 避免 `init ↔ save` 循环依赖 (与 wait_queue 同法).
pub const SNAPSHOT_FD_COUNT: usize = crate::privileged::proc::FdPlan::SMOLTCP.capacity as usize;
/// DNS 槽数
pub const SNAPSHOT_DNS_COUNT: usize = 4;

/// 网络快照 (固定大小, POD)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetSnapshot {
    pub magic: u32,
    pub version: u32,
    /// MAC (6 字节)
    pub mac: [u8; 6],
    /// IPv4 地址
    pub ip: [u8; 4],
    /// 前缀长度 (1-32)
    pub prefix_len: u8,
    /// 默认网关 IPv4
    pub gateway: [u8; 4],
    /// DNS 服务器 (4 槽)
    pub dns: [[u8; 4]; SNAPSHOT_DNS_COUNT],
    /// FD 表 (type, handle u32)
    pub fd_types: [u8; SNAPSHOT_FD_COUNT],
    pub fd_handles: [u32; SNAPSHOT_FD_COUNT],
    /// 状态
    pub net_ready: bool,
    pub net_configured: bool,
    pub sockets_initialized: bool,
    pub init_state: u8,
    /// 校验: 所有字节 XOR (排除 magic 自身)
    pub checksum: u32,
}

impl NetSnapshot {
    pub const fn empty() -> Self {
        Self {
            magic: 0,
            version: 0,
            mac: [0; 6],
            ip: [0; 4],
            prefix_len: 0,
            gateway: [0; 4],
            dns: [[0; 4]; SNAPSHOT_DNS_COUNT],
            fd_types: [0; SNAPSHOT_FD_COUNT],
            fd_handles: [0; SNAPSHOT_FD_COUNT],
            net_ready: false,
            net_configured: false,
            sockets_initialized: false,
            init_state: 0,
            checksum: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == NET_SNAPSHOT_MAGIC
            && self.version == NET_SNAPSHOT_VERSION
            && self.checksum == self.compute_checksum()
    }

    pub fn compute_checksum(&self) -> u32 {
        let mut h: u32 = 0xA5A5_5A5A;
        h ^= self.version;
        for b in &self.mac {
            h ^= u32::from(*b);
        }
        for b in &self.ip {
            h = h.rotate_left(3) ^ u32::from(*b);
        }
        h ^= u32::from(self.prefix_len);
        for b in &self.gateway {
            h = h.rotate_left(5) ^ u32::from(*b);
        }
        for slot in &self.dns {
            for b in slot {
                h = h.rotate_left(7) ^ u32::from(*b);
            }
        }
        for t in &self.fd_types {
            h = h.rotate_left(2) ^ u32::from(*t);
        }
        for v in &self.fd_handles {
            h = h.rotate_left(11) ^ *v;
        }
        h ^= u32::from(self.net_ready);
        h ^= u32::from(self.net_configured);
        h ^= u32::from(self.sockets_initialized);
        h ^= u32::from(self.init_state);
        h
    }

    pub fn seal(&mut self) {
        self.magic = NET_SNAPSHOT_MAGIC;
        self.version = NET_SNAPSHOT_VERSION;
        self.checksum = self.compute_checksum();
    }
}

// ============================================================================
// 全局快照 (NET_SNAPSHOT_LOCK 保护)
// ============================================================================

static NET_SNAPSHOT_LOCK: Mutex<NetSnapshot> = Mutex::new(NetSnapshot::empty());

/// 写快照 (`NET_SNAPSHOT_LOCK` 已持有, 不要二次加锁)
///
/// # SAFETY
///
/// 调用方必须持有 `NET_SNAPSHOT_LOCK` (这是本文件私有约定, 与外部 `NET_STATE`
/// 互不干涉). 通过 `data_ptr` 直接写入快照数据, 必须串行调用.
pub unsafe fn save_unchecked<F>(filler: F)
where
    F: FnOnce(&mut NetSnapshot),
{
    unsafe {
        let mut snap = NetSnapshot::empty();
        filler(&mut snap);
        snap.seal();
        // SAFETY: NET_SNAPSHOT_LOCK 由调用方独占持有, 写路径串行, 与 `save()` 互斥.
        let ptr = NET_SNAPSHOT_LOCK.data_ptr();
        *ptr = snap;
    }
}

/// 读快照副本 (`NET_SNAPSHOT_LOCK` 持有)
///
/// # SAFETY
///
/// 调用方必须持有 `NET_SNAPSHOT_LOCK`. 返回值是 `NetSnapshot` 的副本, 不
/// 持有对全局静态的可变引用, 调用方后续修改不会影响全局.
pub unsafe fn load_unchecked() -> NetSnapshot {
    unsafe { *NET_SNAPSHOT_LOCK.data_ptr() }
}

/// 通过 `NET_SNAPSHOT_LOCK` 串行化的保存入口
pub fn save<F>(filler: F)
where
    F: FnOnce(&mut NetSnapshot),
{
    let _guard = NET_SNAPSHOT_LOCK.lock();
    // SAFETY: NET_SNAPSHOT_LOCK 由本调用方独占持有, 写路径串行.
    unsafe { save_unchecked(filler) };
}

/// 通过 `NET_SNAPSHOT_LOCK` 串行化的读取入口
pub fn load() -> NetSnapshot {
    let _guard = NET_SNAPSHOT_LOCK.lock();
    // SAFETY: NET_SNAPSHOT_LOCK 由本调用方独占持有, 读路径串行, 内存
    // 拷贝避免了 &mut 重入风险.
    unsafe { load_unchecked() }
}

/// 复位快照 (例如 restore 后清空避免脏读)
pub fn clear() {
    let _guard = NET_SNAPSHOT_LOCK.lock();
    // SAFETY: NET_SNAPSHOT_LOCK 由本调用方独占持有, 写路径串行.
    unsafe {
        *NET_SNAPSHOT_LOCK.data_ptr() = NetSnapshot::empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局快照串行锁: `save` / `load` / `clear` 作用于同一 `NET_SNAPSHOT_LOCK`
    /// 静态快照, 而 `cargo test` 默认多线程并行, 用例间会互相覆盖快照导致偶发
    /// 断言失败. 触及全局状态的用例须先获取本锁串行执行 (仅测试期存在).
    static SNAPSHOT_TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn empty_snapshot_is_invalid() {
        let s = NetSnapshot::empty();
        assert!(!s.is_valid(), "empty snapshot must not validate");
    }

    #[test]
    fn seal_makes_snapshot_valid() {
        let mut s = NetSnapshot::empty();
        s.mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        s.ip = [10, 0, 2, 15];
        s.prefix_len = 24;
        s.gateway = [10, 0, 2, 2];
        s.dns = [[8, 8, 8, 8], [1, 1, 1, 1], [0; 4], [0; 4]];
        s.fd_types = core::array::from_fn(|i| match i {
            1 => 1,
            2 => 2,
            _ => 0,
        });
        s.fd_handles = core::array::from_fn(|i| if i == 1 || i == 2 { i as u32 } else { 0 });
        s.net_ready = true;
        s.net_configured = true;
        s.sockets_initialized = true;
        s.init_state = 3;
        s.seal();
        assert!(s.is_valid(), "sealed snapshot must validate");
    }

    #[test]
    fn tampering_breaks_checksum() {
        let mut s = NetSnapshot::empty();
        s.mac = [1, 2, 3, 4, 5, 6];
        s.ip = [192, 168, 0, 1];
        s.prefix_len = 24;
        s.gateway = [192, 168, 0, 254];
        s.seal();
        assert!(s.is_valid());
        // 篡改 IP 后必须校验失败
        s.ip[3] = 2;
        assert!(!s.is_valid(), "checksum must detect tampering");
    }

    #[test]
    fn save_load_roundtrip() {
        // 触及全局快照, 与同模块用例串行
        let _serial = SNAPSHOT_TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // 清空初始
        clear();
        save(|s| {
            s.mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
            s.ip = [172, 16, 0, 1];
            s.prefix_len = 12;
            s.gateway = [172, 16, 0, 254];
        });
        let got = load();
        assert!(got.is_valid());
        assert_eq!(got.mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(got.ip, [172, 16, 0, 1]);
        assert_eq!(got.prefix_len, 12);
        assert_eq!(got.gateway, [172, 16, 0, 254]);
        // 复位
        clear();
        let cleared = load();
        assert!(!cleared.is_valid(), "cleared snapshot must be invalid");
    }

    #[test]
    fn fd_table_persists_through_save() {
        // 触及全局快照, 与同模块用例串行
        let _serial = SNAPSHOT_TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();
        save(|s| {
            s.fd_types = core::array::from_fn(|i| match i {
                1 | 2 => 1,
                3 | 4 => 2,
                _ => 0,
            });
            s.fd_handles = core::array::from_fn(|i| i as u32);
        });
        let got = load();
        assert!(got.is_valid());
        assert_eq!(got.fd_types[3], 2);
        assert_eq!(got.fd_handles[5], 5);
    }
}
