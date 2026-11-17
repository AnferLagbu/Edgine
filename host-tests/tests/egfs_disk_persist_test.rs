//! EGFS 磁盘模式持久化专项测试 (D5-d / D5-g / D5-j)
//!
//! ## 背景
//!
//! aarch64 EGFS 磁盘挂载真跑腿暴露两处数据丢失级缺陷:
//!
//! 1. SPA 扇区 I/O 的 drive 来源不单一: 旧 `vdev_0_drive_id()` 以 `.map_or(0, …)`
//!    静默回落全局下标 0, 而 EGFS 的 `drive` 是 **EGDF 全局设备下标** (0 号未必是
//!    块设备); 且 `mount_drive` 在 `add_vdev` **之前**读 uberblock ⇒ 128 个槽位全读
//!    失败 ⇒ 每次启动误判空盘并重新格式化. 分区偏移另取独立计数器, 多盘循环会漂移.
//! 2. `write_uberblock_to_disk` 两处静默失败 (`!is_valid` 早退返 0 / `let _ =
//!    write_sector` 丢错), 以及 `write_bp` 把非 512 倍数载荷直接交给按整扇区校验的
//!    `write_sectors` ⇒ 拒写; `sync()` 恒返 0 掩盖元数据落盘失败.
//! 3. 元数据写侧丢目录项的对象号 (`write_dir_entry_record` 恒写 0, 而读侧
//!    `parse_dir_entry_record` 一直按对象号解析) ⇒ 冷恢复后名字全绑到对象 0,
//!    名字还在而文件全打不开.
//!
//! ## 覆盖范围
//!
//! - uberblock: 未注册 vdev / magic 无效 / 绑定失败盘 三条显式失败路径, 好盘真往返,
//!   物理 LBA = 分区起始 + 槽位 的落点断言 (错 drive 与漏分区偏移双向负控制)
//! - 数据块: 非扇区对齐载荷补零整扇区写 + 读回逐字节一致
//! - VFS: 磁盘模式 `fs_create` → `Inode::write` → `sync` → 冷挂载重建 → 按名打开读回
//! - 二启: `mount_drive` 成功后 `init()` 不得重做 setup (重入自锁死 + 双 root 数据集
//!   + txg 回退), 且盘上文件仍可按名打开读回
//! - 二启可写性: 冷恢复出来的磁盘根必须仍能建目录/建文件/写入读回 (`state` 停在
//!   `Creating` 会让恢复结果变成只读), 且恢复不得抹掉盘上既有名字
//!
//! ## 隔离说明
//!
//! `EGDF_DEVICES` 是进程内全局单例, 同一测试二进制内 `#[test]` 默认并行执行,
//! 若不加约束会互相清表并错位 drive 下标. 故本文件所有用例统一持 `REGISTRY_LOCK`
//! 串行执行, 并以 `clear_registry()` 开场 (口径同 `egdf_registry_io_host_test.rs`).
//! 内核侧 `EGFS_DATA` 为不可重置 `OnceCell`, 只有 VFS 用例触它, 且冷读断言另建
//! 独立 SPA, 不依赖用例间的先后次序.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use edgine::kernel::functions::fs::FileSystem;
use edgine::kernel::functions::fs::egfs::bp::{EgfsCksumType, EgfsCompType};
use edgine::kernel::functions::fs::egfs::egfs_data::{EgfsData, get_egfs};
use edgine::kernel::functions::fs::egfs::spa::{EgfsSpa, EgfsUberblock};
use edgine::kernel::functions::fs::egfs::vdev::EgfsVdevConfig;
use edgine::kernel::privileged::egdf::{BlockDevice, EGDF_DEVICES, egdf_register_block_dev};
use edgine::kernel::privileged::error::KernelError as FwError;
use edgine::kernel::privileged::sgeg::identity;

/// 注册表类用例的进程内串行锁 (见文件头「隔离说明」)。
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

