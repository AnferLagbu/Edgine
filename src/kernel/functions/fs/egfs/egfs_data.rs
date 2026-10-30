#![deny(unsafe_code)]
#![deny(missing_docs)]

use crate::functions::fs::KernelError;
use crate::functions::fs::egfs::arc::{EgfsArcBufType, EgfsArcKey};
use crate::functions::fs::egfs::bp::{EgfsBlockPointer, EgfsCksumType, EgfsCompType};
use crate::functions::fs::egfs::compress;
use crate::functions::fs::egfs::dataset::EgfsDataset;
use crate::functions::fs::egfs::dmu::{EgfsDmuObject, EgfsObjType, HV_DMU_OBJ_ROOT};
use crate::functions::fs::egfs::snapshot::EgfsSnapshotManager;
use crate::functions::fs::egfs::spa::{EgfsPoolState, EgfsSpa, HV_POOL_BLOCK_SIZE};
use crate::functions::fs::egfs::txg::EgfsTxgGroup;
use crate::functions::fs::egfs::zil::{EgfsZil, EgfsZilRecord};
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use crate::privileged::driver::block;
use crate::privileged::sgeg::api as pwm_api;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::functions::sync::once::OnceCell;

fn egfs_save() {}
fn egfs_restore() {
    let egfs = get_egfs();
    egfs.initialized.store(false, Ordering::Release);
    egfs.mounted.store(false, Ordering::Release);
    egfs.spa.init("edgine-pool");
    egfs.setup_zil_datasets();
    egfs.root_ds_id.store(0, Ordering::Release);
    egfs.current_dir.store(HV_DMU_OBJ_ROOT, Ordering::Release);
    egfs.mounted.store(true, Ordering::Release);
    egfs.initialized.store(true, Ordering::Release);
    crate::slog_info!(FS, "[EGFS] Recovery: domain restored");
}
fn egfs_reset() {
    crate::slog_warn!(FS, "[EGFS] Recovery: domain hard reset");
    // J-03 (2026-09-08, G-10 方案 C): 空壳实装 — 显式重建 objset, 供FREG硬重置恢复路径
    get_egfs().reset();
}

/// 打开文件描述符表容量.
pub const EGFS_MAX_FDS: usize = 256;

#[derive(Debug, Clone, Copy)]
/// 打开文件描述符: 关联对象、数据集、偏移、标志与属主.
pub struct EgfsFd {
    /// 描述符编号.
    pub fd: u32,
    /// 关联对象编号.
    pub obj_id: u64,
    /// 所属数据集编号.
    pub ds_id: u64,
    /// 当前读写偏移.
    pub offset: u64,
    /// 打开标志.
    pub flags: u32,
    /// 属主 PWM 身份字.
    pub pwm: u64,
    /// 槽位是否占用.
    pub used: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// EGFS 运行模式: 纯内存或真实磁盘.
pub enum EgfsMode {
    /// 纯内存模式.
    Memory = 0,
    /// 磁盘模式.
    Disk = 1,
}

/// EGFS 全局状态: 存储池、事务组、数据集、快照、意图日志与描述符表.
pub struct EgfsData {
    /// 存储池抽象.
    pub spa: EgfsSpa,
    /// 当前事务组 (仅在打开期间存在).
    pub txg_group: Mutex<Option<EgfsTxgGroup>>,
    /// 已注册数据集列表.
    pub datasets: Mutex<Vec<EgfsDataset>>,
    /// 快照管理器.
    pub snap_mgr: EgfsSnapshotManager,
    /// 意图日志.
    pub zil: EgfsZil,
    /// 描述符表.
    pub fds: Mutex<[EgfsFd; EGFS_MAX_FDS]>,
    /// 下一个待分配的描述符编号.
    pub next_fd: AtomicU32,
    /// 当前进程的 PWM 身份字.
    pub current_pwm: AtomicU64,
    /// 当前工作目录对象编号.
    pub current_dir: AtomicU64,
    /// 是否已挂载.
    pub mounted: AtomicBool,
    /// 是否已初始化.
    pub initialized: AtomicBool,
    /// 根数据集编号.
    pub root_ds_id: AtomicU64,
    /// 当前运行模式 (`EgfsMode` 编码).
    pub mode: AtomicU8,
    /// 已发现的 Edgine/EGFS 磁盘驱动器列表 (`drive_id`, `partition_start_lba`)
    pub drives_discovered: Mutex<Vec<(u8, u32)>>,
    /// 当前使用的磁盘驱动器号.
    pub disk_drive: AtomicU8,
    /// 当前磁盘分区起始 LBA.
    pub partition_start: AtomicU32,
}

// SAFETY (Framekernel P2.2.2): EgfsData 全部字段 (Mutex<T>, Atomic*, EgfsSpa/EgfsZil/EgfsSnapshotManager)
// 都自动实现 Send + Sync, 无需 unsafe impl。

static EGFS_DATA: OnceCell<EgfsData> = OnceCell::new();

/// 取得 EGFS 全局状态单例 (首次调用时惰性初始化).
pub fn get_egfs() -> &'static EgfsData {
    EGFS_DATA.get_or_init(|slot| {
        slot.write(EgfsData {
            spa: EgfsSpa::new(),
            txg_group: Mutex::new(None),
            datasets: Mutex::new(Vec::new()),
            snap_mgr: EgfsSnapshotManager::new(),
            zil: EgfsZil::new(),
            fds: Mutex::new(
                [EgfsFd {
                    fd: 0,
                    obj_id: 0,
                    ds_id: 0,
                    offset: 0,
                    flags: 0,
                    pwm: 0,
                    used: false,
                }; EGFS_MAX_FDS],
            ),
            next_fd: AtomicU32::new(0),
            current_pwm: AtomicU64::new(0),
            current_dir: AtomicU64::new(HV_DMU_OBJ_ROOT),
            mounted: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            root_ds_id: AtomicU64::new(0),
            mode: AtomicU8::new(EgfsMode::Memory as u8),
            drives_discovered: Mutex::new(Vec::new()),
            disk_drive: AtomicU8::new(0),
            partition_start: AtomicU32::new(0),
        });
    })
}

/// 待序列化的数据集元数据快照: (对象清单, 目录条目清单, 下一个对象编号).
type SerializeSnapshot = (
    Vec<EgfsDmuObject>,
    Vec<(alloc::string::String, Vec<u8>)>,
    u64,
);

impl EgfsData {
    /// 扫描所有已注册的块设备，返回检测到的驱动器列表 (`drive_id`, `partition_start_lba`)
    /// 对于已格式化的磁盘读取 `Edgine` 签名，对于空白磁盘使用默认分区起始偏移
    fn scan_all_drives() -> Vec<(u8, u32)> {
        let mut discovered = Vec::new();
        // 扫描 0..8 号驱动器 (足够覆盖当前硬件)
        for drive in 0..8u8 {
            if !block::hdd_is_present(drive) {
                continue;
            }
            let mut cfg = [0u8; 512];
            let r = block::hdd_read_sector(drive, 2046, &mut cfg);
            // 检查 Edgine/EGFS 签名扇区 (LBA 2046)
            let part_start =
                if r >= 0 && cfg[0] == b'A' && cfg[1] == b'N' && cfg[2] == b'T' && cfg[3] == b'X' {
                    u32::from_le_bytes([cfg[4], cfg[5], cfg[6], cfg[7]])
                } else {
                    16384u32 // BOOT_PART_SECTORS 默认值 (空白磁盘)
                };
            discovered.push((drive, part_start));
        }
        crate::slog_info!(
            FS,
            "[EGFS] scan: {} block device(s) in registry, {} drive(s) available",
            block::block_device_count(),
            discovered.len()
        );
        discovered
    }

