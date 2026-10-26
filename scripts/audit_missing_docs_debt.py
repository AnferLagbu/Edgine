#!/usr/bin/env python3
r"""
audit_missing_docs_debt.py — F8 编译器口径 (rustc `missing_docs`) 的存量债册与棘轮

P-59 裁定 ①: F8「公共 API 中文文档」的**存在性**由 rustc `missing_docs` 作准,
不由审计脚本的自定义口径作准 (旧文档写的 `clippy::missing_docs_in_crate_items`
是不存在的 lint 名; 真名 `clippy::missing_docs_in_private_items` 属 restriction 组,
门槛从不启用 ⇒ kernel crate 的编译器执法长期为零).

本腿做两件事:
1. 以 `cargo clippy … -- -D missing_docs` 取 kernel crate 的编译器读数 (唯一尺);
2. 与机器派生侧车 `audit_missing_docs_debt.baseline.json` 双向对账 ——
   任一 (文件, kind) 计数**上升**即红 (新增未文档的公共项),
   计数**下降**而未重导快照也即红 (快照过期, 须跑 --update-baseline 确认).

"存量不冻结"的落地形态 (用户裁定): 侧车是**待办清单**不是豁免面. 每完成一批补写,
该模块内加 `#![deny(missing_docs)]` 转为无豁免的硬执法 (由现有门槛 1/2 直接生效),
本腿的读数随之下降. 分批工程见 P-66.

射程边界 (本腿自述, 不等于"全仓 F8 已执法"):
- 只跑 x86_64 `--lib` 一维 (aarch64 维与 bins/examples 未纳入);
- 只统计 src/kernel 树内**三个 workspace 成员** (kernel / kernel-privileged / kernel-functions) 的文件; vendored `smoltcp` 按路径组件排除 (锁定不可改);
- 判"文档存在"而不判"文档是中文" —— 中文语言面仍由 `audit_public_api_docs.py` 判;
- 读数恒为真编译: 存量未清零时 `-Dmissing_docs` 让该 unit 每次都编译失败, cargo 不
  判 Fresh ⇒ 不会出现"缓存导致 0 读数"的假绿. 存量归零后 unit 转 Fresh, 此时任何新增
  未文档项都会改源码 ⇒ 重新编译 ⇒ 照样转红;
- 绕过面: 在文件里写 `#![allow(missing_docs)]` 会让读数下降 (重导快照即复绿), 与真补写
  在门禁里同形. F9 的 `audit_dead_code_comment.py` 族清单只含 dead_code/unused 与
  `clippy::` 抑制, **不含任意 rustc lint 抑制** ⇒ 本腿自带这一盏灯的定点检查 (P-67 乙):
  kernel 树内出现 `allow/expect(missing_docs)` 即在取数之前转红 (零耗时); `deny/forbid`
  是合法硬执法不判. 本腿仍只保 missing_docs; 其它 rustc lint 的抑制是否入 F9 射程属
  P-53/P-64 的既有裁定面, 不在本条扩围.

用法:
  python3 scripts/audit_missing_docs_debt.py                    # 与基线对账
  python3 scripts/audit_missing_docs_debt.py --update-baseline  # 重写基线 (唯一写者)
退出码: 0=与基线一致, 1=新增/快照过期/出现 missing_docs 抑制, 2=取数失败/编译失败/基线不可用 (fail-closed)
"""
import argparse
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE_FILE = os.path.join(ROOT, "scripts", "audit_missing_docs_debt.baseline.json")
CARGO_CWD = os.path.join(ROOT, "src", "rust")
BUILD_STD_CFG = [
    '--config', 'unstable.build-std=["core","compiler_builtins","alloc"]',
    '--config', 'unstable.build-std-features=["compiler-builtins-mem"]',
]
# 本腿专用 target 目录, 不与 `other/target` 主目录混用: 尾旗标 `-Dmissing_docs` 会
# 参与 cargo 的 unit fingerprint, 借主目录跑一次就把门槛 1/2 的全部产物判成 Dirty
# (实测主目录门槛形 clippy 被搅脏后重新编译 kernel lib). 隔离目录只付自己的冷启动
# (实测 30.2s 一次, 之后每跑 7.3s, 143M), 不污染别的腿与门槛.
DEBT_TARGET_DIR = "../../other/target/audit-missing-docs"
# 每个取数成员再各用**独立子目录** (P-71). 单目录时会把拆出来的 crate 交叉污染:
# 取 functions / kernel 时 privileged 会作为**依赖**被普通 rustc 编一次 (不带
# `-Dmissing_docs`), 其产物落进同一目录; 下一轮 `-p kernel-privileged` 命中该
# 指纹即判 Fresh —— cargo 对 Fresh unit **不复播**诊断, 读数凭空归零
# (实测 2430 处 → 0). 分目录后每个成员只在本包目录里作为 primary 编译:
# 它带 `-D` 且存量债非零 ⇒ 编译失败 ⇒ 永不进缓存 ⇒ 每次都是真读数.
# `cargo clean -p` 不能替代本修法 (实测 `Removed 0 files`, 该布局下不生效).
# 本腿唯一判据的 lint; 其它诊断只用于识别"编译失败"
LINT = "missing_docs"
# 取数对象: `src/kernel` workspace 的全部成员 (P-71 crate 拆分后为三个). 顺序 =
# 先 TCB 后 functions 再 glue —— kernel 会以普通 rustc 编译两个子 crate 作为依赖,
# 与前一包共用同一 target 目录, 按依赖序取数可复用已编产物.
PACKAGES = ("kernel-privileged", "kernel-functions", "kernel")

