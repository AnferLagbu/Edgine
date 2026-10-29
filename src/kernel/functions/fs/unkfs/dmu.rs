#![deny(unsafe_code)]
#![deny(missing_docs)]
use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// DMU 对象编号空间的保留编号 0.
pub const HV_DMU_OBJ_NUM: u64 = 0;
/// 元数据 ZAP 对象的固定对象编号.
pub const HV_DMU_OBJ_META: u64 = 1;
/// 根目录对象的固定对象编号.
pub const HV_DMU_OBJ_ROOT: u64 = 2;
/// 单个对象可持有的块指针数量上限.
pub const HV_DMU_MAX_BLOCKPTR: usize = 16;
/// 对象名允许的最大字节数.
pub const HV_DMU_MAX_NAME: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// DMU 对象类型编码 (对应 ZFS dnode 的对象类型).
pub enum UnkfsObjType {
    /// 未定义/无效类型.
    None = 0,
    /// 普通文件.
    File = 1,
    /// 目录.
    Dir = 2,
    /// 快照.
    Snapshot = 3,
    /// ZAP 键值对象.
    Zap = 4,
    /// 微 ZAP (小端内嵌键值).
    ZapMicro = 5,
    /// 卷对象.
    Volume = 6,
    /// 空间映射对象.
    SpaceMap = 7,
    /// 对象集元对象.
    ObjSet = 8,
    /// 符号链接.
    Symlink = 9,
}

impl UnkfsObjType {
    /// 由原始类型值解析对象类型; 未知值归为 `None`.
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::File,
            2 => Self::Dir,
            3 => Self::Snapshot,
            4 => Self::Zap,
            5 => Self::ZapMicro,
            6 => Self::Volume,
            7 => Self::SpaceMap,
            8 => Self::ObjSet,
            9 => Self::Symlink,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
/// 一个 DMU 对象的元数据与根块指针 (对应 ZFS dnode 的物理形态).
pub struct UnkfsDmuObject {
    /// 对象编号.
    pub obj_id: u64,
    /// 对象类型.
    pub obj_type: UnkfsObjType,
    /// 数据块大小 (字节).
    pub block_size: u32,
    /// 已分配的数据块数.
    pub nblocks: u64,
    /// 逻辑大小 (字节).
    pub size: u64,
    /// 指向数据块树的根块指针.
    pub bp: UnkfsBlockPointer,
    /// 最近访问时间 (tick).
    pub atime: u64,
    /// 最近修改时间 (tick).
    pub mtime: u64,
    /// 最近状态变更时间 (tick).
    pub ctime: u64,
    /// 属主 PWM 身份字.
    pub owner_pwm: u64,
    /// 属组 PWM 身份字.
    pub group_pwm: u64,
    /// 敏感度等级.
    pub sensitivity: u8,
    /// POSIX 权限位.
    pub pwm_perm: u16,
    /// 硬链接计数.
    pub link_count: u32,
    /// 对象标志位.
    pub flags: u32,
    /// 最近写入的事务组号.
    pub birth_txg: u64,
    /// 内容哈希 (4 个 64 位字).
    pub data_hash: [u64; 4],
    /// 填充/保留字段.
    pub fill: u64,
    /// 是否存在尚未落盘的修改.
    pub dirty: bool,
    /// 是否仍在使用 (释放后置 false).
    pub used: bool,
}

impl UnkfsDmuObject {
    /// 构造普通文件对象, 默认权限 0644、链接数 1.
    #[must_use]
    pub fn new_file(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::File,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            nblocks: 0,
            size: 0,
            bp: UnkfsBlockPointer::null(),
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm,
            group_pwm: 0,
            sensitivity: 0,
            pwm_perm: 0o644,
            link_count: 1,
            flags: 0,
            birth_txg: 0,
            data_hash: [0; 4],
            fill: 0,
            dirty: false,
            used: true,
        }
    }

