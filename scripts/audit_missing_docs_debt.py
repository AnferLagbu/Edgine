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
- 只统计 kernel crate 树内文件; vendored `smoltcp` 按路径组件排除 (锁定不可改);
- 判"文档存在"而不判"文档是中文" —— 中文语言面仍由 `audit_public_api_docs.py` 判;
- 读数恒为真编译: 存量未清零时 `-Dmissing_docs` 让该 unit 每次都编译失败, cargo 不
  判 Fresh ⇒ 不会出现"缓存导致 0 读数"的假绿. 存量归零后 unit 转 Fresh, 此时任何新增
  未文档项都会改源码 ⇒ 重新编译 ⇒ 照样转红;
- 绕过面: 在文件里写 `#![allow(missing_docs)]` 会让读数下降 (须重导快照才不红),
  本腿只拦"静默新增"; `audit_dead_code_comment.py` 的 F9 族清单只含 dead_code/unused
  与 `clippy::` 抑制, **不含任意 rustc lint 抑制**, 故该形态当前无人拦 —— 已立案 P-67.

用法:
  python3 scripts/audit_missing_docs_debt.py                    # 与基线对账
  python3 scripts/audit_missing_docs_debt.py --update-baseline  # 重写基线 (唯一写者)
退出码: 0=与基线一致, 1=新增或快照过期, 2=取数失败/编译失败/基线不可用 (fail-closed)
"""
import argparse
import json
import os
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
# 本腿唯一判据的 lint; 其它诊断只用于识别"编译失败"
LINT = "missing_docs"


class ScanError(Exception):
    """取数无法完成 (cargo 缺失 / 编译失败 / 输出不可解析)."""


def run_compiler() -> dict:
    """跑一次 kernel crate 的 clippy, 返回 {(相对路径, kind): 计数} 与旁证."""
    cmd = [
        "cargo", "clippy",
        "--manifest-path", "../kernel/Cargo.toml",
        "--release", "--lib",
        "--target", "x86_64-unknown-none",
        "--target-dir", DEBT_TARGET_DIR,
        *BUILD_STD_CFG,
        "--message-format=json",
        "--", f"-D{LINT}",
    ]
    try:
        proc = subprocess.run(cmd, cwd=CARGO_CWD, capture_output=True, text=True, timeout=3600)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ScanError(f"取数命令无法执行: {exc}") from exc

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
        raise ScanError(f"clippy 失败且无 missing_docs 读数 (exit={proc.returncode}): "
                        f"{(proc.stderr or '')[-300:]}")
    if hard_errors:
        raise ScanError(f"存在 {hard_errors} 条非 missing_docs 的 error 级诊断, "
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
            "口径": "rustc missing_docs 在 kernel crate x86_64 --lib 单维的读数, "
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
