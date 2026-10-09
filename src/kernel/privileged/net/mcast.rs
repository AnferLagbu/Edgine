//! 组播组成员登记表 (P6 / D11 / DECISION-097)
//!
//! smoltcp 的 `Interface` 组表是**接口级**的 (`LinearMap<IpAddress, GroupState,
//! IFACE_MAX_MULTICAST_GROUP_COUNT>`), 不记录"哪个 socket 加入了该组"。直接把
//! `setsockopt(IP_ADD/DROP_MEMBERSHIP)` 透传给
//! `Interface::join/leave_multicast_group` 会造成错误语义: 两个 socket 加入
//! 同一组后, 任一 socket 的 `IP_DROP_MEMBERSHIP` 会把组对整个接口拆掉, 另一个
//! socket 随即失去组播流 —— 而 POSIX/Linux 的语义是**按 socket 引用计数**, 只有
//! 最后一个成员离开时才真正拆组。
//!
//! 本模块补齐这层引用计数, 是纯 bookkeeping (0 unsafe、不触硬件、可 host 侧单测):
//!
//! - `groups[i]` + `refcount[i]`: 接口级组表快照 (至多 [`MC_MAX_GROUPS`] 项),
//!   与 smoltcp 内部表一一对应.
//! - `slot_mask[slot]`: 该 slot 的组成员位图, bit *i* 置位表示它持有 `groups[i]`
//!   的成员资格.
//!
//! 与 smoltcp 的**调用次序约定** (由 [`super::net::init::raw`] 的包装函数遵守):
//! join 时先本表登记、再 `Interface::join_multicast_group`; 若后者失败必须调用
//! [`McastRegistry::unjoin`] 回滚, 否则本表会谎报"已入组"而 iface 未入组, 导致
//! 后续 join 跳过真实的 iface 调用而永久收不到流. leave 反之: 先本表递减、仅当
//! 引用归零才 `leave_multicast_group`.

use alloc::vec::Vec;

use smoltcp::wire::IpAddress;

/// 接口级组表容量. 必须与 smoltcp 配置的 `IFACE_MAX_MULTICAST_GROUP_COUNT` 一致 ——
/// 本表容量大于 smoltcp 时会在 `join` 处得到 `GroupTableFull`, 小于时则本表先满而
/// iface 仍有空位 (语义同样收敛到 `-E_NOMEM`, 但会提前拒绝合法请求).
///
/// 取 8 而非默认 4: 启用 smoltcp `multicast` feature 后, 它会在 `update_ip_addrs`
/// 路径为 IPv6 **自动 join** solicited-node 组, 与用户组播占用同一张 iface 表;
/// 预留余量避免 NDP 挤占用户配额 (见 `src/kernel/Cargo.toml` 的
/// `iface-max-multicast-group-count-8`).
pub const MC_MAX_GROUPS: usize = 8;

// per-slot 成员位图用 `u8`, 每个组占 1 bit —— 容量不得超 8, 否则静默丢高位.
const _: () = assert!(MC_MAX_GROUPS <= u8::BITS as usize);

/// 组成员操作结果.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McError {
    /// 该 slot 已是此组成员 (Linux `IP_ADD_MEMBERSHIP` 重复加入 → `EADDRINUSE`).
    AlreadyMember,
    /// 该 slot 不在此组成员中 (Linux `IP_DROP_MEMBERSHIP` 非成员 → `EADDRNOTAVAIL`).
    NotMember,
    /// 接口组表已满 ([`MC_MAX_GROUPS`] 用尽) → `-E_NOMEM`.
    TableFull,
    /// `slot` 超出登记表槽位数. 调用方的槽位均取自 `sm_slot`, 正常路径不可达,
    /// 但必须显式拒绝 —— 否则 `join` 会占下一个表项却永远无法释放 (该槽位的
    /// 成员位图读写均为 no-op, 引用计数无从归零).
    InvalidSlot,
}

/// 组播成员登记表: 接口级组表 + 每组成员引用计数 + per-slot 成员位图.
///
/// 字段均为私有, 只经本模块方法读写; 整体由 `NET_STATE` (`IrqSpinLock`) 保护,
/// 嵌于 `NetState` 内, 对外仅经 `privileged::net::init::raw` 的 safe accessor 暴露.
pub struct McastRegistry {
    /// 已登记的组地址 (与 smoltcp `Interface` 内部组表一一对应).
    groups: [Option<IpAddress>; MC_MAX_GROUPS],
    /// 各组的成员 socket 数; 归零即从 `groups` 摘除并请求 iface leave.
    refcount: [u8; MC_MAX_GROUPS],
    /// 每 slot 的组成员位图 (bit *i* = 持有 `groups[i]` 成员资格), len = 槽位数.
    slot_mask: Vec<u8>,
}

