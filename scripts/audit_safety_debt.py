#!/usr/bin/env python3
r"""
audit_safety_debt.py — F4「SAFETY 注释」的 clippy 口径存量债册与棘轮 (D-5 乙)

门禁健康度收敛工程批 3。用户裁定 D-5 乙: SAFETY 面交 clippy
`undocumented_unsafe_blocks` 检测, 存量**冻成待办清单**而不是立即补完 ——
立即补 399 处会产出「调用方保证指针/类型有效」式占位理由, 把自建脚本
「100% 覆盖」的虚高读数重新灌满. 故本腿只拦**新增**, 清零工程分批 (批 5a–5g).

**现状 (批 5g 收口)**: 批 5a–5g 已把四树存量全部补写完毕, 侧车 `totals` 归 0,
本腿由「存量棘轮」升为**硬零门禁** —— 债册为空 ⇒ 任何新增位点都是 `over` ⇒ 当场红,
与 F9 侧车清零后的处置同构. 三树的主 clippy 腿另已显式 `-D` 点名本 lint
(src/user 批5b / host-tests 批5c / 内核双架构 批5g), 故 quick 档即拦; 本腿保留的
独有价值是**七腿位点并集** (含 2b feature 维之外的壳 / edgine-abi 两腿) 与
「读数塌缩」守卫 (全 Fresh 且读数 0 时判疑似而不是放行).

与自建腿的分工 (批 4 收敛后):
- 本腿 = `unsafe {}` 块 + `unsafe impl` 的 SAFETY 注释 (clippy 判据, 宽严见下);
- `audit_safety_coverage.py` / `tools/audit_unsafe.py` 保留 **extern 块**面
  (clippy 两个 lint 都不报 `unsafe extern`, 属生态缺口) 与其它形态;
- pub `unsafe fn` / `unsafe trait` 的 `/// # Safety` 文档面是另一个 lint
  (`missing_safety_doc`), **不在本腿射程** (本腿只启用一个 lint, 见下).

取数形态 (三条实测约束, 改动前先复现):
1. 尾旗标**只** `-W clippy::undocumented_unsafe_blocks`, 不带 `-D warnings`
   也不带 pedantic —— `-D warnings` 会把显式 `-W` 的 lint 提升为 error (L2),
   棘轮就变成硬阻断而非取数; 只 `-W` 时 RC=0 且诊断照出 (L3).
2. 每腿**独立隔离 target 目录**: 尾旗标参与 cargo 的 unit fingerprint, 借主
   `other/target` 跑一次就把门槛 1/2 的产物判成 Dirty (同
   `audit_missing_docs_debt.py` 的隔离理由).
3. 七腿镜像 `ci/audit.sh` 步骤 2 的编译面 (内核双架构 / host-tests
   `--all-targets` / src/user 双裸机三元组 `--workspace` 且**不得**
   `--all-targets` / 壳 / edgine-abi), 一次 `--workspace` 取全树即可:
   本工具链对 Fresh unit **复播**诊断 (L5), 故不必逐包分次.

键的口径: 跨腿先按**位点** (仓内相对路径, 行, 形态) 去重取并集 (同一处被
x86_64 与 aarch64 两腿各报一次只记一次), 再聚合成 **(文件, 形态) 计数**入侧车.
不拿行号当侧车键 —— 补一条 `// SAFETY:` 注释就会把该文件后续所有行号推移,
行号键会让批 5 每补一处都冒出成百上千条假新增/假过期. 计数键对行移不变,
且仍满足「新增即红、减少须重导」的棘轮语义.

射程边界 (本腿自述, 不等于「全仓 F4 已执法」):
- vendored 子树 (`functions/net/smoltcp` 是 workspace `exclude` 的路径依赖,
  `src/user/vendor` 是目录源替换) **非 workspace 成员**, cargo 自动压其 lint
  上限 ⇒ 诊断天然不出, 无需人工排除清单;
- 判「SAFETY 注释存在」而不判「注释是中文」或「理由是否成立」—— 语言面与理由质量
  不在任何机器判据内. 注释位置的判据**比「紧邻上一行」宽** (实测): 块前注释串允许
  空行间隔 (`privileged/mm/copy_user.rs` 四处「占位 SAFETY + 空行 + 块」均不报).
  语句级位点中间隔 `#[cfg(...)]` 属性**也认** (批5g 实测四例: 注释在属性上方 /
  属性与 `unsafe {` 之间 / `let … = unsafe {` 的两种同形位置, 均不报; 无注释的
  对照组照常报). 但 **item 级不认**: 把 SAFETY 写在 `#[unsafe(no_mangle)]` 等属性
  之前, 注释归属该 item 而非其体内的块, 块仍进债册 (批5g 在 glue/string.rs 与
  klog/mod.rs 见到的「属性前已有 SAFETY 却仍欠债」即此形态). 批 5 要求「紧邻上一行」
  是**项目约定**, 严于本 lint, 不是 lint 的判据;
- **抑制面不重复扫**: 关掉这盏灯 (`allow/expect(clippy::undocumented_unsafe_blocks)`)
  会让读数下降而与真补写同形, 但该面已由 F9 的 `audit_dead_code_comment.py` 执法 ——
  其侧车 `totals` 实测为 0 (clippy 与一般 rustc lint 抑制面已硬零), 新增抑制当场红.
  故本腿**依赖那一层**, 不自建第二盏灯 (与 `audit_missing_docs_debt.py` 自带 P-67
  定点检查的处置不同: 那里 F9 族清单不含一般 rustc lint, 必须自看).

用法:
  python3 scripts/audit_safety_debt.py                    # 与基线对账
  python3 scripts/audit_safety_debt.py --update-baseline  # 重写基线 (唯一写者)
  python3 scripts/audit_safety_debt.py --self-test        # 内置夹具 (不跑 cargo)
退出码: 0=与基线一致, 1=新增/快照过期, 2=取数失败/疑似读数塌缩/基线不可用 (fail-closed)
"""
import argparse
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASELINE_FILE = os.path.join(ROOT, "scripts", "audit_safety_debt.baseline.json")
LINT = "clippy::undocumented_unsafe_blocks"
DEBT_TARGET_ROOT = "other/target/audit-safety-debt"
BUILD_STD_CFG = [
    "--config", 'unstable.build-std=["core","compiler_builtins","alloc"]',
    "--config", 'unstable.build-std-features=["compiler-builtins-mem"]',
]
# 本腿射程内的四树 (与 clippy 七腿一致; kani-harness 退役后即全仓 Rust 面).
FACES = ("src/kernel/", "host-tests/", "src/user/", "src/rust/")
# 每腿: (腿名, cwd, workspace root, cargo 参数). workspace root 用于把 rustc 给的
# 相对路径归一成仓内路径 —— cargo 以 workspace root 为 rustc 的 cwd, 与本机 cwd 无关.
_KERNEL_ARGS = ["--manifest-path", "../kernel/Cargo.toml", "--workspace",
                "--release", "--lib", "--bins", "--examples"]