/// 宿主 RAM 盘扇区数 (16 MiB), 覆盖分区起始 + uberblock 槽位 + 数据块。
const DISK_SECTORS: usize = 32_768;
/// Edgine/EGFS 签名扇区 LBA (`scan_all_drives` 读此处取分区起始)。
const SIGNATURE_LBA: usize = 2_046;
/// 分区起始 LBA — 刻意非 0, 以便钉住「扇区 I/O 必须带分区偏移」。
const PART_START: u32 = 2_048;
/// 单盘 vdev 容量 (4 MiB), 大于 vdev label + 测试载荷。
const VDEV_ASIZE: u64 = 4 * 1024 * 1024;
/// 坏盘全局下标 (I/O 失败且不在位, `scan_all_drives` 会跳过)。
const BAD_DRIVE: u16 = 0;
/// 好盘全局下标 — 刻意非 0, 钉住 drive 取自 vdev 而非回落 0。
const GOOD_DRIVE: u16 = 1;
/// 非扇区对齐载荷长度 (取 init.bin 实测值: 90552 % 512 = 440)。
const PAYLOAD_LEN: usize = 90_552;

/// 清空全局注册表, 保证每个用例从空表出发、drive 下标可预期。
fn clear_registry() {
    EGDF_DEVICES.lock().clear();
}

/// 宿主 RAM 盘: 扇区存储经 `Arc` 共享, 注册进内核后测试侧仍可直读原始扇区。
#[derive(Clone)]
struct RamDisk {
    sectors: Arc<Mutex<Vec<[u8; 512]>>>,
    fail_io: bool,
    present: bool,
}

impl RamDisk {
    /// 可正常 I/O 且在位的全零盘。
    fn working() -> Self {
        Self {
            sectors: Arc::new(Mutex::new(vec![[0u8; 512]; DISK_SECTORS])),
            fail_io: false,
            present: true,
        }
    }

    /// I/O 恒失败且不在位的盘 (错 drive 负控制用)。
    fn failing() -> Self {
        Self {
            fail_io: true,
            present: false,
            ..Self::working()
        }
    }

    /// 写入 Edgine/EGFS 签名扇区, 使 `scan_all_drives` 发现 `(1, PART_START)`。
    ///
    /// 魔数逐字节写而非字面串: 它是早期项目代号遗留的**盘上持久格式**, 更名不能改字节;
    /// 仓内写侧 (`boot_install`) 与读侧 (`scan_all_drives`) 同用逐字节形态。
    fn with_signature(self) -> Self {
        {
            let mut sectors = self.sectors.lock().expect("RAM 盘锁中毒");
            let sig = &mut sectors[SIGNATURE_LBA];
            sig[0] = b'A';
            sig[1] = b'N';
            sig[2] = b'T';
            sig[3] = b'X';
            sig[4..8].copy_from_slice(&PART_START.to_le_bytes());
        }
        self
    }

    /// 直读一个物理 LBA 的原始扇区 (落点断言用)。
    fn sector(&self, lba: usize) -> [u8; 512] {
        self.sectors.lock().expect("RAM 盘锁中毒")[lba]
    }
}

impl BlockDevice for RamDisk {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        if self.fail_io {
            return FwError::Io.as_i32();
        }
        let idx = usize::try_from(sector).expect("测试值必须能无损收窄");
        let sectors = self.sectors.lock().expect("RAM 盘锁中毒");
        if idx >= sectors.len() {
            return FwError::Io.as_i32();
        }
        buf[..512].copy_from_slice(&sectors[idx]);
        0
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        if self.fail_io {
            return FwError::Io.as_i32();
        }
        let idx = usize::try_from(sector).expect("测试值必须能无损收窄");
        let mut sectors = self.sectors.lock().expect("RAM 盘锁中毒");
        if idx >= sectors.len() {
            return FwError::Io.as_i32();
        }
        sectors[idx].copy_from_slice(&buf[..512]);
        0
    }

    fn blk_is_present(&self) -> bool {
        self.present
    }

    fn blk_total_sectors(&self) -> u64 {
        u64::try_from(self.sectors.lock().expect("RAM 盘锁中毒").len())
            .expect("测试值必须能无损收窄")
    }
}

/// 把 RAM 盘注册进全局 EGDF 注册表, 返回其**全局下标** (即 EGFS 的 drive)。
fn register_disk(disk: &RamDisk) -> u32 {
    egdf_register_block_dev(
        "egfs_persist_disk",
        Some(0),
        None,
        Box::leak(Box::new(disk.clone())),
    )
}