    /// 构造目录对象, 默认权限 0755、链接数 2.
    #[must_use]
    pub fn new_dir(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Dir,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            nblocks: 0,
            size: 0,
            bp: UnkfsBlockPointer::null(),
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm,
            group_pwm: 0,
            sensitivity: 0,
            pwm_perm: 0o755,
            link_count: 2,
            flags: 0,
            birth_txg: 0,
            data_hash: [0; 4],
            fill: 0,
            dirty: false,
            used: true,
        }
    }

    /// 构造 ZAP 键值对象.
    #[must_use]
    pub fn new_zap(obj_id: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Zap,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            nblocks: 0,
            size: 0,
            bp: UnkfsBlockPointer::null(),
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm: 0,
            group_pwm: 0,
            sensitivity: 0,
            pwm_perm: 0o644,
            link_count: 1,
            flags: 0,
            birth_txg: 0,
            data_hash: [0; 4],
            fill: 0,
            dirty: false,
            used: true,
        }
    }

    /// 构造符号链接对象, 默认权限 0777.
    #[must_use]
    pub fn new_symlink(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Symlink,
            block_size: crate::functions::intcast::low_u32_from_u64(HV_POOL_BLOCK_SIZE),
            nblocks: 0,
            size: 0,
            bp: UnkfsBlockPointer::null(),
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm,
            group_pwm: 0,
            sensitivity: 0,
            pwm_perm: 0o777,
            link_count: 1,
            flags: 0,
            birth_txg: 0,
            data_hash: [0; 4],
            fill: 0,
            dirty: false,
            used: true,
        }
    }

    /// 是否为普通文件.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.obj_type == UnkfsObjType::File
    }
    /// 是否为目录.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.obj_type == UnkfsObjType::Dir
    }
    /// 是否为 ZAP 或微 ZAP 对象.
    #[must_use]
    pub fn is_zap(&self) -> bool {
        self.obj_type == UnkfsObjType::Zap || self.obj_type == UnkfsObjType::ZapMicro
    }
    /// 是否为快照对象.
    #[must_use]
    pub fn is_snapshot(&self) -> bool {
        self.obj_type == UnkfsObjType::Snapshot
    }

    /// 标记对象为脏并记录写入的事务组号.
    pub fn mark_dirty(&mut self, txg: u64) {
        self.dirty = true;
        self.birth_txg = txg;
    }

    /// 写时复制替换根块指针, 同时记录新事务组号并置脏.
    pub fn cow_bp(&mut self, new_bp: UnkfsBlockPointer, txg: u64) {
        self.bp = new_bp;
        self.birth_txg = txg;
        self.dirty = true;
    }
}

/// DMU 对象集: 持有全部对象并负责编号分配与生命周期管理.
pub struct UnkfsObjSet {
    /// 对象表 (受锁保护).
    pub objects: Mutex<Vec<UnkfsDmuObject>>,
    /// 下一个可分配的对象编号.
    pub next_obj_id: AtomicU64,
    /// 根目录对象编号.
    pub root_obj: u64,
    /// 是否已完成 [`UnkfsObjSet::init`] 初始化.
    pub initialized: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): UnkfsObjSet 全部字段 (Mutex<T>, Atomic*, Vec) 自动 Send + Sync。

impl UnkfsObjSet {
    /// 构造空对象集, 根对象编号取 [`HV_DMU_OBJ_ROOT`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(Vec::new()),
            next_obj_id: AtomicU64::new(HV_DMU_OBJ_ROOT + 1),
            root_obj: HV_DMU_OBJ_ROOT,
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化对象集: 清空对象表, 建立根目录与元数据 ZAP 对象.
    pub fn init(&self, owner_pwm: u64) {
        let mut objs = self.objects.lock();
        objs.clear();
        let mut root = UnkfsDmuObject::new_dir(HV_DMU_OBJ_ROOT, owner_pwm);
        root.birth_txg = 1;
        objs.push(root);
        let zap = UnkfsDmuObject::new_zap(HV_DMU_OBJ_META);
        objs.push(zap);
        self.next_obj_id
            .store(HV_DMU_OBJ_ROOT + 2, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
    }

    /// 按类型分配新对象编号并插入对象表; 不支持的类型返回 `None`.
    pub fn alloc_obj(&self, obj_type: UnkfsObjType, owner_pwm: u64) -> Option<u64> {
        let obj_id = self.next_obj_id.fetch_add(1, Ordering::AcqRel);
        let obj = match obj_type {
            UnkfsObjType::File => UnkfsDmuObject::new_file(obj_id, owner_pwm),
            UnkfsObjType::Dir => UnkfsDmuObject::new_dir(obj_id, owner_pwm),
            UnkfsObjType::Zap | UnkfsObjType::ZapMicro => UnkfsDmuObject::new_zap(obj_id),
            UnkfsObjType::Symlink => UnkfsDmuObject::new_symlink(obj_id, owner_pwm),
            _ => return None,
        };
        self.objects.lock().push(obj);
        Some(obj_id)
    }

    /// 释放指定对象: 置 unused 并递减链接数; 对象不存在返回 false.
    pub fn free_obj(&self, obj_id: u64) -> bool {
        let mut objs = self.objects.lock();
        if let Some(obj) = objs.iter_mut().find(|o| o.obj_id == obj_id) {
            obj.used = false;
            obj.link_count = obj.link_count.saturating_sub(1);
            if obj.link_count == 0 {
                obj.used = false;
            }
            true
        } else {
            false
        }
    }

    /// 按编号取对象副本; 不存在或已释放返回 `None`.
    pub fn get_obj(&self, obj_id: u64) -> Option<UnkfsDmuObject> {
        let objs = self.objects.lock();
        objs.iter().find(|o| o.obj_id == obj_id && o.used).copied()
    }

    /// 按编号取对象副本 (语义同 [`UnkfsObjSet::get_obj`], 保留可变访问命名).
    pub fn get_obj_mut(&self, obj_id: u64) -> Option<UnkfsDmuObject> {
        self.get_obj(obj_id)
    }

    /// 用给定对象覆盖同编号的现有对象; 对象不存在返回 false.
    pub fn update_obj(&self, obj: &UnkfsDmuObject) -> bool {
        let mut objs = self.objects.lock();
        objs.iter_mut()
            .find(|o| o.obj_id == obj.obj_id)
            .is_some_and(|existing| {
                *existing = *obj;
                true
            })
    }

    /// 取根目录对象副本.
    pub fn get_root(&self) -> Option<UnkfsDmuObject> {
        self.get_obj(self.root_obj)
    }

    /// 统计仍在使用的对象个数.
    pub fn obj_count(&self) -> u64 {
        self.objects.lock().iter().filter(|o| o.used).count() as u64
    }
}

impl Default for UnkfsObjSet {
    /// 委托 [`UnkfsObjSet::new`] 构造空对象集.
    fn default() -> Self {
        Self::new()
    }
}

// DECISION-080: DMU 对象 / ObjSet 纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    // ==== DMU 对象基础语义 ====

