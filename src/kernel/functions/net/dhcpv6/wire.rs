#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `DHCPv6` 报文编解码 (RFC 8415 §8 / §21) — 纯字节, 自包含.
//!
//! 报文格式: msg-type (1B) + transaction-id (3B) + 变长选项. 选项为 TLV
//! (option-code 2B + option-len 2B + option-data). 本模块只处理客户端-服务器
//! 报文 (3 字节 transaction-id 形态); 中继报文 (`RELAY-FORW` / `RELAY-REPL`)
//! 不在客户端射程, 其 msg-type 归入 [`MessageType::Unknown`] 由消费方忽略.
//!
//! 设计立场: 不可信输入解析器, 所有边界分支完整闭合 —— 截断 / 畸形长度 /
//! 未知选项跳过 / 单例选项重复 / `IAAddr` 生命周期校验. 解析与序列化均无
//! panic (游标 + `get`/`try_into` 显式判错), 0 unsafe, 不依赖 smoltcp.

use alloc::vec::Vec;

use crate::functions::intcast::low_u16_from_usize;

// ============================================================================
// 选项码常量 (RFC 8415 §21, 仅客户端所需子集)
// ============================================================================

/// `OPTION_CLIENTID` (1): 客户端 DUID, 内容对本编解码器透明.
pub const OPT_CLIENTID: u16 = 1;
/// `OPTION_SERVERID` (2): 服务器 DUID, 内容对本编解码器透明.
pub const OPT_SERVERID: u16 = 2;
/// `OPTION_IA_NA` (3): 非临时地址身份关联 (IAID + T1 + T2 + 嵌套选项).
pub const OPT_IA_NA: u16 = 3;
/// `OPTION_IAADDR` (5): `IA_NA` 内嵌的 IPv6 地址 + 首选/有效生命周期.
pub const OPT_IAADDR: u16 = 5;
/// `OPTION_ORO` (6): 请求选项列表 (若干 2 字节选项码).
pub const OPT_ORO: u16 = 6;
/// `OPTION_PREFERENCE` (7): 服务器偏好值 (1 字节), 客户端择优用.
pub const OPT_PREFERENCE: u16 = 7;
/// `OPTION_ELAPSED_TIME` (8): 已用时间 (2 字节, 百分之一秒).
pub const OPT_ELAPSED_TIME: u16 = 8;
/// `OPTION_STATUS_CODE` (13): 状态码 (2B) + 状态消息 (变长 UTF-8).
pub const OPT_STATUS_CODE: u16 = 13;
/// `OPTION_RAPID_COMMIT` (14): 快速提交 (零长度), 两报文交换.
pub const OPT_RAPID_COMMIT: u16 = 14;
/// `OPTION_DNS_SERVERS` (23, RFC 3646): DNS 递归服务器 IPv6 地址列表.
pub const OPT_DNS_SERVERS: u16 = 23;
/// `OPTION_INFORMATION_REFRESH_TIME` (32): 无状态信息刷新间隔 (4B, 秒).
pub const OPT_INF_REFRESH_TIME: u16 = 32;

// ============================================================================
// 消息类型
// ============================================================================

/// `DHCPv6` 消息类型 (RFC 8415 §7.3, 客户端相关子集 + 未知兜底).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageType {
    /// `SOLICIT` (1): 客户端定位服务器.
    Solicit,
    /// `ADVERTISE` (2): 服务器响应 Solicit.
    Advertise,
    /// `REQUEST` (3): 客户端请求地址/选项.
    Request,
    /// `CONFIRM` (4): 客户端确认地址是否仍适用当前链路.
    Confirm,
    /// `RENEW` (5): 客户端向原服务器续约 (T1 触发).
    Renew,
    /// `REBIND` (6): 客户端向任意服务器续约 (T2 触发).
    Rebind,
    /// `REPLY` (7): 服务器对 Request/Renew/Rebind/Confirm 的应答.
    Reply,
    /// `RELEASE` (8): 客户端释放地址.
    Release,
    /// `DECLINE` (9): 客户端声明地址已被占用.
    Decline,
    /// `RECONFIGURE` (10): 服务器通知客户端重新获取配置.
    Reconfigure,
    /// `INFORMATION-REQUEST` (11): 无状态客户端仅请求选项 (不含地址).
    InformationRequest,
    /// 未识别的消息类型 (保留原始码, 消费方忽略).
    Unknown(u8),
}