LEGS = (
    ("kernel-x86_64", "src/rust", "src/kernel",
     _KERNEL_ARGS + ["--target", "x86_64-unknown-none"] + BUILD_STD_CFG),
    ("kernel-aarch64", "src/rust", "src/kernel",
     _KERNEL_ARGS + ["--target", "aarch64-unknown-none-softfloat"] + BUILD_STD_CFG),
    ("host-tests", ".", "host-tests",
     ["--manifest-path", "host-tests/Cargo.toml", "--release", "--all-targets", "--no-deps"]),
    ("user-x86_64", ".", "src/user",
     ["--manifest-path", "src/user/Cargo.toml", "--release", "--workspace", "--no-deps",
      "--target", "x86_64-unknown-none"]),
    ("user-aarch64", ".", "src/user",
     ["--manifest-path", "src/user/Cargo.toml", "--release", "--workspace", "--no-deps",
      "--target", "aarch64-unknown-none"]),
    ("shell-rust", ".", "src/rust",
     ["--manifest-path", "src/rust/Cargo.toml", "--release", "--no-deps"]),
    ("shell-edgine-abi", ".", "src/rust/edgine-abi",
     ["--manifest-path", "src/rust/edgine-abi/Cargo.toml", "--release", "--no-deps"]),
)


class ScanError(Exception):
    """取数无法完成 (cargo 缺失 / 编译失败 / 输出不可解析)."""