# ── P-67: 抑制面定点检查 (在取数之前, 零耗时) ────────────────────
# 债册只读编译器 ⇒「真补写」与「关掉这盏灯」都只表现为读数下降, 重导快照即可复绿.
# F9 的族清单不含一般 rustc lint, 故这一盏灯由唯一关心它的腿自己看住.
# 只判 allow/expect: deny/forbid 是 P-59 裁定 ④ 要求的模块级硬执法, 属合法写法.
SCAN_ROOT = os.path.join(ROOT, "src", "kernel")
VENDORED_DIRS = ("smoltcp",)  # vendored 锁定不可改, 按路径组件排除
# 属性整段 `#[...]` / `#![...]` (本仓属性实参不嵌方括号; cfg_attr 的圆括号由下一正则覆盖)
ATTRIBUTE_RE = re.compile(r"#!?\[([^\]]*)\]")
SUPPRESSION_RE = re.compile(r"\b(allow|expect)\s*\(([^()]*)\)")
STRING_RE = re.compile(r'"(?:[^"\\]|\\.)*"')  # reason = "…" 里的词不是抑制


class ScanError(Exception):
    """取数无法完成 (cargo 缺失 / 编译失败 / 输出不可解析)."""


def scan_suppressions() -> list:
    """P-67: 扫 kernel 树里的 `allow/expect(missing_docs)`, 返回「路径:行号 片段」列表.

    扫描根不存在或读取失败一律抛 OSError, 由 main 走 fail-closed (return 2) ——
    与取数失败的处置同语义, 不允许「扫不动就当没抑制」.
    """
    if not os.path.isdir(SCAN_ROOT):
        raise OSError(f"扫描根不存在: {os.path.relpath(SCAN_ROOT, ROOT)}")
    hits = []
    for dirpath, dirnames, filenames in os.walk(SCAN_ROOT):
        dirnames[:] = sorted(d for d in dirnames if d not in VENDORED_DIRS)
        for name in sorted(filenames):
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, ROOT).replace(os.sep, "/")
            with open(path, encoding="utf-8", errors="replace") as f:
                text = f.read()
            for attr in ATTRIBUTE_RE.finditer(text):
                body = STRING_RE.sub('""', attr.group(1))
                sup = SUPPRESSION_RE.search(body)
                if not sup:
                    continue
                lints = {t.strip() for t in sup.group(2).split(",")}
                if LINT not in lints:
                    continue
                line = text.count("\n", 0, attr.start()) + 1
                hits.append(f"{rel}:{line}  {' '.join(attr.group(0).split())[:120]}")
    return hits


def run_compiler() -> dict:
    """跑 workspace 各成员的 clippy, 合并 {(相对路径, kind): 计数}."""
    entries: dict = {}
    for pkg in PACKAGES:
        for key, cnt in _run_clippy_for(pkg).items():
            if key in entries:
                # 各成员源树互斥, 同一 (文件, 类型) 同时出现在两个包里只可能是
                # 路径归一出了岔子 ⇒ 读数不可信, fail-closed.
                raise ScanError(f"读数归属歧义: {key} 同时来自多个 workspace 成员")
            entries[key] = cnt
    return entries


def _target_dir_for(package: str) -> str:
    """该成员的隔离 target 目录 (见 DEBT_TARGET_DIR 下方的分目录理由)."""
    return f"{DEBT_TARGET_DIR}/{package}"