impl MessageType {
    /// 由线上 1 字节码构造消息类型, 未识别码归入 [`MessageType::Unknown`].
    #[inline]
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Solicit,
            2 => Self::Advertise,
            3 => Self::Request,
            4 => Self::Confirm,
            5 => Self::Renew,
            6 => Self::Rebind,
            7 => Self::Reply,
            8 => Self::Release,
            9 => Self::Decline,
            10 => Self::Reconfigure,
            11 => Self::InformationRequest,
            _ => Self::Unknown(v),
        }
    }

    /// 序列化为线上 1 字节码.
    #[inline]
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Solicit => 1,
            Self::Advertise => 2,
            Self::Request => 3,
            Self::Confirm => 4,
            Self::Renew => 5,
            Self::Rebind => 6,
            Self::Reply => 7,
            Self::Release => 8,
            Self::Decline => 9,
            Self::Reconfigure => 10,
            Self::InformationRequest => 11,
            Self::Unknown(v) => v,
        }
    }
}

// ============================================================================
// 错误
// ============================================================================

/// 编解码错误 — 解析与序列化共用, 全部显式判错不 panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// 缓冲区在字段中途结束 (报文头 / TLV 头 / 选项数据不足).
    Truncated,
    /// 已知选项长度畸形 (定长选项长度不符 / `ORO` 奇数 / `DNS` 非 16 倍数 /
    /// `RapidCommit` 非零长).
    MalformedOption(u16),
    /// `IAAddr` 首选生命周期大于有效生命周期 (RFC 8415 §21.6 禁止).
    InvalidLifetime,
    /// 单例选项 (`ClientID` / `ServerID`) 在同一报文重复出现.
    DuplicateOption(u16),
    /// 序列化时目标缓冲区不足.
    BufferTooSmall,
    /// 序列化时单选项载荷超过 65535 字节 (TLV 长度字段上限).
    OptionTooLarge,
}

// ============================================================================
// 报文表示
// ============================================================================

/// 一条已解析的 `DHCPv6` 报文 (拥有式数据, 可跨 poll 存储).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repr {
    /// 消息类型.
    pub msg_type: MessageType,
    /// 事务标识 (3 字节), 客户端用于匹配请求与应答.
    pub transaction_id: [u8; 3],
    /// 顶层选项列表 (按线上顺序保留, 含未知选项).
    pub options: Vec<Opt>,
}

/// 一个 `DHCPv6` 选项 (TLV 解析后的类型化表示).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opt {
    /// `ClientID`: 客户端 DUID (原始字节, 编解码器不解释内部结构).
    ClientId(Vec<u8>),
    /// `ServerID`: 服务器 DUID (原始字节).
    ServerId(Vec<u8>),
    /// `IA_NA`: 非临时地址身份关联 (含嵌套 `IAAddr` / `StatusCode`).
    IaNa(IaNa),
    /// `IAAddr`: IPv6 地址 + 生命周期 (通常嵌于 `IA_NA`, 亦可独立解析).
    IaAddr(IaAddr),
    /// `StatusCode`: 状态码 + 状态消息.
    StatusCode(StatusCode),
    /// `OptionRequest`: 请求的选项码列表.
    OptionRequest(Vec<u16>),
    /// `DnsServers`: DNS 递归服务器 IPv6 地址列表.
    DnsServers(Vec<[u8; 16]>),
    /// `RapidCommit`: 快速提交标记 (零长度).
    RapidCommit,
    /// `ElapsedTime`: 已用时间 (百分之一秒).
    ElapsedTime(u16),
    /// `Preference`: 服务器偏好值.
    Preference(u8),
    /// `InformationRefreshTime`: 无状态信息刷新间隔 (秒).
    InformationRefreshTime(u32),
    /// 未识别选项: 保留原始码与数据, 序列化时原样回写 (往返保真).
    Unknown {
        /// 原始选项码.
        code: u16,
        /// 原始选项数据.
        data: Vec<u8>,
    },
}

/// `IA_NA` 选项体 (RFC 8415 §21.4): IAID + T1 + T2 + 嵌套选项.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IaNa {
    /// 身份关联标识 (客户端选定, 服务器原样回显).
    pub iaid: u32,
    /// T1: 开始向原服务器续约的时刻 (秒, 0 表示由服务器决定).
    pub t1: u32,
    /// T2: 开始向任意服务器 rebind 的时刻 (秒, 0 表示由服务器决定).
    pub t2: u32,
    /// 嵌套选项 (仅 `IAAddr` / `StatusCode` / 未知, 不再递归 `IA_NA`).
    pub options: Vec<Opt>,
}

/// `IAAddr` 选项体 (RFC 8415 §21.6): IPv6 地址 + 首选/有效生命周期.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IaAddr {
    /// 分配的 IPv6 地址 (16 字节网络序).
    pub addr: [u8; 16],
    /// 首选生命周期 (秒); 必须 <= `valid_lifetime`.
    pub preferred_lifetime: u32,
    /// 有效生命周期 (秒).
    pub valid_lifetime: u32,
}