impl McastRegistry {
    /// 构造空登记表 (`slot_mask` 为空, 须由 [`Self::allocate`] 填充).
    pub const fn new() -> Self {
        Self {
            groups: [None; MC_MAX_GROUPS],
            refcount: [0; MC_MAX_GROUPS],
            slot_mask: Vec::new(),
        }
    }

    /// 逐元素填充 `slots` 项成员位图 (复位语义, 每次网络 (重) 初始化调用).
    ///
    /// 与 `NetState::allocate` 同款做法: 用 `Vec` 覆盖旧内容, 不构造大栈临时数组.
    pub fn allocate(&mut self, slots: usize) {
        self.groups = [None; MC_MAX_GROUPS];
        self.refcount = [0; MC_MAX_GROUPS];
        self.slot_mask = (0..slots).map(|_| 0u8).collect();
    }

    /// 登记 `slot` 对 `addr` 组的成员资格.
    ///
    /// 返回 `Ok(true)` 表示该组**首次**被登记, 调用方须接着请求
    /// `Interface::join_multicast_group(addr)` (失败时须调 [`Self::unjoin`] 回滚);
    /// `Ok(false)` 表示组已在表中 (其他 socket 已持有成员资格), 引用计数已递增,
    /// 无需再触发 iface 动作.
    ///
    /// # Errors
    ///
    /// - [`McError::InvalidSlot`]: `slot` 超出登记表槽位数 (拒于占表项前).
    /// - [`McError::AlreadyMember`]: 该 slot 已是此组成员.
    /// - [`McError::TableFull`]: 接口级组表已用尽且 `addr` 未登记.
    pub fn join(&mut self, slot: usize, addr: IpAddress) -> Result<bool, McError> {
        if self.slot_mask.get(slot).is_none() {
            return Err(McError::InvalidSlot);
        }
        let idx = if let Some(i) = self.find_group(addr) {
            i
        } else {
            let free = self
                .groups
                .iter()
                .position(Option::is_none)
                .ok_or(McError::TableFull)?;
            self.groups[free] = Some(addr);
            self.refcount[free] = 0;
            free
        };

        let bit = 1u8 << idx;
        let mask = self.mask_of(slot);
        if mask & bit != 0 {
            // 回滚本轮可能刚占用的空位 (重复加入同一组时不应留下幽灵条目).
            self.release_index_if_orphan(idx);
            return Err(McError::AlreadyMember);
        }

        self.set_mask(slot, mask | bit);
        self.refcount[idx] = self.refcount[idx].saturating_add(1);
        // 首次登记 = 该 index 的引用计数由 0 变 1.
        Ok(self.refcount[idx] == 1)
    }

    /// 撤销一次 [`Self::join`] (仅供 iface `join_multicast_group` 失败时回滚).
    pub fn unjoin(&mut self, slot: usize, addr: IpAddress) {
        let Some(idx) = self.find_group(addr) else {
            return;
        };
        let bit = 1u8 << idx;
        if self.mask_of(slot) & bit == 0 {
            return;
        }
        self.set_mask(slot, self.mask_of(slot) & !bit);
        self.refcount[idx] = self.refcount[idx].saturating_sub(1);
        self.release_index_if_orphan(idx);
    }

    /// 撤销 `slot` 对 `addr` 组的成员资格.
    ///
    /// 返回 `Ok(true)` 表示引用计数已归零, 调用方须请求
    /// `Interface::leave_multicast_group(addr)`; `Ok(false)` 表示仍有其他 socket
    /// 持有该组, iface 侧保持不动.
    ///
    /// # Errors
    ///
    /// [`McError::NotMember`]: 组未登记, 或 `slot` 不持有该组成员资格.
    pub fn leave(&mut self, slot: usize, addr: IpAddress) -> Result<bool, McError> {
        let Some(idx) = self.find_group(addr) else {
            return Err(McError::NotMember);
        };
        let bit = 1u8 << idx;
        if self.mask_of(slot) & bit == 0 {
            return Err(McError::NotMember);
        }
        self.set_mask(slot, self.mask_of(slot) & !bit);
        self.refcount[idx] = self.refcount[idx].saturating_sub(1);
        let dropped = self.refcount[idx] == 0;
        if dropped {
            self.groups[idx] = None;
        }
        Ok(dropped)
    }