    /// 首次初始化: 探测并挂载磁盘 (磁盘模式) 或建立内存数据集, 幂等.
    pub fn init(&self) {
        // J-03 (2026-09-08, G-10 方案 C): init 幂等化 — 重复 init 不再重建 objset.
        // 原行为: 重复 init 经 setup_zil_datasets → EgfsObjSet::init 清空 objects,
        // 静默清空磁盘数据 (挂载重试/热插拔重建/FREG故障恢复场景灾难性).
        // 显式重建改经 reset() (FREG恢复钩子 egfs_reset/egfs_restore 调用).
        if self.is_initialized() {
            crate::slog_info!(
                FS,
                "[EGFS] init() skipped: already initialized (idempotent, data preserved)"
            );
            return;
        }
        crate::slog_info!(FS, "[EGFS] Initializing...");

        // Step 1: 扫描所有块设备, 发现 Edgine/EGFS 磁盘
        let discovered = Self::scan_all_drives();
        let has_any_disk = !discovered.is_empty();

        self.spa.disk_present.store(has_any_disk, Ordering::Release);
        {
            let mut list = self.drives_discovered.lock();
            list.clone_from(&discovered);
        }

        // 初始化 SPA (根据是否有磁盘选择 disk / memory 模式)
        self.spa.init("edgine-pool");

        if has_any_disk {
            // Step 2: 尝试从已发现的磁盘挂载 EGFS (uberblock 验证)
            let first = discovered[0];
            self.disk_drive.store(first.0, Ordering::Release);
            let mut mounted_any = false;

            for (drive_id, part_start) in &discovered {
                self.disk_drive.store(*drive_id, Ordering::Release);
                self.partition_start.store(*part_start, Ordering::Release);
                self.spa
                    .partition_start
                    .store(*part_start, Ordering::Release);

                if self.mount_drive(*drive_id, *part_start) {
                    mounted_any = true;
                }
            }

            if !mounted_any {
                // Step 3: 格式化第一个磁盘 (全新 EGFS)
                crate::slog_warn!(
                    FS,
                    "[EGFS] No valid uberblock found, formatting first drive..."
                );
                let (drive_id, part_start) = discovered[0];
                self.disk_drive.store(drive_id, Ordering::Release);
                self.format_drive(drive_id, part_start);

                // 其余磁盘也添加为 vdev
                for (drive_id, part_start) in &discovered[1..] {
                    self.disk_drive.store(*drive_id, Ordering::Release);
                    let mut vdev_cfg = crate::functions::fs::egfs::vdev::EgfsVdevConfig::new_disk(
                        u16::from(*drive_id),
                        "disk",
                        12,
                    );
                    vdev_cfg.asize = Self::probe_partition_size_for_drive(*drive_id, *part_start);
                    vdev_cfg.partition_start = *part_start;
                    self.spa.add_vdev(vdev_cfg);
                }
            }

            // 恢复到 primary drive
            self.disk_drive.store(discovered[0].0, Ordering::Release);
            self.partition_start
                .store(discovered[0].1, Ordering::Release);
            crate::slog_info!(
                FS,
                "[EGFS] Initialized: pool=edgine-pool (disk, {} drive(s))",
                discovered.len()
            );
        } else {
            crate::slog_info!(FS, "[EGFS] No disk, running in memory mode");
            self.spa
                .add_vdev(crate::functions::fs::egfs::vdev::EgfsVdevConfig::new_disk(
                    0, "ata0", 12,
                ));
        }

        self.setup_zil_datasets();
        self.root_ds_id.store(0, Ordering::Release);
        self.current_dir.store(HV_DMU_OBJ_ROOT, Ordering::Release);
        self.mounted.store(true, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        if !self.is_disk_mode() {
            self.mode.store(EgfsMode::Memory as u8, Ordering::Release);
        }
        if !has_any_disk {
            crate::slog_info!(FS, "[EGFS] Initialized: pool=edgine-pool (memory)");
        }

        crate::privileged::freg::recovery::recovery_domain_register(
            "egfs",
            2,
            &[],
            egfs_save,
            egfs_restore,
            egfs_reset,
        );
    }

    /// J-03 (2026-09-08, G-10 方案 C): 显式重建 objset — 供FREG恢复钩子
    /// (`egfs_reset` 硬重置 / `egfs_restore` 域恢复) 调用.
    ///
    /// 与幂等化后的 `init()` 不同, `reset()` 是**显式意图**的重建:
    /// 清空数据集后重新初始化 SPA/ZIL/objset. 仅恢复路径调用, 正常挂载
    /// (重复 init) 不再触发重建, 消除"重复 init 静默清空数据" (G-10).
    pub fn reset(&self) {
        crate::slog_warn!(FS, "[EGFS] Hard reset: rebuilding objset (explicit)");
        self.initialized.store(false, Ordering::Release);
        self.mounted.store(false, Ordering::Release);
        self.spa.init("edgine-pool");
        {
            let mut datasets = self.datasets.lock();
            datasets.clear();
        }
        self.setup_zil_datasets();
        self.root_ds_id.store(0, Ordering::Release);
        self.current_dir.store(HV_DMU_OBJ_ROOT, Ordering::Release);
        self.mounted.store(true, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        crate::slog_info!(FS, "[EGFS] Reset complete: objset rebuilt");
    }

    fn setup_zil_datasets(&self) {
        {
            let mut txg_guard = self.txg_group.lock();
            let mut txg_group = EgfsTxgGroup::new();
            txg_group.init(1);
            *txg_guard = Some(txg_group);
        }
        self.zil.init();
        let mut has_persisted = false;
        {
            let mut datasets = self.datasets.lock();
            let root_ds = EgfsDataset::new(0, "root", 0);
            datasets.push(root_ds);
        }
        {
            let datasets = self.datasets.lock();
            let ub_copy = *self.spa.uberblock.lock();
            if !ub_copy.root_bp.is_null() && self.deserialize_dataset_metadata(&ub_copy.root_bp) {
                has_persisted = true;
            }
            if !has_persisted {
                datasets[0].init(0);
            }
        }
    }

    /// 格式化指定驱动器: 建立存储池元数据并使用给定分区起始偏移.
    pub fn format_drive(&self, drive_id: u8, part_start: u32) {
        if !block::hdd_is_present(drive_id) {
            crate::slog_warn!(FS, "[EGFS] FORMAT: No disk present");
            self.spa.disk_present.store(false, Ordering::Release);
            self.spa.formatted.store(false, Ordering::Release);
            return;
        }
        // 读取 Edgine 配置扇区获取 EGFS 分区起始 LBA
        self.partition_start.store(part_start, Ordering::Release);
        self.spa
            .partition_start
            .store(part_start, Ordering::Release);
        crate::slog_info!(
            FS,
            "[EGFS] FORMAT: Writing fresh EGFS v2 to disk (partition @LBA {})...",
            part_start
        );
        self.spa.disk_present.store(true, Ordering::Release);
        let mut vdev_cfg = crate::functions::fs::egfs::vdev::EgfsVdevConfig::new_disk(
            u16::from(drive_id),
            "disk",
            12,
        );
        vdev_cfg.asize = Self::probe_partition_size_for_drive(drive_id, part_start);
        vdev_cfg.partition_start = part_start;
        self.spa.add_vdev(vdev_cfg);
        self.spa.formatted.store(true, Ordering::Release);
        self.mode.store(EgfsMode::Disk as u8, Ordering::Release);
        self.spa.write_uberblock_to_disk();
        crate::slog_info!(FS, "[EGFS] FORMAT: Complete");
    }

    /// 热插拔: 新磁盘插入后将其添加为 vdev。
    ///
    /// 自动探测 `Edgine` 签名以获取 `partition_start。如果磁盘未格式化则使用默认值`。
    /// 返回 true 表示成功添加。
    pub fn hotplug_add_disk(&self, drive: u8) -> bool {
        if !block::hdd_is_present(drive) {
            crate::slog_warn!(FS, "[EGFS] HOTPLUG: drive not present, skip");
            return false;
        }

        // 检查是否已存在该驱动
        {
            let discovered = self.drives_discovered.lock();
            if discovered.iter().any(|(d, _)| *d == drive) {
                crate::slog_info!(FS, "[EGFS] HOTPLUG: drive already known, skip");
                return false;
            }
        }

        let part_start = {
            let mut cfg = [0u8; 512];
            let r = block::hdd_read_sector(drive, 2046, &mut cfg);
            if r >= 0 && cfg[0] == b'A' && cfg[1] == b'N' && cfg[2] == b'T' && cfg[3] == b'X' {
                u32::from_le_bytes([cfg[4], cfg[5], cfg[6], cfg[7]])
            } else {
                16384u32
            }
        };

        let mut vdev_cfg = crate::functions::fs::egfs::vdev::EgfsVdevConfig::new_disk(
            u16::from(drive),
            "disk",
            12,
        );
        vdev_cfg.asize = Self::probe_partition_size_for_drive(drive, part_start);
        vdev_cfg.partition_start = part_start;
        self.spa.add_vdev(vdev_cfg);

        {
            let mut list = self.drives_discovered.lock();
            list.push((drive, part_start));
        }

        crate::slog_info!(FS, "[EGFS] HOTPLUG: disk added (drive={})", drive);
        true
    }

    /// 热插拔: 磁盘移除后将对应 vdev 标记为离线。
    ///
    /// 不移除 vdev (保持 uberblock 一致性)，仅标记状态为 Removed，
    /// 后续 I/O 将跳过该设备。
    /// 返回 true 表示找到并标记成功。
    pub fn hotplug_remove_disk(&self, drive: u8) -> bool {
        let mut vdevs = self.spa.vdevs.lock();
        if let Some(vdev) = vdevs
            .iter_mut()
            .find(|v| v.config.vdev_id == u16::from(drive))
        {
            vdev.state = crate::functions::fs::egfs::vdev::EgfsVdevState::Removed;
            crate::slog_info!(FS, "[EGFS] HOTPLUG: disk removed (drive={})", drive);
            return true;
        }
        crate::slog_warn!(
            FS,
            "[EGFS] HOTPLUG: disk not found in vdevs (drive={})",
            drive
        );
        false
    }

    fn probe_partition_size_for_drive(drive_id: u8, part_start: u32) -> u64 {
        if !block::hdd_is_present(drive_id) {
            return 0;
        }
        let mut lo: u32 = part_start;
        let mut hi: u32 = 0xFFFF;
        let mut buf = [0u8; 512];
        let mut last_ok = lo;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if block::hdd_read_sector(drive_id, u64::from(mid), &mut buf) >= 0 {
                last_ok = mid;
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if last_ok > part_start {
            (u64::from(last_ok) - u64::from(part_start)) * 512
        } else {
            crate::functions::fs::egfs::vdev::EgfsVdev::probe_disk_size(drive_id)
        }
    }

    /// 以给定分区起始偏移挂载指定驱动器, 成功返回 true.
    pub fn mount_drive(&self, drive_id: u8, part_start: u32) -> bool {
        if !block::hdd_is_present(drive_id) {
            return false;
        }
        self.partition_start.store(part_start, Ordering::Release);
        self.spa
            .partition_start
            .store(part_start, Ordering::Release);
        crate::slog_info!(
            FS,
            "[EGFS] MOUNT: Reading uberblock from disk (partition @LBA {})...",
            part_start
        );
        let Some(ub) = self.spa.read_uberblock_from_disk() else {
            crate::slog_warn!(FS, "[EGFS] MOUNT: No valid uberblock found");
            return false;
        };
        crate::slog_info!(FS, "[EGFS] MOUNT: Valid uberblock found (txg={})", ub.txg);
        {
            let mut stored = self.spa.uberblock.lock();
            *stored = ub;
        }
        self.spa.txg_current.store(ub.txg, Ordering::Release);
        self.spa.formatted.store(true, Ordering::Release);
        self.spa.disk_present.store(true, Ordering::Release);
        self.mode.store(EgfsMode::Disk as u8, Ordering::Release);
        let mut vdev_cfg = crate::functions::fs::egfs::vdev::EgfsVdevConfig::new_disk(
            u16::from(drive_id),
            "disk",
            12,
        );
        vdev_cfg.asize = Self::probe_partition_size_for_drive(drive_id, part_start);
        vdev_cfg.partition_start = part_start;
        self.spa.add_vdev(vdev_cfg);
        {
            let mut txg_guard = self.txg_group.lock();
            let mut txg_group = EgfsTxgGroup::new();
            txg_group.init(ub.txg);
            *txg_guard = Some(txg_group);
        }
        self.zil.init();
        {
            let mut datasets = self.datasets.lock();
            let root_ds = EgfsDataset::new(0, "root", 0);
            datasets.push(root_ds);
        }
        {
            let root_bp = { self.spa.uberblock.lock().root_bp };
            if root_bp.is_null() {
                let datasets = self.datasets.lock();
                datasets[0].init(0);
            } else if self.deserialize_dataset_metadata(&root_bp) {
                crate::slog_info!(FS, "[EGFS] MOUNT: Restored dataset from uberblock");
            } else {
                let datasets = self.datasets.lock();
                datasets[0].init(0);
            }
        }
        self.root_ds_id.store(0, Ordering::Release);
        self.current_dir.store(HV_DMU_OBJ_ROOT, Ordering::Release);
        self.mounted.store(true, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        self.spa
            .state
            .store(EgfsPoolState::Active as u8, Ordering::Release);
        crate::slog_info!(
            FS,
            "[EGFS] MOUNT: Ready (pool_guid={})",
            self.spa.config.lock().guid
        );
        true
    }

    /// 是否已初始化.
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// 当前是否运行于磁盘模式.
    pub fn is_disk_mode(&self) -> bool {
        self.mode.load(Ordering::Acquire) == EgfsMode::Disk as u8
    }

    fn alloc_fd(&self) -> Option<usize> {
        let mut fds = self.fds.lock();
        for i in 0..EGFS_MAX_FDS {
            if !fds[i].used {
                let fd = self.next_fd.fetch_add(1, Ordering::AcqRel);
                fds[i].fd = fd;
                fds[i].used = true;
                return Some(i);
            }
        }
        None
    }

    fn check_permission(obj: &EgfsDmuObject, pwm: u64, cap: u64) -> bool {
        if pwm == 0 {
            return false;
        }
        // Framekernel P2.2.2: 使用 safe 包装
        let level = pwm_api::pwm_get_privilege_level(pwm);
        if level == 0xFF {
            return false;
        }
        if level == 0 {
            return true;
        }
        if obj.owner_pwm == pwm {
            return true;
        }
        // domain=3 (DS = dataset)
        pwm_api::pwm_has_capability(pwm, 3, cap)
    }

    /// 打开 egfs 中的对象并分配文件描述符.
    ///
    /// # Errors
    /// 未初始化时返回 `NotInitialized`; 对象不存在时返回 `FileNotFound`;
    /// 权限不足时返回 `PermissionDenied`; fd 表满时返回 `NoSpace`.
    pub fn open(&self, path: &str, flags: u32, pwm: u64) -> Result<i32, KernelError> {
        if !self.is_initialized() {
            return Err(KernelError::NotInitialized);
        }
        let name = path.trim_start_matches('/');
        let obj_id = {
            let datasets = self.datasets.lock();
            let ds = &datasets[0];
            ds.lookup(name).map_or_else(
                || {
                    if flags & 0x0100 != 0 {
                        ds.create_file(name, pwm)
                    } else {
                        None
                    }
                },
                Some,
            )
        };
        let Some(obj_id) = obj_id else {
            return Err(KernelError::FileNotFound);
        };
        let obj = {
            let datasets = self.datasets.lock();
            datasets[0].objset.get_obj(obj_id)
        };
        let Some(obj) = obj else {
            return Err(KernelError::FileNotFound);
        };
        if !Self::check_permission(&obj, pwm, 0x01) {
            return Err(KernelError::PermissionDenied);
        }
        let Some(fd_idx) = self.alloc_fd() else {
            return Err(KernelError::NoSpace);
        };
        {
            let mut fds = self.fds.lock();
            fds[fd_idx].obj_id = obj_id;
            fds[fd_idx].ds_id = self.root_ds_id.load(Ordering::Acquire);
            fds[fd_idx].offset = if flags & 0x0400 != 0 { obj.size } else { 0 };
            fds[fd_idx].flags = flags;
            fds[fd_idx].pwm = pwm;
        }
        self.zil.add_record(EgfsZilRecord::new_create(0, 0, name));
        Ok(crate::functions::intcast::low_i32_from_usize(fd_idx))
    }

    /// 关闭描述符并释放槽位, 成功返回 0.
    pub fn close(&self, fd: u32) -> i32 {
        let idx = fd as usize;
        if idx >= EGFS_MAX_FDS {
            return KernelError::InvalidArgument.as_i32();
        }
        // TD-03: 原子 claim-and-clear — 锁内同时检查 used 并清零, 杜绝双 close 穿透
        // 与 alloc_fd 之间的 TOCTOU 竞态.
        {
            let mut fds = self.fds.lock();
            if !fds[idx].used {
                return KernelError::InvalidArgument.as_i32();
            }
            fds[idx].used = false;
            fds[idx].offset = 0;
        }
        0
    }

    /// 从描述符当前偏移读取数据并推进偏移, 返回实际读取字节数.
    pub fn read(&self, fd: u32, buf: &mut [u8], count: u32) -> i32 {
        let (obj_id, offset, pwm) = {
            let fds = self.fds.lock();
            let idx = fd as usize;
            if idx >= EGFS_MAX_FDS || !fds[idx].used {
                return KernelError::InvalidArgument.as_i32();
            }
            (fds[idx].obj_id, fds[idx].offset, fds[idx].pwm)
        };
        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x01) {
            return KernelError::PermissionDenied.as_i32();
        }
        let available = if offset < obj.size {
            crate::functions::intcast::usize_from_u64(obj.size - offset)
        } else {
            0
        };
        let to_read = (count as usize).min(available).min(buf.len());
        if to_read == 0 {
            return 0;
        }
        let block_offset = (offset / HV_POOL_BLOCK_SIZE) * HV_POOL_BLOCK_SIZE;
        let block_key = EgfsArcKey::new(0, (obj_id << 40) | block_offset, obj.birth_txg);
        if let Some(data) = self.spa.arc.lookup_slice(&block_key) {
            let start = crate::functions::intcast::usize_from_u64(offset - block_offset);
            let end = (start + to_read).min(crate::functions::intcast::usize_from_u64(
                HV_POOL_BLOCK_SIZE,
            ));
            // 缓存条目可能只覆盖已写入的前缀, 超出部分按稀疏块补零.
            let src_end = end.min(data.len());
            let copied = if start < src_end {
                buf[..src_end - start].copy_from_slice(&data[start..src_end]);
                src_end - start
            } else {
                0
            };
            buf[copied..to_read].fill(0);
        } else if self.is_disk_mode() && !obj.bp.is_null() {
            let mut disk_buf = vec![0u8; obj.bp.prop.physical_size as usize];
            if self.spa.read_bp(&obj.bp, &mut disk_buf) == 0 {
                let start = crate::functions::intcast::usize_from_u64(offset);
                let end = (start + to_read).min(disk_buf.len());
                buf[..end - start].copy_from_slice(&disk_buf[start..end]);
                let arc_key = EgfsArcKey::new(0, (obj_id << 40) | block_offset, obj.birth_txg);
                self.spa
                    .arc
                    .insert(arc_key, &disk_buf, EgfsArcBufType::Data);
            }
        }
        {
            let mut fds = self.fds.lock();
            fds[fd as usize].offset += to_read as u64;
        }
        crate::functions::intcast::low_i32_from_usize(to_read)
    }

    /// 向描述符当前偏移写入数据并推进偏移, 返回实际写入字节数.
    pub fn write(&self, fd: u32, buf: &[u8], count: u32) -> i32 {
        let (obj_id, offset, pwm, flags) = {
            let fds = self.fds.lock();
            let idx = fd as usize;
            if idx >= EGFS_MAX_FDS || !fds[idx].used {
                return KernelError::InvalidArgument.as_i32();
            }
            (
                fds[idx].obj_id,
                fds[idx].offset,
                fds[idx].pwm,
                fds[idx].flags,
            )
        };
        if flags & 0x0001 != 0 && flags & 0x0002 == 0 {
            return KernelError::PermissionDenied.as_i32();
        }
        let mut obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }
        let to_write = (count as usize).min(buf.len());
        if to_write == 0 {
            return 0;
        }
        let txg = self.spa.current_txg();
        let cksum_type = EgfsCksumType::Fletcher4;
        let comp_type = EgfsCompType::Off;
        let compressed = compress::compress(&buf[..to_write], comp_type);
        let write_data = compressed.as_deref().unwrap_or(&buf[..to_write]);
        let Some(new_bp) = self
            .spa
            .allocate(write_data.len() as u64, cksum_type, comp_type, txg)
        else {
            return KernelError::NoSpace.as_i32();
        };
        if self.is_disk_mode() && self.spa.write_bp(&new_bp, write_data) != 0 {
            self.spa.free(&new_bp, txg);
            return KernelError::Io.as_i32();
        }
        obj.cow_bp(new_bp, txg);
        obj.size = (offset + to_write as u64).max(obj.size);
        // Framekernel P2.2.2: 使用 safe timestamp() 替代 extern "C" timer_get_ticks
        obj.mtime = crate::arch!(timestamp());
        if !self.is_disk_mode() {
            let block_offset = (offset / HV_POOL_BLOCK_SIZE) * HV_POOL_BLOCK_SIZE;
            let arc_key = EgfsArcKey::new(0, (obj_id << 40) | block_offset, txg);
            self.spa
                .arc
                .insert(arc_key, &buf[..to_write], EgfsArcBufType::Data);
        }
        {
            let datasets = self.datasets.lock();
            datasets[0].objset.update_obj(&obj);
        }
        {
            let txg_guard = self.txg_group.lock();
            if let Some(ref txg_group) = *txg_guard {
                txg_group.add_dirty_to_open(obj.bp);
            }
        }
        self.zil.add_record(EgfsZilRecord::new_write(
            txg,
            obj_id,
            offset,
            crate::functions::intcast::low_u32_from_usize(to_write),
        ));
        {
            let mut fds = self.fds.lock();
            fds[fd as usize].offset += to_write as u64;
        }
        crate::functions::intcast::low_i32_from_usize(to_write)
    }

    /// 创建目录, 成功返回 0.
    pub fn mkdir(&self, path: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let name = path.trim_start_matches('/');
        let datasets = self.datasets.lock();
        let ds = &datasets[0];
        ds.create_dir(name, pwm).map_or_else(
            || KernelError::Io.as_i32(),
            |obj_id| {
                let txg = self.spa.current_txg();
                self.zil.add_record(EgfsZilRecord::new_mkdir(txg, 0, name));
                crate::functions::intcast::low_i32_from_u64(obj_id)
            },
        )
    }

    /// 删除文件或目录项, 成功返回 0.
    pub fn unlink(&self, path: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let name = path.trim_start_matches('/');
        let obj_id = {
            let datasets = self.datasets.lock();
            datasets[0].lookup(name)
        };
        let Some(obj_id) = obj_id else {
            return KernelError::FileNotFound.as_i32();
        };
        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }
        {
            let datasets = self.datasets.lock();
            if !datasets[0].unlink(name) {
                return KernelError::Io.as_i32();
            }
        }
        let txg = self.spa.current_txg();
        if !obj.bp.is_null() {
            self.spa.free(&obj.bp, txg);
        }
        self.zil.add_record(EgfsZilRecord::new_remove(txg, 0, name));
        0
    }

    /// 查询路径对应对象元数据, 不存在或无权限时返回 None.
    pub fn stat(&self, path: &str, pwm: u64) -> Option<EgfsDmuObject> {
        if !self.is_initialized() {
            return None;
        }
        let name = path.trim_start_matches('/');
        let datasets = self.datasets.lock();
        let ds = &datasets[0];
        let obj_id = ds.lookup(name)?;
        let obj = ds.objset.get_obj(obj_id)?;
        if !Self::check_permission(&obj, pwm, 0x01) {
            return None;
        }
        Some(obj)
    }

    /// 修改文件权限位, 成功返回 0.
    pub fn chmod(&self, path: &str, mode: u16, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let name = path.trim_start_matches('/');

        let mut datasets = self.datasets.lock();
        let ds = &mut datasets[0];
        let Some(obj_id) = ds.lookup(name) else {
            return KernelError::FileNotFound.as_i32();
        };

        let Some(mut obj) = ds.objset.get_obj(obj_id) else {
            return KernelError::FileNotFound.as_i32();
        };

        if obj.owner_pwm != pwm {
            // Framekernel P2.2.2: safe 包装
            let level = pwm_api::pwm_get_privilege_level(pwm);
            if level != 0 {
                return KernelError::PermissionDenied.as_i32();
            }
        }

        obj.pwm_perm = mode;
        // Framekernel P2.2.2: 安全时间戳
        obj.ctime = crate::arch!(timestamp());
        obj.dirty = true;

        if ds.objset.update_obj(&obj) {
            return 0;
        }

        KernelError::Io.as_i32()
    }

    /// 修改文件属主, 成功返回 0.
    pub fn chown(&self, path: &str, owner_pwm: u64, pwm: u64) -> i32 {
        self.chown_ext(path, owner_pwm, 0, pwm)
    }

    /// 同时修改文件属主与属组, 成功返回 0.
    pub fn chown_ext(&self, path: &str, owner_pwm: u64, group_pwm: u64, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let name = path.trim_start_matches('/');

        // Framekernel P2.2.2: safe 包装
        let level = pwm_api::pwm_get_privilege_level(pwm);
        if level != 0 {
            return KernelError::PermissionDenied.as_i32();
        }

        let mut datasets = self.datasets.lock();
        let ds = &mut datasets[0];
        let Some(obj_id) = ds.lookup(name) else {
            return KernelError::FileNotFound.as_i32();
        };

        let Some(mut obj) = ds.objset.get_obj(obj_id) else {
            return KernelError::FileNotFound.as_i32();
        };

        obj.owner_pwm = owner_pwm;
        if group_pwm != 0 {
            obj.group_pwm = group_pwm;
        }
        // Framekernel P2.2.2: 安全时间戳
        obj.ctime = crate::arch!(timestamp());
        obj.dirty = true;

        if ds.objset.update_obj(&obj) {
            return 0;
        }

        KernelError::Io.as_i32()
    }

    /// 设置文件时间戳 (utimensat, POSIX 语义)
    ///
    /// `atime` / `mtime` 为 `u64::MAX` 时表示该字段不修改 (对应 utimensat 的
    /// `UTIME_OMIT` 语义). 权限判据与 `chmod` 一致 (属主或特权级 0).
    ///
    /// # Errors
    /// 未初始化时返回 `NotInitialized`; 路径不存在返回 `FileNotFound`;
    /// 非属主且无特权返回 `PermissionDenied`; 元数据落盘失败返回 `Io`.
    pub fn set_times(
        &self,
        path: &str,
        atime: u64,
        mtime: u64,
        pwm: u64,
    ) -> Result<(), KernelError> {
        if !self.is_initialized() {
            return Err(KernelError::NotInitialized);
        }
        let name = path.trim_start_matches('/');

        let mut datasets = self.datasets.lock();
        let ds = &mut datasets[0];
        let Some(obj_id) = ds.lookup(name) else {
            return Err(KernelError::FileNotFound);
        };

        let Some(mut obj) = ds.objset.get_obj(obj_id) else {
            return Err(KernelError::FileNotFound);
        };

        if obj.owner_pwm != pwm {
            let level = pwm_api::pwm_get_privilege_level(pwm);
            if level != 0 {
                return Err(KernelError::PermissionDenied);
            }
        }

        if atime != u64::MAX {
            obj.atime = atime;
        }
        if mtime != u64::MAX {
            obj.mtime = mtime;
        }
        obj.ctime = crate::arch!(timestamp());
        obj.dirty = true;

        if ds.objset.update_obj(&obj) {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    /// 重命名文件或目录, 成功返回 0.
    pub fn rename(&self, old_path: &str, new_path: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let old_name = old_path.trim_start_matches('/');
        let new_name = new_path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(old_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }

        {
            let datasets = self.datasets.lock();
            if datasets[0].lookup(new_name).is_some() {
                return KernelError::AlreadyExists.as_i32();
            }
        }

        {
            let datasets = self.datasets.lock();
            let ds = &datasets[0];
            ds.dir_zap.remove(old_name);
            ds.dir_zap.insert_u64(new_name, obj_id);
        }

        let txg = self.spa.current_txg();
        self.zil
            .add_record(EgfsZilRecord::new_rename(txg, 0, old_name, new_name));

        0
    }

    /// 创建符号链接, 成功返回 0.
    pub fn symlink(&self, target: &str, linkpath: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let link_name = linkpath.trim_start_matches('/');

        {
            let datasets = self.datasets.lock();
            if datasets[0].lookup(link_name).is_some() {
                return KernelError::AlreadyExists.as_i32();
            }
        }

        let obj_id = {
            let mut datasets = self.datasets.lock();
            let ds = &mut datasets[0];

            match ds.objset.alloc_obj(EgfsObjType::Symlink, pwm) {
                Some(id) => id,
                None => return KernelError::NoSpace.as_i32(),
            }
        };

        {
            let mut datasets = self.datasets.lock();
            let ds = &mut datasets[0];

            if let Some(mut obj) = ds.objset.get_obj_mut(obj_id) {
                obj.obj_type = EgfsObjType::Symlink;
                obj.size = target.len() as u64;
                obj.dirty = true;
                ds.objset.update_obj(&obj);
            }

            let target_bytes = target.as_bytes();
            let txg = self.spa.current_txg();
            let cksum_type = EgfsCksumType::Fletcher4;
            let comp_type = EgfsCompType::Off;

            if let Some(new_bp) =
                self.spa
                    .allocate(target_bytes.len() as u64, cksum_type, comp_type, txg)
                && let Some(mut obj) = ds.objset.get_obj_mut(obj_id)
            {
                obj.bp = new_bp;
                ds.objset.update_obj(&obj);
            }

            if !ds.link(link_name, obj_id) {
                return KernelError::Io.as_i32();
            }
        }

        let txg = self.spa.current_txg();
        self.zil
            .add_record(EgfsZilRecord::new_symlink(txg, 0, link_name, target));

        0
    }

    /// 创建硬链接, 成功返回 0.
    pub fn link(&self, old_path: &str, new_path: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let old_name = old_path.trim_start_matches('/');
        let new_name = new_path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(old_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if obj.obj_type == EgfsObjType::Dir {
            return KernelError::IsDirectory.as_i32();
        }

        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }

        {
            let datasets = self.datasets.lock();
            if datasets[0].lookup(new_name).is_some() {
                return KernelError::AlreadyExists.as_i32();
            }
        }

        {
            let mut datasets = self.datasets.lock();
            let ds = &mut datasets[0];

            if let Some(mut obj) = ds.objset.get_obj_mut(obj_id) {
                obj.link_count += 1;
                obj.dirty = true;
                ds.objset.update_obj(&obj);
            }

            if !ds.link(new_name, obj_id) {
                return KernelError::Io.as_i32();
            }
        }

        let txg = self.spa.current_txg();
        self.zil
            .add_record(EgfsZilRecord::new_link(txg, 0, new_name, obj_id));

        0
    }

    /// 读取符号链接目标, 返回写入缓冲区的字节数.
    pub fn readlink(&self, path: &str, buf: &mut [u8], pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let name = path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        if obj.obj_type != EgfsObjType::Symlink {
            return KernelError::InvalidArgument.as_i32();
        }

        if !Self::check_permission(&obj, pwm, 0x01) {
            return KernelError::PermissionDenied.as_i32();
        }

        if obj.bp.is_null() {
            return 0;
        }

        let target_len = crate::functions::intcast::usize_from_u64(obj.size);
        let to_read = target_len.min(buf.len());

        let block_key = EgfsArcKey::new(0, 0, obj.birth_txg);
        if let Some(data) = self.spa.arc.lookup_slice(&block_key) {
            let n = to_read.min(data.len());
            buf[..n].copy_from_slice(&data[..n]);
            // 缓存条目短于请求长度时, 尾部按稀疏块补零.
            buf[n..to_read].fill(0);
            return crate::functions::intcast::low_i32_from_usize(to_read);
        }

        KernelError::Io.as_i32()
    }

    /// 设置扩展属性
    pub fn setxattr(&self, path: &str, name: &str, value: &[u8], pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let obj_name = path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(obj_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }

        {
            let mut datasets = self.datasets.lock();
            let ds = &mut datasets[0];

            if let Some(mut obj) = ds.objset.get_obj_mut(obj_id) {
                let name_hash = Self::hash_xattr_name(name);
                if name_hash < 4 {
                    let mut hash = [0u64; 4];
                    hash.copy_from_slice(&obj.data_hash);
                    hash[name_hash] = Self::hash_xattr_value(value);
                    obj.data_hash = hash;
                    obj.dirty = true;
                    ds.objset.update_obj(&obj);
                    return 0;
                }
            }
        }

        KernelError::NotSupported.as_i32()
    }

    /// 获取扩展属性
    pub fn getxattr(&self, path: &str, name: &str, buf: &mut [u8], pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let obj_name = path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(obj_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        if !Self::check_permission(&obj, pwm, 0x01) {
            return KernelError::PermissionDenied.as_i32();
        }

        let name_hash = Self::hash_xattr_name(name);
        if name_hash < 4 {
            let value_hash = obj.data_hash[name_hash];
            if value_hash != 0 {
                let hash_bytes = value_hash.to_le_bytes();
                let to_copy = hash_bytes.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&hash_bytes[..to_copy]);
                return crate::functions::intcast::low_i32_from_usize(to_copy);
            }
        }

        KernelError::FileNotFound.as_i32()
    }

    /// 列出扩展属性
    pub fn listxattr(&self, path: &str, buf: &mut [u8], pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let obj_name = path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(obj_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        if !Self::check_permission(&obj, pwm, 0x01) {
            return KernelError::PermissionDenied.as_i32();
        }

        let mut offset = 0;
        for (i, hash) in obj.data_hash.iter().enumerate() {
            if *hash != 0 {
                let attr_name = alloc::format!("user.attr{i}\0");
                let name_bytes = attr_name.as_bytes();
                if offset + name_bytes.len() <= buf.len() {
                    buf[offset..offset + name_bytes.len()].copy_from_slice(name_bytes);
                    offset += name_bytes.len();
                }
            }
        }

        crate::functions::intcast::low_i32_from_usize(offset)
    }

    /// 删除扩展属性
    pub fn removexattr(&self, path: &str, name: &str, pwm: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let obj_name = path.trim_start_matches('/');

        let obj_id = {
            let datasets = self.datasets.lock();
            match datasets[0].lookup(obj_name) {
                Some(id) => id,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };

        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return KernelError::FileNotFound.as_i32(),
            }
        };
        if !Self::check_permission(&obj, pwm, 0x02) {
            return KernelError::PermissionDenied.as_i32();
        }

        {
            let mut datasets = self.datasets.lock();
            let ds = &mut datasets[0];

            if let Some(mut obj) = ds.objset.get_obj_mut(obj_id) {
                let name_hash = Self::hash_xattr_name(name);
                if name_hash < 4 {
                    let mut hash = [0u64; 4];
                    hash.copy_from_slice(&obj.data_hash);
                    hash[name_hash] = 0;
                    obj.data_hash = hash;
                    obj.dirty = true;
                    ds.objset.update_obj(&obj);
                    return 0;
                }
            }
        }

        KernelError::NotSupported.as_i32()
    }

    fn hash_xattr_name(name: &str) -> usize {
        let mut hash: u64 = 5381;
        for byte in name.bytes() {
            hash = ((hash << 5).wrapping_add(hash)).wrapping_add(u64::from(byte));
        }
        (hash % 4) as usize
    }

    fn hash_xattr_value(value: &[u8]) -> u64 {
        let mut hash: u64 = 5381;
        for byte in value {
            hash = ((hash << 5).wrapping_add(hash)).wrapping_add(u64::from(*byte));
        }
        hash
    }

    /// 同步所有未提交数据 (提交事务组并落盘), 成功返回 0.
    pub fn sync(&self) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let txg = self.spa.advance_txg();
        let meta_bp = self.serialize_dataset_metadata(txg);
        {
            let mut ub = self.spa.uberblock.lock();
            ub.txg = txg;
            ub.timestamp = crate::arch!(timestamp());
            if let Some(bp) = meta_bp {
                ub.root_bp = bp;
            }
        }
        self.zil.sync(txg);
        if self.is_disk_mode() {
            self.spa.write_uberblock_to_disk();
        }
        {
            let mut txg_guard = self.txg_group.lock();
            if let Some(ref mut txg_group) = *txg_guard {
                txg_group.transition();
            }
        }
        self.spa.arc.flush_dirty();
        0
    }

    /// 序列化数据集元数据为块指针 (仅磁盘模式有效, 失败返回 `None`)
    fn serialize_dataset_metadata(&self, txg: u64) -> Option<EgfsBlockPointer> {
        let (objects, dir_entries, next_id) = self.collect_serialize_snapshot()?;
        let total = Self::metadata_total_size(&objects, &dir_entries);
        let buf = Self::encode_metadata(&objects, &dir_entries, next_id, total)?;

        if !self.is_disk_mode() {
            return None;
        }
        let bp = self.spa.allocate(
            buf.len() as u64,
            EgfsCksumType::Fletcher4,
            EgfsCompType::Off,
            txg,
        )?;
        if self.spa.write_bp(&bp, &buf) != 0 {
            self.spa.free(&bp, txg);
            return None;
        }
        Some(bp)
    }

    /// 收集待序列化的对象/目录条目快照 (超出安全上限返回 `None`)
    fn collect_serialize_snapshot(&self) -> Option<SerializeSnapshot> {
        const MAX_SERIALIZE_OBJECTS: usize = 65536;
        const MAX_SERIALIZE_ENTRIES: usize = 65536;

        let (objects, dir_entries, next_id) = {
            let datasets = self.datasets.lock();
            let ds = &datasets[0];
            let objs = ds.objset.objects.lock();
            let obj_clones: Vec<EgfsDmuObject> = objs.iter().filter(|o| o.used).copied().collect();
            let dir_list = ds.dir_zap.entries();
            let next = ds.objset.next_obj_id.load(Ordering::Acquire);
            (obj_clones, dir_list, next)
        };

        if objects.len() > MAX_SERIALIZE_OBJECTS || dir_entries.len() > MAX_SERIALIZE_ENTRIES {
            crate::slog_err!(
                FS,
                "[EGFS] serialize: object/entry count exceeds safety limit"
            );
            return None;
        }

        Some((objects, dir_entries, next_id))
    }

    /// 计算元数据序列化缓冲区的总字节数
    fn metadata_total_size(
        objects: &[EgfsDmuObject],
        dir_entries: &[(alloc::string::String, Vec<u8>)],
    ) -> u32 {
        const OBJ_RECORD_SIZE: usize = 222;

        let mut total = 32u32;
        for _ in objects {
            total += crate::functions::intcast::low_u32_from_usize(OBJ_RECORD_SIZE);
        }
        for (name, _) in dir_entries {
            total += 2 + crate::functions::intcast::low_u32_from_usize(name.len()) + 8;
        }
        total
    }

    /// 将元数据头、对象记录与目录条目编码进缓冲区 (失败返回 `None`)
    fn encode_metadata(
        objects: &[EgfsDmuObject],
        dir_entries: &[(alloc::string::String, Vec<u8>)],
        next_id: u64,
        total: u32,
    ) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; total as usize];
        buf[0] = b'H';
        buf[1] = b'V';
        buf[2] = b'M';
        buf[3] = b'1';
        if !Self::write_metadata_header(
            &mut buf,
            crate::functions::intcast::low_u32_from_usize(objects.len()),
            crate::functions::intcast::low_u32_from_usize(dir_entries.len()),
            next_id,
            total,
        ) {
            return None;
        }

        let mut off = 32usize;
        for obj in objects {
            off = Self::write_object_record(&mut buf, off, obj)?;
        }
        for (name, _value) in dir_entries {
            off = Self::write_dir_entry_record(&mut buf, off, name)?;
        }

        Some(buf)
    }