/// `StatusCode` 选项体 (RFC 8415 §21.13): 状态码 + 状态消息.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusCode {
    /// 状态码 (0=Success, 1=UnspecFail, 2=NoAddrsAvail, 3=NoBinding,
    /// 4=NotOnLink, 5=UseMulticast; 语义判定属状态机层).
    pub code: u16,
    /// 状态消息 (UTF-8 字节, 可为空).
    pub message: Vec<u8>,
}

// ============================================================================
// 解析
// ============================================================================

impl Repr {
    /// 从字节缓冲区解析一条 `DHCPv6` 报文.
    ///
    /// 未知消息类型归入 [`MessageType::Unknown`], 未知选项保留为
    /// [`Opt::Unknown`] (不报错); 单例选项重复 / 截断 / 畸形长度 / 生命周期
    /// 违例则判错.
    ///
    /// # Errors
    ///
    /// 缓冲区截断返回 [`ParseError::Truncated`]; 已知选项长度畸形返回
    /// [`ParseError::MalformedOption`]; `IAAddr` 首选 > 有效返回
    /// [`ParseError::InvalidLifetime`]; `ClientID`/`ServerID` 重复返回
    /// [`ParseError::DuplicateOption`].
    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(buf);
        let msg_type = MessageType::from_u8(r.u8()?);
        let transaction_id: [u8; 3] = r.array()?;
        let mut options = Vec::new();
        let mut seen_clientid = false;
        let mut seen_serverid = false;
        while !r.is_empty() {
            let (code, data) = read_tlv(&mut r)?;
            match code {
                OPT_CLIENTID => {
                    if seen_clientid {
                        return Err(ParseError::DuplicateOption(code));
                    }
                    seen_clientid = true;
                }
                OPT_SERVERID => {
                    if seen_serverid {
                        return Err(ParseError::DuplicateOption(code));
                    }
                    seen_serverid = true;
                }
                _ => {}
            }
            options.push(parse_opt(code, data)?);
        }
        Ok(Self {
            msg_type,
            transaction_id,
            options,
        })
    }

    /// 将报文序列化到 `buf`, 返回写入字节数.
    ///
    /// # Errors
    ///
    /// 目标缓冲区不足返回 [`ParseError::BufferTooSmall`]; 单选项载荷超过
    /// 65535 字节返回 [`ParseError::OptionTooLarge`].
    pub fn emit(&self, buf: &mut [u8]) -> Result<usize, ParseError> {
        let mut w = Writer::new(buf);
        w.u8(self.msg_type.to_u8())?;
        w.write(&self.transaction_id)?;
        for opt in &self.options {
            emit_opt(opt, &mut w)?;
        }
        Ok(w.pos)
    }
}

/// 读取一个 TLV 头 + 数据, 返回 (选项码, 数据切片).
fn read_tlv<'a>(r: &mut Reader<'a>) -> Result<(u16, &'a [u8]), ParseError> {
    let code = r.u16()?;
    let len = r.u16()?;
    let data = r.take(usize::from(len))?;
    Ok((code, data))
}

/// 按选项码分派解析 (顶层).
fn parse_opt(code: u16, data: &[u8]) -> Result<Opt, ParseError> {
    match code {
        OPT_CLIENTID => Ok(Opt::ClientId(data.to_vec())),
        OPT_SERVERID => Ok(Opt::ServerId(data.to_vec())),
        OPT_IA_NA => parse_ia_na(data).map(Opt::IaNa),
        OPT_IAADDR => parse_ia_addr(data).map(Opt::IaAddr),
        OPT_STATUS_CODE => parse_status_code(data).map(Opt::StatusCode),
        OPT_ORO => parse_oro(data).map(Opt::OptionRequest),
        OPT_DNS_SERVERS => parse_dns(data).map(Opt::DnsServers),
        OPT_RAPID_COMMIT => {
            if !data.is_empty() {
                return Err(ParseError::MalformedOption(code));
            }
            Ok(Opt::RapidCommit)
        }
        OPT_ELAPSED_TIME => {
            let b: [u8; 2] = data
                .try_into()
                .map_err(|_| ParseError::MalformedOption(code))?;
            Ok(Opt::ElapsedTime(u16::from_be_bytes(b)))
        }
        OPT_PREFERENCE => {
            let b: [u8; 1] = data
                .try_into()
                .map_err(|_| ParseError::MalformedOption(code))?;
            Ok(Opt::Preference(b[0]))
        }
        OPT_INF_REFRESH_TIME => {
            let b: [u8; 4] = data
                .try_into()
                .map_err(|_| ParseError::MalformedOption(code))?;
            Ok(Opt::InformationRefreshTime(u32::from_be_bytes(b)))
        }
        _ => Ok(Opt::Unknown {
            code,
            data: data.to_vec(),
        }),
    }
}

