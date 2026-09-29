#![deny(unsafe_code)]
//! Credo 持久化存储编排 (services 层)
//!
//! 二进制格式 v5: 头部 + 条目, 存储路径 `/pwm.db`, 支持从 v4 格式迁移.
//!
//! 本模块是原 `framework/credo/storage.rs` 的下沉落点:
//! - 序列化纯函数 (小端序 读/写 + 条目编解码) 归本模块;
//! - `save_database` / `load_database` / `remove_database` 编排归本模块;
//! - VFS 文件 I/O 走 [`crate::services::fs`] 提供的 safe 包装 (无 `unsafe`);
//! - 身份表读写走 `framework::credo::identity::IdentityTable` 的
//!   `valid_count` / `for_each_valid` / `load_entry` 访问面, 不直穿 TCB 原子字段.

use crate::framework::credo::identity::get_table;
use crate::framework::credo::types::{
    MAX_PWM_ENTRIES, PWM_HASH_LEN, PWM_NOTE_LEN, PwmEntrySnapshot,
};
use crate::services::fs::{
    vfs_close_safe, vfs_open_safe, vfs_read_safe, vfs_unlink_safe, vfs_write_safe,
};

/// 数据库文件路径
const DB_PATH: &str = "/pwm.db";
/// 数据库文件魔数
const DB_MAGIC: [u8; 4] = *b"PWID";
/// 数据库主版本号
const DB_VER_MAJOR: u16 = 5;
/// 数据库次版本号
const DB_VER_MINOR: u16 = 0;

/// 单条目 v5 磁盘尺寸(字节): pwm(8) + creator(8) + level(1) + flags(2) + caps(16×8) + note(64) + hash(48) + created(8) + expires(8).
const ENTRY_SZ: usize = 8 + 8 + 1 + 2 + 128 + PWM_NOTE_LEN + PWM_HASH_LEN + 8 + 8;
/// 单条目 v4 磁盘尺寸: note 段 128 字节, 且无 creator_pwm 字段.
const V4_ENTRY_SZ: usize = 8 + 1 + 2 + 128 + 128 + PWM_HASH_LEN + 8 + 8;
/// 头部尺寸: magic(4) + ver_major(2) + ver_minor(2) + count(4).
const HDR_SZ: usize = 4 + 2 + 2 + 4;
/// 序列化缓冲上限 (头部 + 全表条目).
const BUF_SZ: usize = 80000;

/// 只读打开标志
const O_RDONLY: u32 = 0x0001;
/// 只写打开标志
const O_WRONLY: u32 = 0x0002;
/// 不存在则创建标志
const O_CREAT: u32 = 0x0100;
/// 打开时截断标志
const O_TRUNC: u32 = 0x0200;

/// 按小端序写 u32 到 4 字节. `& 0xFF` 显式收窄避免 cast 警告.
fn w32(buf: &mut [u8], p: &mut usize, v: u32) {
    buf[*p] = (v & 0xFF) as u8;
    buf[*p + 1] = ((v >> 8) & 0xFF) as u8;
    buf[*p + 2] = ((v >> 16) & 0xFF) as u8;
    buf[*p + 3] = ((v >> 24) & 0xFF) as u8;
    *p += 4;
}
/// 按小端序写 u64 到 8 字节. 位移取每个字节, `& 0xFF` 显式收窄避免 cast 警告.
fn w64(buf: &mut [u8], p: &mut usize, v: u64) {
    for i in 0..8 {
        buf[*p + i] = ((v >> (i * 8)) & 0xFF) as u8;
    }
    *p += 8;
}
/// 按小端序写 u16 到 2 字节. `& 0xFF` 显式收窄避免 cast 警告.
fn w16(buf: &mut [u8], p: &mut usize, v: u16) {
    buf[*p] = (v & 0xFF) as u8;
    buf[*p + 1] = ((v >> 8) & 0xFF) as u8;
    *p += 2;
}
fn w8(buf: &mut [u8], p: &mut usize, v: u8) {
    buf[*p] = v;
    *p += 1;
}