def _run_clippy_for(package: str) -> dict:
    """跑单个 workspace 成员的 clippy, 返回 {(相对路径, kind): 计数}."""
    cmd = [
        "cargo", "clippy",
        "--manifest-path", "../kernel/Cargo.toml",
        "-p", package,
        # P-71: privileged / functions 已拆为独立 crate. 每个 crate 各自有存量债, 而
        # `-Dmissing_docs` 让每个 unit 都以 error 收尾; 若一次 `--workspace` 取数,
        # 依赖方 kernel-privileged / kernel-functions 失败即令 kernel 不可编译
        # (依赖失败 ⇒ 依赖方不编, `--keep-going` 也救不了), 读数只剩 TCB, functions
        # 与 glue 树凭空消失. 故分次取数, 并用 `--no-deps` 把 `-D` 关在本包内
        # (依赖只按普通 rustc 编译).
        "--no-deps",
        "--release", "--lib",
        "--target", "x86_64-unknown-none",
        "--target-dir", _target_dir_for(package),
        *BUILD_STD_CFG,
        "--message-format=json",
        "--", f"-D{LINT}",
    ]
    try:
        proc = subprocess.run(cmd, cwd=CARGO_CWD, capture_output=True, text=True, timeout=3600)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ScanError(f"取数命令无法执行 ({package}): {exc}") from exc

    entries: dict = {}
    hard_errors = 0
    seen_any_message = False
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if obj.get("reason") != "compiler-message":
            continue
        msg = obj.get("message") or {}
        code = (msg.get("code") or {}).get("code") if isinstance(msg.get("code"), dict) else None
        spans = msg.get("spans") or []
        rendered = (msg.get("rendered") or "").strip()
        if code != LINT:
            if msg.get("level") == "error" and "could not compile" not in rendered:
                hard_errors += 1
            continue
        seen_any_message = True
        if not spans:
            raise ScanError("missing_docs 诊断无 span 位置, 读数不可信")
        raw = spans[0].get("file_name") or ""
        rel = _to_repo_rel(raw)
        if rel is None:
            continue  # crate 外来源 (依赖/标准库) 不计入本债册
        if "(smoltcp)" in rel or "/smoltcp/" in rel:
            continue  # vendored 锁定, 不可改
        entries[(rel, _kind_of(rendered))] = entries.get((rel, _kind_of(rendered)), 0) + 1

    if not seen_any_message and proc.returncode != 0:
        raise ScanError(f"{package}: clippy 失败且无 missing_docs 读数 "
                        f"(exit={proc.returncode}): {(proc.stderr or '')[-300:]}")
    if hard_errors:
        raise ScanError(f"{package}: 存在 {hard_errors} 条非 missing_docs 的 error 级诊断, "
                        f"编译未走通 ⇒ 读数不可信 (fail-closed)")
    return entries


def _to_repo_rel(raw: str):
    """把 rustc 给的绝对/相对路径归一成仓内相对路径; 非 kernel 树返回 None."""
    path = raw if os.path.isabs(raw) else os.path.join(ROOT, "src", "kernel", raw)
    try:
        rel = os.path.relpath(os.path.realpath(path), ROOT)
    except ValueError:
        return None
    if rel.startswith(".."):
        return None
    if not (rel.startswith("src/kernel/") or rel.startswith("src\\kernel\\")):
        return None
    return rel.replace(os.sep, "/")


def _kind_of(rendered: str) -> str:
    marker = "missing documentation for "
    idx = rendered.find(marker)
    if idx < 0:
        return "未知项"
    tail = rendered[idx + len(marker):].split("\n")[0].strip().rstrip(":")
    return tail or "未知项"


def totals_of(entries: dict) -> dict:
    """由条目自身求和的自洽校验位 (手改快照而不重跑 --update-baseline 即不自洽)."""
    return {"sites": sum(entries.values()),
            "files": len({f for f, _ in entries}),
            "keys": len(entries)}


