//! 扁平设备树 (Flattened Device Tree, FDT/DTB) 最小解析器
//!
//! 为 aarch64 真机/SoC 移植提供运行时硬件探测能力: 从引导程序
//! (QEMU `-kernel` / U-Boot) 传入的 DTB 中提取内存 (memory)、串口 (UART)
//! 与中断控制器 (GICv3) 的物理基址, 使内核无需硬编码 QEMU virt 内存映射
//! 即可在兼容的 aarch64 SoC 上启动.
//!
//! 布局遵循 Devicetree Specification v0.4 (FDT 版本 17): 固定 40 字节头部 +
//! 结构块 (structure block) + 字符串块 (strings block). 结构块由大端 u32
//! token 与 4 字节对齐的负载组成.
//!
//! 本模块只解析内核启动所需的最小字段集, 不做设备树全量建模.
//!
//! SIMPLIFIED: 仅提取 memory / uart / gicv3 三类资源基址, 未实现 phandle
//!   解引用 / interrupt-map / 按 device_type 批量枚举; 影响面: 仅满足启动阶段
//!   硬件探测, 无法支撑通用驱动树枚举; 何时需扩展: 引入多 SoC 驱动发现或需
//!   解析时钟/电源等依赖关系时.

/// FDT 头部幻数 ("d00dfeed", 大端)
const FDT_MAGIC: u32 = 0xd00d_feed;

/// 结构块 token: 节点开始
const FDT_BEGIN_NODE: u32 = 1;
/// 结构块 token: 节点结束
const FDT_END_NODE: u32 = 2;
/// 结构块 token: 属性
const FDT_PROP: u32 = 3;
/// 结构块 token: 空操作 (可忽略)
const FDT_NOP: u32 = 4;
/// 结构块 token: 解析结束
const FDT_END: u32 = 9;

/// FDT 头部固定长度 (版本 17, 10 个 u32)
const FDT_HEADER_SIZE: usize = 40;

/// 头部字段偏移: 整棵树的字节数
const HDR_TOTALSIZE: usize = 4;
/// 头部字段偏移: 结构块起始
const HDR_OFF_STRUCT: usize = 8;
/// 头部字段偏移: 字符串块起始
const HDR_OFF_STRINGS: usize = 12;
/// 头部字段偏移: 字符串块长度
const HDR_SIZE_STRINGS: usize = 32;
/// 头部字段偏移: 结构块长度
const HDR_SIZE_STRUCT: usize = 36;

/// 设备树体积上限 (1 MiB, 防御恶意/畸形的 `totalsize` 触发超大切片)
const FDT_MAX_SIZE: usize = 1 << 20;

/// 解析节点深度上限 (防御畸形设备树造成的栈溢出)
const MAX_DEPTH: usize = 16;

/// 单元格数量上限 (address-cells / size-cells 的合理性上限)
const CELLS_MAX: u32 = 4;

/// 根节点深度 (根节点属性在 depth==1 时被读取)
const ROOT_DEPTH: usize = 1;

/// GICv3 `reg` 属性中重分发器 (GICR) 所在条目序号 (条目 0 为分发器 GICD)
const GIC_REDIST_INDEX: usize = 1;

/// 启动阶段从设备树提取的硬件资源物理基址
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DtbInfo {
    /// 物理内存 (DRAM) 起始地址
    pub memory_base: u64,
    /// 物理内存 (DRAM) 字节大小
    pub memory_size: u64,
    /// UART 寄存器基址
    pub uart_base: Option<u64>,
    /// GICv3 分发器 (GICD) 基址
    pub gic_dist_base: Option<u64>,
    /// GICv3 重分发器 (GICR) 基址
    pub gic_redist_base: Option<u64>,
}

/// 解析 DTB 二进制块, 失败时返回 `None`
///
/// 仅当幻数、头部长度与各块边界全部合法时返回结果; 任何越界或截断
/// 都被视为无效设备树. 返回的 `DtbInfo` 中未发现的资源保持 `None`.
pub fn parse(blob: &[u8]) -> Option<DtbInfo> {
    let mut parser = Parser::new(blob)?;
    parser.run()
}