fn r32(buf: &[u8], p: &mut usize) -> u32 {
    let v = u32::from(buf[*p])
        | u32::from(buf[*p + 1]) << 8
        | u32::from(buf[*p + 2]) << 16
        | u32::from(buf[*p + 3]) << 24;
    *p += 4;
    v
}
fn r64(buf: &[u8], p: &mut usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= u64::from(buf[*p + i]) << (i * 8);
    }
    *p += 8;
    v
}
fn r16(buf: &[u8], p: &mut usize) -> u16 {
    let v = u16::from(buf[*p]) | u16::from(buf[*p + 1]) << 8;
    *p += 2;
    v
}
fn r8(buf: &[u8], p: &mut usize) -> u8 {
    let v = buf[*p];
    *p += 1;
    v
}

/// 把条目快照按 v5 布局写入缓冲.
fn serialize(snap: &PwmEntrySnapshot, buf: &mut [u8], p: &mut usize) {
    w64(buf, p, snap.pwm);
    w64(buf, p, snap.creator_pwm);
    w8(buf, p, snap.privilege_level);
    w16(buf, p, snap.flags);
    for i in 0..16 {
        w64(buf, p, snap.caps[i]);
    }
    for i in 0..PWM_NOTE_LEN {
        buf[*p] = snap.note[i];
        *p += 1;
    }
    for i in 0..PWM_HASH_LEN {
        buf[*p] = snap.password_hash[i];
        *p += 1;
    }
    w64(buf, p, snap.created_time);
    w64(buf, p, snap.expires_at);
}

/// 从缓冲按 v5 布局解析条目快照; 空间不足返回 `None`.
fn deserialize(buf: &[u8], p: &mut usize) -> Option<PwmEntrySnapshot> {
    if *p + ENTRY_SZ > buf.len() {
        return None;
    }
    let pwm = r64(buf, p);
    let creator_pwm = r64(buf, p);
    let privilege_level = r8(buf, p);
    let flags = r16(buf, p);
    let mut caps = [0u64; 16];
    for i in 0..16 {
        caps[i] = r64(buf, p);
    }
    let mut note = [0u8; PWM_NOTE_LEN];
    note.copy_from_slice(&buf[*p..*p + PWM_NOTE_LEN]);
    *p += PWM_NOTE_LEN;
    let mut password_hash = [0u8; PWM_HASH_LEN];
    password_hash.copy_from_slice(&buf[*p..*p + PWM_HASH_LEN]);
    *p += PWM_HASH_LEN;
    let created_time = r64(buf, p);
    let expires_at = r64(buf, p);
    Some(PwmEntrySnapshot {
        pwm,
        creator_pwm,
        privilege_level,
        flags,
        caps,
        note,
        password_hash,
        created_time,
        expires_at,
    })
}

/// 从缓冲按 v4 布局解析条目快照; 空间不足返回 `None`.
///
/// v4 语义: `note` 段 128 字节截断为前 63 字节 (末字节置 0), 且无 `creator_pwm`
/// (默认 0).
fn deserialize_v4(buf: &[u8], p: &mut usize) -> Option<PwmEntrySnapshot> {
    if *p + V4_ENTRY_SZ > buf.len() {
        return None;
    }
    let pwm = r64(buf, p);
    let privilege_level = r8(buf, p);
    let flags = r16(buf, p);
    let mut caps = [0u64; 16];
    for i in 0..16 {
        caps[i] = r64(buf, p);
    }
    let mut v4_note = [0u8; 128];
    v4_note.copy_from_slice(&buf[*p..*p + 128]);
    *p += 128;
    let mut password_hash = [0u8; PWM_HASH_LEN];
    password_hash.copy_from_slice(&buf[*p..*p + PWM_HASH_LEN]);
    *p += PWM_HASH_LEN;
    let created_time = r64(buf, p);
    let expires_at = r64(buf, p);

    // v4 note 截断: 取前 PWM_NOTE_LEN-1 字节, 末字节置 0 (保持 v4 迁移语义).
    let mut note = [0u8; PWM_NOTE_LEN];
    let trunc = 128_usize.min(PWM_NOTE_LEN - 1);
    note[..trunc].copy_from_slice(&v4_note[..trunc]);
    note[PWM_NOTE_LEN - 1] = 0;

    Some(PwmEntrySnapshot {
        pwm,
        creator_pwm: 0,
        privilege_level,
        flags,
        caps,
        note,
        password_hash,
        created_time,
        expires_at,
    })
}