/// 注册坏盘 (下标 0) 与带签名好盘 (下标 1), 返回好盘句柄供落点断言。
fn register_bad_and_good() -> RamDisk {
    assert_eq!(register_disk(&RamDisk::failing()), u32::from(BAD_DRIVE));
    let good = RamDisk::working().with_signature();
    assert_eq!(register_disk(&good), u32::from(GOOD_DRIVE));
    good
}

/// 构造指向给定全局 drive 的单盘 vdev 配置。
fn disk_vdev(vdev_id: u16) -> EgfsVdevConfig {
    let mut cfg = EgfsVdevConfig::new_disk(vdev_id, "disk", 12);
    cfg.asize = VDEV_ASIZE;
    cfg.partition_start = PART_START;
    cfg.sector_count = u64::try_from(DISK_SECTORS).expect("测试值必须能无损收窄");
    cfg
}

/// 构造刻意非扇区对齐的确定性载荷。
fn payload() -> Vec<u8> {
    (0..PAYLOAD_LEN)
        .map(|i| u8::try_from(i % 251).expect("测试值必须能无损收窄"))
        .collect()
}

/// 注册并缓存一个测试身份 (creator=0 → 最高特权级), 供 VFS 用例作 pwm 参数。
fn test_pwm() -> u64 {
    static PWM: OnceLock<u64> = OnceLock::new();
    *PWM.get_or_init(|| {
        identity::get_table()
            .create("test-pw", "egfs-disk-persist", 0)
            .expect("注册测试身份失败")
    })
}

/// D5-d 负控制: 未注册 vdev 时必须显式失败, **不得**回落全局下标 0 落盘。
///
/// 下标 0 刻意放一块可正常 I/O 的空白盘: 旧 `.map_or(0, …)` 兜底会在这块未经
/// vdev 授权的盘上留下有效 uberblock, 本用例以「全盘仍为零」钉死该回归。
#[test]
fn uberblock_without_vdev_fails_and_never_falls_back_to_drive0() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let disk = RamDisk::working();
    assert_eq!(register_disk(&disk), 0, "空白盘应占全局下标 0");

    let spa = EgfsSpa::new();
    spa.init("no-vdev-pool");
    assert!(spa.uberblock.lock().is_valid(), "init 后 magic 应有效");
    assert_ne!(
        spa.write_uberblock_to_disk(),
        0,
        "未注册 vdev ⇒ 无 drive 权威, 必须显式失败"
    );
    assert_eq!(
        spa.last_sync_time.load(Ordering::Relaxed),
        0,
        "失败不得推进落盘时间戳 (否则被读成已同步)"
    );
    assert!(
        spa.read_uberblock_from_disk().is_none(),
        "无 drive 时读回应为 None"
    );
    let sectors = disk.sectors.lock().expect("RAM 盘锁中毒");
    assert!(
        sectors.iter().all(|s| s.iter().all(|&b| b == 0)),
        "下标 0 的盘必须一字节未写 (旧兜底路径会在此留下 uberblock)"
    );
}

/// D5-d: magic 无效 (池未初始化) 时早退必须报错, 不得返回 0。
///
/// 刻意先绑定好盘, 把失败原因唯一归到 magic 而非缺 drive。
#[test]
fn uberblock_with_invalid_magic_fails_explicitly() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let spa = EgfsSpa::new();
    assert!(spa.add_vdev(disk_vdev(GOOD_DRIVE)));
    assert!(
        !spa.uberblock.lock().is_valid(),
        "未 init 的 SPA magic 应为 0"
    );
    assert_ne!(spa.write_uberblock_to_disk(), 0, "magic 无效必须显式失败");
    assert_eq!(spa.last_sync_time.load(Ordering::Relaxed), 0);
}

