//! TCB 内部常量集中
//!
//! 职责:
//! - 集中 privileged 自治的容量/数值常量
//! - 记录每个常量的"超限行为"约定
//!
//! 与 `privileged::config` 职责正交:
//! - `constants`: TCB 内部实现细节, 不暴露给 functions
//! - `config`: functions 公共 API 桥接 (sysctl / 调参)

pub mod limits;