/// 从 FDT 头部前缀解出整棵树的字节数
///
/// 调用方往往只能先拿到一个短切片 (头部), 需据此得知完整 blob 长度后再构造
/// 完整切片. 校验幻数与 `totalsize` 合理性 (不小于头部, 不超过 [`FDT_MAX_SIZE`]).
pub fn decode_header_prefix(header: &[u8]) -> Option<usize> {
    if read_be32(header, 0)? != FDT_MAGIC {
        return None;
    }
    let totalsize = read_be32(header, HDR_TOTALSIZE)? as usize;
    if !(FDT_HEADER_SIZE..=FDT_MAX_SIZE).contains(&totalsize) {
        return None;
    }
    Some(totalsize)
}

/// 单个节点的属性切片集合 (仅在节点结束时用于资源识别)
#[derive(Debug, Clone, Copy, Default)]
struct NodeScan<'a> {
    name: &'a [u8],
    compatible: &'a [u8],
    device_type: &'a [u8],
    reg: &'a [u8],
}

/// FDT 解析器状态
struct Parser<'a> {
    blob: &'a [u8],
    struct_cur: usize,
    struct_end: usize,
    strings: &'a [u8],
    addr_cells: u32,
    size_cells: u32,
    stack: [NodeScan<'a>; MAX_DEPTH],
    depth: usize,
    info: DtbInfo,
}

impl<'a> Parser<'a> {
    /// 校验头部并构造解析器
    fn new(blob: &'a [u8]) -> Option<Self> {
        if blob.len() < FDT_HEADER_SIZE {
            return None;
        }
        if read_be32(blob, 0)? != FDT_MAGIC {
            return None;
        }
        let totalsize = read_be32(blob, HDR_TOTALSIZE)? as usize;
        if totalsize > blob.len() {
            return None;
        }
        let struct_off = read_be32(blob, HDR_OFF_STRUCT)? as usize;
        let strings_off = read_be32(blob, HDR_OFF_STRINGS)? as usize;
        let struct_len = read_be32(blob, HDR_SIZE_STRUCT)? as usize;
        let strings_len = read_be32(blob, HDR_SIZE_STRINGS)? as usize;

        let struct_end = struct_off.checked_add(struct_len)?;
        let strings_end = strings_off.checked_add(strings_len)?;
        if struct_end > totalsize || strings_end > totalsize {
            return None;
        }
        let strings = blob.get(strings_off..strings_end)?;

        Some(Self {
            blob,
            struct_cur: struct_off,
            struct_end,
            strings,
            addr_cells: 2,
            size_cells: 2,
            stack: [NodeScan::default(); MAX_DEPTH],
            depth: 0,
            info: DtbInfo {
                memory_base: 0,
                memory_size: 0,
                uart_base: None,
                gic_dist_base: None,
                gic_redist_base: None,
            },
        })
    }

    /// 遍历结构块直至 `FDT_END`
    fn run(&mut self) -> Option<DtbInfo> {
        loop {
            match self.read_struct_u32()? {
                FDT_BEGIN_NODE => self.begin_node()?,
                FDT_END_NODE => self.end_node()?,
                FDT_PROP => self.read_property()?,
                FDT_NOP => {}
                FDT_END => break,
                _ => return None,
            }
        }
        Some(self.info)
    }

    /// 读取结构块内一个大端 u32, 并前移游标
    fn read_struct_u32(&mut self) -> Option<u32> {
        let next = self.struct_cur.checked_add(4)?;
        if next > self.struct_end {
            return None;
        }
        let value = read_be32(self.blob, self.struct_cur)?;
        self.struct_cur = next;
        Some(value)
    }