    /// 写入元数据头 (版本/对象数/条目数/`next_id`/总长), 越界返回 `false`
    fn write_metadata_header(
        buf: &mut [u8],
        obj_count: u32,
        entry_count: u32,
        next_id: u64,
        total: u32,
    ) -> bool {
        if !Self::write_le32(buf, 4, 1) {
            return false;
        }
        if !Self::write_le32(buf, 8, obj_count) {
            return false;
        }
        if !Self::write_le32(buf, 12, entry_count) {
            return false;
        }
        if !Self::write_le64(buf, 16, next_id) {
            return false;
        }
        if !Self::write_le32(buf, 24, total) {
            return false;
        }
        true
    }

    /// 写入单条对象记录, 返回下一个写入偏移; 越界返回 `None`
    fn write_object_record(buf: &mut [u8], mut off: usize, obj: &EgfsDmuObject) -> Option<usize> {
        const OBJ_RECORD_SIZE: usize = 222;

        if off + OBJ_RECORD_SIZE > buf.len() {
            return None;
        }
        if !Self::write_le64(buf, off, obj.obj_id) {
            return None;
        }
        off += 8;
        buf[off] = obj.obj_type as u8;
        off += 1;
        if !Self::write_le32(buf, off, obj.block_size) {
            return None;
        }
        off += 4;
        if !Self::write_le64(buf, off, obj.nblocks) {
            return None;
        }
        off += 8;
        if !Self::write_le64(buf, off, obj.size) {
            return None;
        }
        off += 8;
        if off + EgfsBlockPointer::BYTES > buf.len() {
            return None;
        }
        let bp_bytes = obj.bp.as_bytes();
        buf[off..off + EgfsBlockPointer::BYTES].copy_from_slice(bp_bytes);
        off += EgfsBlockPointer::BYTES;
        if !Self::write_le64(buf, off, obj.atime) {
            return None;
        }
        off += 8;
        if !Self::write_le64(buf, off, obj.mtime) {
            return None;
        }
        off += 8;
        if !Self::write_le64(buf, off, obj.ctime) {
            return None;
        }
        off += 8;
        if !Self::write_le64(buf, off, obj.owner_pwm) {
            return None;
        }
        off += 8;
        if !Self::write_le64(buf, off, obj.group_pwm) {
            return None;
        }
        off += 8;
        buf[off] = obj.sensitivity;
        off += 1;
        if !Self::write_le16(buf, off, obj.pwm_perm) {
            return None;
        }
        off += 2;
        if !Self::write_le32(buf, off, obj.link_count) {
            return None;
        }
        off += 4;
        if !Self::write_le32(buf, off, obj.flags) {
            return None;
        }
        off += 4;
        if !Self::write_le64(buf, off, obj.birth_txg) {
            return None;
        }
        off += 8;
        buf[off] = u8::from(obj.used);
        off += 1;
        off += 1;
        Some(off)
    }