    /// 取 `slot` 的组成员位图 (供 `close` 路径逐位拆离, 见 [`Self::force_leave`]).
    pub fn slot_bits(&self, slot: usize) -> u8 {
        self.mask_of(slot)
    }

    /// 取位图第 `idx` 位对应的组地址 (与 [`Self::slot_bits`] 配套遍历).
    pub fn addr_at(&self, idx: usize) -> Option<IpAddress> {
        self.groups.get(idx).copied().flatten()
    }

    /// `close` 专用: 无条件清除 `slot` 在 `idx` 组的成员资格.
    ///
    /// 返回 `true` 表示引用归零、调用方须 `leave_multicast_group`. 与
    /// [`Self::leave`] 的差别是不做地址匹配、不报 `NotMember` —— socket 销毁路径
    /// 不应因簿记不一致而失败.
    pub fn force_leave(&mut self, slot: usize, idx: usize) -> bool {
        let bit = match 1u8.checked_shl(idx as u32) {
            Some(b) if idx < MC_MAX_GROUPS => b,
            _ => return false,
        };
        if self.mask_of(slot) & bit == 0 {
            return false;
        }
        self.set_mask(slot, self.mask_of(slot) & !bit);
        self.refcount[idx] = self.refcount[idx].saturating_sub(1);
        let dropped = self.refcount[idx] == 0;
        if dropped {
            self.groups[idx] = None;
        }
        dropped
    }

    fn find_group(&self, addr: IpAddress) -> Option<usize> {
        self.groups.iter().position(|&g| g == Some(addr))
    }

    fn mask_of(&self, slot: usize) -> u8 {
        self.slot_mask.get(slot).copied().unwrap_or(0)
    }

    fn set_mask(&mut self, slot: usize, value: u8) {
        if let Some(m) = self.slot_mask.get_mut(slot) {
            *m = value;
        }
    }

    /// 引用归零时释放表项 (join 回滚与异常路径共用).
    fn release_index_if_orphan(&mut self, idx: usize) {
        if self.refcount.get(idx).copied() == Some(0) {
            self.groups[idx] = None;
        }
    }
}

