// SPDX-License-Identifier: MPL-2.0
// lockdep 低开销化 (M1+M2+M5+M6) 结构契约测试.
//
// 验收面 = 记账路径的**结构**, 而非某次运行的耗时读数 (实测时长随宿主负载漂移,
// 不能作判据):
//   - M2: 一次 `acquire()` 记账只进入一次 `LOCK_DEP_MAP` 全局临界区、只用一个
//     per-CPU 持有栈窗口 (原形态为 `1 + 持有深度` 次全局加锁)
//   - M1: 记账路径零堆分配 (原形态 `held_classes().to_vec()` 每 op 一次 malloc+free,
//     且落在 `&mut` 持有栈的关中断窗口内)
//   - M5: 违规报告只登记进栈上队列, 临界区释放后才发射 (原形态在持有全局锁期间打印)
//   - M6: `map_sections` 与 `acquire_ops` 同临界区成对自增 (临界区数未被合并的证据),
//     `new_edges` 只在新边处自增 (BFS 只对**新边**执行的证据)
//   - 报告文本逐条不变: 低开销化不得改变用户可见的告警面
//
// 全部为源码契约测试: 被测面只在裸机记账热路径上才有语义, host 侧无法用运行期
// 断言区分「一次临界区」与「N 次临界区」(与 lockdep_cfg_mask_test 同口径).

use std::fs;

const LOCKDEP: &str = "../src/kernel/privileged/sync/lockdep.rs";

fn src() -> String {
    fs::read_to_string(LOCKDEP).unwrap_or_else(|e| panic!("read {LOCKDEP}: {e}"))
}

