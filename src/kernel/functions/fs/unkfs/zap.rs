#![deny(unsafe_code)]
//! UNKFS ZAP 键值存储 — 属性名到字节值的哈希表
//!
//! 提供 `ZFS` ZAP (ZFS Attribute Processor) 的语义子集: 定长条目槽位 +
//! 名字哈希加速查找, 支持插入 / 覆盖 / 查询 / 删除与容量上限.
#![deny(missing_docs)]
use crate::functions::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::string::String;
use alloc::vec::Vec;

/// 条目名的最大字节数 (定长槽位容量, 超长部分截断)
pub const HV_ZAP_MAX_NAME: usize = 64;
/// 条目值的最大字节数 (定长槽位容量, 超长部分截断)
pub const HV_ZAP_MAX_VALUE: usize = 128;
/// ZAP 表的默认条目容量上限
pub const HV_ZAP_MAX_ENTRIES: usize = 256;

/// 单个 ZAP 条目: 定长名字与值槽位 + 预计算的名字哈希
#[derive(Debug, Clone)]
pub struct UnkfsZapEntry {
    /// 条目名 (定长字节数组, 未使用部分为 0)
    pub name: [u8; HV_ZAP_MAX_NAME],
    /// 条目值 (定长字节数组)
    pub value: [u8; HV_ZAP_MAX_VALUE],
    /// 值的有效字节数
    pub value_len: u16,
    /// 条目名的 FNV-1a 哈希 (加速查找)
    pub hash: u64,
    /// 条目是否被占用
    pub used: bool,
}

impl UnkfsZapEntry {
    /// 由名字与字节值构造条目 (超出定长槽位的部分截断)
    #[must_use]
    pub fn new(name: &str, value: &[u8]) -> Self {
        let mut n = [0u8; HV_ZAP_MAX_NAME];
        let mut v = [0u8; HV_ZAP_MAX_VALUE];
        let nlen = name.len().min(HV_ZAP_MAX_NAME);
        let vlen = value.len().min(HV_ZAP_MAX_VALUE);
        n[..nlen].copy_from_slice(&name.as_bytes()[..nlen]);
        v[..vlen].copy_from_slice(&value[..vlen]);
        let hash = Self::hash_name(name);
        Self {
            name: n,
            value: v,
            value_len: crate::functions::intcast::low_u16_from_usize(vlen),
            hash,
            used: true,
        }
    }

    /// 以 `&str` 形式返回条目名 (非 UTF-8 时退化为空串)
    #[must_use]
    pub fn get_name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(HV_ZAP_MAX_NAME);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    /// 返回值的有效字节切片
    #[must_use]
    pub fn get_value(&self) -> &[u8] {
        &self.value[..self.value_len as usize]
    }

    /// 将值按小端解释为 `u64` (不足 8 字节返回 0)
    #[must_use]
    pub fn get_value_u64(&self) -> u64 {
        if self.value_len as usize >= 8 {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&self.value[..8]);
            u64::from_le_bytes(arr)
        } else {
            0
        }
    }

    /// 计算名字的 FNV-1a 64 位哈希
    #[must_use]
    pub fn hash_name(name: &str) -> u64 {
        let mut h: u64 = 14_695_981_039_346_656_037;
        for &b in name.as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1_099_511_628_211);
        }
        h
    }
}

/// ZAP 键值表: 受锁保护的条目集合, 带容量上限与表类型
pub struct UnkfsZap {
    /// 受自旋锁保护的条目列表
    pub entries: Mutex<Vec<UnkfsZapEntry>>,
    /// 条目容量上限
    pub capacity: usize,
    /// 表类型 (micro / normal / leaf)
    pub zap_type: UnkfsZapType,
}

/// ZAP 表类型 (决定容量与槽位布局策略)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsZapType {
    /// 微表: 小容量 (≤64 条目)
    Micro = 0,
    /// 普通表
    Normal = 1,
    /// 叶子表 (供更大规模索引使用)
    Leaf = 2,
}

// SAFETY (Framekernel P2.2.2): UnkfsZap 全部字段 (Mutex<T>, AtomicU64) 自动 Send + Sync。

