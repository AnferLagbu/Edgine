// SPDX-License-Identifier: MPL-2.0
// eash 管道 argv 偏移量形态回归锁 (门禁健康度收敛 批5b).
//
// 病灶 (本批修复的预存缺陷):
//   `parse_single_segment` 曾在**函数局部**的 `Segment` 里取 `&raw const seg.cmd_buf[pos]`
//   存进 `argv`, 再把该结构**按值返回**; 调用方把它移进 `segments` 数组槽后, 那些指针仍指向
//   已死的 parse 帧 ⇒ `run_pipeline_child` 的 `CStr::from_ptr` 读悬垂指针. 是否"看起来能跑"
//   取决于 LLVM 是否恰好省掉返回拷贝, 不是可依赖的契约.
//   同一形态还带一处越界: `argv` 只有 16 槽而 argc 上限也是 16, 收尾写 `argv[argc]` 哨兵在
//   满 16 个参数时越界 panic.
//
// 为何是静态形状锁 (不是行为测试):
//   该解析器活在 no_std 用户态 bin crate (`src/user/eash`) 里, 依赖 fork/exec/pipe syscall,
//   host 侧无法直接编译执行; 既有 `eash_cmd_parser_test.rs` 锁的是 `Cmd` 模型, 覆盖不到
//   `Segment` 的指针来源. 故本文件锁"指针不得在移动前取"这一形态本身.
//
// 修复形态 (与同 crate `commands/mod.rs` 的 `run_external` 既有写法同构):
//   `Segment` 存 `cmd_buf` 内**偏移**, 指针留到使用点 (`run_pipeline_child`, 此时 `seg` 已是
//   数组槽内的最终位置) 再取; 路径串走 safe 的 `CStr::from_bytes_until_nul`, 哨兵由 17 槽
//   指针数组承担.

use std::fs;

const PIPELINE: &str = "../src/user/eash/src/commands/pipeline.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释, 避免说明文本被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 压缩连续空白为单空格, 使断言不受换行/列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取 `signature` 对应函数的源码窗口 (从签名起到首个行首 `}` 为止).
fn fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("未找到函数签名 {signature:?}"));
    let rest = &src[start..];
    let end = rest.find("\n}").map_or(rest.len(), |i| i + 2);
    rest[..end].to_string()
}

/// 1. `Segment` 的 argv 面必须是偏移数组, 不得是裸指针数组, 也不得在 parse 内取自指指针.
#[test]
fn test_segment_stores_offsets_not_pointers() {
    let src = code_only(&read(PIPELINE));
    assert!(
        src.contains("argv_off: [usize; 16]"),
        "Segment 必须以 cmd_buf 内偏移承载 argv (结构按值返回后要移进数组槽, 指针会悬垂)"
    );
    assert!(
        !src.contains("argv: [*const u8; 16]"),
        "不得把裸指针数组放回 Segment —— 自指指针随移动失效, 是本批修复的病灶"
    );
    assert!(
        !src.contains("&raw const seg.cmd_buf"),
        "不得在按值返回的结构里取指向自身 cmd_buf 的指针"
    );
}

/// 2. parse 只记偏移, 且不在 16 槽数组上写 `[argc]` 哨兵 (argc 上限同为 16 ⇒ 越界).
#[test]
fn test_parse_records_offset_without_sentinel_write() {
    let src = code_only(&read(PIPELINE));
    let flat = norm(&fn_body(&src, "fn parse_single_segment"));
    assert!(
        flat.contains("seg.argv_off[seg.argc] = pos;"),
        "parse 必须记录 token 在 cmd_buf 内的起始偏移"
    );
    assert!(
        flat.contains("if seg.argc >= 16"),
        "argc 上限须与偏移数组容量 16 一致 (改容量须同批改本锁)"
    );
    assert!(
        !flat.contains("seg.argv[seg.argc]"),
        "parse 不得写 argv[argc]: 满 16 个参数时该下标越界, 哨兵改由使用点的 17 槽数组承担"
    );
}

/// 3. 指针与路径串都在使用点取, 且该路径全程 safe (无 unsafe), 哨兵随切片交给 exec.
#[test]
fn test_child_builds_pointers_at_use_site() {
    let src = code_only(&read(PIPELINE));
    let body = fn_body(&src, "fn run_pipeline_child");
    let flat = norm(&body);
    assert!(
        flat.contains("let mut argv_ptrs: [*const u8; 17]"),
        "使用点的指针数组须 17 槽 = 16 个参数上限 + NULL 哨兵"
    );
    assert!(
        flat.contains("argv_ptrs[i] = seg.cmd_buf[seg.argv_off[i]..].as_ptr();"),
        "argv 指针必须在此处由偏移换算 —— 此刻 seg 已是数组槽内的最终位置, cmd_buf 不再移动"
    );
    assert!(
        flat.contains("CStr::from_bytes_until_nul(&seg.cmd_buf[path_off..])"),
        "路径串须走 safe 的 from_bytes_until_nul (cmd_buf 在 cmd_len 处恒有 NUL)"
    );
    assert!(
        !flat.contains("CStr::from_ptr"),
        "不得用 from_ptr 消费 argv 指针: 那正是读悬垂指针的入口"
    );
    assert!(
        flat.contains("let path_arg = &seg.cmd_buf[path_off..=path_off + path_slice.len()];"),
        "交给 exec 的路径切片须含 NUL, 满足 proc_exec 对 path 的 NUL 结尾契约"
    );
    assert!(
        flat.contains("proc_exec(path_arg, &argv_ptrs[..=seg.argc], &[])"),
        "argv 切片须含末位 NULL 哨兵 (内核按 NULL 结尾解析指针数组)"
    );
    assert!(
        !body.contains("unsafe"),
        "argv 与路径构造已全程 safe, 本函数不应再出现 unsafe 块"
    );
}