/// D5-d 错 drive 负控制: 绑定到 I/O 失败盘时必须报错, 不得静默成功。
#[test]
fn uberblock_to_failing_drive_fails_explicitly() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let spa = EgfsSpa::new();
    spa.init("wrong-drive-pool");
    assert!(spa.add_vdev(disk_vdev(BAD_DRIVE)));
    assert_ne!(
        spa.write_uberblock_to_disk(),
        0,
        "扇区写失败必须如实上报 (旧 let _ = write_sector 会丢错)"
    );
    assert_eq!(spa.last_sync_time.load(Ordering::Relaxed), 0);
    assert!(spa.read_uberblock_from_disk().is_none());
}

/// D5-d 正路: 好盘真往返 + 落点必须是「分区起始 + txg 槽位」的物理 LBA。
#[test]
fn uberblock_roundtrip_lands_at_partition_plus_slot() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let good = register_bad_and_good();

    let spa = EgfsSpa::new();
    spa.init("roundtrip-pool");
    assert!(spa.add_vdev(disk_vdev(GOOD_DRIVE)));
    {
        let mut ub = spa.uberblock.lock();
        ub.txg = 7;
        ub.pool_guid = 0xABCD;
        ub.root_dataset_obj = 3;
    }
    spa.txg_current.store(7, Ordering::Relaxed);
    assert_eq!(spa.write_uberblock_to_disk(), 0, "已注册好盘上落盘应成功");
    assert_ne!(spa.last_sync_time.load(Ordering::Relaxed), 0);

    let read_back = spa
        .read_uberblock_from_disk()
        .expect("落盘后应能读回 uberblock");
    assert_eq!(read_back.txg, 7);
    assert_eq!(read_back.pool_guid, 0xABCD);
    assert_eq!(read_back.root_dataset_obj, 3);
    assert!(read_back.verify_checksum(), "读回校验和应自洽");

    // 落点断言: 分区偏移取自同一 vdev, 不得漏加也不得漂移到别的盘。
    let part = usize::try_from(PART_START).expect("测试值必须能无损收窄");
    let raw = good.sector(part + 7);
    let on_disk =
        EgfsUberblock::from_bytes_unaligned(&raw).expect("分区起始+槽位 处应是合法 uberblock");
    assert_eq!(on_disk.txg, 7, "物理落点必须是 PART_START + txg 槽位");
    assert!(on_disk.verify_checksum());
    assert!(
        good.sector(7).iter().all(|&b| b == 0),
        "裸 LBA 槽位不得被写 (漏分区偏移即回归)"
    );
}

/// D5-g: 非 512 倍数载荷必须补零到整扇区写入, 且读回逐字节一致。
#[test]
fn write_bp_pads_non_sector_multiple_payload() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let spa = EgfsSpa::new();
    spa.init("pad-pool");
    assert!(spa.add_vdev(disk_vdev(GOOD_DRIVE)));

    let data = payload();
    assert_ne!(data.len() % 512, 0, "载荷必须刻意非扇区对齐");
    let bp = spa
        .allocate(
            u64::try_from(data.len()).expect("测试值必须能无损收窄"),
            EgfsCksumType::Fletcher4,
            EgfsCompType::Off,
            1,
        )
        .expect("metaslab 分配应成功");
    let phys = usize::try_from(bp.prop.physical_size).expect("测试值必须能无损收窄");
    assert_eq!(phys % 512, 0, "物理尺寸应为整扇区");
    assert_eq!(
        spa.write_bp(&bp, &data),
        0,
        "非对齐载荷不得被 write_sectors 缓冲区下界校验拒写"
    );

    let mut back = vec![0u8; phys];
    assert_eq!(spa.read_bp(&bp, &mut back), 0);
    assert_eq!(&back[..data.len()], &data[..], "数据前缀必须逐字节一致");
    assert!(back[data.len()..].iter().all(|&b| b == 0), "补齐尾部应为零");
}