/// 取 `sig` 起 (含) 到配对右括号止的函数体文本.
fn fn_body(text: &str, sig: &str) -> String {
    let start = text
        .find(sig)
        .unwrap_or_else(|| panic!("lockdep.rs 中找不到 `{sig}` —— 记账路径已被改写, 本契约需同步"));
    let open = text[start..]
        .find('{')
        .unwrap_or_else(|| panic!("`{sig}` 没有函数体"));
    let bytes = &text.as_bytes()[start..];
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return text[start..=start + i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("`{sig}` 的函数体括号未配对");
}

/// 去掉行注释后的代码文本 —— 注释里的 `to_vec()` 等字样不构成调用.
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn count_of(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// M2: `acquire()` 只取一次全局锁、只开一个 per-CPU 持有栈窗口.
#[test]
fn acquire_enters_global_map_once_per_op() {
    let body = strip_line_comments(&fn_body(&src(), "pub fn acquire("));
    assert_eq!(
        count_of(&body, "LOCK_DEP_MAP.lock()"),
        1,
        "acquire() 内有 {} 处 `LOCK_DEP_MAP.lock()`: 每次有名锁获取就是 {} 次全局临界区, \
         M2 的单临界区合并已失效",
        count_of(&body, "LOCK_DEP_MAP.lock()"),
        count_of(&body, "LOCK_DEP_MAP.lock()"),
    );
    assert_eq!(
        count_of(&body, "with_current_stack("),
        1,
        "acquire() 内有 {} 处 `with_current_stack()`: 持有栈被分多次访问, \
         每次多一对关中断/开中断",
        count_of(&body, "with_current_stack("),
    );
    // M6 判据: 两计数器都在这唯一一次临界区内成对自增.
    let lock_at = body.find("LOCK_DEP_MAP.lock()").unwrap();
    for token in ["stats.acquire_ops += 1", "stats.map_sections += 1"] {
        let at = body
            .find(token)
            .unwrap_or_else(|| panic!("acquire() 缺 `{token}` 自增: 临界区计数腿已被拆除"));
        assert!(
            at > lock_at,
            "acquire() 的 `{token}` 在临界区之外, 计数不再证明单临界区"
        );
    }
}

/// M1: 记账路径零堆分配 (直读持有栈切片).
#[test]
fn acquire_bookkeeping_is_allocation_free() {
    let body = strip_line_comments(&fn_body(&src(), "pub fn acquire("));
    for forbidden in ["to_vec()", "to_owned(", "Vec::", "Box::", "kmalloc("] {
        assert!(
            !body.contains(forbidden),
            "acquire() 记账路径出现 `{forbidden}`: 每次锁获取多出一次堆分配, \
             且分配落在持有栈的关中断窗口内 (M1 零分配判据破裂)"
        );
    }
    assert!(
        body.contains("stack.held_classes()"),
        "acquire() 不再直读 `stack.held_classes()` 切片 —— M1 的零分配遍历形态已丢失"
    );
    // 发射队列必须是栈上定长数组, 否则"零分配"只搬到打印阶段.
    assert!(
        src().contains("items: [PendingReport; PENDING_CAPACITY]"),
        "PendingReports 不再是栈上定长数组 (M5 的延迟发射引入了堆分配)"
    );
}

/// M5: 违规打印全部延迟到临界区之外 —— acquire/release/push 体内不得直接发射日志.
#[test]
fn violation_logs_are_deferred_outside_the_guard() {
    let text = src();
    for sig in [
        "pub fn acquire(",
        "pub fn release(",
        "fn push(&mut self, class_id: LockClassId",
    ] {
        let body = strip_line_comments(&fn_body(&text, sig));
        assert!(
            !body.contains("lockdep_log!"),
            "`{sig}` 体内仍有 `lockdep_log!`: 日志在持有 `LOCK_DEP_MAP`/持有栈临界区内发出 \
             (M5 延迟发射失效)"
        );
    }
    let acquire = strip_line_comments(&fn_body(&text, "pub fn acquire("));
    assert!(
        acquire.contains("PendingReports::new()") && acquire.contains("reports.record("),
        "acquire() 不再把违规登记进 PendingReports 队列 (M5 的报告通道缺失)"
    );
    // 队列的发射点本身在 guard 之外: emit 体内允许日志, 但它不取 LOCK_DEP_MAP.
    let emit = strip_line_comments(&fn_body(&text, "fn emit(self)"));
    assert!(
        !emit.contains("LOCK_DEP_MAP"),
        "PendingReport::emit() 反向取 `LOCK_DEP_MAP`: 发射点又回到全局临界区"
    );
}

/// M6: `new_edges` 只在新边处自增, 且自增点在 BFS 之前 —— 「稳态成本与 BFS 无关」的证据.
#[test]
fn bfs_runs_only_on_new_edges() {
    let body = strip_line_comments(&fn_body(&src(), "fn add_dependency("));
    let counted = body
        .find("self.stats.new_edges += 1")
        .expect("add_dependency() 缺 `new_edges` 自增 (M6 归因计数腿被拆除)");
    let bfs = body
        .find("self.has_path(")
        .expect("add_dependency() 不再调用 has_path: 环检测形态已改变, 本契约需同步");
    assert!(
        counted < bfs,
        "new_edges 自增点在 BFS 之后 ({counted} vs {bfs}): 计数不再区分「新边」与「已验证边」"
    );
    // 已知边必须在自增之前短路返回, 否则稳态每 op 仍进 BFS.
    let short_circuit = body.find("self.adjacency[fi][ti] != 0").unwrap_or_else(|| {
        panic!("add_dependency() 缺已知边短路: 重复边会重跑 BFS, 稳态成本回到 O(n^2)")
    });
    assert!(
        short_circuit < counted,
        "已知边短路点在 new_edges 自增之后, 稳态仍对重复边跑 BFS"
    );
}

/// 报告文本逐条不变: 低开销化只动发射时机, 不动用户可见告警面.
#[test]
fn violation_messages_are_unchanged() {
    let emit = fn_body(&src(), "fn emit(self)");
    for msg in [
        "lockdep VIOLATION: acquiring sleep lock '{}' ({:?}) in IRQ context!",
        "lockdep WARNING: acquiring sleep lock '{}' while holding IRQ-context lock",
        "lockdep DEADLOCK: circular dependency detected: {} → {} (AB-BA)",
        "lockdep VIOLATION: recursive acquire of non-recursive lock '{}'!",
        "lockdep VIOLATION: releasing lock '{}' not held by current context!",
        "lockdep: held lock stack overflow (depth={})",
    ] {
        assert!(
            emit.contains(msg),
            "违规报告文本丢失: `{msg}` (门禁取证脚本按这些串判因)"
        );
    }
    assert_eq!(
        count_of(&emit, "lockdep_log!"),
        6,
        "PendingReport::emit() 有 {} 处发射点, 与六条报告不一致",
        count_of(&emit, "lockdep_log!")
    );
}

/// 归因入口必须存在且只读一次全局锁 (A-1 常开裁定的量化前置).
#[test]
fn stats_accessor_is_exposed() {
    let text = src();
    assert!(
        text.contains("pub fn stats() -> LockdepStats"),
        "缺 `stats()` 归因入口: 无法量化「每 op 几次临界区」, A-1 常开裁定失去读数"
    );
    for field in ["acquire_ops", "map_sections", "edge_attempts", "new_edges"] {
        assert!(
            text.contains(&format!("pub {field}: u64")),
            "LockdepStats 缺字段 `{field}`"
        );
    }
    assert!(
        fn_body(&text, "pub fn stats()").contains("LOCK_DEP_MAP.lock().stats"),
        "stats() 不在 `LOCK_DEP_MAP` guard 内取快照: 计数读数可能撕裂"
    );
    // functions 安全代理层同样要转发, 否则策略侧拿不到读数.
    let proxy = fs::read_to_string("../src/kernel/functions/sync/lockdep.rs")
        .expect("read functions/sync/lockdep.rs");
    assert!(
        proxy.contains("LockdepStats") || proxy.contains("pub fn stats"),
        "functions 层安全代理未暴露 stats/LockdepStats (F2 禁止直连 privileged 内部)"
    );
}