    /// 写入单条目录条目记录, 返回下一个写入偏移; 越界返回 `None`
    fn write_dir_entry_record(buf: &mut [u8], mut off: usize, name: &str) -> Option<usize> {
        let name_bytes = name.as_bytes();
        if off + 2 + name_bytes.len() + 8 > buf.len() {
            return None;
        }
        if !Self::write_le16(
            buf,
            off,
            crate::functions::intcast::low_u16_from_usize(name_bytes.len()),
        ) {
            return None;
        }
        off += 2;
        buf[off..off + name_bytes.len()].copy_from_slice(name_bytes);
        off += name_bytes.len();
        if !Self::write_le64(buf, off, 0) {
            return None;
        }
        off += 8;
        Some(off)
    }

    /// 从块指针反序列化数据集元数据, 成功返回 `true`
    fn deserialize_dataset_metadata(&self, bp: &EgfsBlockPointer) -> bool {
        if bp.is_null() || !self.is_disk_mode() {
            return false;
        }
        let mut buf = vec![0u8; bp.prop.physical_size as usize];
        if self.spa.read_bp(bp, &mut buf) != 0 {
            return false;
        }
        if buf.len() < 32 {
            return false;
        }
        if buf[0] != b'H' || buf[1] != b'V' || buf[2] != b'M' || buf[3] != b'1' {
            return false;
        }

        let Some((obj_count, zap_count, next_obj_id)) = Self::parse_metadata_header(&buf) else {
            return false;
        };

        self.restore_metadata(&buf, obj_count, zap_count, next_obj_id)
    }

