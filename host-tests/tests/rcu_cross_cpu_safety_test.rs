//! RCU 跨 CPU 安全 — 静态契约测试 (P-35)
//!
//! SPDX-License-Identifier: MPL-2.0
//!
//! ## 背景
//!
//! RCU 的回调链表头尾 (`PerCpuRcu::callbacks` / `callback_tail`) 是 `UnsafeCell`,
//! 只允许所属 CPU 在关中断下访问; 而 `rcu_data` 在槽位为 `null` 时回退 BSP,
//! 跨 CPU 路径误用会把远程 CPU 的状态写到 BSP 槽位上. RCU 全部 API 当前无
//! 生产调用点, 无法用运行期断言覆盖上述路径, 故以源码文本分析固化契约
//! (同 `arch_ap_startup_lock_removed_test.rs` 手法).
//!
//! 契约:
//! 1. `synchronize_rcu_impl` 的跨 CPU 循环只经 `rcu_data_allocated(i)` 取槽位,
//!    不得出现会回退 BSP 的 `rcu_data(i)`;
//! 2. `rcu_process_all_callbacks` 只处理本 CPU 回调, 远程 CPU 仅发 IPI 通知,
//!    不得触碰远程 `callbacks` / `callback_tail`;
//! 3. 三个触碰全局状态的 RCU 用例都经 `SerialGuard::acquire()` 串行化
//!    (host 侧共用 BSP 槽位); 另有一组只操作栈上独立槽位的 epoch 状态机用例,
//!    不触碰全局, 无需串行;
//! 4. 宽限期槽位字只经 epoch 状态机 (`request_gp` / `report_qs` / `finish_gp`)
//!    读写, 等待循环内必须自报静止 (P-76).

use std::fs;
use std::path::Path;

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // Edgine 工作区根目录
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

/// 截取 `src` 中 `begin` 起点到其后首个 `end` 之间的片段.
fn slice_between<'a>(src: &'a str, begin: &str, end: &str) -> &'a str {
    let start = src
        .find(begin)
        .unwrap_or_else(|| panic!("未找到起点标记: {begin}"));
    let rest = &src[start..];
    let end_off = rest
        .find(end)
        .unwrap_or_else(|| panic!("未找到终点标记: {end}"));
    &rest[..end_off]
}

/// 只保留代码行 (剔除以 `//` 开头的整行注释), 避免注释里的提法被当成代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

const RCU_RS: &str = "src/kernel/privileged/sync/rcu.rs";

/// `synchronize_rcu_impl` 函数体 (到下一个函数 `call_rcu` 为止).
fn synchronize_body() -> String {
    let src = read(RCU_RS);
    code_only(slice_between(
        &src,
        "fn synchronize_rcu_impl(",
        "fn call_rcu(",
    ))
}

/// `rcu_process_all_callbacks` 函数体 (到下一个函数 `rcu_gp_count` 为止).
fn process_all_body() -> String {
    let src = read(RCU_RS);
    code_only(slice_between(
        &src,
        "pub fn rcu_process_all_callbacks(",
        "pub fn rcu_gp_count(",
    ))
}

fn tests_module() -> String {
    let src = read(RCU_RS);
    let start = src.find("#[cfg(test)]").expect("未找到 tests 模块");
    src[start..].to_string()
}

#[test]
fn synchronize_never_falls_back_to_bsp_slot() {
    let body = synchronize_body();
    assert_eq!(
        count(&body, "rcu_data_allocated(i)"),
        3,
        "宽限期的标记/等待/复位三个跨 CPU 循环都必须经 rcu_data_allocated(i) 取槽位"
    );
    assert!(
        !body.contains("rcu_data(i)"),
        "跨 CPU 路径不得用 rcu_data(i): 槽位未分配时它会回退 BSP, 把远程状态写到 BSP 槽位"
    );
    assert_eq!(
        count(&body, "is_cpu_online(i)"),
        2,
        "标记与等待两个循环都必须跳过离线 CPU (离线核不会被置 GP_WAIT, 等待处会空转到超时)"
    );
}

#[test]
fn process_all_callbacks_notifies_remotes_instead_of_touching_lists() {
    let body = process_all_body();
    assert!(
        body.contains("process_callbacks();"),
        "本 CPU 的回调必须就地处理"
    );
    assert!(
        body.contains("rcu_data_allocated(i)") && !body.contains("rcu_data(i)"),
        "远程槽位必须经 rcu_data_allocated 获取, 不得回退 BSP 槽位"
    );
    assert!(
        body.contains("send_reschedule_ipi("),
        "远程 CPU 的挂起回调必须经重调度 IPI 通知, 由目标 CPU 本地处理"
    );
    assert!(
        !body.contains(".callbacks.get()"),
        "不得跨 CPU 读取远程回调链表头 (与目标 CPU 关中断下的链表操作竞争)"
    );
    assert!(
        !body.contains(".callback_tail.get()"),
        "不得跨 CPU 改写远程回调链表尾"
    );
}

#[test]
fn synchronize_uses_epoch_guarded_state_machine() {
    let body = synchronize_body();
    assert!(
        body.contains("request_gp(data, ticket)"),
        "远端槽位必须以本轮序号登记请求, 不得直接写 GP_WAIT (并发宽限期会互相覆盖)"
    );
    assert!(
        body.contains("gp_satisfied(data, ticket)"),
        "等待判据必须携带本轮序号, 裸 GP_DONE 会被旧轮次的完成记录骗过"
    );
    assert!(
        body.contains("finish_gp(data, ticket)"),
        "复位必须只清本轮序号的槽位, 不得无条件写 GP_IDLE"
    );
    assert_eq!(
        count(&body, "report_qs(current_rcu());"),
        2,
        "本 CPU 静止须在登记后与等待循环内各上报一次: 后者用于打破并发宽限期的互相等待"
    );
    assert!(
        !body.contains("gp_epoch_state.store("),
        "槽位字只能经 CAS 状态机改写, 直接 store 会撕裂序号与三态"
    );
}

#[test]
fn rcu_tests_are_serialized() {
    let tests = tests_module();
    for test_name in [
        "fn test_rcu_read_lock_unlock(",
        "fn test_rcu_gp_counter(",
        "fn test_call_rcu(",
    ] {
        let start = tests
            .find(test_name)
            .unwrap_or_else(|| panic!("未找到用例 {test_name}"));
        let rest = &tests[start..];
        let body = match rest.find("\n    #[test]") {
            Some(end) => &rest[..end],
            None => rest,
        };
        assert!(
            body.contains("SerialGuard::acquire()"),
            "{test_name} 必须经串行闸门互斥执行 (host 侧共用 BSP 槽位)"
        );
    }
}
