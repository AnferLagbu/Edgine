//! PCI / `PCIe` 热插拔检测与事件源
//!
//! 基于 PCI Express 规范的 Slot Capability / Slot Status 寄存器
//! 检测设备插入/移除事件，不依赖操作系统特定的 ACPI SHPC。

use super::{read_config_byte, read_config_dword, read_config_word};
use alloc::vec::Vec;

// ── PCI Capability IDs ──

const PCI_CAP_ID_PCIE: u8 = 0x10;

// ── PCI Express Capability 寄存器偏移 ──

const PCIE_CAP_REG: u8 = 0x02;
const _PCIE_DEVCAP: u8 = 0x04;
const _PCIE_LINKCAP: u8 = 0x0C;
const _PCIE_LINKSTS: u8 = 0x12;
const PCIE_SLOTCAP: u8 = 0x14;
const _PCIE_SLOTCTL: u8 = 0x18;
const PCIE_SLOTSTS: u8 = 0x1A;

// ── Slot Capabilities 位定义 (PCIE_SLOTCAP) ──

const SLOTCAP_ATTN_BTN: u32 = 1 << 0;
const SLOTCAP_PWR_CTRL: u32 = 1 << 1;
const SLOTCAP_MRL_SENSOR: u32 = 1 << 2;
const _SLOTCAP_ATTN_IND: u32 = 1 << 3;
const _SLOTCAP_PWR_IND: u32 = 1 << 4;
const SLOTCAP_HOTPLUG_SURPRISE: u32 = 1 << 5;
const SLOTCAP_HOTPLUG: u32 = 1 << 6;
const SLOTCAP_SLOT_NUM_MASK: u32 = 0x00007F80;
const SLOTCAP_SLOT_NUM_SHIFT: u32 = 7;

// ── Slot Status 位定义 (PCIE_SLOTSTS) ──

const _SLOTSTS_ATTN_BTN: u16 = 1 << 0;
const _SLOTSTS_PWR_FAULT: u16 = 1 << 1;
const _SLOTSTS_MRL_CHANGED: u16 = 1 << 2;
const SLOTSTS_PRESENCE_DETECT: u16 = 1 << 3;
const _SLOTSTS_COMMAND_COMPLETED: u16 = 1 << 4;
const SLOTSTS_PRESENCE_STATE: u16 = 1 << 6;
const SLOTSTS_DLL_STATE_CHANGED: u16 = 1 << 7;

/// `PCIe` 端口类型 (Capability Register bits 7:4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciePortType {
    PcieEndpoint = 0,
    LegacyEndpoint = 1,
    RootPort = 4,
    UpstreamPort = 5,
    DownstreamPort = 6,
    PcieToPciBridge = 7,
}

impl PciePortType {
    pub fn from_cap_reg(val: u16) -> Option<Self> {
        match (val >> 4) & 0xF {
            0 => Some(Self::PcieEndpoint),
            1 => Some(Self::LegacyEndpoint),
            4 => Some(Self::RootPort),
            5 => Some(Self::UpstreamPort),
            6 => Some(Self::DownstreamPort),
            7 => Some(Self::PcieToPciBridge),
            _ => None,
        }
    }

    pub fn is_hotplug_capable(self) -> bool {
        matches!(self, Self::RootPort | Self::DownstreamPort)
    }
}

/// `PCIe` 热插拔槽位描述
#[derive(Debug, Clone)]
pub struct PcieHotplugSlot {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub slot_number: u8,
    pub port_type: PciePortType,
    pub attn_button: bool,
    pub power_controller: bool,
    pub mrl_sensor: bool,
    pub hotplug_capable: bool,
    pub surprise_removal: bool,
    pub presence_state: bool,
    pready_events: u16,
}