    /// 解析元数据头部的计数与 `next_obj_id` (含上限与长度校验)
    fn parse_metadata_header(buf: &[u8]) -> Option<(usize, usize, u64)> {
        const OBJ_RECORD_SIZE: usize = 222;
        const MAX_DESERIALIZE_OBJECTS: usize = 65536;
        const MAX_DESERIALIZE_ENTRIES: usize = 65536;

        let obj_count = Self::read_le32(buf, 8)? as usize;
        let zap_count = Self::read_le32(buf, 12)? as usize;
        let next_obj_id = Self::read_le64(buf, 16)?;

        if obj_count > MAX_DESERIALIZE_OBJECTS || zap_count > MAX_DESERIALIZE_ENTRIES {
            crate::slog_err!(
                FS,
                "[EGFS] deserialize: count exceeds safety limit, possible corruption"
            );
            return None;
        }

        let expected_min =
            32u64 + obj_count as u64 * OBJ_RECORD_SIZE as u64 + zap_count as u64 * (2 + 8);
        if (buf.len() as u64) < expected_min {
            crate::slog_err!(
                FS,
                "[EGFS] deserialize: buffer too small for declared counts"
            );
            return None;
        }

        Some((obj_count, zap_count, next_obj_id))
    }

    /// 用反序列化结果替换对象表与目录索引
    fn restore_metadata(
        &self,
        buf: &[u8],
        obj_count: usize,
        zap_count: usize,
        next_obj_id: u64,
    ) -> bool {
        let datasets = self.datasets.lock();
        let ds = &datasets[0];
        let mut objs = ds.objset.objects.lock();
        objs.clear();

        let mut off = 32usize;
        for _ in 0..obj_count {
            let Some((obj, next_off)) = Self::parse_object_record(buf, off) else {
                return false;
            };
            objs.push(obj);
            off = next_off;
        }

        ds.objset.next_obj_id.store(next_obj_id, Ordering::Release);

        ds.dir_zap.clear();
        for _ in 0..zap_count {
            let Some((name, obj_id, next_off)) = Self::parse_dir_entry_record(buf, off) else {
                return false;
            };
            ds.dir_zap.insert_u64(name, obj_id);
            off = next_off;
        }

        true
    }