/// 冷读断言: 另建独立 SPA (模拟二次启动), 只凭盘上字节恢复 uberblock 与元数据块。
///
/// 只判「元数据块本身可读且 magic 自洽」; 名字→对象的绑定是否正确由调用方随后的
/// 冷挂载 + 按名打开读回判定 (仅查名字字节出现过无法发现绑定丢失)。
fn assert_cold_read(good_drive: u16) {
    let cold = EgfsSpa::new();
    cold.init("cold-boot");
    assert!(cold.add_vdev(disk_vdev(good_drive)));
    let ub = cold
        .read_uberblock_from_disk()
        .expect("二次启动应能从盘上读回 uberblock (否则每次启动重新格式化)");
    assert!(ub.verify_checksum());
    assert!(
        !ub.root_bp.is_null(),
        "sync 后 root_bp 必须指向已落盘元数据"
    );
    let phys = usize::try_from(ub.root_bp.prop.physical_size).expect("测试值必须能无损收窄");
    let mut meta = vec![0u8; phys];
    assert_eq!(cold.read_bp(&ub.root_bp, &mut meta), 0);
    assert_eq!(&meta[..4], b"HVM1", "元数据 blob magic 应在盘上");
}

/// D5-g: 磁盘模式 VFS 全链 — `fs_create` → write → sync → 冷挂载 → 按名打开读回。
///
/// `fs_create` 缺 override 会走 trait 默认 `NotSupported`, 令 initramfs 解包对每个
/// 常规文件静默失败 (实测 12 条目只落 1 条, 那 1 条是被无条件计数的目录)。
///
/// 读回前刻意重走 `mount_drive`: 对象表与目录索引全部由盘上元数据重建, 才是二次
/// 启动的真实形态。若只重开同一份内存对象表, 元数据写侧丢掉目录项的对象号
/// (`write_dir_entry_record` 曾恒写 0) 这类缺陷会被内存态掩盖。
#[test]
fn egfs_disk_mode_vfs_roundtrip() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let egfs = get_egfs();
    egfs.init();
    assert!(egfs.is_disk_mode(), "发现好盘后应进入磁盘模式");

    let pwm = test_pwm();
    let data = payload();
    let inode = egfs
        .fs_create("/", "init", pwm)
        .expect("fs_create 必须实装, 不得回落 trait 默认 NotSupported");
    assert_eq!(
        inode.write(0, &data, pwm).expect("磁盘模式写入应成功"),
        data.len()
    );
    assert_eq!(egfs.sync(), 0, "sync 必须真落盘并如实返回错误码");

    assert_cold_read(GOOD_DRIVE);

    // 磁盘模式的 write 不填 ARC, 且此前从未读过该块 ⇒ 下面的读必然取自盘。
    assert!(
        egfs.mount_drive(
            u8::try_from(GOOD_DRIVE).expect("测试值必须能无损收窄"),
            PART_START
        ),
        "二次启动应能凭盘上 uberblock 挂载成功"
    );
    let rd = egfs
        .fs_open("/init", 0x0001, pwm)
        .expect("冷恢复后应能按名打开已落盘文件 (目录项对象号丢失即在此失败)");
    let mut back = vec![0u8; data.len()];
    assert_eq!(
        rd.read(0, &mut back, pwm).expect("磁盘模式读回应成功"),
        data.len()
    );
    assert_eq!(back, data, "冷恢复后往返内容必须逐字节一致");
}

/// 把 EGFS 单例拨回冷进程形态 (与 FREG `egfs_restore` 同一组字段)。
///
/// `EGFS_DATA` 是不可重置 `OnceCell`, 同一测试二进制内可能已被别的用例初始化过;
/// 不拨回冷态, `init()` 会在幂等早退处返回, 走不到探测 → 挂载 → 恢复全链。
fn cold_start(egfs: &EgfsData) {
    egfs.initialized.store(false, Ordering::Release);
    egfs.mounted.store(false, Ordering::Release);
    egfs.datasets.lock().clear();
}

/// 读盘上 uberblock 的 txg (另建独立 SPA, 不借被测单例的内存态)。
fn disk_txg(good_drive: u16) -> u64 {
    let cold = EgfsSpa::new();
    cold.init("txg-authority");
    assert!(cold.add_vdev(disk_vdev(good_drive)));
    cold.read_uberblock_from_disk()
        .expect("盘上应有有效 uberblock")
        .txg
}

