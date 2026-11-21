#!/usr/bin/env python3
r"""
audit_safety_content.py — F4「SAFETY 注释理由内容面」的存量棘轮 (C1 / ⑮ 甲)

背景: 门禁健康度收敛 批 5a–5g 把四树 SAFETY **缺失**存量清零, 但 clippy
`undocumented_unsafe_blocks` 与 `audit_safety_debt.py` 只判「有没有、在不在正确位置」,
从不判理由是否真实 —— 占位短语 (「调用方保证指针/类型有效 (详见上下文)」等) 照样通过,
且不计入债册. 即 **债册归零 ≠ 理由真实**. 本腿补的就是这层生态无对应物的内容面.

判据 (与 gate-health 册 ⑮ 终态读数同源, 机器派生侧车是唯一数量来源):
- 窄口径 (2 条): `调用方保证指针/类型有效 (详见上下文)` / `调用方保证指针合法`
- 宽口径 (8 条): `详见上下文` / `调用方保证指针` / `调用方保证` / `TODO` / `待补` /
  `占位` / `由调用方` / `保证安全`
窄口径是宽口径的子集 (前者的两个短语都含后者的短语) ⇒ 同一条注释可能同时计入两口径,
侧车分别记数, 不把宽口径当窄口径的替代读数.

扫描面 = `src/kernel` (排除 vendored `functions/net/smoltcp`), 含 `#[cfg(test)]` 模块.
内容判据是**注释文本本身**, 与是否编译无关, 故本腿是纯文本扫描, 不跑 clippy 七腿 cargo;
`src/user` / `host-tests` 两树的 SAFETY 内容面不在本册射程 (C2–C5 只改写 `src/kernel`),
若把它们并进来, 侧车存量将无对应改写批次可清零.

与 `audit_safety_debt.py` 同构的棘轮形态:
- 位点 = (仓内相对路径, 行, 口径); 跨文件/跨腿按位点去重后聚合成 (文件, 口径) 计数;
- 侧车只由 `--update-baseline` 写; 手改 `totals` 会与 `entries` 不自洽而 fail-closed;
- `_meta.criteria` 把判据清单冻进侧车: 改了短语却不重导 ⇒ RC=2, 不允许静默沿用旧口径;
- 双向对账: 当前 > 债册 = 新增 (拦), 当前 < 债册 = 快照过期 (须重导);
- 塌缩守卫: 读数 0 而债册非零, 且扫描面不可读 (0 文件或 0 条 SAFETY 标签) ⇒ 判疑似.
  本面是纯文本扫描, 没有「真编译」可作活性信号, 故以「扫描面非空」替代; 正常清零时
  扫描面仍非空, 守卫不误触发.

用法:
  python3 scripts/audit_safety_content.py                    # 与基线对账
  python3 scripts/audit_safety_content.py --update-baseline  # 重写基线 (唯一写者)
  python3 scripts/audit_safety_content.py --self-test        # 内置夹具 (不扫真仓库)
退出码: 0=与基线一致, 1=新增/快照过期, 2=取数失败/疑似读数塌缩/基线不可用 (fail-closed)
"""
import argparse
import json
import os
import re
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE_FILE = os.path.join(ROOT, "scripts", "audit_safety_content.baseline.json")
FACE_ROOT = os.path.join(ROOT, "src", "kernel")
# vendored 子树相对扫描根的路径: 与 audit_safety_debt 的射程边界一致 (非 workspace 成员).
VENDORED_SUBPATH = "functions/net/smoltcp"

NARROW_PHRASES = (
    "调用方保证指针/类型有效 (详见上下文)",
    "调用方保证指针合法",
)
WIDE_PHRASES = (
    "详见上下文",
    "调用方保证指针",
    "调用方保证",
    "TODO",
    "待补",
    "占位",
    "由调用方",
    "保证安全",
)
CALIBERS = ("narrow", "wide")

SAFETY_TAG = re.compile(r"^\s*//\s*SAFETY\s*:")
LINE_COMMENT = re.compile(r"^\s*//")


class ScanError(Exception):
    """取数无法完成 (扫描面不可读 / 文件读取失败)."""


def _repo_rel(path: str) -> str:
    """把绝对路径归一成仓内相对路径 (fixture 在临时目录时保留 .. 前缀, 仅用于自测)."""
    try:
        rel = os.path.relpath(os.path.realpath(path), ROOT)
    except ValueError:
        rel = os.path.relpath(path, ROOT)
    return rel.replace(os.sep, "/")


