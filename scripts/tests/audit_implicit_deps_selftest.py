#!/usr/bin/env python3
"""`scripts/audit_implicit_deps.py` 自测 (P-49 甲・自测锁).

沿用 `scripts/tests/verify_image_arch_selftest.py` 的设计: 统一入口
`python3 scripts/tests/audit_implicit_deps_selftest.py`, 用 tempfile 构造合成源码,
regex/口径类判据不依赖真实仓库形态.

覆盖:
- 收割正则: 类型位置 `'static T` 不得被当全局定义 (17 处伪命中的成因)
- 收割正则: `pub static mut NAME: T =` 与跨行声明 `static NAME: Lock<` 必须纳入
- §7.3 口径: 整行注释 / 内联 `#[cfg(test)] mod` 块不计生产
- 基线语义: 超出基线转红、少于基线同样转红、逐项一致才绿 (双向对账)
- 基线形态: 侧车快照 `scripts/audit_implicit_deps.baseline.json` 由 --update-baseline
  机器派生, 不手写数字; 本类锁它的 totals 自洽校验、序列化幂等、缺失即 fail-closed、
  写读 round-trip, 以及快照必须是被提交物 (不被 gitignore)
- 仓库真值: 在生产根上跑完整 `main()` 必须返回 0 (检测器与自己冻结的基线自洽)
- 仓库真值: 9 个只出现在类型位置的名字不得重新进入收割集
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(PROJECT_ROOT / "scripts"))

import audit_implicit_deps as aid  # noqa: E402  (需先注入 sys.path)

# 修复前被 `-> &'static X {` 误收割的 9 个类型位置名字 (17 处伪命中的全量去重集)
TYPE_POSITION_NAMES = [
    "BpfSubsystem", "CetSubsystem", "KexecSubsystem", "NetOps", "SecureBootSubsystem",
    "TicklessSubsystem", "TimeSyncSubsystem", "TpmSubsystem", "UefiSubsystem",
]


class HarvestTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.priv = self.root / "src/kernel/privileged"
        self.func = self.root / "src/kernel/functions"
        self.priv.mkdir(parents=True)
        self.func.mkdir(parents=True)
        self._saved = (aid.PRIVILEGED_BASE, aid.FUNCTIONS_BASE)
        aid.PRIVILEGED_BASE = self.priv
        aid.FUNCTIONS_BASE = self.func

    def tearDown(self):
        aid.PRIVILEGED_BASE, aid.FUNCTIONS_BASE = self._saved
        self._tmp.cleanup()

    def write(self, base: Path, name: str, text: str) -> Path:
        path = base / name
        path.write_text(text, encoding="utf-8")
        return path

    def test_type_position_static_is_not_harvested(self):
        """修复前的 `[=;{]` 收尾让 `'static BpfSubsystem {` 进收割集 —— 必须已失效."""
        self.write(self.priv, "bpf.rs", (
            "pub fn bpf_subsystem() -> &'static BpfSubsystem {\n"
            "    &SUBSYS\n"
            "}\n"
            "fn other() {\n"
            "    let view: &'static [u8] = &[];\n"
            "    let _ = view;\n"
            "}\n"
        ))
        self.write(self.priv, "real.rs", "static SUBSYS: u32 = 0;\n")
        found = aid.harvest_privileged_globals()
        self.assertIn("SUBSYS", found)
        self.assertNotIn("BpfSubsystem", found)
        self.assertNotIn("view", found)
        self.assertNotIn("u8", found)

    def test_static_mut_and_multiline_declarations_are_harvested(self):
        self.write(self.priv, "g.rs", (
            "pub static mut SOCKET_SET: core::mem::MaybeUninit<Set<'static>> =\n"
            "    core::mem::MaybeUninit::uninit();\n"
            "static GENERAL_CACHES: crate::privileged::sync::IrqSpinLock<\n"
            "    [Cache; N],\n"
            "> = crate::privileged::sync::IrqSpinLock::new([Cache::ZERO; N]);\n"
        ))
        found = aid.harvest_privileged_globals()
        self.assertEqual(found, {"SOCKET_SET", "GENERAL_CACHES"})

class ScanScopeTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.func = Path(self._tmp.name) / "functions"
        self.func.mkdir(parents=True)
        self._saved = aid.FUNCTIONS_BASE
        aid.FUNCTIONS_BASE = self.func

    def tearDown(self):
        aid.FUNCTIONS_BASE = self._saved
        self._tmp.cleanup()

    def test_comment_and_inline_test_context_are_not_production(self):
        (self.func / "x.rs").write_text(
            "use crate::privileged::FOO;\n"
            "// 注释里的 FOO\n"
            "/// 文档里的 FOO\n"
            "fn real() { let _ = FOO; }\n"
            "#[cfg(test)]\n"
            "mod t {\n"
            "    fn case() { let _ = FOO; }\n"
            "}\n",
            encoding="utf-8",
        )
        prod, comment, test = aid.scan_functions({"FOO"})
        self.assertEqual(len(prod), 2, [h["line"] for h in prod])
        self.assertEqual(len(comment), 2, [h["line"] for h in comment])
        self.assertEqual(len(test), 1, [h["line"] for h in test])

    def test_word_boundary_prevents_prefix_hit(self):
        (self.func / "y.rs").write_text(
            "fn f() { let _ = SCHEDULER_READY; }\n", encoding="utf-8"
        )
        prod, _, _ = aid.scan_functions({"SCHEDULER"})
        self.assertEqual(prod, [])


class BaselineTest(unittest.TestCase):
    """基线是侧车快照 (机器派生), 本类锁它的对账语义与自洽性."""

    def setUp(self):
        baseline, err = aid.load_baseline()
        self.assertIsNotNone(baseline, f"基线快照读不出: {err}")
        self.baseline = baseline

    def test_snapshot_agrees_with_its_own_entries(self):
        # totals 由 entries 求和; 手改条目不重跑 --update-baseline ⇒ load_baseline 拒绝
        # 篡改值由快照自身派生 (不写死数字): 基线站点数随债项增减而变, 写死会让本锁
        # 随一次合法的 --update-baseline 静默失效.
        raw = aid.BASELINE_FILE.read_text(encoding="utf-8")
        payload_raw = json.loads(raw)
        payload_raw["totals"]["sites"] += 1
        tampered = json.dumps(payload_raw, ensure_ascii=False, indent=2) + "\n"
        payload = json.loads(tampered)
        self.assertNotEqual(payload["totals"], aid.totals_of(aid.parse_baseline(tampered)),
                            "篡改条目后 totals 仍自洽 —— 自洽校验位失效")
        with tempfile.TemporaryDirectory() as tmp:
            ghost = Path(tmp) / "absent.json"
            saved = aid.BASELINE_FILE
            aid.BASELINE_FILE = ghost
            try:
                entries, err = aid.load_baseline()
            finally:
                aid.BASELINE_FILE = saved
        self.assertIsNone(entries)
        self.assertIn("缺失", err)

    def test_serialization_is_deterministic(self):
        a = aid.dump_baseline(self.baseline)
        b = aid.dump_baseline(dict(sorted(self.baseline.items(), reverse=True)))
        self.assertEqual(a, b, "快照序列化随输入顺序变化 ⇒ --update-baseline 不幂等")
        self.assertEqual(aid.parse_baseline(a), self.baseline)

    def test_exact_match_is_green(self):
        over, under = aid.diff_baseline(Counter(self.baseline), self.baseline)
        self.assertEqual((over, under), ([], []))

    def test_new_site_is_red(self):
        actual = Counter(self.baseline)
        key = ("src/kernel/functions/proc/table.rs", "PROCESS_TABLE")
        actual[key] += 1
        over, under = aid.diff_baseline(actual, self.baseline)
        self.assertEqual([k for k, _, _ in over], [key])
        self.assertEqual(under, [])

    def test_unseen_file_is_red(self):
        actual = Counter(self.baseline)
        actual[("src/kernel/functions/net/new.rs", "NET_STACK")] = 1
        over, _ = aid.diff_baseline(actual, self.baseline)
        self.assertIn(("src/kernel/functions/net/new.rs", "NET_STACK"),
                      [k for k, _, _ in over])

    def test_debt_reduction_still_requires_baseline_update(self):
        actual = Counter(self.baseline)
        key = ("src/kernel/functions/proc/mod.rs", "SCHEDULER")
        del actual[key]
        _, under = aid.diff_baseline(actual, self.baseline)
        self.assertIn(key, [k for k, _, _ in under])

    def test_update_writes_exactly_what_the_gate_reads(self):
        """--update-baseline 写到临时路径 ⇒ 门禁立刻读得通同一份 (round-trip)."""
        with tempfile.TemporaryDirectory() as tmp:
            ghost = Path(tmp) / "baseline.json"
            saved = aid.BASELINE_FILE
            aid.BASELINE_FILE = ghost
            try:
                self.assertEqual(aid.main(["--update-baseline"]), 0)
                self.assertTrue(ghost.is_file())
                first = ghost.read_text(encoding="utf-8")
                self.assertEqual(aid.main([]), 0, "刚生成的快照被门禁判红")
                self.assertEqual(aid.main(["--update-baseline"]), 0)
                self.assertEqual(first, ghost.read_text(encoding="utf-8"),
                                 "重复 --update-baseline 产物不逐字节一致")
            finally:
                aid.BASELINE_FILE = saved


class RepoTruthTest(unittest.TestCase):
    def test_detector_agrees_with_frozen_baseline_on_the_real_repo(self):
        self.assertEqual(aid.main([]), 0)

    def test_repo_harvest_excludes_the_false_positive_class(self):
        """仓库真值锁: 这 9 个名字只出现在 `'static T` 类型位置, 重新进收割集 = 正则回退.

        若将来某个名字真的成了 privileged 的 static 项, 本锁转红 —— 那是要求人确认的
        信号, 从名单里删掉该名字即可, 不许反过来放宽收割正则.
        """
        found = aid.harvest_privileged_globals()
        self.assertTrue(found, "收割结果为空 —— 收割正则或扫描根已失效")
        for name in TYPE_POSITION_NAMES:
            self.assertNotIn(name, found, f"{name} 不应被收割为 privileged 全局定义")

    def test_snapshot_exists_in_tree_and_is_self_consistent(self):
        """快照是被提交物, 不在 .gitignore 里 —— 否则门禁在别人机器上必然 FAIL-CLOSED."""
        self.assertTrue(aid.BASELINE_FILE.is_file())
        entries, err = aid.load_baseline()
        self.assertIsNotNone(entries, err)
        rel = str(aid.BASELINE_FILE.relative_to(PROJECT_ROOT))
        # git 不在 PATH 时跳过这一问 (快照存在性与自洽性已在上面断言, 与 git 无关).
        # 不跳过会让审计腿在无 git 环境下红得与债项无关 —— 属 P-47 记录的「腿自身失效」族.
        git = shutil.which("git")
        if git is None:
            self.skipTest("环境无 git, 无法核验快照是否被 gitignore")
        proc = subprocess.run([git, "check-ignore", "-q", "--", str(rel)],
                              cwd=PROJECT_ROOT, capture_output=True)
        self.assertLess(proc.returncode, 128,
                        f"git check-ignore 执行失败 (rc={proc.returncode}): "
                        f"{proc.stderr.decode('utf-8', 'replace').strip()}")
        self.assertNotEqual(proc.returncode, 0, f"{rel} 被 gitignore, 快照不会随仓库分发")


if __name__ == "__main__":
    unittest.main(verbosity=2)