/// D5-j: 二启恢复路径 — `mount_drive` 成功后 `init()` 不得重做 setup。
///
/// 归因 (aarch64 三相腿相 2 真跑 + gdb 栈): `init()` 曾无条件调
/// `setup_zil_datasets()`, 而后者持 `datasets` 锁跨 `deserialize_dataset_metadata`
/// → `restore_metadata` (自取同一把锁), `IrqSpinLock` 不可重入 ⇒ 同核自旋死等。
/// 只在盘上 `root_bp` 非空 (即成功恢复过的二启) 时可达; 首启格式化写的是 null
/// `root_bp`, 故相 1 恒绿掩盖了它。同一次重做还 push 出第二个 root 数据集, 并把
/// txg 从盘上值回退到 1 ⇒ 新写入落回旧 uberblock 槽位。
#[test]
fn second_boot_init_keeps_mount_setup_and_disk_txg() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let egfs = get_egfs();
    cold_start(egfs);
    egfs.init();
    assert!(egfs.is_disk_mode(), "发现好盘后应进入磁盘模式");
    assert_eq!(
        egfs.datasets.lock().len(),
        1,
        "首启 (格式化路径) 只应建立一个 root 数据集"
    );

    let pwm = test_pwm();
    let data = payload();
    let inode = egfs
        .fs_create("/", "second-boot-probe", pwm)
        .expect("fs_create 必须实装");
    assert_eq!(
        inode.write(0, &data, pwm).expect("磁盘模式写入应成功"),
        data.len()
    );
    assert_eq!(egfs.sync(), 0, "sync 必须真落盘并如实返回错误码");
    assert_cold_read(GOOD_DRIVE);

    let on_disk_txg = disk_txg(GOOD_DRIVE);
    assert!(on_disk_txg > 1, "sync 必须推进 txg, 否则下面的回退判据恒真");

    // 二启: 冷态重走 init。旧实现在此死锁 (不可重入锁自旋), 不会走到下面的断言。
    cold_start(egfs);
    egfs.init();

    assert!(egfs.is_initialized(), "二启应完成初始化");
    assert_eq!(
        egfs.datasets.lock().len(),
        1,
        "二启不得 push 出第二个 root 数据集 (mount_drive 已建过)"
    );
    assert_eq!(
        egfs.spa.txg_current.load(Ordering::Acquire),
        on_disk_txg,
        "二启 txg 必须取自盘上值, 不得被重做的 setup 回退到 1"
    );
    let rd = egfs
        .fs_open("/second-boot-probe", 0x0001, pwm)
        .expect("二启后应能按名打开已落盘文件");
    let mut back = vec![0u8; data.len()];
    assert_eq!(
        rd.read(0, &mut back, pwm).expect("二启读回应成功"),
        data.len()
    );
    assert_eq!(back, data, "二启往返内容必须逐字节一致");

    // 收尾拨回冷态: 与既有用例共用单例, 留下已初始化态会让后跑的用例在 init()
    // 幂等早退处跳过探测, 判据失真。
    cold_start(egfs);
}

/// D5-j 机制锁: `setup_zil_datasets` 不得在持 `datasets` 锁期间调
/// `deserialize_dataset_metadata` (→ `restore_metadata` 自取同一把锁)。
///
/// 上面的行为用例只覆盖 `init()` 路径; `reset()` / FREG `egfs_restore` 仍会调
/// `setup_zil_datasets`, 而它们一旦撞上非空 `root_bp` 就是同核自旋死等 —— 行为
/// 用例无法安全覆盖 (回归形态是挂死而非断言失败), 故这里按源码形状钉住不变式:
/// 调用点之前最后一次取的 `datasets` 锁, 其作用域必须已闭合且未再开新锁。
#[test]
fn setup_zil_datasets_releases_datasets_lock_before_restore() {
    let src = std::fs::read_to_string("../src/kernel/functions/fs/egfs/egfs_data.rs")
        .expect("读取 egfs_data.rs 失败");
    let body = src
        .split("fn setup_zil_datasets(&self) {")
        .nth(1)
        .expect("未找到 setup_zil_datasets");
    let prefix = body
        .split("self.deserialize_dataset_metadata(")
        .next()
        .expect("setup 必须经 deserialize 恢复盘上元数据");
    let flat: String = prefix.chars().filter(|c| !c.is_whitespace()).collect();
    let lock = flat
        .rfind("self.datasets.lock()")
        .expect("setup 必须先取 datasets 锁建立 root 数据集");
    let after = &flat[lock..];
    let close = after.find('}').expect(
        "datasets 锁作用域必须在 deserialize 之前闭合 —— IrqSpinLock 不可重入, \
         持锁跨调用即同核自旋死等 (二启恢复路径挂死的根因)",
    );
    assert!(
        !after[close..].contains("self.datasets.lock()"),
        "deserialize 之前不得再取 datasets 锁: restore_metadata 会自取同一把锁"
    );
}

