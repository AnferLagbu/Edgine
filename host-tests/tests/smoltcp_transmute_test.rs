//! W5 防回归: smoltcp 框架层 0 处 unsafe transmute
//!
//! REVAL-W W5 (2026-06-25): 把 smoltcp SocketHandle ↔ u32 的 unsafe transmute
//! 替换为 transmute_copy (编译期 size 检查, 不依赖 repr 假设).
//!
//! ## 验收
//!
//! - `framework/net/init.rs` 中 0 处 `unsafe { core::mem::transmute(...) }`
//! - 仅允许 `transmute_copy` (W5 安全路径)
//! - 注释中提到 `transmute` 是允许的 (历史包袱说明)
//!
//! ## 历史 bug 修复
//!
//! W5 完成后, 第 2161 行 (smoltcp_net_stack_socket_open 内部) 仍残留 1 处
//! unsafe transmute, 2026-06-25 在本测试触发后修复.

use std::fs;
use std::path::Path;

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

fn init_rs() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/kernel/framework/net/init.rs")
}

#[test]
fn test_no_unsafe_transmute_in_init_rs() {
    // 验收: W5 transmute 移除完整, framework/net/init.rs 0 处 unsafe transmute
    let content = read(&init_rs());

    // 匹配 `unsafe { core::mem::transmute(`
    let unsafe_transmute_count = content.matches("unsafe { core::mem::transmute(").count();
    assert_eq!(
        unsafe_transmute_count, 0,
        "framework/net/init.rs 仍有 {} 处 unsafe transmute (W5 反模式), 应改用 transmute_copy",
        unsafe_transmute_count
    );
}

#[test]
fn test_only_transmute_copy_in_init_rs() {
    // 验收: SocketHandle ↔ usize 的转换仅走 transmute_copy
    let content = read(&init_rs());

    // transmute_copy 必须存在 (W5 路径)
    let transmute_copy_count = content.matches("core::mem::transmute_copy").count();
    assert!(
        transmute_copy_count >= 2,
        "transmute_copy 应至少 2 处 (as_u32_handle + smol_handle_from_u32), 实测: {}",
        transmute_copy_count
    );
}

#[test]
fn test_smoltcp_net_stack_socket_open_uses_as_u32_handle() {
    // 验收: smoltcp_net_stack_socket_open 复用 as_u32_handle helper,
    //       不再独立 transmute (修复 W5 遗漏)
    let content = read(&init_rs());

    // 找到 smoltcp_net_stack_socket_open 函数
    let sig = "fn smoltcp_net_stack_socket_open(";
    let idx = content
        .find(sig)
        .expect("应有 smoltcp_net_stack_socket_open 函数");

    // 找到函数体结束 (下一个顶层 fn 起始或文件结束)
    let body_end = content[idx..]
        .find("\npub fn ")
        .or_else(|| content[idx..].find("\nfn "))
        .or_else(|| content[idx..].find("\n// ===="))
        .unwrap_or(content.len() - idx);
    let body = &content[idx..idx + body_end];

    // 函数体内应调用 as_u32_handle
    assert!(
        body.contains("as_u32_handle(smol_handle)"),
        "smoltcp_net_stack_socket_open 应调用 as_u32_handle(smol_handle), 避免独立 transmute"
    );

    // 函数体内不应有 unsafe transmute
    assert!(
        !body.contains("unsafe { core::mem::transmute("),
        "smoltcp_net_stack_socket_open 不应有独立 unsafe transmute (W5 遗漏 bug)"
    );
}

#[test]
fn test_as_u32_handle_and_smol_handle_from_u32_both_use_transmute_copy() {
    // 验收: 两个核心 helper 都走 transmute_copy (W5 安全路径)
    let content = read(&init_rs());

    // as_u32_handle
    let as_u32_idx = content
        .find("fn as_u32_handle(")
        .expect("应有 as_u32_handle 函数");
    let as_u32_end = as_u32_idx
        + content[as_u32_idx..]
            .find("\n}\n")
            .expect("as_u32_handle 应有函数体");
    let as_u32_body = &content[as_u32_idx..as_u32_end + 1];
    assert!(
        as_u32_body.contains("transmute_copy(&h)"),
        "as_u32_handle 应使用 transmute_copy(&h) (而非 transmute)"
    );

    // smol_handle_from_u32
    let from_u32_idx = content
        .find("fn smol_handle_from_u32(")
        .expect("应有 smol_handle_from_u32 函数");
    let from_u32_end = from_u32_idx
        + content[from_u32_idx..]
            .find("\n}\n")
            .expect("smol_handle_from_u32 应有函数体");
    let from_u32_body = &content[from_u32_idx..from_u32_end + 1];
    assert!(
        from_u32_body.contains("transmute_copy::<usize"),
        "smol_handle_from_u32 应使用 transmute_copy::<usize, ...> (而非 transmute)"
    );
}
