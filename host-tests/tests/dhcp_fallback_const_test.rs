//! I-46: DHCP fallback 静态 IP 集中常量验证
//!
//! 验证:
//! 1. net::types 模块导出 FALLBACK_IPV4 / FALLBACK_PREFIX / FALLBACK_GATEWAY
//! 2. 常量值与 QEMU user-mode networking 默认 (10.0.2.0/24) 一致
//! 3. init.rs 中 fallback 路径已引用本常量 (不再硬编码)
//! 4. 集中化后, 任何修改需走单一来源
//!
//! 主机端无法跑真实网络, 这里做静态契约验证.
//!
//! DECISION-J (2026-09-12): FALLBACK_* 为机制常量 (被 privileged net init/dns
//! 机制消费), 已反转归位至 `privileged/net/types.rs`, 本测试路径同步更新。

use std::fs;

const TYPES_RS: &str = "../src/kernel/privileged/net/types.rs";
const INIT_RS: &str = "../src/kernel/privileged/net/init.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

#[test]
fn test_fallback_ipv4_constant_exported() {
    let src = read(TYPES_RS);
    assert!(
        src.contains("pub const FALLBACK_IPV4"),
        "FALLBACK_IPV4 未在 types.rs 导出"
    );
    assert!(
        src.contains("pub const FALLBACK_PREFIX"),
        "FALLBACK_PREFIX 未导出"
    );
    assert!(
        src.contains("pub const FALLBACK_GATEWAY"),
        "FALLBACK_GATEWAY 未导出"
    );
}

#[test]
fn test_fallback_values_match_qemu_default() {
    let src = read(TYPES_RS);
    // QEMU user-mode 默认 10.0.2.0/24, 客户端 10.0.2.15, 网关 10.0.2.2
    assert!(
        src.contains("FALLBACK_IPV4: [u8; 4] = [10, 0, 2, 15]"),
        "FALLBACK_IPV4 值与 QEMU 默认不符: {}",
        src.lines()
            .find(|l| l.contains("FALLBACK_IPV4"))
            .unwrap_or("")
    );
    assert!(
        src.contains("FALLBACK_PREFIX: u8 = 24"),
        "FALLBACK_PREFIX 不是 24"
    );
    assert!(
        src.contains("FALLBACK_GATEWAY: [u8; 4] = [10, 0, 2, 2]"),
        "FALLBACK_GATEWAY 值与 QEMU 默认不符"
    );
}

#[test]
fn test_init_uses_fallback_constants() {
    let src = read(INIT_RS);
    // init.rs 的 fallback 分支必须使用新常量
    assert!(
        src.contains("FALLBACK_IPV4"),
        "init.rs 未引用 FALLBACK_IPV4"
    );
    assert!(
        src.contains("FALLBACK_GATEWAY"),
        "init.rs 未引用 FALLBACK_GATEWAY"
    );
    assert!(
        src.contains("FALLBACK_PREFIX"),
        "init.rs 未引用 FALLBACK_PREFIX"
    );
    // 反向: 旧的硬编码 10.0.2.15 应不再出现在 fallback 分支
    // 注意: 函数注释 ("格式: 10.0.2.15/24,10.0.2.2") 仍可保留, 这是文档
    // 我们检查 Ipv4Address::new(10, 0, 2, 15) 这种实际硬编码用法
    assert!(
        !src.contains("Ipv4Address::new(10, 0, 2, 15)"),
        "init.rs 仍有 Ipv4Address::new(10, 0, 2, 15) 硬编码"
    );
    assert!(
        !src.contains("Ipv4Address::new(10, 0, 2, 2)"),
        "init.rs 仍有 Ipv4Address::new(10, 0, 2, 2) 硬编码"
    );
}

#[test]
fn test_no_duplicate_fallback_magic_numbers() {
    // 反向校验: 在 net/init.rs 的非测试代码中不应有 10.0.2.15 硬编码.
    // 测试代码 (#[cfg(test)] mod tests) 使用这些字面量做断言是允许的.
    let init_src = read(INIT_RS);
    let types_src = read(TYPES_RS);

    // 剥离 #[cfg(test)] mod tests { ... } 块, 只看生产代码
    fn strip_test_blocks(src: &str) -> String {
        let mut out = String::new();
        let mut depth: i32 = 0;
        let mut in_test_block = false;
        let mut pending_cfg_test = false;
        for line in src.lines() {
            let trimmed = line.trim();
            // 检测形式: #[cfg(test)] / #![cfg(test)] 单行 + 紧跟的 mod tests {
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#![cfg(test)]") {
                pending_cfg_test = true;
                continue;
            }
            if pending_cfg_test {
                pending_cfg_test = false;
                if trimmed.starts_with("mod ") {
                    in_test_block = true;
                    depth = 0;
                    // 当前行的 { 计入深度
                    for c in line.chars() {
                        if c == '{' {
                            depth += 1;
                        }
                        if c == '}' {
                            depth -= 1;
                        }
                    }
                    continue;
                }
                // 不构成测试块, 输出原 cfg(test) 行
                out.push_str(line);
                out.push('\n');
                continue;
            }
            if in_test_block {
                for c in line.chars() {
                    if c == '{' {
                        depth += 1;
                    }
                    if c == '}' {
                        depth -= 1;
                        if depth <= 0 {
                            in_test_block = false;
                        }
                    }
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }
    let prod_init = strip_test_blocks(&init_src);

    let literal_in_init = prod_init.matches("[10, 0, 2, 15]").count();
    let literal_in_types = types_src.matches("[10, 0, 2, 15]").count();
    let literal_in_init_gw = prod_init.matches("[10, 0, 2, 2]").count();
    let literal_in_types_gw = types_src.matches("[10, 0, 2, 2]").count();
    assert_eq!(literal_in_init, 0, "init.rs 生产代码残留 [10,0,2,15]");
    assert_eq!(literal_in_types, 1, "types.rs 应仅 1 处 [10,0,2,15]");
    assert_eq!(literal_in_init_gw, 0, "init.rs 生产代码残留 [10,0,2,2]");
    assert_eq!(literal_in_types_gw, 1, "types.rs 应仅 1 处 [10,0,2,2]");
}

#[test]
fn test_fallback_documents_qemu_origin() {
    // 类型模块的注释应说明这个值的来源 (QEMU user-mode default)
    // 防止后续维护者误以为是项目自定义的 link-local
    let src = read(TYPES_RS);
    assert!(
        src.contains("QEMU"),
        "fallback 常量应说明 QEMU user-mode 来源"
    );
    assert!(
        src.contains("user-mode") || src.contains("10.0.2.0/24"),
        "fallback 注释应说明子网"
    );
}