/// D5-g: 二启冷恢复出来的磁盘根必须仍可写 (建目录 / 建文件 / 写入 / 读回全链)。
///
/// 归因 (aarch64 三相腿相 2 真跑, `[SWROOT] FAIL: mkdir /disk/oldroot rc=-1` 连测 5 轮):
/// `EgfsDataset::new` 的 `state` 缺省是 `Creating`, 而 `mount_drive` 的成功恢复分支只调
/// `restore_metadata` 换对象表与目录索引, 从不翻状态位 ⇒ `is_writeable()` 恒假,
/// `create_dir`/`create_file` 一律 `None`, 恢复出来的磁盘根是只读的。首启走 `init()`
/// (它置 Active), 故相 1 恒绿掩盖了它。
///
/// 恢复路径不能改调 `init()`: 它会 `objset.init()` 推入全新根目录并复位 `next_obj_id`,
/// 把刚恢复的表抹掉 —— 故本用例两侧都钉: 盘上既有名字仍在, 且新名字能建能写。
/// 只判后者会让"重建空表"冒充修复。
#[test]
fn second_boot_restored_disk_root_stays_writeable() {
    let _guard = REGISTRY_LOCK.lock().expect("REGISTRY_LOCK 中毒");
    clear_registry();
    let _good = register_bad_and_good();

    let egfs = get_egfs();
    cold_start(egfs);
    egfs.init();
    assert!(egfs.is_disk_mode(), "发现好盘后应进入磁盘模式");

    let pwm = test_pwm();
    let data = payload();
    assert_eq!(egfs.mkdir("/first-boot-dir", pwm), 0, "首启建目录应成功");
    let inode = egfs
        .fs_create("/", "first-boot-file", pwm)
        .expect("首启建文件应成功");
    assert_eq!(
        inode.write(0, &data, pwm).expect("首启写入应成功"),
        data.len()
    );
    assert_eq!(egfs.sync(), 0, "sync 必须真落盘");
    assert_cold_read(GOOD_DRIVE);

    // 二启: 冷态重走 init, 对象表与目录索引全部由盘上元数据重建。
    cold_start(egfs);
    egfs.init();
    assert!(
        egfs.stat("/first-boot-dir", pwm).is_some(),
        "夹具前提: 盘上目录必须真被恢复 (否则下面判的是重建出来的空表)"
    );
    let rd = egfs
        .fs_open("/first-boot-file", 0x0001, pwm)
        .expect("二启后盘上文件应可按名打开");
    let mut back = vec![0u8; data.len()];
    assert_eq!(
        rd.read(0, &mut back, pwm).expect("二启读回应成功"),
        data.len()
    );
    assert_eq!(back, data, "恢复不得改写既有内容");

    assert_eq!(
        egfs.mkdir("/second-boot-dir", pwm),
        0,
        "恢复出来的磁盘根必须可建目录 (state 停在 Creating 即只读回归)"
    );
    let new_inode = egfs
        .fs_create("/", "second-boot-file", pwm)
        .expect("恢复出来的磁盘根必须可建文件");
    assert_eq!(
        new_inode.write(0, &data, pwm).expect("二启写入应成功"),
        data.len()
    );
    let wr = egfs
        .fs_open("/second-boot-file", 0x0001, pwm)
        .expect("二启新建的文件应可按名打开");
    let mut new_back = vec![0u8; data.len()];
    assert_eq!(
        wr.read(0, &mut new_back, pwm)
            .expect("二启新文件读回应成功"),
        data.len()
    );
    assert_eq!(new_back, data, "二启新写入必须逐字节读回");

    cold_start(egfs);
}