def _kind_of(rendered: str) -> str:
    """由 rendered 首行分形态: block / impl. 认不出的形态 fail-closed 而不是归到「其它」."""
    head = rendered.strip().split("\n")[0]
    if "unsafe block missing a safety comment" in head:
        return "block"
    if "unsafe impl missing a safety comment" in head:
        return "impl"
    raise ScanError(f"无法识别的 {LINT} 诊断形态: {head[:120]}")


def _to_repo_rel(raw: str, ws_root: str, cwd: str):
    """把 rustc 给的路径归一成仓内相对路径; 不在四树内返回 None (依赖/标准库)."""
    if os.path.isabs(raw):
        path = raw
    else:
        path = os.path.join(ROOT, ws_root, raw)
        if not os.path.exists(path):
            path = os.path.join(ROOT, cwd, raw)
    try:
        rel = os.path.relpath(os.path.realpath(path), ROOT)
    except ValueError:
        return None
    if rel.startswith(".."):
        return None
    rel = rel.replace(os.sep, "/")
    return rel if rel.startswith(FACES) else None


def parse_messages(stdout: str, ws_root: str, cwd: str) -> tuple:
    """解析 `--message-format=json` 输出, 返回 (位点集合, 统计).

    位点 = (仓内相对路径, 行, 形态). 统计含射程外来源数与非本 lint 的 error 数,
    后者非零即由调用方 fail-closed (编译没走通 ⇒ 读数不可信).
    """
    sites = set()
    stats = {"out_of_face": 0, "hard_errors": 0, "lint_msgs": 0}
    for line in stdout.splitlines():
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
        code = msg.get("code")
        code = code.get("code") if isinstance(code, dict) else None
        rendered = msg.get("rendered") or ""
        if code != LINT:
            if msg.get("level") == "error" and "could not compile" not in rendered:
                stats["hard_errors"] += 1
            continue
        stats["lint_msgs"] += 1
        spans = msg.get("spans") or []
        if not spans:
            raise ScanError(f"{LINT} 诊断无 span 位置, 读数不可信")
        span = spans[0]
        rel = _to_repo_rel(span.get("file_name") or "", ws_root, cwd)
        if rel is None:
            stats["out_of_face"] += 1
            continue
        sites.add((rel, int(span.get("line_start") or 0), _kind_of(rendered)))
    return sites, stats


def aggregate(sites) -> dict:
    """位点并集 → {(文件, 形态): 计数}. 行号只用于跨腿去重, 不入侧车键."""
    entries: dict = {}
    for path, _line, kind in sites:
        entries[(path, kind)] = entries.get((path, kind), 0) + 1
    return entries