/// 解析 `IA_NA` 内嵌选项: 仅 `IAAddr` / `StatusCode`, 其余归未知 (不递归
/// `IA_NA`, 将嵌套深度限定为两层, 杜绝恶意深嵌套栈溢出).
fn parse_ia_na_sub_opt(code: u16, data: &[u8]) -> Result<Opt, ParseError> {
    match code {
        OPT_IAADDR => parse_ia_addr(data).map(Opt::IaAddr),
        OPT_STATUS_CODE => parse_status_code(data).map(Opt::StatusCode),
        _ => Ok(Opt::Unknown {
            code,
            data: data.to_vec(),
        }),
    }
}

/// 解析 `IA_NA`: IAID(4) + T1(4) + T2(4) + 嵌套 TLV 选项.
fn parse_ia_na(data: &[u8]) -> Result<IaNa, ParseError> {
    let mut r = Reader::new(data);
    let iaid = r.u32()?;
    let t1 = r.u32()?;
    let t2 = r.u32()?;
    let mut options = Vec::new();
    while !r.is_empty() {
        let (code, sub) = read_tlv(&mut r)?;
        options.push(parse_ia_na_sub_opt(code, sub)?);
    }
    Ok(IaNa {
        iaid,
        t1,
        t2,
        options,
    })
}

/// 解析 `IAAddr`: 地址(16) + 首选(4) + 有效(4); 尾部子选项不解释.
fn parse_ia_addr(data: &[u8]) -> Result<IaAddr, ParseError> {
    let mut r = Reader::new(data);
    let addr: [u8; 16] = r.array()?;
    let preferred_lifetime = r.u32()?;
    let valid_lifetime = r.u32()?;
    if preferred_lifetime > valid_lifetime {
        return Err(ParseError::InvalidLifetime);
    }
    Ok(IaAddr {
        addr,
        preferred_lifetime,
        valid_lifetime,
    })
}

/// 解析 `StatusCode`: 状态码(2) + 状态消息(变长).
fn parse_status_code(data: &[u8]) -> Result<StatusCode, ParseError> {
    let mut r = Reader::new(data);
    let code = r.u16()?;
    let message = r.rest().to_vec();
    Ok(StatusCode { code, message })
}

/// 解析 `ORO`: 若干 2 字节选项码, 长度必须为偶数.
fn parse_oro(data: &[u8]) -> Result<Vec<u16>, ParseError> {
    if !data.len().is_multiple_of(2) {
        return Err(ParseError::MalformedOption(OPT_ORO));
    }
    let mut r = Reader::new(data);
    let mut codes = Vec::new();
    while !r.is_empty() {
        codes.push(r.u16()?);
    }
    Ok(codes)
}

/// 解析 `DNS_SERVERS`: 若干 16 字节 IPv6 地址, 长度必须为 16 倍数.
fn parse_dns(data: &[u8]) -> Result<Vec<[u8; 16]>, ParseError> {
    if !data.len().is_multiple_of(16) {
        return Err(ParseError::MalformedOption(OPT_DNS_SERVERS));
    }
    let mut r = Reader::new(data);
    let mut addrs = Vec::new();
    while !r.is_empty() {
        addrs.push(r.array()?);
    }
    Ok(addrs)
}

// ============================================================================
// 序列化
// ============================================================================

impl Opt {
    /// 返回本选项的线上选项码.
    fn code(&self) -> u16 {
        match self {
            Self::ClientId(_) => OPT_CLIENTID,
            Self::ServerId(_) => OPT_SERVERID,
            Self::IaNa(_) => OPT_IA_NA,
            Self::IaAddr(_) => OPT_IAADDR,
            Self::StatusCode(_) => OPT_STATUS_CODE,
            Self::OptionRequest(_) => OPT_ORO,
            Self::DnsServers(_) => OPT_DNS_SERVERS,
            Self::RapidCommit => OPT_RAPID_COMMIT,
            Self::ElapsedTime(_) => OPT_ELAPSED_TIME,
            Self::Preference(_) => OPT_PREFERENCE,
            Self::InformationRefreshTime(_) => OPT_INF_REFRESH_TIME,
            Self::Unknown { code, .. } => *code,
        }
    }
}