def iter_rs(root: str):
    """递归归集扫描面内的 .rs 文件; 跳过 vendored smoltcp 与 target 目录."""
    for dirpath, dirnames, filenames in os.walk(root):
        rel_dir = os.path.relpath(dirpath, root).replace(os.sep, "/")
        if rel_dir == VENDORED_SUBPATH or rel_dir.startswith(VENDORED_SUBPATH + "/"):
            dirnames[:] = []
            continue
        dirnames[:] = sorted(d for d in dirnames if d != "target")
        for name in sorted(filenames):
            if name.endswith(".rs"):
                yield os.path.join(dirpath, name)


def scan_file(path: str):
    """提取一个文件里的 SAFETY 注释块, 返回 [(起始行, 合并文本)].

    块 = `// SAFETY:` 标签行 + 紧随其后的连续 `//` 续行; 空行/非注释行/下一条
    `// SAFETY:` 都终止当前块. 续行里的占位短语同样计入 —— 只看标签行会漏掉
    「第一行写依据、第二行写占位」的真实形态.
    """
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines = handle.read().splitlines()
    except OSError as exc:
        raise ScanError(f"无法读取 {path}: {exc}") from exc

    blocks = []
    index = 0
    while index < len(lines):
        match = SAFETY_TAG.match(lines[index])
        if match is None:
            index += 1
            continue
        start = index + 1
        parts = [lines[index][match.end():].strip()]
        index += 1
        while index < len(lines):
            if SAFETY_TAG.match(lines[index]):
                break
            if not LINE_COMMENT.match(lines[index]):
                break
            parts.append(lines[index].lstrip().lstrip("/").strip())
            index += 1
        blocks.append((start, " ".join(part for part in parts if part)))
    return blocks


def classify(text: str) -> tuple:
    """返回 (是否窄口径命中, 是否宽口径命中); 宽口径显式包含窄口径."""
    narrow = any(phrase in text for phrase in NARROW_PHRASES)
    wide = narrow or any(phrase in text for phrase in WIDE_PHRASES)
    return narrow, wide


def run_scan(root: str = FACE_ROOT) -> tuple:
    """扫扫描面, 返回 (位点集合, 统计, 扫描面是否可读).

    位点 = (仓内相对路径, 行, 口径). 一行的注释同时命中两口径时记两个位点.
    """
    sites = set()
    stats = {"files": 0, "tags": 0, "narrow": 0, "wide": 0}
    for path in iter_rs(root):
        stats["files"] += 1
        rel = _repo_rel(path)
        for line, text in scan_file(path):
            stats["tags"] += 1
            narrow, wide = classify(text)
            if narrow:
                sites.add((rel, line, "narrow"))
                stats["narrow"] += 1
            if wide:
                sites.add((rel, line, "wide"))
                stats["wide"] += 1
    readable = stats["files"] > 0 and stats["tags"] > 0
    return sites, stats, readable


def aggregate(sites) -> dict:
    """位点并集 → {(文件, 口径): 计数}. 行号只用于去重, 不入侧车键."""
    entries: dict = {}
    for path, _line, caliber in sites:
        key = (path, caliber)
        entries[key] = entries.get(key, 0) + 1
    return entries


def totals_of(entries: dict) -> dict:
    """由条目自身求和的自洽校验位 (手改快照而不重跑 --update-baseline 即不自洽)."""
    return {"sites": sum(entries.values()),
            "files": len({path for path, _caliber in entries}),
            "keys": len(entries)}


def dump_baseline(entries: dict) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        "_meta": {
            "generated_by": "python3 scripts/audit_safety_content.py --update-baseline",
            "口径": "SAFETY 注释理由内容面: 窄口径 (2 条占位短语) + 宽口径 (8 条空泛短语), "
                    "按 (文件, 口径) 记数; 窄口径是宽口径的子集, 两口径分别记数不互相替代",
            "scan_face": "src/kernel (排除 vendored functions/net/smoltcp); 含 #[cfg(test)] "
                         "与全部 .rs; 内容判据不依赖编译, 故为纯文本扫描",
            "criteria": {"narrow": list(NARROW_PHRASES), "wide": list(WIDE_PHRASES)},
            "disposition": "只拦新增占位/空泛理由; 改写工程 C2–C5 逐批收缩本册, "
                           "两口径归零后由 --update-baseline 转硬零形态",
        },
        "entries": [[path, caliber, count]
                    for (path, caliber), count in sorted(entries.items())],
        "totals": totals_of(entries),
    }
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def load_baseline(path: str = None) -> tuple:
    """读快照并校验 totals 自洽与判据清单一致; 失败返回 (None, 原因)."""
    path = BASELINE_FILE if path is None else path
    if not os.path.exists(path):
        return None, f"基线快照缺失: {path}"
    try:
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
        entries = {}
        for row in payload["entries"]:
            file_name, caliber, count = row[0], row[1], int(row[2])
            if caliber not in CALIBERS:
                return None, f"基线快照含未知口径: {caliber}"
            entries[(file_name, caliber)] = count
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return None, f"基线快照无法解析: {exc}"

    declared = payload.get("totals", {})
    if declared != totals_of(entries):
        return None, (f"基线快照 totals 与 entries 不自洽: 声明 {declared} / "
                      f"实算 {totals_of(entries)} —— 手改过快照, 请重跑 --update-baseline")

    expected = {"narrow": list(NARROW_PHRASES), "wide": list(WIDE_PHRASES)}
    recorded = (payload.get("_meta") or {}).get("criteria")
    if recorded != expected:
        return None, ("基线快照的判据清单与脚本不一致 —— 判据已改, 须重跑 "
                      "--update-baseline 重钉, 不允许静默沿用旧口径")
    return entries, ""