    /// 新建 File 对象应带上默认类型与零大小.
    #[test]
    fn test_dmu_object_default() {
        let obj = UnkfsDmuObject::new_file(1, 0);
        assert_eq!(obj.obj_id, 1, "obj_id mismatch");
        assert_eq!(obj.obj_type, UnkfsObjType::File, "obj_type should be File");
        assert_eq!(obj.size, 0, "new object size should be 0");
    }

    /// cow_bp 应更新 birth_txg.
    #[test]
    fn test_dmu_object_cow() {
        let mut obj = UnkfsDmuObject::new_file(2, 0);
        let new_bp = UnkfsBlockPointer::null();
        obj.cow_bp(new_bp, 5);
        assert_eq!(obj.birth_txg, 5, "birth txg should be 5");
    }

    /// 新建 Dir 对象应报告为目录类型.
    #[test]
    fn test_dmu_object_dir_type() {
        let obj = UnkfsDmuObject::new_dir(3, 0);
        assert!(obj.is_dir(), "Dir should report as dir");
    }

    // ==== ObjSet 分配 / 释放 ====

    /// alloc_obj 应返回有效 id 且可被 get_obj 取回.
    #[test]
    fn test_dmu_objset_alloc() {
        let os = UnkfsObjSet::new();
        os.init(0);
        let Some(id) = os.alloc_obj(UnkfsObjType::File, 0) else {
            panic!("alloc_obj should succeed");
        };
        assert!(id > 0, "allocated obj_id should be > 0");
        let Some(o) = os.get_obj(id) else {
            panic!("get_obj should find allocated object");
        };
        assert!(o.is_file(), "allocated type should be File");
    }

    /// alloc_obj 的 Dir 类型应被正确记录.
    #[test]
    fn test_dmu_objset_dir() {
        let os = UnkfsObjSet::new();
        os.init(0);
        let Some(obj_id) = os.alloc_obj(UnkfsObjType::Dir, 0) else {
            panic!("alloc_obj Dir should succeed");
        };
        let Some(o) = os.get_obj(obj_id) else {
            panic!("get_obj should find Dir object");
        };
        assert!(o.is_dir(), "should be Dir type");
        assert!(!o.is_file(), "Dir should not be File");
    }

    /// free_obj 后对象不应再被 get_obj 取回.
    #[test]
    fn test_dmu_objset_free() {
        let os = UnkfsObjSet::new();
        os.init(0);
        let Some(obj_id) = os.alloc_obj(UnkfsObjType::File, 0) else {
            panic!("alloc_obj should succeed");
        };
        let _count_before = os.obj_count();
        assert!(os.free_obj(obj_id), "free_obj should succeed");
        assert!(
            os.get_obj(obj_id).is_none(),
            "freed obj should not be found"
        );
    }

    /// cow_bp 后旧 birth_txg 应被新值覆盖.
    #[test]
    fn test_dmu_cow_preserves_old() {
        let mut obj = UnkfsDmuObject::new_file(1, 0);
        let old_bp = {
            let mut bp = UnkfsBlockPointer::null();
            bp.set_birth(10);
            bp
        };
        obj.cow_bp(old_bp, 20);
        assert_eq!(obj.birth_txg, 20, "birth_txg should be 20 after cow");
    }
}