def run_legs() -> tuple:
    """跑七腿, 返回 (位点并集, 每腿统计, 是否有腿真编译)."""
    all_sites = set()
    per_leg = {}
    any_compiled = False
    env = dict(os.environ)
    env.pop("RUSTC_WRAPPER", None)  # 与 ci/audit.sh 步骤 2 同: 不让 wrapper 改写诊断
    for name, cwd, ws_root, args in LEGS:
        # 绝对路径: 相对路径按各腿 cwd 解析, 内核两腿 cwd=src/rust ⇒ 产物会落到
        # src/rust/other/... 而非仓库根 other/, 隔离根被劈成两处 (实测首轮即如此).
        target_dir = os.path.join(ROOT, DEBT_TARGET_ROOT, name)
        cmd = ["cargo", "clippy", *args, "--target-dir", target_dir,
               "--message-format=json", "--", "-W", LINT]
        try:
            proc = subprocess.run(cmd, cwd=os.path.join(ROOT, cwd), env=env,
                                  capture_output=True, text=True, timeout=3600)
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise ScanError(f"取数命令无法执行 ({name}): {exc}") from exc
        sites, stats = parse_messages(proc.stdout, ws_root, cwd)
        if stats["hard_errors"]:
            raise ScanError(f"{name}: 存在 {stats['hard_errors']} 条非 {LINT} 的 error 级诊断, "
                            f"编译未走通 ⇒ 读数不可信 (fail-closed)")
        if not sites and proc.returncode != 0:
            raise ScanError(f"{name}: clippy 失败且无读数 (exit={proc.returncode}): "
                            f"{(proc.stderr or '')[-300:]}")
        stderr = proc.stderr or ""
        compiled = ("Checking" in stderr) or ("Compiling" in stderr)
        any_compiled = any_compiled or compiled
        stats["rc"] = proc.returncode
        stats["sites"] = len(sites)
        stats["compiled"] = compiled
        per_leg[name] = stats
        all_sites |= sites
    return all_sites, per_leg, any_compiled


def totals_of(entries: dict) -> dict:
    """由条目自身求和的自洽校验位 (手改快照而不重跑 --update-baseline 即不自洽)."""
    return {"sites": sum(entries.values()),
            "files": len({f for f, _ in entries}),
            "keys": len(entries)}