/// 保存身份表到磁盘 (`/pwm.db`).
///
/// 未修改时直接返回 `0`; 写入成功返回 `0`, IO 失败或缓冲不足返回 `-1`.
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::large_stack_arrays,
    reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
)]
pub fn save_database() -> i32 {
    let t = get_table();
    if !t.is_modified() {
        return 0;
    }

    let n = t.valid_count();
    let sz = HDR_SZ + n * ENTRY_SZ;
    let mut buf = [0u8; BUF_SZ];
    if sz > buf.len() {
        return -1;
    }

    let mut p: usize = 0;
    buf[p] = DB_MAGIC[0];
    buf[p + 1] = DB_MAGIC[1];
    buf[p + 2] = DB_MAGIC[2];
    buf[p + 3] = DB_MAGIC[3];
    p += 4;
    w16(&mut buf, &mut p, DB_VER_MAJOR);
    w16(&mut buf, &mut p, DB_VER_MINOR);
    w32(&mut buf, &mut p, n as u32);

    t.for_each_valid(|snap| {
        serialize(snap, &mut buf, &mut p);
    });

    let fd = vfs_open_safe(DB_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0);
    if fd < 0 {
        return -1;
    }

    let written = vfs_write_safe(fd as u32, &buf[..sz]);
    vfs_close_safe(fd as u32);
    if written as usize != sz {
        return -1;
    }

    t.clear_modified();
    0
}

/// 从磁盘加载身份表 (`/pwm.db`).
///
/// 文件不存在返回 `0` (视为空库); 校验失败/IO 失败返回 `-1`.
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::large_stack_arrays,
    reason = "large_stack_arrays: 大栈数组是性能权衡 (避免堆分配); 当前优先 expect"
)]
pub fn load_database() -> i32 {
    let fd = vfs_open_safe(DB_PATH, O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }

    let mut hdr = [0u8; HDR_SZ];
    let rd = vfs_read_safe(fd as u32, &mut hdr);
    if rd < HDR_SZ as i32 {
        vfs_close_safe(fd as u32);
        return -1;
    }

    if hdr[0] != DB_MAGIC[0]
        || hdr[1] != DB_MAGIC[1]
        || hdr[2] != DB_MAGIC[2]
        || hdr[3] != DB_MAGIC[3]
    {
        vfs_close_safe(fd as u32);
        return -1;
    }

    let mut hp: usize = 4;
    let vmaj = r16(&hdr, &mut hp);
    let _vmin = r16(&hdr, &mut hp);
    let count = r32(&hdr, &mut hp) as usize;
    if count == 0 || count > MAX_PWM_ENTRIES {
        vfs_close_safe(fd as u32);
        return -1;
    }

    let ds = count * if vmaj < 5 { V4_ENTRY_SZ } else { ENTRY_SZ };
    // 安全切片边界: v4 大 count 时 `ds` 可超缓冲, 返回 -1 (对齐 save 路径的守护).
    if ds > BUF_SZ {
        vfs_close_safe(fd as u32);
        return -1;
    }
    let mut data = [0u8; BUF_SZ];
    let dr = vfs_read_safe(fd as u32, &mut data[..ds]);
    vfs_close_safe(fd as u32);
    if dr < ds as i32 {
        return -1;
    }

    let t = get_table();
    let mut p: usize = 0;
    for _ in 0..count {
        let snap = if vmaj < 5 {
            deserialize_v4(&data, &mut p)
        } else {
            deserialize(&data, &mut p)
        };
        if let Some(snap) = snap {
            t.load_entry(&snap);
        }
    }

    // 交叉验证: 加载后条目计数应与头部声明的 count 一致
    debug_assert_eq!(
        t.valid_count(),
        count,
        "Credo 存储加载: 条目计数不一致 (header={}, loaded={})",
        count,
        t.valid_count(),
    );
    t.clear_modified();
    0
}

/// 从磁盘删除身份表文件 (`/pwm.db`).
pub fn remove_database() -> i32 {
    vfs_unlink_safe(DB_PATH, 0)
}