    /// 读取结构块内以 NUL 结尾的字符串 (节点名), 游标按 4 字节对齐前移
    fn read_cstr_struct(&mut self) -> Option<&'a [u8]> {
        let start = self.struct_cur;
        let mut idx = start;
        while idx < self.struct_end {
            if self.blob.get(idx).copied()? == 0 {
                let name = self.blob.get(start..idx)?;
                let next = align4(idx.checked_add(1)?)?;
                if next > self.struct_end {
                    return None;
                }
                self.struct_cur = next;
                return Some(name);
            }
            idx = idx.checked_add(1)?;
        }
        None
    }

    /// 处理 `FDT_BEGIN_NODE`: 压入新节点帧
    fn begin_node(&mut self) -> Option<()> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        let name = self.read_cstr_struct()?;
        self.stack[self.depth] = NodeScan {
            name,
            compatible: &[],
            device_type: &[],
            reg: &[],
        };
        self.depth += 1;
        Some(())
    }

    /// 处理 `FDT_END_NODE`: 弹出节点帧并识别资源
    fn end_node(&mut self) -> Option<()> {
        if self.depth == 0 {
            return None;
        }
        self.depth -= 1;
        let scan = self.stack[self.depth];
        self.record_node(&scan);
        Some(())
    }

    /// 处理 `FDT_PROP`: 记录节点属性, 或采样根节点的单元格数量
    fn read_property(&mut self) -> Option<()> {
        let len = self.read_struct_u32()? as usize;
        let name_off = self.read_struct_u32()? as usize;
        let value_start = self.struct_cur;
        let value_end = value_start.checked_add(len)?;
        if value_end > self.struct_end {
            return None;
        }
        let value = self.blob.get(value_start..value_end)?;
        let padded = align4(value_end)?;
        if padded > self.struct_end {
            return None;
        }
        self.struct_cur = padded;

        let name = cstr_at(self.strings, name_off)?;

        // 根节点的 #address-cells / #size-cells 决定子节点 reg 的解码宽度
        if self.depth == ROOT_DEPTH {
            if name == b"#address-cells" {
                self.addr_cells = read_be32(value, 0)?;
                return Some(());
            }
            if name == b"#size-cells" {
                self.size_cells = read_be32(value, 0)?;
                return Some(());
            }
        }

        let idx = self.depth.checked_sub(1)?;
        let scan = self.stack.get_mut(idx)?;
        if name == b"compatible" {
            scan.compatible = value;
        } else if name == b"device_type" {
            scan.device_type = value;
        } else if name == b"reg" {
            scan.reg = value;
        }
        Some(())
    }

    /// 依据节点属性识别并记录内存/UART/GICv3 资源
    fn record_node(&mut self, scan: &NodeScan<'a>) {
        if self.info.memory_size == 0
            && (scan.device_type == b"memory" || node_name_is(scan.name, b"memory"))
        {
            if let Some((base, size)) = decode_reg(scan.reg, self.addr_cells, self.size_cells, 0) {
                self.info.memory_base = base;
                self.info.memory_size = size;
            }
            return;
        }

        if self.info.uart_base.is_none() && cstr_list_contains(scan.compatible, b"arm,pl011") {
            if let Some((base, _)) = decode_reg(scan.reg, self.addr_cells, self.size_cells, 0) {
                self.info.uart_base = Some(base);
            }
            return;
        }

        if self.info.gic_dist_base.is_none() && cstr_list_contains(scan.compatible, b"arm,gic-v3") {
            if let Some((dist, _)) = decode_reg(scan.reg, self.addr_cells, self.size_cells, 0) {
                self.info.gic_dist_base = Some(dist);
            }
            if let Some((redist, _)) =
                decode_reg(scan.reg, self.addr_cells, self.size_cells, GIC_REDIST_INDEX)
            {
                self.info.gic_redist_base = Some(redist);
            }
        }
    }
}

/// 读取大端 u32, 越界返回 `None`
fn read_be32(buf: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes: [u8; 4] = buf.get(offset..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// 向上对齐到 4 字节边界
fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|v| v & !3)
}