def dump_baseline(entries: dict) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        "_meta": {
            "generated_by": "python3 scripts/audit_safety_debt.py --update-baseline",
            "口径": f"clippy {LINT} 在七腿 (内核双架构 / host-tests --all-targets / "
                    "src/user 双裸机三元组 / 壳 / edgine-abi) 的位点并集, 按 (文件, 形态) 记数; "
                    "形态 block=unsafe 块 / impl=unsafe impl; vendored 非 workspace 成员天然不出; "
                    "unsafe extern / missing_safety_doc 面不在射程",
            "disposition": "D-5 乙: 待办清单而非豁免面 —— 批 5a–5g 已逐树补写完毕, "
                           "四树主 clippy 腿均显式 -D 点名本 lint; 侧车 totals 归 0 后"
                           "本腿即硬零门禁 (债册为空 ⇒ 任何新增位点当场红), 不再是棘轮",
        },
        "totals": totals_of(entries),
        "entries": [[f, k, c] for (f, k), c in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + "\n"


def load_baseline(path=None):
    """读快照; 读不到/格式坏/totals 不自洽一律 fail-closed.

    缺省路径在调用时取模块常量 (不写成默认实参 —— 那会在 def 时冻结成真实侧车路径,
    注入式自测改 BASELINE_FILE 便读不到临时快照).
    """
    path = BASELINE_FILE if path is None else path
    if not os.path.isfile(path):
        return None, (f"基线快照缺失: {os.path.basename(path)} "
                      f"(须显式跑 --update-baseline 生成并提交)")
    try:
        with open(path, encoding="utf-8") as f:
            payload = json.load(f)
        entries = {(e[0], e[1]): int(e[2]) for e in payload["entries"]}
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return None, f"基线快照无法解析: {exc}"
    declared = payload.get("totals", {})
    if declared != totals_of(entries):
        return None, (f"基线快照 totals 与 entries 不自洽: 声明 {declared} / "
                      f"实算 {totals_of(entries)} —— 手改过快照, 请重跑 --update-baseline")
    return entries, ""


def reconcile(entries: dict, baseline: dict) -> tuple:
    """双向对账: over=新增 (当前 > 债册), under=快照过期 (债册 > 当前)."""
    over = sorted((key, entries[key], baseline.get(key, 0))
                  for key in entries if entries[key] > baseline.get(key, 0))
    under = sorted((key, entries.get(key, 0), baseline[key])
                   for key in baseline if baseline[key] > entries.get(key, 0))
    return over, under


def collapse_suspected(total: int, baseline_total: int, any_compiled: bool) -> bool:
    """读数塌缩守卫 (L5 的反面保险): 读数 0 而债册非零, 且七腿全是 Fresh 无重编.

    本工具链实测**会**复播 Fresh 诊断, 故正常路径不触发; 一旦换工具链或 cargo
    行为变化导致「全 Fresh ⇒ 诊断不复播 ⇒ 读数凭空归零」, 这里判疑似而不是放行,
    `--update-baseline` 同样拦 (防把侧车一键洗成 0).
    """
    return total == 0 and baseline_total > 0 and not any_compiled


def _self_test() -> int:
    """内置夹具自测 (不跑 cargo): 解析 / 去重 / 聚合 / 对账 / 塌缩守卫 / 快照自洽."""
    checks = []

    def ck(label, cond, detail=""):
        checks.append((label, bool(cond), detail))

    def msg(path, line, kind, code=LINT):
        text = ("unsafe block missing a safety comment" if kind == "block"
                else "unsafe impl missing a safety comment")
        return json.dumps({"reason": "compiler-message", "message": {
            "code": {"code": code}, "level": "warning", "rendered": text + "\n",
            "spans": [{"file_name": path, "line_start": line}]}})

    # ① 形态分辨 + 位点解析 (相对路径按 workspace root 归一)
    out = "\n".join([msg("privileged/mm/cow.rs", 10, "block"),
                     msg("privileged/sync/atomic.rs", 20, "impl"),
                     "not json", json.dumps({"reason": "build-finished"})])
    sites, stats = parse_messages(out, "src/kernel", "src/rust")
    ck("形态分辨 block/impl", {k for _, _, k in sites} == {"block", "impl"}, str(sites))
    ck("相对路径归一到仓内", all(p.startswith("src/kernel/") for p, _, _ in sites), str(sites))
    ck("非 JSON 与非诊断消息被跳过", stats["lint_msgs"] == 2, str(stats))

    # ② 跨腿同一位点去重 (x86_64 与 aarch64 各报一次只记一处)
    a, _ = parse_messages(msg("privileged/mm/cow.rs", 10, "block"), "src/kernel", "src/rust")
    b, _ = parse_messages(msg("privileged/mm/cow.rs", 10, "block"), "src/kernel", "src/rust")
    ck("跨腿位点去重", len(a | b) == 1, str(a | b))
    # 同文件不同行是两处, 不能被去重吃掉
    c, _ = parse_messages(msg("privileged/mm/cow.rs", 11, "block"), "src/kernel", "src/rust")
    ck("同文件不同行不去重", len(a | c) == 2, str(a | c))

    # ③ 聚合成 (文件, 形态) 计数, 行号不入键
    entries = aggregate(a | b | c)
    ck("聚合计数键不含行号", entries == {("src/kernel/privileged/mm/cow.rs", "block"): 2},
       str(entries))

    # ④ 射程外来源不计入 (依赖/标准库路径)
    out2, st2 = parse_messages(msg("/nonexistent/registry/foo.rs", 1, "block"),
                               "src/kernel", "src/rust")
    ck("射程外来源被排除且计数", not out2 and st2["out_of_face"] == 1, str(st2))

    # ⑤ 非本 lint 的 error 级诊断计数 (供 fail-closed), could not compile 不计
    err = json.dumps({"reason": "compiler-message", "message": {
        "code": {"code": "E0433"}, "level": "error", "rendered": "could not find `x`\n",
        "spans": []}})
    tail = json.dumps({"reason": "compiler-message", "message": {
        "code": None, "level": "error", "rendered": "could not compile `kernel`\n", "spans": []}})
    _, st3 = parse_messages(err + "\n" + tail, "src/kernel", "src/rust")
    ck("硬错误计数排除汇总行", st3["hard_errors"] == 1, str(st3))

    # ⑥ 认不出的形态 fail-closed (不静默归到「其它」)
    odd = json.dumps({"reason": "compiler-message", "message": {
        "code": {"code": LINT}, "level": "warning", "rendered": "unsafe 新形态\n",
        "spans": [{"file_name": "privileged/x.rs", "line_start": 1}]}})
    try:
        parse_messages(odd, "src/kernel", "src/rust")
        ck("未知形态抛 ScanError", False, "未抛")
    except ScanError:
        ck("未知形态抛 ScanError", True)

    # ⑦ 双向对账: 新增判 over, 补写判 under
    base = {("src/kernel/privileged/mm/cow.rs", "block"): 2}
    over, under = reconcile({("src/kernel/privileged/mm/cow.rs", "block"): 3}, base)
    ck("新增判 over", len(over) == 1 and not under, str((over, under)))
    over, under = reconcile({("src/kernel/privileged/mm/cow.rs", "block"): 1}, base)
    ck("补写判 under (须重导)", len(under) == 1 and not over, str((over, under)))
    over, under = reconcile(dict(base), base)
    ck("一致时双向皆空", not over and not under, str((over, under)))
    # 行移不变性: 同文件同形态计数不变 ⇒ 补注释推移行号不产生假新增
    shifted = aggregate({("src/kernel/privileged/mm/cow.rs", 99, "block"),
                         ("src/kernel/privileged/mm/cow.rs", 100, "block")})
    over, under = reconcile(shifted, base)
    ck("行号推移不产生假新增/假过期", not over and not under, str((over, under)))

    # ⑧ 塌缩守卫
    ck("读数0+债册非零+全Fresh ⇒ 疑似", collapse_suspected(0, 399, False))
    ck("有腿真编译 ⇒ 不判疑似", not collapse_suspected(0, 399, True))
    ck("债册也为0 ⇒ 不判疑似 (真清零)", not collapse_suspected(0, 0, False))
    ck("读数非零 ⇒ 不判疑似", not collapse_suspected(5, 399, False))

    # ⑨ 快照自洽校验 + 确定性序列化
    text = dump_baseline(base)
    ck("序列化幂等", text == dump_baseline(dict(base)))
    ck("totals 由条目求和", json.loads(text)["totals"] == totals_of(base))
    import tempfile
    with tempfile.TemporaryDirectory() as td:
        p = os.path.join(td, "b.json")
        with open(p, "w", encoding="utf-8") as f:
            f.write(text)
        got, err_msg = load_baseline(p)
        ck("正常快照可读", got == base and not err_msg, err_msg)
        payload = json.loads(text)
        payload["totals"]["sites"] = 999
        with open(p, "w", encoding="utf-8") as f:
            json.dump(payload, f)
        got, err_msg = load_baseline(p)
        ck("totals 不自洽 ⇒ fail-closed", got is None and "不自洽" in err_msg, err_msg)
        with open(p, "w", encoding="utf-8") as f:
            f.write("{ 坏 json")
        got, err_msg = load_baseline(p)
        ck("坏 json ⇒ fail-closed", got is None and "无法解析" in err_msg, err_msg)
        got, err_msg = load_baseline(os.path.join(td, "缺失.json"))
        ck("快照缺失 ⇒ fail-closed", got is None and "缺失" in err_msg, err_msg)

    print("=== audit_safety_debt --self-test (内置夹具, 不跑 cargo) ===")
    failed = 0
    for label, ok, detail in checks:
        print(f"  {'✓' if ok else '✗'} {label}" + (f" — {detail}" if not ok else ""))
        failed += 0 if ok else 1
    print(f"结果: {len(checks) - failed}/{len(checks)} 通过")
    return 1 if failed else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=f"F4 SAFETY clippy 口径存量债册 ({LINT})")
    parser.add_argument("--update-baseline", action="store_true", help="按当前读数重写快照")
    parser.add_argument("--self-test", action="store_true", help="内置夹具自测 (不跑 cargo)")
    args = parser.parse_args(argv)

    if args.self_test:
        return _self_test()

    try:
        sites, per_leg, any_compiled = run_legs()
    except ScanError as exc:
        print(f"✗ audit_safety_debt: {exc} (fail-closed, 不接受假绿)")
        return 2

    entries = aggregate(sites)
    total = totals_of(entries)
    print(f"=== audit_safety_debt: clippy {LINT} 七腿位点并集 ===")
    for name, _cwd, _ws, _a in LEGS:
        st = per_leg[name]
        print(f"  {name:<20} 位点 {st['sites']:>4} / 诊断 {st['lint_msgs']:>4} "
              f"/ 射程外 {st['out_of_face']:>3} / RC={st['rc']} "
              f"/ {'真编译' if st['compiled'] else 'Fresh'}")
    by_kind: dict = {}
    for (_f, kind), cnt in entries.items():
        by_kind[kind] = by_kind.get(kind, 0) + cnt
    print(f"  并集: {total['sites']} 处 / {total['files']} 文件 / {total['keys']} 键")
    for kind, cnt in sorted(by_kind.items(), key=lambda x: -x[1]):
        print(f"    {kind}: {cnt}")

    baseline, err_msg = load_baseline()
    if baseline is None:
        if args.update_baseline:
            # 首次生成: 无快照可对账, 但塌缩守卫仍须先判 (防把 0 冻成基线).
            if collapse_suspected(total["sites"], 0, any_compiled):
                print("✗ 疑似读数塌缩 (七腿全 Fresh 且读数 0), 拒绝生成基线 (fail-closed)")
                return 2
            with open(BASELINE_FILE, "w", encoding="utf-8") as f:
                f.write(dump_baseline(entries))
            print(f"\n基线快照已生成: {os.path.basename(BASELINE_FILE)} "
                  f"({total['sites']} 处 / {total['keys']} 键)")
            return 0
        print(f"✗ {err_msg} (fail-closed, 不接受假绿)")
        return 2

    base_total = totals_of(baseline)["sites"]
    if collapse_suspected(total["sites"], base_total, any_compiled):
        print(f"✗ 疑似读数塌缩: 并集 0 处而债册 {base_total} 处, 且七腿全无重编 "
              f"⇒ 诊断可能未复播, 读数不可信 (fail-closed, --update-baseline 同样拦)")
        return 2

    if args.update_baseline:
        with open(BASELINE_FILE, "w", encoding="utf-8") as f:
            f.write(dump_baseline(entries))
        print(f"\n基线快照已重写: {os.path.basename(BASELINE_FILE)} "
              f"({total['sites']} 处 / {total['keys']} 键)")
        return 0

    over, under = reconcile(entries, baseline)
    print(f"  债册: {base_total} 处 / {len(baseline)} 键 ({os.path.basename(BASELINE_FILE)})")
    if not over and not under:
        if base_total == 0:
            print("✓ 硬零门禁: 七腿位点并集 0 处, 债册亦空 (批5g 收口) —— "
                  "新增无 SAFETY 的 unsafe 块/impl 即红, 无存量可豁免")
        else:
            print(f"✓ clippy 口径与债册一致 ({total['sites']} 处待补写; "
                  f"新增无 SAFETY 的 unsafe 块/impl 即红, 补写后需重跑 --update-baseline)")
        return 0
    for (path, kind), cur, base in over[:15]:
        print(f"    ✗ 新增 {path} — {kind}: 当前 {cur} / 债册 {base}")
    for (path, kind), cur, base in under[:15]:
        print(f"    ✗ 债册过期 {path} — {kind}: 当前 {cur} / 债册 {base}")
    if len(over) + len(under) > 30:
        print(f"    ... 新增 {len(over)} 键 / 减少 {len(under)} 键")
    print("新增 ⇒ 给该 unsafe 块/impl 补紧邻的 // SAFETY: 注释, 而不是扩大债册; "
          "减少 ⇒ 确属补写, 跑 --update-baseline 重导快照.")
    return 1


if __name__ == "__main__":
    sys.exit(main())