    /// 解析单条对象记录, 返回 (对象, 下一个偏移); 越界或字段缺失返回 `None`
    fn parse_object_record(buf: &[u8], mut off: usize) -> Option<(EgfsDmuObject, usize)> {
        const OBJ_RECORD_SIZE: usize = 222;

        if off + OBJ_RECORD_SIZE > buf.len() {
            return None;
        }

        let obj_id = Self::read_le64(buf, off)?;
        off += 8;
        let obj_type = EgfsObjType::from_u8(buf[off]);
        off += 1;
        let _block_size = Self::read_le32(buf, off)?;
        off += 4;
        let nblocks = Self::read_le64(buf, off)?;
        off += 8;
        let size = Self::read_le64(buf, off)?;
        off += 8;

        if off + EgfsBlockPointer::BYTES > buf.len() {
            return None;
        }
        let bp_val = EgfsBlockPointer::from_bytes(&buf[off..off + EgfsBlockPointer::BYTES])?;
        off += EgfsBlockPointer::BYTES;

        let atime = Self::read_le64(buf, off)?;
        off += 8;
        let mtime = Self::read_le64(buf, off)?;
        off += 8;
        let ctime = Self::read_le64(buf, off)?;
        off += 8;
        let owner_pwm = Self::read_le64(buf, off)?;
        off += 8;
        let group_pwm = Self::read_le64(buf, off)?;
        off += 8;
        let sensitivity = buf[off];
        off += 1;
        let pwm_perm = Self::read_le16(buf, off)?;
        off += 2;
        let link_count = Self::read_le32(buf, off)?;
        off += 4;
        let flags = Self::read_le32(buf, off)?;
        off += 4;
        let birth_txg = Self::read_le64(buf, off)?;
        off += 8;
        let used = buf[off] != 0;
        off += 1;
        off += 1;
        let obj = EgfsDmuObject {
            obj_id,
            obj_type,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            nblocks,
            size,
            bp: bp_val,
            atime,
            mtime,
            ctime,
            owner_pwm,
            group_pwm,
            sensitivity,
            pwm_perm,
            link_count,
            flags,
            birth_txg,
            data_hash: [0; 4],
            fill: 0,
            dirty: false,
            used,
        };
        Some((obj, off))
    }