impl Default for McastRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// host 侧单元测试 (`make test-kernel-host`): 覆盖引用计数全部分支
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const SLOT_A: usize = 3;
    const SLOT_B: usize = 7;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddress {
        IpAddress::v4(a, b, c, d)
    }

    fn registry() -> McastRegistry {
        let mut r = McastRegistry::new();
        r.allocate(16);
        r
    }

    #[test]
    fn first_join_requires_iface_action_second_does_not() {
        let mut r = registry();
        let g = v4(224, 0, 0, 1);
        // 首 socket: 表空 → 须 iface join.
        assert!(r.join(SLOT_A, g).unwrap());
        // 同组第二个 socket: 已在表 → 不再触发 iface 动作.
        assert!(!r.join(SLOT_B, g).unwrap());
        assert_eq!(r.addr_at(0), Some(g));
    }

    #[test]
    fn duplicate_join_on_same_slot_is_already_member() {
        let mut r = registry();
        let g = v4(232, 1, 2, 3);
        assert!(r.join(SLOT_A, g).unwrap());
        assert_eq!(r.join(SLOT_A, g), Err(McError::AlreadyMember));
        // 重复加入被拒后不得留下引用泄漏: 仍应只有一份成员资格.
        assert!(r.force_leave(SLOT_A, 0));
        assert_eq!(r.addr_at(0), None);
    }

    #[test]
    fn last_leave_is_what_reaches_iface() {
        let mut r = registry();
        let g = v4(239, 192, 1, 1);
        assert!(r.join(SLOT_A, g).unwrap());
        assert!(!r.join(SLOT_B, g).unwrap());
        // A 先走: B 仍在 → iface 不动.
        assert_eq!(r.leave(SLOT_A, g), Ok(false));
        assert_eq!(r.addr_at(0), Some(g));
        // B 是最后一个: 引用归零 → 须 iface leave.
        assert_eq!(r.leave(SLOT_B, g), Ok(true));
        assert_eq!(r.addr_at(0), None);
    }

    #[test]
    fn leave_without_membership_reports_not_member() {
        let mut r = registry();
        let g = v4(224, 0, 0, 5);
        assert!(r.join(SLOT_A, g).unwrap());
        // 从未加入的 slot.
        assert_eq!(r.leave(SLOT_B, g), Err(McError::NotMember));
        // 不存在的组.
        assert_eq!(r.leave(SLOT_A, v4(224, 0, 0, 9)), Err(McError::NotMember));
        // A 的成员资格未被误伤.
        assert_eq!(r.slot_bits(SLOT_A), 0b1);
    }

    #[test]
    fn table_full_after_max_groups() {
        let mut r = registry();
        for i in 0..MC_MAX_GROUPS {
            let g = v4(232, 0, 0, i as u8 + 1);
            assert!(r.join(SLOT_A, g).unwrap());
        }
        // 第 `MC_MAX_GROUPS + 1` 个组: 表满.
        assert_eq!(r.join(SLOT_A, v4(232, 0, 0, 99)), Err(McError::TableFull));
        // 已满时, 已登记的组仍可对其他 slot 正常加入 (不占新表项).
        assert!(!r.join(SLOT_B, v4(232, 0, 0, 1)).unwrap());
    }

    #[test]
    fn unjoin_rolls_back_join() {
        let mut r = registry();
        let g = v4(224, 0, 0, 13);
        assert!(r.join(SLOT_A, g).unwrap());
        // 模拟 iface join 失败的回滚: 表项与位图都应复位.
        r.unjoin(SLOT_A, g);
        assert_eq!(r.addr_at(0), None);
        assert_eq!(r.slot_bits(SLOT_A), 0);
        // 回滚后可重新登记为"首个".
        assert!(r.join(SLOT_A, g).unwrap());
    }

    #[test]
    fn close_releases_all_groups_of_slot_only_when_last() {
        let mut r = registry();
        let g1 = v4(232, 0, 0, 1);
        let g2 = v4(232, 0, 0, 2);
        assert!(r.join(SLOT_A, g1).unwrap());
        assert!(r.join(SLOT_A, g2).unwrap());
        // B 也加入 g1 (g1 还有另一个成员).
        assert!(!r.join(SLOT_B, g1).unwrap());

        // A 关闭: 逐位拆离.
        let bits = r.slot_bits(SLOT_A);
        let mut dropped = Vec::new();
        for idx in 0..MC_MAX_GROUPS {
            if bits & (1u8 << idx) == 0 {
                continue;
            }
            // 先取址再拆: `force_leave` 使引用归零时会清空 `groups[idx]`.
            let addr = r.addr_at(idx).expect("位图指向的表项必须存在");
            if r.force_leave(SLOT_A, idx) {
                dropped.push(addr);
            }
        }
        // g2 归零需 iface leave; g1 因 B 仍在而不该拆.
        assert_eq!(dropped, Vec::from([g2]));
        assert_eq!(r.slot_bits(SLOT_A), 0);
        assert_eq!(r.slot_bits(SLOT_B), 0b1);
    }

    #[test]
    fn out_of_range_slot_is_inert() {
        let mut r = registry();
        // 越界 slot 不得 panic, 也不得影响合法 slot.
        assert_eq!(r.slot_bits(9999), 0);
        assert!(!r.force_leave(9999, 0));
        assert_eq!(r.leave(9999, v4(224, 0, 0, 1)), Err(McError::NotMember));
        // 越界 join 必须被拒且**不占表项** (否则是不可回收的泄漏).
        assert_eq!(r.join(9999, v4(224, 0, 0, 1)), Err(McError::InvalidSlot));
        assert_eq!(r.addr_at(0), None);
        // 合法 slot 仍可正常使用.
        assert!(r.join(SLOT_A, v4(224, 0, 0, 1)).unwrap());
    }

    #[test]
    fn allocate_resets_prior_membership() {
        let mut r = registry();
        assert!(r.join(SLOT_A, v4(224, 0, 0, 1)).unwrap());
        r.allocate(16);
        assert_eq!(r.slot_bits(SLOT_A), 0);
        assert_eq!(r.addr_at(0), None);
    }

    #[test]
    fn ipv6_group_coexists_with_ipv4() {
        let mut r = registry();
        let v6 = IpAddress::v6(0xff02, 0, 0, 0, 0, 0, 0, 1);
        assert!(r.join(SLOT_A, v6).unwrap());
        assert!(r.join(SLOT_B, v4(224, 0, 0, 1)).unwrap());
        assert_eq!(r.leave(SLOT_A, v6), Ok(true));
        assert_eq!(r.leave(SLOT_B, v4(224, 0, 0, 1)), Ok(true));
    }
}
