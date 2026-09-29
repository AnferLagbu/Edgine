#![deny(unsafe_code)]
use crate::services::sync::irq_lock::IrqSpinLock as Mutex;
use alloc::string::String;
use alloc::vec::Vec;

pub const HV_ZAP_MAX_NAME: usize = 64;
pub const HV_ZAP_MAX_VALUE: usize = 128;
pub const HV_ZAP_MAX_ENTRIES: usize = 256;

#[derive(Debug, Clone)]
pub struct NestZapEntry {
    pub name: [u8; HV_ZAP_MAX_NAME],
    pub value: [u8; HV_ZAP_MAX_VALUE],
    pub value_len: u16,
    pub hash: u64,
    pub used: bool,
}

impl NestZapEntry {
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
            value_len: vlen as u16,
            hash,
            used: true,
        }
    }

    pub fn get_name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(HV_ZAP_MAX_NAME);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    pub fn get_value(&self) -> &[u8] {
        &self.value[..self.value_len as usize]
    }

    pub fn get_value_u64(&self) -> u64 {
        if self.value_len as usize >= 8 {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&self.value[..8]);
            u64::from_le_bytes(arr)
        } else {
            0
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    pub fn hash_name(name: &str) -> u64 {
        let mut h: u64 = 14695981039346656037;
        for &b in name.as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(1099511628211);
        }
        h
    }
}

pub struct NestZap {
    pub entries: Mutex<Vec<NestZapEntry>>,
    pub capacity: usize,
    pub zap_type: NestZapType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NestZapType {
    Micro = 0,
    Normal = 1,
    Leaf = 2,
}

// SAFETY (Framekernel P2.2.2): NestZap 全部字段 (Mutex<T>, AtomicU64) 自动 Send + Sync。

impl NestZap {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            capacity: HV_ZAP_MAX_ENTRIES,
            zap_type: NestZapType::Micro,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            capacity,
            zap_type: if capacity <= 64 {
                NestZapType::Micro
            } else {
                NestZapType::Normal
            },
        }
    }

    pub fn insert(&self, name: &str, value: &[u8]) -> bool {
        let mut entries = self.entries.lock();
        if entries.len() >= self.capacity {
            return false;
        }
        let hash = NestZapEntry::hash_name(name);
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.used && e.hash == hash && e.get_name() == name)
        {
            let vlen = value.len().min(HV_ZAP_MAX_VALUE);
            existing.value[..vlen].copy_from_slice(&value[..vlen]);
            existing.value_len = vlen as u16;
            return true;
        }
        entries.push(NestZapEntry::new(name, value));
        true
    }

    pub fn insert_u64(&self, name: &str, value: u64) -> bool {
        self.insert(name, &value.to_le_bytes())
    }

    pub fn lookup(&self, name: &str) -> Option<Vec<u8>> {
        let entries = self.entries.lock();
        let hash = NestZapEntry::hash_name(name);
        for entry in entries.iter() {
            if entry.used && entry.hash == hash && entry.get_name() == name {
                return Some(entry.get_value().to_vec());
            }
        }
        None
    }

    pub fn lookup_u64(&self, name: &str) -> Option<u64> {
        let entries = self.entries.lock();
        let hash = NestZapEntry::hash_name(name);
        for entry in entries.iter() {
            if entry.used && entry.hash == hash && entry.get_name() == name {
                return Some(entry.get_value_u64());
            }
        }
        None
    }

    pub fn remove(&self, name: &str) -> bool {
        let mut entries = self.entries.lock();
        let hash = NestZapEntry::hash_name(name);
        let idx = entries
            .iter()
            .position(|e| e.used && e.hash == hash && e.get_name() == name);
        idx.map_or(false, |i| {
            entries.remove(i);
            true
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        let entries = self.entries.lock();
        let hash = NestZapEntry::hash_name(name);
        entries
            .iter()
            .any(|e| e.used && e.hash == hash && e.get_name() == name)
    }

    pub fn len(&self) -> usize {
        self.entries.lock().iter().filter(|e| e.used).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn keys(&self) -> Vec<String> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| e.used)
            .map(|e| String::from(e.get_name()))
            .collect()
    }

    pub fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| e.used)
            .map(|e| (String::from(e.get_name()), e.get_value().to_vec()))
            .collect()
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
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
        let zap = NestZap::new();
        zap.insert_u64("key1", 42);
        let Some(val) = zap.lookup_u64("key1") else {
            panic!("key1 not found");
        };
        assert_eq!(val, 42, "value mismatch");
    }

    /// 重复插入同名键应覆盖旧值.
    #[test]
    fn test_zap_overwrite() {
        let zap = NestZap::new();
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
        let zap = NestZap::new();
        assert!(
            zap.lookup_u64("no_such_key").is_none(),
            "nonexistent should be None"
        );
    }

    // ==== 删除 / 容量 / 清空 ====

    /// remove 后键应不可查, 删除前应可查.
    #[test]
    fn test_zap_remove() {
        let zap = NestZap::new();
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
        let zap = NestZap::with_capacity(64);
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
        let zap = NestZap::new();
        zap.insert_u64("test", 42);
        assert!(zap.contains("test"), "should contain test");
        assert!(!zap.contains("other"), "should not contain other");
        assert!(!zap.is_empty(), "should not be empty");

        zap.clear();
        assert!(zap.is_empty(), "should be empty after clear");
        assert!(!zap.contains("test"), "should not contain after clear");
    }
}
