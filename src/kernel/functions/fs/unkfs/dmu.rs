#![deny(unsafe_code)]
use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
use crate::functions::fs::unkfs::spa::HV_POOL_BLOCK_SIZE;
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const HV_DMU_OBJ_NUM: u64 = 0;
pub const HV_DMU_OBJ_META: u64 = 1;
pub const HV_DMU_OBJ_ROOT: u64 = 2;
pub const HV_DMU_MAX_BLOCKPTR: usize = 16;
pub const HV_DMU_MAX_NAME: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsObjType {
    None = 0,
    File = 1,
    Dir = 2,
    Snapshot = 3,
    Zap = 4,
    ZapMicro = 5,
    Volume = 6,
    SpaceMap = 7,
    ObjSet = 8,
    Symlink = 9,
}

impl UnkfsObjType {
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
pub struct UnkfsDmuObject {
    pub obj_id: u64,
    pub obj_type: UnkfsObjType,
    pub block_size: u32,
    pub nblocks: u64,
    pub size: u64,
    pub bp: UnkfsBlockPointer,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub owner_pwm: u64,
    pub group_pwm: u64,
    pub sensitivity: u8,
    pub pwm_perm: u16,
    pub link_count: u32,
    pub flags: u32,
    pub birth_txg: u64,
    pub data_hash: [u64; 4],
    pub fill: u64,
    pub dirty: bool,
    pub used: bool,
}

impl UnkfsDmuObject {
    pub fn new_file(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::File,
            block_size: HV_POOL_BLOCK_SIZE as u32,
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

    pub fn new_dir(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Dir,
            block_size: HV_POOL_BLOCK_SIZE as u32,
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

    pub fn new_zap(obj_id: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Zap,
            block_size: HV_POOL_BLOCK_SIZE as u32,
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

    pub fn new_symlink(obj_id: u64, owner_pwm: u64) -> Self {
        Self {
            obj_id,
            obj_type: UnkfsObjType::Symlink,
            block_size: HV_POOL_BLOCK_SIZE as u32,
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

    pub fn is_file(&self) -> bool {
        self.obj_type == UnkfsObjType::File
    }
    pub fn is_dir(&self) -> bool {
        self.obj_type == UnkfsObjType::Dir
    }
    pub fn is_zap(&self) -> bool {
        self.obj_type == UnkfsObjType::Zap || self.obj_type == UnkfsObjType::ZapMicro
    }
    pub fn is_snapshot(&self) -> bool {
        self.obj_type == UnkfsObjType::Snapshot
    }

    pub fn mark_dirty(&mut self, txg: u64) {
        self.dirty = true;
        self.birth_txg = txg;
    }

    pub fn cow_bp(&mut self, new_bp: UnkfsBlockPointer, txg: u64) {
        self.bp = new_bp;
        self.birth_txg = txg;
        self.dirty = true;
    }
}

pub struct UnkfsObjSet {
    pub objects: Mutex<Vec<UnkfsDmuObject>>,
    pub next_obj_id: AtomicU64,
    pub root_obj: u64,
    pub initialized: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): UnkfsObjSet 全部字段 (Mutex<T>, Atomic*, Vec) 自动 Send + Sync。

impl UnkfsObjSet {
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(Vec::new()),
            next_obj_id: AtomicU64::new(HV_DMU_OBJ_ROOT + 1),
            root_obj: HV_DMU_OBJ_ROOT,
            initialized: AtomicBool::new(false),
        }
    }

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

    pub fn get_obj(&self, obj_id: u64) -> Option<UnkfsDmuObject> {
        let objs = self.objects.lock();
        objs.iter().find(|o| o.obj_id == obj_id && o.used).copied()
    }

    pub fn get_obj_mut(&self, obj_id: u64) -> Option<UnkfsDmuObject> {
        self.get_obj(obj_id)
    }

    pub fn update_obj(&self, obj: &UnkfsDmuObject) -> bool {
        let mut objs = self.objects.lock();
        objs.iter_mut()
            .find(|o| o.obj_id == obj.obj_id)
            .map_or(false, |existing| {
                *existing = *obj;
                true
            })
    }

    pub fn get_root(&self) -> Option<UnkfsDmuObject> {
        self.get_obj(self.root_obj)
    }

    pub fn obj_count(&self) -> u64 {
        self.objects.lock().iter().filter(|o| o.used).count() as u64
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
