//! privileged 通用密码原语 — 与安全策略无关的算法部件
//!
//! 归属判据: 本模块只放「任何子系统都可中性使用」的纯算法 (哈希等), 不放判定、
//! 不放策略、不放身份. 安全模块 (`sgeg`) 与文件系统 (`egfs` 校验和) 都从这里取
//! 原语, 依赖方向是「上层 → crypto」而不是「文件系统 → 安全模块」.
//!
//! 对照 Linux: 通用 hash 归 `crypto/`, 访问控制钩子归 `security/`; 二者不互相
//! 冒充. 详见 `docs/design/subsystem-decoupling-design.md`.

pub mod sha256;