/// 从字符串块中按偏移读取以 NUL 结尾的字符串
fn cstr_at(strings: &[u8], offset: usize) -> Option<&[u8]> {
    let rest = strings.get(offset..)?;
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// 判断以 NUL 分隔的字符串列表中是否包含目标字符串
fn cstr_list_contains(list: &[u8], needle: &[u8]) -> bool {
    let mut rest = list;
    while !rest.is_empty() {
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        if &rest[..end] == needle {
            return true;
        }
        if end >= rest.len() {
            return false;
        }
        rest = &rest[end + 1..];
    }
    false
}

/// 判断节点名 (忽略 `@` 后的单元地址) 是否等于目标名
fn node_name_is(name: &[u8], target: &[u8]) -> bool {
    let base = name.iter().position(|&b| b == b'@').map_or(name, |idx| &name[..idx]);
    base == target
}

/// 按大端读取指定起始单元格处的连续 `count` 个 32 位单元格, 拼接为 u64
fn read_be_cells(buf: &[u8], start_cell: usize, count: usize) -> Option<u64> {
    let mut value: u64 = 0;
    for i in 0..count {
        let cell = start_cell.checked_add(i)?;
        let word = read_be32(buf, cell.checked_mul(4)?)?;
        value = (value << 32) | u64::from(word);
    }
    Some(value)
}

/// 解码 `reg` 属性第 `index` 个条目为 (基址, 长度)
///
/// 条目宽度由父节点 (`addr_cells` + `size_cells`) 决定.
fn decode_reg(reg: &[u8], addr_cells: u32, size_cells: u32, index: usize) -> Option<(u64, u64)> {
    if addr_cells == 0 || size_cells == 0 || addr_cells > CELLS_MAX || size_cells > CELLS_MAX {
        return None;
    }
    let entry_cells = addr_cells.checked_add(size_cells)? as usize;
    let start_cell = index.checked_mul(entry_cells)?;
    let base = read_be_cells(reg, start_cell, addr_cells as usize)?;
    let size_start = start_cell.checked_add(addr_cells as usize)?;
    let size = read_be_cells(reg, size_start, size_cells as usize)?;
    Some((base, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// 测试用 FDT 构造器 (扁平化结构块 + 字符串块)
    struct BlobBuilder {
        block: Vec<u8>,
        strings: Vec<u8>,
    }

    impl BlobBuilder {
        fn new() -> Self {
            Self {
                block: Vec::new(),
                strings: Vec::new(),
            }
        }

        fn push_token(&mut self, token: u32) {
            self.block.extend_from_slice(&token.to_be_bytes());
        }

        fn begin_node(&mut self, name: &str) {
            self.push_token(FDT_BEGIN_NODE);
            self.block.extend_from_slice(name.as_bytes());
            self.block.push(0);
            while self.block.len() % 4 != 0 {
                self.block.push(0);
            }
        }

        fn end_node(&mut self) {
            self.push_token(FDT_END_NODE);
        }

        fn prop(&mut self, name: &str, value: &[u8]) {
            let name_off = self.strings.len();
            self.strings.extend_from_slice(name.as_bytes());
            self.strings.push(0);

            self.push_token(FDT_PROP);
            self.block.extend_from_slice(&(value.len() as u32).to_be_bytes());
            self.block.extend_from_slice(&(name_off as u32).to_be_bytes());
            self.block.extend_from_slice(value);
            while self.block.len() % 4 != 0 {
                self.block.push(0);
            }
        }

        fn prop_u32(&mut self, name: &str, value: u32) {
            self.prop(name, &value.to_be_bytes());
        }

        fn prop_cells(&mut self, name: &str, cells: &[u32]) {
            let mut bytes = Vec::with_capacity(cells.len() * 4);
            for cell in cells {
                bytes.extend_from_slice(&cell.to_be_bytes());
            }
            self.prop(name, &bytes);
        }

        /// 收尾: 追加 FDT_END 并拼装为完整 blob
        fn finish(mut self) -> Vec<u8> {
            self.push_token(FDT_END);

            let struct_off = FDT_HEADER_SIZE;
            let struct_len = self.block.len();
            let strings_off = struct_off + struct_len;
            let strings_len = self.strings.len();
            let totalsize = strings_off + strings_len;

            let mut blob = Vec::with_capacity(totalsize);
            blob.extend_from_slice(&FDT_MAGIC.to_be_bytes());
            blob.extend_from_slice(&(totalsize as u32).to_be_bytes());
            blob.extend_from_slice(&(struct_off as u32).to_be_bytes());
            blob.extend_from_slice(&(strings_off as u32).to_be_bytes());
            blob.extend_from_slice(&0u32.to_be_bytes()); // off_mem_rsvmap
            blob.extend_from_slice(&17u32.to_be_bytes()); // version
            blob.extend_from_slice(&16u32.to_be_bytes()); // last_comp_version
            blob.extend_from_slice(&0u32.to_be_bytes()); // boot_cpuid_phys
            blob.extend_from_slice(&(strings_len as u32).to_be_bytes());
            blob.extend_from_slice(&(struct_len as u32).to_be_bytes());
            blob.extend_from_slice(&self.block);
            blob.extend_from_slice(&self.strings);
            blob
        }
    }

    /// 构造 QEMU virt 风格的设备树 blob
    fn qemu_virt_blob() -> Vec<u8> {
        let mut b = BlobBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);

        b.begin_node("memory@40000000");
        b.prop("device_type", b"memory\0");
        b.prop_cells("reg", &[0, 0x4000_0000, 0, 0x2000_0000]);
        b.end_node();

        b.begin_node("pl011@9000000");
        b.prop("compatible", b"arm,pl011\0arm,primecell\0");
        b.prop_cells("reg", &[0, 0x0900_0000, 0, 0x1000]);
        b.end_node();

        b.begin_node("intc@8000000");
        b.prop("compatible", b"arm,gic-v3\0");
        b.prop_cells(
            "reg",
            &[0, 0x0800_0000, 0, 0x1_0000, 0, 0x080A_0000, 0, 0x2_0000],
        );
        b.end_node();

        b.end_node();
        b.finish()
    }

    #[test]
    fn parse_qemu_virt_extracts_resources() {
        let info = parse(&qemu_virt_blob()).expect("valid blob");
        assert_eq!(info.memory_base, 0x4000_0000);
        assert_eq!(info.memory_size, 0x2000_0000);
        assert_eq!(info.uart_base, Some(0x0900_0000));
        assert_eq!(info.gic_dist_base, Some(0x0800_0000));
        assert_eq!(info.gic_redist_base, Some(0x080A_0000));
    }

    #[test]
    fn parse_only_root_yields_defaults() {
        let mut b = BlobBuilder::new();
        b.begin_node("");
        b.end_node();
        let info = parse(&b.finish()).expect("valid blob");
        assert_eq!(info.memory_size, 0);
        assert_eq!(info.uart_base, None);
        assert_eq!(info.gic_dist_base, None);
        assert_eq!(info.gic_redist_base, None);
    }

    #[test]
    fn parse_memory_without_device_type() {
        let mut b = BlobBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@80000000");
        b.prop_cells("reg", &[0, 0x8000_0000, 0, 0x4000_0000]);
        b.end_node();
        b.end_node();
        let info = parse(&b.finish()).expect("valid blob");
        assert_eq!(info.memory_base, 0x8000_0000);
        assert_eq!(info.memory_size, 0x4000_0000);
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut blob = qemu_virt_blob();
        blob[0] = 0xFF;
        assert_eq!(parse(&blob), None);
    }

    #[test]
    fn parse_rejects_truncated_header() {
        let blob = [0u8; FDT_HEADER_SIZE - 1];
        assert_eq!(parse(&blob), None);
    }

    #[test]
    fn parse_rejects_totalsize_overflow() {
        let mut blob = qemu_virt_blob();
        let bogus = (blob.len() as u32) + 0x1000;
        blob[HDR_TOTALSIZE..HDR_TOTALSIZE + 4].copy_from_slice(&bogus.to_be_bytes());
        assert_eq!(parse(&blob), None);
    }

    #[test]
    fn header_prefix_yields_totalsize() {
        let blob = qemu_virt_blob();
        assert_eq!(decode_header_prefix(&blob), Some(blob.len()));
    }

    #[test]
    fn header_prefix_rejects_bad_magic_and_size() {
        let blob = qemu_virt_blob();
        assert_eq!(decode_header_prefix(&blob[..4]), None);

        let mut small = qemu_virt_blob();
        small[HDR_TOTALSIZE..HDR_TOTALSIZE + 4].copy_from_slice(&4u32.to_be_bytes());
        assert_eq!(decode_header_prefix(&small), None);

        let mut huge = qemu_virt_blob();
        let oversize = (FDT_MAX_SIZE as u32) + 1;
        huge[HDR_TOTALSIZE..HDR_TOTALSIZE + 4].copy_from_slice(&oversize.to_be_bytes());
        assert_eq!(decode_header_prefix(&huge), None);
    }
}