/// 序列化一个选项: 写 TLV 头 (长度占位) + 载荷, 再回填长度.
fn emit_opt(opt: &Opt, w: &mut Writer<'_>) -> Result<(), ParseError> {
    w.u16(opt.code())?;
    let len_pos = w.pos;
    w.u16(0)?;
    let payload_start = w.pos;
    match opt {
        Opt::ClientId(d) | Opt::ServerId(d) => w.write(d)?,
        Opt::IaNa(ia) => {
            w.u32(ia.iaid)?;
            w.u32(ia.t1)?;
            w.u32(ia.t2)?;
            for sub in &ia.options {
                emit_opt(sub, w)?;
            }
        }
        Opt::IaAddr(a) => {
            w.write(&a.addr)?;
            w.u32(a.preferred_lifetime)?;
            w.u32(a.valid_lifetime)?;
        }
        Opt::StatusCode(s) => {
            w.u16(s.code)?;
            w.write(&s.message)?;
        }
        Opt::OptionRequest(codes) => {
            for &c in codes {
                w.u16(c)?;
            }
        }
        Opt::DnsServers(addrs) => {
            for a in addrs {
                w.write(a)?;
            }
        }
        Opt::RapidCommit => {}
        Opt::ElapsedTime(v) => w.u16(*v)?,
        Opt::Preference(v) => w.u8(*v)?,
        Opt::InformationRefreshTime(v) => w.u32(*v)?,
        Opt::Unknown { data, .. } => w.write(data)?,
    }
    let payload_len = w.pos - payload_start;
    if payload_len > usize::from(u16::MAX) {
        return Err(ParseError::OptionTooLarge);
    }
    w.backfill_u16(len_pos, low_u16_from_usize(payload_len))?;
    Ok(())
}

// ============================================================================
// 无 panic 游标助手
// ============================================================================

/// 只读游标: 所有取数经 `get` + `checked_add` 显式判错, 不 panic.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.pos.checked_add(n).ok_or(ParseError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(ParseError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        self.take(N)?.try_into().map_err(|_| ParseError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, ParseError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
}