def dump_baseline(entries: dict) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        "_meta": {
            "generated_by": "python3 scripts/audit_missing_docs_debt.py --update-baseline",
            "口径": "rustc missing_docs 在 src/kernel workspace 三个成员 "
                    "(kernel + kernel-privileged + kernel-functions) x86_64 --lib 单维的读数, "
                    "按 (文件, 项类型) 记数; vendored smoltcp 排除; bins/examples 与 aarch64 维不在射程",
            "disposition": "P-59 裁定 ①: 待办清单而非豁免面 —— 每批补写完成后该模块加 "
                           "#![deny(missing_docs)] 转硬执法, 清零工程归 P-66",
        },
        "totals": totals_of(entries),
        "entries": [[f, k, c] for (f, k), c in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + "\n"


def load_baseline():
    """读快照; 读不到/格式坏/totals 不自洽一律 fail-closed."""
    if not os.path.isfile(BASELINE_FILE):
        return None, (f"基线快照缺失: {os.path.basename(BASELINE_FILE)} "
                      f"(须显式跑 --update-baseline 生成并提交)")
    try:
        with open(BASELINE_FILE, encoding="utf-8") as f:
            payload = json.load(f)
        entries = {(e[0], e[1]): int(e[2]) for e in payload["entries"]}
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return None, f"基线快照无法解析: {exc}"
    declared = payload.get("totals", {})
    if declared != totals_of(entries):
        return None, (f"基线快照 totals 与 entries 不自洽: 声明 {declared} / "
                      f"实算 {totals_of(entries)} —— 手改过快照, 请重跑 --update-baseline")
    return entries, ""


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="F8 编译器口径存量债册 (rustc missing_docs)")
    parser.add_argument("--update-baseline", action="store_true", help="按当前读数重写快照")
    args = parser.parse_args(argv)

    # P-67: 先判抑制面再取数 —— 有 allow/expect(missing_docs) 时读数下降与真补写同形,
    # 且此时跑编译器也只会给出一个「更绿」的读数, 故零耗时转红, 连 --update-baseline 也拦.
    try:
        suppressions = scan_suppressions()
    except OSError as exc:
        print(f"✗ audit_missing_docs_debt: 抑制扫描失败: {exc} (fail-closed, 不接受假绿)")
        return 2
    if suppressions:
        print("=== audit_missing_docs_debt: F8 抑制检查 (P-67) ===")
        for item in suppressions[:15]:
            print(f"    ✗ {item}")
        if len(suppressions) > 15:
            print(f"    ... 共 {len(suppressions)} 处")
        print("allow/expect(missing_docs) 让编译器读数下降 ⇒ 与真补写同形, 重导快照即可绕过."
              "请补文档; 要模块级硬执法就写 #![deny(missing_docs)], 不要关灯.")
        return 1
    print(f"✓ P-67 抑制检查: 无 allow/expect({LINT})(deny/forbid 属硬执法, 不判)")

    try:
        entries = run_compiler()
    except ScanError as exc:
        print(f"✗ audit_missing_docs_debt: {exc} (fail-closed, 不接受假绿)")
        return 2

    total = totals_of(entries)
    by_kind: dict = {}
    for (_f, kind), cnt in entries.items():
        by_kind[kind] = by_kind.get(kind, 0) + cnt
    print("=== audit_missing_docs_debt: rustc missing_docs 编译器口径读数 ===")
    print(f"  读数: {total['sites']} 处 / {total['files']} 文件 / {total['keys']} 键 "
          f"(x86_64 --lib 单维, 不含 vendored smoltcp)")
    for kind, cnt in sorted(by_kind.items(), key=lambda x: -x[1]):
        print(f"    {kind}: {cnt}")

    if args.update_baseline:
        with open(BASELINE_FILE, "w", encoding="utf-8") as f:
            f.write(dump_baseline(entries))
        print(f"\n基线快照已重写: {os.path.basename(BASELINE_FILE)} "
              f"({total['sites']} 处 / {total['keys']} 键)")
        return 0

    baseline, err_msg = load_baseline()
    if baseline is None:
        print(f"✗ {err_msg} (fail-closed, 不接受假绿)")
        return 2

    over = sorted((key, entries[key], baseline.get(key, 0))
                  for key in entries if entries[key] > baseline.get(key, 0))
    under = sorted((key, baseline[key], entries.get(key, 0))
                   for key in baseline if baseline[key] > entries.get(key, 0))
    print(f"  债册: {totals_of(baseline)['sites']} 处 / {len(baseline)} 键 "
          f"({os.path.basename(BASELINE_FILE)})")
    if not over and not under:
        print(f"✓ 编译器口径与债册一致 ({total['sites']} 处待补写; "
              f"新增未文档公共项即红, 补写后需重跑 --update-baseline)")
        return 0
    for (path, kind), cur, base in over[:15]:
        print(f"    ✗ 新增 {path} — {kind}: 当前 {cur} / 债册 {base}")
    for (path, kind), cur, base in under[:15]:
        print(f"    ✗ 债册过期 {path} — {kind}: 当前 {cur} / 债册 {base}")
    if len(over) + len(under) > 30:
        print(f"    ... 新增 {len(over)} 键 / 减少 {len(under)} 键")
    print("新增 ⇒ 补该公共项的文档注释, 而不是扩大债册; "
          "减少 ⇒ 确属补写, 跑 --update-baseline 重导快照.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