impl UnkfsZap {
    /// 创建默认容量 (`HV_ZAP_MAX_ENTRIES`) 的 micro 表
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            capacity: HV_ZAP_MAX_ENTRIES,
            zap_type: UnkfsZapType::Micro,
        }
    }

    /// 按指定容量创建表 (≤64 为 micro, 否则 normal)
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            capacity,
            zap_type: if capacity <= 64 {
                UnkfsZapType::Micro
            } else {
                UnkfsZapType::Normal
            },
        }
    }

    /// 插入条目 (同名则覆盖), 超出容量上限返回 false
    pub fn insert(&self, name: &str, value: &[u8]) -> bool {
        let mut entries = self.entries.lock();
        if entries.len() >= self.capacity {
            return false;
        }
        let hash = UnkfsZapEntry::hash_name(name);
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.used && e.hash == hash && e.get_name() == name)
        {
            let vlen = value.len().min(HV_ZAP_MAX_VALUE);
            existing.value[..vlen].copy_from_slice(&value[..vlen]);
            existing.value_len = crate::functions::intcast::low_u16_from_usize(vlen);
            return true;
        }
        entries.push(UnkfsZapEntry::new(name, value));
        true
    }

    /// 以小端 `u64` 值插入条目
    pub fn insert_u64(&self, name: &str, value: u64) -> bool {
        self.insert(name, &value.to_le_bytes())
    }

    /// 按名查询条目值 (不存在返回 `None`)
    pub fn lookup(&self, name: &str) -> Option<Vec<u8>> {
        let entries = self.entries.lock();
        let hash = UnkfsZapEntry::hash_name(name);
        for entry in entries.iter() {
            if entry.used && entry.hash == hash && entry.get_name() == name {
                return Some(entry.get_value().to_vec());
            }
        }
        None
    }

    /// 按名查询 `u64` 值 (不存在返回 `None`)
    pub fn lookup_u64(&self, name: &str) -> Option<u64> {
        let entries = self.entries.lock();
        let hash = UnkfsZapEntry::hash_name(name);
        for entry in entries.iter() {
            if entry.used && entry.hash == hash && entry.get_name() == name {
                return Some(entry.get_value_u64());
            }
        }
        None
    }

    /// 删除指定名字的条目 (不存在返回 false)
    pub fn remove(&self, name: &str) -> bool {
        let mut entries = self.entries.lock();
        let hash = UnkfsZapEntry::hash_name(name);
        let idx = entries
            .iter()
            .position(|e| e.used && e.hash == hash && e.get_name() == name);
        idx.is_some_and(|i| {
            entries.remove(i);
            true
        })
    }

    /// 判断指定名字的条目是否存在
    pub fn contains(&self, name: &str) -> bool {
        let entries = self.entries.lock();
        let hash = UnkfsZapEntry::hash_name(name);
        entries
            .iter()
            .any(|e| e.used && e.hash == hash && e.get_name() == name)
    }

    /// 返回当前条目数
    pub fn len(&self) -> usize {
        self.entries.lock().iter().filter(|e| e.used).count()
    }

    /// 表是否为空 (无条目)
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 返回全部条目名
    pub fn keys(&self) -> Vec<String> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| e.used)
            .map(|e| String::from(e.get_name()))
            .collect()
    }

    /// 返回全部 (名字, 值) 对
    pub fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| e.used)
            .map(|e| (String::from(e.get_name()), e.get_value().to_vec()))
            .collect()
    }

    /// 清空全部条目
    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}

impl Default for UnkfsZap {
    /// 委托 [`UnkfsZap::new`] 创建默认容量的 micro 表.
    fn default() -> Self {
        Self::new()
    }
}

// DECISION-080: ZAP 纯逻辑断言 (插入/查询/删除/容量) 以本文件源侧 #[cfg(test)] 为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 基础插入 / 查询 ====

    /// 插入后应能按名查回同一值.
    #[test]
    fn test_zap_insert_lookup() {
        let zap = UnkfsZap::new();
        zap.insert_u64("key1", 42);
        let Some(val) = zap.lookup_u64("key1") else {
            panic!("key1 not found");
        };
        assert_eq!(val, 42, "value mismatch");
    }

    /// 重复插入同名键应覆盖旧值.
    #[test]
    fn test_zap_overwrite() {
        let zap = UnkfsZap::new();
        zap.insert_u64("key1", 10);
        zap.insert_u64("key1", 99);
        let Some(val) = zap.lookup_u64("key1") else {
            panic!("key1 not found after overwrite");
        };
        assert_eq!(val, 99, "overwrite should set 99");
    }

    /// 查询不存在的键应返回 None.
    #[test]
    fn test_zap_nonexistent() {
        let zap = UnkfsZap::new();
        assert!(
            zap.lookup_u64("no_such_key").is_none(),
            "nonexistent should be None"
        );
    }

    // ==== 删除 / 容量 / 清空 ====

    /// remove 后键应不可查, 删除前应可查.
    #[test]
    fn test_zap_remove() {
        let zap = UnkfsZap::new();
        zap.insert_u64("rm_me", 7);
        assert!(
            zap.lookup_u64("rm_me").is_some(),
            "should exist before remove"
        );
        zap.remove("rm_me");
        assert!(
            zap.lookup_u64("rm_me").is_none(),
            "should not exist after remove"
        );
    }

    /// 大批量键插入后应全部可查回.
    #[test]
    fn test_zap_large_namespace() {
        let zap = UnkfsZap::with_capacity(64);
        for i in 0..30u64 {
            let key = alloc::format!("key_{i}");
            zap.insert_u64(&key, i * 100);
        }
        assert_eq!(zap.len(), 30, "zap should have 30 entries");

        for i in 0..30u64 {
            let key = alloc::format!("key_{i}");
            let Some(val) = zap.lookup_u64(&key) else {
                panic!("key not found");
            };
            assert_eq!(val, i * 100, "value mismatch");
        }
    }

    /// contains 与 clear 语义应一致.
    #[test]
    fn test_zap_contains_clear() {
        let zap = UnkfsZap::new();
        zap.insert_u64("test", 42);
        assert!(zap.contains("test"), "should contain test");
        assert!(!zap.contains("other"), "should not contain other");
        assert!(!zap.is_empty(), "should not be empty");

        zap.clear();
        assert!(zap.is_empty(), "should be empty after clear");
        assert!(!zap.contains("test"), "should not contain after clear");
    }
}