/// 只写游标: 写入前校验剩余空间, 不足即判错, 不 panic.
struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ParseError> {
        let end = self
            .pos
            .checked_add(bytes.len())
            .ok_or(ParseError::BufferTooSmall)?;
        let slot = self
            .buf
            .get_mut(self.pos..end)
            .ok_or(ParseError::BufferTooSmall)?;
        slot.copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }

    fn u8(&mut self, v: u8) -> Result<(), ParseError> {
        self.write(&[v])
    }

    fn u16(&mut self, v: u16) -> Result<(), ParseError> {
        self.write(&v.to_be_bytes())
    }

    fn u32(&mut self, v: u32) -> Result<(), ParseError> {
        self.write(&v.to_be_bytes())
    }

    fn backfill_u16(&mut self, pos: usize, v: u16) -> Result<(), ParseError> {
        let end = pos.checked_add(2).ok_or(ParseError::BufferTooSmall)?;
        let slot = self
            .buf
            .get_mut(pos..end)
            .ok_or(ParseError::BufferTooSmall)?;
        slot.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// 构造一条仅含报文头 + 给定选项的报文.
    fn repr(msg_type: MessageType, options: Vec<Opt>) -> Repr {
        Repr {
            msg_type,
            transaction_id: [0x0a, 0x0b, 0x0c],
            options,
        }
    }

    /// 往返: emit 后 parse 应还原同一 Repr.
    fn round_trip(r: &Repr) {
        let mut buf = [0u8; 512];
        let n = r.emit(&mut buf).expect("emit");
        let back = Repr::parse(&buf[..n]).expect("parse");
        assert_eq!(&back, r);
    }

    /// 报文头: 各消息类型与事务标识往返保真.
    #[test]
    fn test_header_round_trip() {
        for mt in [
            MessageType::Solicit,
            MessageType::Advertise,
            MessageType::Request,
            MessageType::Confirm,
            MessageType::Renew,
            MessageType::Rebind,
            MessageType::Reply,
            MessageType::Release,
            MessageType::Decline,
            MessageType::Reconfigure,
            MessageType::InformationRequest,
            MessageType::Unknown(200),
        ] {
            round_trip(&repr(mt, vec![]));
        }
    }

    /// 消息类型码双向映射逐值一致.
    #[test]
    fn test_message_type_code_map() {
        for v in 0u8..=255 {
            assert_eq!(MessageType::from_u8(v).to_u8(), v);
        }
        assert_eq!(MessageType::from_u8(1), MessageType::Solicit);
        assert_eq!(MessageType::from_u8(11), MessageType::InformationRequest);
        assert_eq!(MessageType::from_u8(12), MessageType::Unknown(12));
    }

    /// `ClientID` / `ServerID` DUID 原始字节往返.
    #[test]
    fn test_duid_options_round_trip() {
        let r = repr(
            MessageType::Solicit,
            vec![
                Opt::ClientId(vec![
                    0x00, 0x03, 0x00, 0x01, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
                ]),
                Opt::ServerId(vec![0x00, 0x02, 0x00, 0x00, 0x0e, 0x10, 0x01, 0x02]),
            ],
        );
        round_trip(&r);
    }

    /// `IA_NA` 含嵌套 `IAAddr` + `StatusCode` 往返.
    #[test]
    fn test_ia_na_nested_round_trip() {
        let ia = IaNa {
            iaid: 0x1234_5678,
            t1: 1800,
            t2: 2700,
            options: vec![
                Opt::IaAddr(IaAddr {
                    addr: [
                        0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
                    ],
                    preferred_lifetime: 3600,
                    valid_lifetime: 7200,
                }),
                Opt::StatusCode(StatusCode {
                    code: 0,
                    message: b"success".to_vec(),
                }),
            ],
        };
        round_trip(&repr(MessageType::Reply, vec![Opt::IaNa(ia)]));
    }

    /// 多个 `IA_NA` 与多地址 `DNS` 列表往返.
    #[test]
    fn test_multiple_ia_na_and_dns_round_trip() {
        let mk = |iaid: u32| {
            Opt::IaNa(IaNa {
                iaid,
                t1: 0,
                t2: 0,
                options: vec![Opt::IaAddr(IaAddr {
                    addr: [
                        0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02,
                    ],
                    preferred_lifetime: 100,
                    valid_lifetime: 200,
                })],
            })
        };
        let dns = vec![
            [
                0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
            ],
            [
                0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x44,
            ],
        ];
        round_trip(&repr(
            MessageType::Reply,
            vec![mk(1), mk(2), Opt::DnsServers(dns)],
        ));
    }

    /// `ORO` / `RapidCommit` / `ElapsedTime` / `Preference` /
    /// `InformationRefreshTime` 往返.
    #[test]
    fn test_misc_options_round_trip() {
        let r = repr(
            MessageType::InformationRequest,
            vec![
                Opt::OptionRequest(vec![OPT_DNS_SERVERS, OPT_INF_REFRESH_TIME]),
                Opt::RapidCommit,
                Opt::ElapsedTime(1234),
                Opt::Preference(255),
                Opt::InformationRefreshTime(86400),
            ],
        );
        round_trip(&r);
    }

    /// 空 `StatusCode` 消息往返.
    #[test]
    fn test_status_code_empty_message() {
        round_trip(&repr(
            MessageType::Reply,
            vec![Opt::StatusCode(StatusCode {
                code: 2,
                message: vec![],
            })],
        ));
    }

    /// 未知选项与未知消息类型保留原始数据, 往返保真.
    #[test]
    fn test_unknown_option_preserved() {
        let r = repr(
            MessageType::Unknown(99),
            vec![Opt::Unknown {
                code: 0x1234,
                data: vec![0xde, 0xad, 0xbe, 0xef],
            }],
        );
        round_trip(&r);
    }

    /// 报文头不足 4 字节判截断.
    #[test]
    fn test_truncated_header() {
        for n in 0..4 {
            let buf = [0u8; 4];
            assert_eq!(Repr::parse(&buf[..n]), Err(ParseError::Truncated));
        }
    }

    /// TLV 头不足 4 字节判截断.
    #[test]
    fn test_truncated_tlv_header() {
        // 报文头 (4) + 选项码 (2) 但缺长度字段.
        let buf = [1, 0, 0, 1, 0x00, 0x01];
        assert_eq!(Repr::parse(&buf), Err(ParseError::Truncated));
    }

    /// 选项数据声明长度超出缓冲区判截断.
    #[test]
    fn test_option_data_exceeds_buffer() {
        // 报文头 + ClientID 声明 len=10 但只给 2 字节.
        let buf = [1, 0, 0, 1, 0x00, 0x01, 0x00, 0x0a, 0xaa, 0xbb];
        assert_eq!(Repr::parse(&buf), Err(ParseError::Truncated));
    }

    /// 定长选项长度畸形判错.
    #[test]
    fn test_malformed_fixed_length_options() {
        // Preference 应为 1 字节, 给 2 字节.
        let buf = [1, 0, 0, 1, 0x00, 0x07, 0x00, 0x02, 0x01, 0x02];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::MalformedOption(OPT_PREFERENCE))
        );
        // ElapsedTime 应为 2 字节, 给 1 字节.
        let buf = [1, 0, 0, 1, 0x00, 0x08, 0x00, 0x01, 0x01];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::MalformedOption(OPT_ELAPSED_TIME))
        );
        // InformationRefreshTime 应为 4 字节, 给 2 字节.
        let buf = [1, 0, 0, 1, 0x00, 0x20, 0x00, 0x02, 0x00, 0x01];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::MalformedOption(OPT_INF_REFRESH_TIME))
        );
    }

    /// `RapidCommit` 必须零长度.
    #[test]
    fn test_rapid_commit_nonzero_length() {
        let buf = [1, 0, 0, 1, 0x00, 0x0e, 0x00, 0x01, 0x00];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::MalformedOption(OPT_RAPID_COMMIT))
        );
    }

    /// 回归: dnsmasq 2.93 的 Advertise 选项集必须整体解析成功.
    ///
    /// 曾因 `OPT_STATUS_CODE` 误编为 12、`OPT_RAPID_COMMIT` 误编为 13, 使
    /// status-code(13) 落入 rapid-commit 分支并以「非空载荷」判错; 解析失败后
    /// 客户端永久停在 Soliciting, 只重发 Solicit (RA M=1 实测现象).
    #[test]
    fn test_dnsmasq_advertise_parses() {
        let mut buf = vec![2u8, 0x57, 0x00, 0x01];
        // 选项 1 客户端 ID: DUID-LL (硬件类型 1 + MAC 52:54:00:12:34:56)
        buf.extend_from_slice(&[0x00, 0x01, 0x00, 0x0a, 0x00, 0x01, 0x00, 0x01]);
        buf.extend_from_slice(&[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        // option 2 server-id: DUID-LLT (14 字节)
        buf.extend_from_slice(&[0x00, 0x02, 0x00, 0x0e, 0x00, 0x01, 0x00, 0x01]);
        buf.extend_from_slice(&[0x32, 0x8a, 0x73, 0xe1]);
        buf.extend_from_slice(&[0x78, 0x55, 0x36, 0x0a, 0x82, 0x39]);
        // option 3 IA_NA: iaid=1, t1=21600, t2=37800, 嵌套 IA_ADDR
        buf.extend_from_slice(&[0x00, 0x03, 0x00, 0x28]);
        buf.extend_from_slice(&1u32.to_be_bytes());
        buf.extend_from_slice(&21600u32.to_be_bytes());
        buf.extend_from_slice(&37800u32.to_be_bytes());
        // 嵌套 option 5 IA_ADDR: fd00::1086, pltime/vltime 43200
        buf.extend_from_slice(&[0x00, 0x05, 0x00, 0x18]);
        buf.extend_from_slice(&[
            0xfd, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x10, 0x86,
        ]);
        buf.extend_from_slice(&43200u32.to_be_bytes());
        buf.extend_from_slice(&43200u32.to_be_bytes());
        // 选项 13 状态码: Success (0)
        buf.extend_from_slice(&[0x00, 0x0d, 0x00, 0x02, 0x00, 0x00]);
        // 选项 7 偏好值: 0
        buf.extend_from_slice(&[0x00, 0x07, 0x00, 0x01, 0x00]);

        let r = Repr::parse(&buf).expect("dnsmasq Advertise 必须可解析");
        assert_eq!(r.msg_type, MessageType::Advertise);
        assert_eq!(r.transaction_id, [0x57, 0x00, 0x01]);
        assert!(
            r.options
                .iter()
                .any(|o| matches!(o, Opt::StatusCode(s) if s.code == 0))
        );
        assert!(r.options.iter().any(|o| matches!(o, Opt::Preference(0))));
        let ia = r
            .options
            .iter()
            .find_map(|o| match o {
                Opt::IaNa(ia) => Some(ia),
                _ => None,
            })
            .expect("Advertise 必须含 IA_NA");
        assert!(ia.options.iter().any(|o| matches!(
            o,
            Opt::IaAddr(a) if a.addr == [0xfd, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10, 0x86]
        )));
    }

    /// `ORO` 奇数长度判错.
    #[test]
    fn test_oro_odd_length() {
        let buf = [1, 0, 0, 1, 0x00, 0x06, 0x00, 0x03, 0x00, 0x17, 0x00];
        assert_eq!(Repr::parse(&buf), Err(ParseError::MalformedOption(OPT_ORO)));
    }

    /// `DNS` 非 16 倍数长度判错.
    #[test]
    fn test_dns_non_multiple_of_16() {
        let buf = [1, 0, 0, 1, 0x00, 0x17, 0x00, 0x04, 0, 0, 0, 0];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::MalformedOption(OPT_DNS_SERVERS))
        );
    }

    /// `IAAddr` 首选 > 有效生命周期判错.
    #[test]
    fn test_ia_addr_invalid_lifetime() {
        let mut data = [0u8; 24];
        data[16..20].copy_from_slice(&200u32.to_be_bytes()); // preferred
        data[20..24].copy_from_slice(&100u32.to_be_bytes()); // valid
        assert_eq!(parse_ia_addr(&data), Err(ParseError::InvalidLifetime));
    }

    /// `IAAddr` 不足 24 字节判截断.
    #[test]
    fn test_ia_addr_truncated() {
        let data = [0u8; 23];
        assert_eq!(parse_ia_addr(&data), Err(ParseError::Truncated));
    }

    /// `IA_NA` 不足 12 字节判截断.
    #[test]
    fn test_ia_na_truncated() {
        let data = [0u8; 11];
        assert_eq!(parse_ia_na(&data), Err(ParseError::Truncated));
    }

    /// `IA_NA` 内嵌 `IAAddr` 生命周期违例向上传播.
    #[test]
    fn test_ia_na_nested_invalid_lifetime() {
        let mut ia_addr = [0u8; 24];
        ia_addr[16..20].copy_from_slice(&300u32.to_be_bytes());
        ia_addr[20..24].copy_from_slice(&100u32.to_be_bytes());
        let mut data = [0u8; 12 + 4 + 24];
        // 嵌套 TLV: IAADDR(5) len=24.
        data[12..14].copy_from_slice(&OPT_IAADDR.to_be_bytes());
        data[14..16].copy_from_slice(&24u16.to_be_bytes());
        data[16..].copy_from_slice(&ia_addr);
        assert_eq!(parse_ia_na(&data), Err(ParseError::InvalidLifetime));
    }

    /// `ClientID` 重复判错.
    #[test]
    fn test_duplicate_client_id() {
        let buf = [
            1, 0, 0, 1, // header
            0x00, 0x01, 0x00, 0x02, 0xaa, 0xbb, // ClientID #1
            0x00, 0x01, 0x00, 0x02, 0xcc, 0xdd, // ClientID #2
        ];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::DuplicateOption(OPT_CLIENTID))
        );
    }

    /// `ServerID` 重复判错.
    #[test]
    fn test_duplicate_server_id() {
        let buf = [
            2, 0, 0, 1, // header
            0x00, 0x02, 0x00, 0x02, 0xaa, 0xbb, // ServerID #1
            0x00, 0x02, 0x00, 0x02, 0xcc, 0xdd, // ServerID #2
        ];
        assert_eq!(
            Repr::parse(&buf),
            Err(ParseError::DuplicateOption(OPT_SERVERID))
        );
    }

    /// 序列化缓冲区不足判错 (报文头即放不下).
    #[test]
    fn test_emit_buffer_too_small_header() {
        let r = repr(MessageType::Solicit, vec![]);
        let mut buf = [0u8; 3];
        assert_eq!(r.emit(&mut buf), Err(ParseError::BufferTooSmall));
    }

    /// 序列化缓冲区不足判错 (选项载荷放不下).
    #[test]
    fn test_emit_buffer_too_small_option() {
        let r = repr(MessageType::Solicit, vec![Opt::ClientId(vec![0xaa; 32])]);
        let mut buf = [0u8; 8];
        assert_eq!(r.emit(&mut buf), Err(ParseError::BufferTooSmall));
    }

    /// 序列化恰好填满缓冲区成功, 返回写入长度.
    #[test]
    fn test_emit_exact_fit() {
        let r = repr(MessageType::Solicit, vec![Opt::RapidCommit]);
        let mut buf = [0u8; 8]; // 4 header + 4 TLV (zero-len)
        assert_eq!(r.emit(&mut buf), Ok(8));
        assert_eq!(Repr::parse(&buf), Ok(r));
    }

    /// `IA_NA` 内嵌未知选项归未知且不再递归 `IA_NA` (深度限定).
    #[test]
    fn test_ia_na_nested_unknown_not_recursive() {
        // IA_NA 内嵌一个 code=IA_NA 的选项, 应归 Unknown 而非递归解析.
        let inner = [0u8; 4];
        let mut data = [0u8; 12 + 4 + 4];
        data[12..14].copy_from_slice(&OPT_IA_NA.to_be_bytes());
        data[14..16].copy_from_slice(&4u16.to_be_bytes());
        data[16..].copy_from_slice(&inner);
        let ia = parse_ia_na(&data).expect("parse ia_na");
        assert_eq!(ia.options.len(), 1);
        assert!(matches!(&ia.options[0], Opt::Unknown { .. }));
        assert_eq!(ia.options[0].code(), OPT_IA_NA);
    }

    /// 空选项列表的报文往返 (仅报文头).
    #[test]
    fn test_empty_options_round_trip() {
        round_trip(&repr(MessageType::Renew, vec![]));
    }

    /// `StatusCode` 非空消息往返.
    #[test]
    fn test_status_code_with_message() {
        round_trip(&repr(
            MessageType::Reply,
            vec![Opt::StatusCode(StatusCode {
                code: 3,
                message: b"NoBinding".to_vec(),
            })],
        ));
    }
}