    /// 解析单条目录条目记录, 返回 (名字, 对象号, 下一个偏移); 失败返回 `None`
    fn parse_dir_entry_record(buf: &[u8], mut off: usize) -> Option<(&str, u64, usize)> {
        const MAX_NAME_LEN: usize = 4096;

        if off + 2 > buf.len() {
            return None;
        }
        let name_len = Self::read_le16(buf, off)? as usize;
        off += 2;
        if name_len > MAX_NAME_LEN {
            crate::slog_err!(
                FS,
                "[EGFS] deserialize: name length exceeds limit, possible corruption"
            );
            return None;
        }
        if off + name_len + 8 > buf.len() {
            return None;
        }
        let name = core::str::from_utf8(&buf[off..off + name_len]).unwrap_or("?");
        off += name_len;
        let obj_id = Self::read_le64(buf, off)?;
        off += 8;
        Some((name, obj_id, off))
    }

    fn write_le16(buf: &mut [u8], off: usize, v: u16) -> bool {
        if off + 2 > buf.len() {
            return false;
        }
        let b = v.to_le_bytes();
        buf[off] = b[0];
        buf[off + 1] = b[1];
        true
    }

    fn write_le32(buf: &mut [u8], off: usize, v: u32) -> bool {
        if off + 4 > buf.len() {
            return false;
        }
        let b = v.to_le_bytes();
        buf[off] = b[0];
        buf[off + 1] = b[1];
        buf[off + 2] = b[2];
        buf[off + 3] = b[3];
        true
    }