impl PcieHotplugSlot {
    /// 查询指定 PCI 设备的 PCI Express Capability 偏移。
    pub fn find_pcie_cap(bus: u8, dev: u8, func: u8) -> Option<u8> {
        let cap_ptr = read_config_byte(bus, dev, func, 0x34);
        if cap_ptr == 0 {
            return None;
        }

        let mut ptr = cap_ptr;
        for _ in 0..48 {
            let cap_id = read_config_byte(bus, dev, func, ptr);
            if cap_id == PCI_CAP_ID_PCIE {
                return Some(ptr);
            }
            let next = read_config_byte(bus, dev, func, ptr + 1);
            if next == 0 || next < 0x40 {
                break;
            }
            ptr = next;
        }
        None
    }

    /// 尝试从 PCI 设备检测热插拔槽位能力。
    /// 返回 Some(PcieHotplugSlot) 如果设备是支持热插拔的 Root/Downstream Port。
    pub fn probe(bus: u8, dev: u8, func: u8) -> Option<Self> {
        let cap_off = Self::find_pcie_cap(bus, dev, func)?;

        let cap_reg = read_config_word(bus, dev, func, cap_off + PCIE_CAP_REG);
        let port_type = PciePortType::from_cap_reg(cap_reg)?;
        if !port_type.is_hotplug_capable() {
            return None;
        }

        let slotcap = read_config_dword(bus, dev, func, cap_off + PCIE_SLOTCAP);
        let slotsts = read_config_word(bus, dev, func, cap_off + PCIE_SLOTSTS);

        let slot_number = ((slotcap & SLOTCAP_SLOT_NUM_MASK) >> SLOTCAP_SLOT_NUM_SHIFT) as u8;

        Some(Self {
            bus,
            device: dev,
            function: func,
            slot_number,
            port_type,
            attn_button: slotcap & SLOTCAP_ATTN_BTN != 0,
            power_controller: slotcap & SLOTCAP_PWR_CTRL != 0,
            mrl_sensor: slotcap & SLOTCAP_MRL_SENSOR != 0,
            hotplug_capable: slotcap & SLOTCAP_HOTPLUG != 0,
            surprise_removal: slotcap & SLOTCAP_HOTPLUG_SURPRISE != 0,
            presence_state: slotsts & SLOTSTS_PRESENCE_STATE != 0,
            pready_events: slotsts & 0x3F,
        })
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 读取 Slot Status 寄存器，返回变化事件位掩码。
    /// 自动清除已读事件。
    pub fn read_and_clear_events(&mut self) -> u16 {
        let cap_off = match Self::find_pcie_cap(self.bus, self.device, self.function) {
            Some(off) => off,
            None => return 0,
        };

        let slotsts =
            read_config_word(self.bus, self.device, self.function, cap_off + PCIE_SLOTSTS);

        let changed = slotsts & !self.pready_events;
        self.pready_events = slotsts & 0x3F;

        // 检查 PresDet 变化: 设备插入或移除
        if slotsts & SLOTSTS_PRESENCE_DETECT != 0 {
            self.presence_state = slotsts & SLOTSTS_PRESENCE_STATE != 0;
        }

        changed
    }

    /// 是否有设备插入事件
    pub fn has_insertion_event(&self, events: u16) -> bool {
        events & SLOTSTS_PRESENCE_DETECT != 0 && self.presence_state
    }

    /// 是否有设备移除事件
    pub fn has_removal_event(&self, events: u16) -> bool {
        events & SLOTSTS_PRESENCE_DETECT != 0 && !self.presence_state
    }

    /// 是否为意外拔出
    pub fn has_surprise_removal(&self, events: u16) -> bool {
        events & SLOTSTS_DLL_STATE_CHANGED != 0 && !self.presence_state
    }
}

/// 扫描所有 PCI 总线，返回支持热插拔的槽位列表。
pub fn scan_hotplug_slots() -> Vec<PcieHotplugSlot> {
    let devices = super::scan_all_buses();
    let mut slots = Vec::new();

    for dev in &devices {
        if let Some(slot) = PcieHotplugSlot::probe(dev.bus, dev.device, dev.function) {
            slots.push(slot);
        }
    }

    slots
}