def reconcile(entries: dict, baseline: dict) -> tuple:
    """双向对账: over=新增 (当前 > 债册), under=快照过期 (债册 > 当前)."""
    over = sorted((key, entries[key], baseline.get(key, 0))
                  for key in entries if entries[key] > baseline.get(key, 0))
    under = sorted((key, entries.get(key, 0), baseline[key])
                   for key in baseline if baseline[key] > entries.get(key, 0))
    return over, under


def collapse_suspected(total: int, baseline_total: int, readable: bool) -> bool:
    """读数塌缩守卫: 读数 0 而债册非零, 且扫描面不可读 (0 文件 / 0 条 SAFETY 标签).

    本面是纯文本扫描, 没有「真编译」可作活性信号; 正常清零时扫描面仍然可读,
    故「扫描面非空」是等价的活性判据. 疑似即 fail-closed, `--update-baseline`
    同样拦 (防把侧车一键洗成 0).
    """
    return total == 0 and baseline_total > 0 and not readable


def _self_test() -> int:
    """内置夹具自测 (不扫真仓库): 解析 / 续行 / 两口径 / 聚合并账 / 快照自洽 / 塌缩守卫."""
    checks = []

    def ck(label, cond, detail=""):
        checks.append((label, bool(cond), detail))

    def blocks_of(text):
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "fixture.rs")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
            return scan_file(path)

    # ① 两口径分辨: 窄口径占位 / 宽口径空泛 / 具体依据不判
    got = blocks_of("fn f() {\n"
                    "    // SAFETY: 调用方保证指针/类型有效 (详见上下文)\n"
                    "    unsafe { 0u8; }\n"
                    "}\n")
    ck("窄口径占位短语同时命中两口径",
       len(got) == 1 and classify(got[0][1]) == (True, True), str(got))
    got = blocks_of("// SAFETY: TODO 待补\nunsafe { }\n")
    ck("宽口径空泛短语只命中宽口径",
       len(got) == 1 and classify(got[0][1]) == (False, True), str(got))
    got = blocks_of("// SAFETY: 缓冲区由 pmm 在 alloc_zeroed 中清零, 长度取自页表项\n"
                    "unsafe { }\n")
    ck("指向具体实装依据的注释不判",
       len(got) == 1 and classify(got[0][1]) == (False, False), str(got))

    # ② 续行: 占位短语写在第二行仍计入; 空行终止当前块
    got = blocks_of("// SAFETY: 指针来自 mm::pmm 的 alloc\n"
                    "// 调用方保证指针合法\n")
    ck("续行里的占位短语被合并识别",
       len(got) == 1 and classify(got[0][1]) == (True, True), str(got))
    got = blocks_of("// SAFETY: 指针来自 mm::pmm 的 alloc\n"
                    "\n"
                    "// 调用方保证指针合法\n")
    ck("空行终止块, 后续孤立行不并入前一条",
       len(got) == 1 and classify(got[0][1]) == (False, False), str(got))

    # ③ 连续两条 SAFETY 各记一块 (批 5 的重复注释形态不得被合并吃掉一条)
    got = blocks_of("// SAFETY: 指针来自 pmm\n"
                    "// SAFETY: 调用方保证指针合法\n")
    ck("连续两条 SAFETY 各记一块",
       len(got) == 2 and classify(got[1][1]) == (True, True), str(got))

    # ④ 文档注释与大小写变体不是 SAFETY 标签
    got = blocks_of("/// # Safety\n/// 调用方保证指针合法\n")
    ck("文档注释 # Safety 不计入标签面", not got, str(got))

    # ⑤ cfg(test) 模块在扫描面内 (文本扫描不做编译剔除, 口径须与侧车一致)
    got = blocks_of("#[cfg(test)]\nmod tests {\n"
                    "    // SAFETY: 调用方保证指针合法\n"
                    "    unsafe { }\n}\n")
    ck("#[cfg(test)] 模块内的 SAFETY 同样计入",
       len(got) == 1 and classify(got[0][1]) == (True, True), str(got))

    # ⑥ vendored smoltcp 排除面
    with tempfile.TemporaryDirectory() as tmpdir:
        vendored = os.path.join(tmpdir, *VENDORED_SUBPATH.split("/"))
        os.makedirs(vendored)
        with open(os.path.join(vendored, "vendored.rs"), "w", encoding="utf-8") as handle:
            handle.write("// SAFETY: 调用方保证指针合法\n")
        with open(os.path.join(tmpdir, "keep.rs"), "w", encoding="utf-8") as handle:
            handle.write("// SAFETY: 指针来自 mm::pmm\n")
        keep_rel = _repo_rel(os.path.join(tmpdir, "keep.rs"))
        sites, stats, readable = run_scan(tmpdir)
    ck("vendored smoltcp 不在扫描面",
       stats["files"] == 1 and stats["tags"] == 1 and not sites,
       f"{stats} / keep={keep_rel} / sites={sites}")
    ck("扫描面非空即可读", readable, str(stats))

    # ⑦ 聚合 / 双向对账 / 行移不变
    sites = {("src/kernel/a.rs", 10, "narrow"), ("src/kernel/a.rs", 10, "wide"),
             ("src/kernel/a.rs", 20, "wide")}
    entries = aggregate(sites)
    ck("聚合按 (文件, 口径) 计数",
       entries == {("src/kernel/a.rs", "narrow"): 1,
                   ("src/kernel/a.rs", "wide"): 2}, str(entries))
    base = {("src/kernel/a.rs", "wide"): 2}
    over, under = reconcile(entries, base)
    ck("新增判 over", [(k[1], cur, old) for k, cur, old in over]
       == [("narrow", 1, 0)] and not under, str((over, under)))
    over, under = reconcile({("src/kernel/a.rs", "wide"): 1}, base)
    ck("减少判 under (须重导)", not over and len(under) == 1, str((over, under)))
    shifted = aggregate({("src/kernel/a.rs", 99, "wide"),
                         ("src/kernel/a.rs", 100, "wide")})
    over, under = reconcile(shifted, base)
    ck("行号推移不产生假新增/假过期", not over and not under, str((over, under)))

    # ⑧ 快照自洽: 正常 / totals 被手改 / 判据清单被换 / 坏 json / 缺失
    text = dump_baseline(base)
    ck("序列化幂等", text == dump_baseline(dict(base)))
    ck("totals 由条目求和", json.loads(text)["totals"] == totals_of(base))
    with tempfile.TemporaryDirectory() as tmpdir:
        path = os.path.join(tmpdir, "b.json")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        got, err_msg = load_baseline(path)
        ck("正常快照可读", got == base and not err_msg, err_msg)
        payload = json.loads(text)
        payload["totals"]["sites"] = 999
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
        got, err_msg = load_baseline(path)
        ck("totals 不自洽 ⇒ fail-closed", got is None and "不自洽" in err_msg, err_msg)
        payload = json.loads(text)
        payload["_meta"]["criteria"]["narrow"][0] = "被改掉的判据"
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
        got, err_msg = load_baseline(path)
        ck("判据清单与脚本不一致 ⇒ fail-closed",
           got is None and "判据清单" in err_msg, err_msg)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("{ 坏 json")
        got, err_msg = load_baseline(path)
        ck("坏 json ⇒ fail-closed", got is None and "无法解析" in err_msg, err_msg)
        got, err_msg = load_baseline(os.path.join(tmpdir, "缺失.json"))
        ck("快照缺失 ⇒ fail-closed", got is None and "缺失" in err_msg, err_msg)

    # ⑨ 塌缩守卫
    ck("读数0+债册非零+扫描面不可读 ⇒ 疑似", collapse_suspected(0, 443, False))
    ck("扫描面可读 ⇒ 不判疑似", not collapse_suspected(0, 443, True))
    ck("债册也为0 ⇒ 不判疑似 (真清零)", not collapse_suspected(0, 0, False))
    ck("读数非零 ⇒ 不判疑似", not collapse_suspected(5, 443, False))

    print("=== audit_safety_content --self-test (内置夹具, 不扫真仓库) ===")
    failed = 0
    for label, ok, detail in checks:
        print(f"  {'✓' if ok else '✗'} {label}" + (f" — {detail}" if not ok else ""))
        failed += 0 if ok else 1
    print(f"结果: {len(checks) - failed}/{len(checks)} 通过")
    return 1 if failed else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="F4 SAFETY 注释理由内容面存量棘轮")
    parser.add_argument("--update-baseline", action="store_true", help="按当前读数重写快照")
    parser.add_argument("--self-test", action="store_true", help="内置夹具自测 (不扫真仓库)")
    args = parser.parse_args(argv)

    if args.self_test:
        return _self_test()

    try:
        sites, stats, readable = run_scan()
    except ScanError as exc:
        print(f"✗ audit_safety_content: {exc} (fail-closed, 不接受假绿)")
        return 2

    entries = aggregate(sites)
    total = totals_of(entries)
    narrow_files = len({path for path, caliber in entries if caliber == "narrow"})
    wide_files = len({path for path, caliber in entries if caliber == "wide"})
    print("=== audit_safety_content: SAFETY 注释理由内容面 (纯文本扫描) ===")
    print("  扫描面: src/kernel (排除 vendored smoltcp); 含 #[cfg(test)]")
    print(f"  文件 {stats['files']} / SAFETY 标签 {stats['tags']} / "
          f"窄口径 {stats['narrow']} 处 / 宽口径 {stats['wide']} 处")
    print(f"  窄口径: {narrow_files} 文件 / "
          f"{sum(1 for _p, c in entries if c == 'narrow')} 键")
    print(f"  宽口径: {wide_files} 文件 / "
          f"{sum(1 for _p, c in entries if c == 'wide')} 键")

    if not readable:
        print("✗ 扫描面不可读 (0 文件或 0 条 SAFETY 标签) —— 读数不可信 "
              "(fail-closed, --update-baseline 同样拦)")
        return 2

    baseline, err_msg = load_baseline()
    if baseline is None:
        if args.update_baseline:
            # 首次生成: 无快照可对账, 但塌缩守卫仍须先判 (防把 0 冻成基线).
            if collapse_suspected(total["sites"], 0, readable):
                print("✗ 疑似读数塌缩, 拒绝生成基线 (fail-closed)")
                return 2
            with open(BASELINE_FILE, "w", encoding="utf-8") as handle:
                handle.write(dump_baseline(entries))
            print(f"\n基线快照已生成: {os.path.basename(BASELINE_FILE)} "
                  f"({total['sites']} 处 / {total['keys']} 键)")
            return 0
        print(f"✗ {err_msg} (fail-closed, 不接受假绿)")
        return 2

    base_total = totals_of(baseline)["sites"]
    if collapse_suspected(total["sites"], base_total, readable):
        print(f"✗ 疑似读数塌缩: 并集 0 处而债册 {base_total} 处, 且扫描面不可读 "
              f"⇒ 判据可能未生效, 读数不可信 (fail-closed, --update-baseline 同样拦)")
        return 2

    if args.update_baseline:
        with open(BASELINE_FILE, "w", encoding="utf-8") as handle:
            handle.write(dump_baseline(entries))
        print(f"\n基线快照已重写: {os.path.basename(BASELINE_FILE)} "
              f"({total['sites']} 处 / {total['keys']} 键)")
        return 0

    over, under = reconcile(entries, baseline)
    print(f"  债册: {base_total} 处 / {len(baseline)} 键 "
          f"({os.path.basename(BASELINE_FILE)})")
    if not over and not under:
        if base_total == 0:
            print("✓ 硬零门禁: 窄/宽口径均 0 处, 债册亦空 —— "
                  "新增占位/空泛理由即红, 无存量可豁免")
        else:
            print(f"✓ 内容面读数与债册一致 ({total['sites']} 处待改写; "
                  "新增占位/空泛理由即红, 改写后需重跑 --update-baseline)")
        return 0
    for (path, caliber), cur, base in over[:15]:
        print(f"    ✗ 新增 {path} — {caliber}: 当前 {cur} / 债册 {base}")
    for (path, caliber), cur, base in under[:15]:
        print(f"    ✗ 债册过期 {path} — {caliber}: 当前 {cur} / 债册 {base}")
    if len(over) + len(under) > 30:
        print(f"    ... 新增 {len(over)} 键 / 减少 {len(under)} 键")
    print("新增 ⇒ 把占位/空泛理由改写为具体实装依据, 而不是扩大债册; "
          "减少 ⇒ 确属改写, 跑 --update-baseline 重导快照.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