    fn write_le64(buf: &mut [u8], off: usize, v: u64) -> bool {
        if off + 8 > buf.len() {
            return false;
        }
        let b = v.to_le_bytes();
        buf[off] = b[0];
        buf[off + 1] = b[1];
        buf[off + 2] = b[2];
        buf[off + 3] = b[3];
        buf[off + 4] = b[4];
        buf[off + 5] = b[5];
        buf[off + 6] = b[6];
        buf[off + 7] = b[7];
        true
    }

    fn read_le16(buf: &[u8], off: usize) -> Option<u16> {
        if off + 2 > buf.len() {
            return None;
        }
        Some(u16::from_le_bytes([buf[off], buf[off + 1]]))
    }

    fn read_le32(buf: &[u8], off: usize) -> Option<u32> {
        if off + 4 > buf.len() {
            return None;
        }
        Some(u32::from_le_bytes([
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3],
        ]))
    }

    fn read_le64(buf: &[u8], off: usize) -> Option<u64> {
        if off + 8 > buf.len() {
            return None;
        }
        Some(u64::from_le_bytes([
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3],
            buf[off + 4],
            buf[off + 5],
            buf[off + 6],
            buf[off + 7],
        ]))
    }

    /// 创建快照
    pub fn snapshot_create(&self, name: &str) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let datasets = self.datasets.lock();
        let ds = &datasets[0];
        let txg = self.spa.current_txg();
        crate::functions::intcast::low_i32_from_u64(self.snap_mgr.create_snapshot(ds, name, txg))
    }

    /// 销毁快照
    pub fn snapshot_destroy(&self, snap_id: u64) -> i32 {
        if self.snap_mgr.destroy_snapshot(snap_id) {
            0
        } else {
            KernelError::FileNotFound.as_i32()
        }
    }

    /// 回滚快照
    pub fn snapshot_rollback(&self, snap_id: u64) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let datasets = self.datasets.lock();
        let ds = &datasets[0];
        if self.snap_mgr.rollback(snap_id, ds) {
            0
        } else {
            KernelError::Io.as_i32()
        }
    }

    /// 从快照创建克隆
    pub fn clone_create(&self, snap_id: u64, name: &str) -> i32 {
        if !self.is_initialized() {
            return KernelError::NotInitialized.as_i32();
        }
        let ds_id = { self.datasets.lock().len() as u64 };
        let txg = self.spa.current_txg();
        self.snap_mgr
            .create_clone(snap_id, ds_id, name, txg)
            .map_or_else(
                || KernelError::Io.as_i32(),
                |ds| {
                    ds.init(0);
                    self.datasets.lock().push(ds);
                    crate::functions::intcast::low_i32_from_u64(ds_id)
                },
            )
    }

    /// 调整描述符偏移 (`whence` 语义同 lseek), 返回新偏移.
    pub fn seek(&self, fd: u32, offset: i64, whence: u32) -> i64 {
        let (obj_id, cur_offset) = {
            let fds = self.fds.lock();
            let idx = fd as usize;
            if idx >= EGFS_MAX_FDS || !fds[idx].used {
                return i64::from(KernelError::InvalidArgument.as_i32());
            }
            (fds[idx].obj_id, fds[idx].offset)
        };
        let obj = {
            let datasets = self.datasets.lock();
            match datasets[0].objset.get_obj(obj_id) {
                Some(o) => o,
                None => return i64::from(KernelError::FileNotFound.as_i32()),
            }
        };
        let new_offset = match whence {
            0 => offset.cast_unsigned(),
            1 => (cur_offset.cast_signed() + offset).cast_unsigned(),
            2 => (obj.size.cast_signed() + offset).cast_unsigned(),
            _ => return i64::from(KernelError::InvalidArgument.as_i32()),
        };
        {
            let mut fds = self.fds.lock();
            fds[fd as usize].offset = new_offset;
        }
        new_offset.cast_signed()
    }

    /// 获取 `EGFS` 池统计 (allocs, frees, reads, writes)
    pub fn get_stats(&self) -> (u64, u64, u64, u64) {
        if !self.is_initialized() {
            return (0, 0, 0, 0);
        }
        let (allocs, frees, reads, writes, _) = self.spa.get_stats();
        (allocs, frees, reads, writes)
    }
}
