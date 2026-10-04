#!/usr/bin/env python3
"""
sync_track_backlog 工具的单元测试.

覆盖三类核心行为:
1. parse_backlog 正确解析 roadmap Backlog 段
2. classify 正确区分 in_source / mismatch / no_todo / file_gone
3. --apply 模式正确落盘 (创建临时 roadmap 进行隔离)

执行: python3 tools/tests/test_sync_track_backlog.py
退出: 0 = 全部通过
"""

import os
import sys
import tempfile
import unittest
from pathlib import Path

# 让 sync_track_backlog 可被 import
TOOLS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(TOOLS))
import sync_track_backlog as stb  # noqa: E402

REAL_ROADMAP = TOOLS.parent / "docs" / "plan" / "kernel-roadmap.md"


class TestParseBacklog(unittest.TestCase):
    """测试 parse_backlog 正确解析 Backlog 段."""

    SAMPLE = """\
# Edgine 内核工程规划书

> 主题

## Backlog: 过期 TODO 跟踪

> 维护说明.

- [TRACK-AAAAAA] `src/foo.rs:10` TODO
- [TRACK-BBBBBB] `src/bar/baz.rs:42` TODO

## 后续章节

正文
"""

    # 注: TRACK ID 必须是大写 16 进制 (0-9, A-F) 6 字符, 由 track_todo.py 的 sha1[:6].upper() 约束

    def test_parse_count(self) -> None:
        start, end, items = stb.parse_backlog(self.SAMPLE)
        self.assertEqual(start, 5)
        self.assertGreater(end, 5)
        self.assertEqual(len(items), 2)
        self.assertEqual(items[0][0], "TRACK-AAAAAA")
        self.assertEqual(items[0][1], "src/foo.rs")
        self.assertEqual(items[0][2], 10)
        self.assertEqual(items[1][0], "TRACK-BBBBBB")
        self.assertEqual(items[1][1], "src/bar/baz.rs")
        self.assertEqual(items[1][2], 42)

    def test_parse_no_backlog(self) -> None:
        s = "# 标题\n## 其他章节\n正文\n"
        start, end, items = stb.parse_backlog(s)
        self.assertEqual(items, [])
        self.assertEqual(start, -1)

    def test_parse_real_roadmap(self) -> None:
        """真实 roadmap 应解析出 >= 50 条 TRACK 项 (2026-06 状态)."""
        if not REAL_ROADMAP.exists():
            self.skipTest(f"roadmap 不存在: {REAL_ROADMAP}")
        content = REAL_ROADMAP.read_text(encoding="utf-8")
        _, _, items = stb.parse_backlog(content)
        self.assertGreaterEqual(
            len(items), 50,
            f"roadmap 应有 >= 50 条 TRACK 项, 实际 {len(items)}"
        )


class TestClassify(unittest.TestCase):
    """测试 classify 正确区分 4 种状态."""

    def setUp(self) -> None:
        # 创建临时源码目录: 4 个测试文件覆盖 4 种状态
        self.tmp = tempfile.TemporaryDirectory()
        self.src_root = Path(self.tmp.name) / "src"
        self.src_root.mkdir()

        # 文件 1: in_source (TODO 在指定行)
        (self.src_root / "in.rs").write_text(
            "// line 1\n" * 9 + "// TODO(TRACK-A00001): x\n", encoding="utf-8"
        )
        # 文件 2: mismatch (TODO 在其他行)
        (self.src_root / "mis.rs").write_text(
            "// line 1\n" * 4 + "// TODO(TRACK-B00002): y\n", encoding="utf-8"
        )
        # 文件 3: no_todo (无 TODO)
        (self.src_root / "none.rs").write_text(
            "// line 1\n" * 10, encoding="utf-8"
        )
        # 文件 4: file_gone (不创建)

        # 临时改变 ROOT 让 classify 找到这些文件 — 用 monkey-patch
        self._orig_root = stb.ROOT
        stb.ROOT = self.tmp.name

    def tearDown(self) -> None:
        stb.ROOT = self._orig_root
        self.tmp.cleanup()

    def test_in_source(self) -> None:
        self.assertEqual(
            stb.classify("TRACK-A00001", "src/in.rs", 10),
            "in_source",
        )

    def test_mismatch(self) -> None:
        # TODO 在 line 5, roadmap 引用 line 1
        self.assertEqual(
            stb.classify("TRACK-B00002", "src/mis.rs", 1),
            "mismatch",
        )

    def test_no_todo(self) -> None:
        self.assertEqual(
            stb.classify("TRACK-CDEFFF", "src/none.rs", 5),
            "no_todo",
        )

    def test_file_gone(self) -> None:
        self.assertEqual(
            stb.classify("TRACK-CDEFFF", "src/gone.rs", 5),
            "file_gone",
        )


class TestApplyRoundTrip(unittest.TestCase):
    """测试 --apply 模式的端到端: 临时 roadmap + 模拟源码, 验证落盘正确性."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.src_root = Path(self.tmp.name) / "src"
        self.src_root.mkdir()
        self.roadmap_path = Path(self.tmp.name) / "roadmap.md"

        # 真实 TRACK ID 形式 (sha1[:6].upper()), 用 hex 字符 (0-9, A-F)
        (self.src_root / "keep.rs").write_text(
            "// 1\n" * 9 + "// TODO(TRACK-123456): keep\n", encoding="utf-8"
        )
        (self.src_root / "mis.rs").write_text(
            "// 1\n" * 4 + "// TODO(TRACK-ABCDEF): mis\n", encoding="utf-8"
        )
        # TRACK-100003 不创建, 用于 no_todo
        # TRACK-100004 文件不创建, 用于 file_gone

        self.roadmap_path.write_text(
            "# 标题\n"
            "\n"
            "## Backlog: 过期 TODO 跟踪\n"
            "\n"
            "> 维护\n"
            "\n"
            "- [TRACK-123456] `src/keep.rs:10` TODO\n"
            "- [TRACK-ABCDEF] `src/mis.rs:1` TODO\n"
            "- [TRACK-100003] `src/none.rs:5` TODO\n"
            "- [TRACK-100004] `src/gone.rs:5` TODO\n"
            "\n"
            "## 其他\n",
            encoding="utf-8",
        )

        self._orig_roadmap = stb.ROADMAP
        self._orig_root = stb.ROOT
        stb.ROADMAP = self.roadmap_path
        stb.ROOT = self.tmp.name

    def tearDown(self) -> None:
        stb.ROADMAP = self._orig_roadmap
        stb.ROOT = self._orig_root
        self.tmp.cleanup()

    def test_apply_modifies_roadmap(self) -> None:
        # 跑主流程, --apply 模式
        sys.argv = ["sync_track_backlog.py", "--apply"]
        rc = stb.main()
        self.assertEqual(rc, 0)

        result = self.roadmap_path.read_text(encoding="utf-8")

        # TRACK-123456 (keep) — 应保留
        self.assertIn("TRACK-123456", result)
        self.assertIn("src/keep.rs:10", result)

        # TRACK-ABCDEF (mismatch) — 行号应修正 1 -> 5
        self.assertIn("TRACK-ABCDEF", result)
        self.assertIn("src/mis.rs:5", result)
        self.assertNotIn("src/mis.rs:1`", result)

        # TRACK-100003 (no_todo) — 应删除
        self.assertNotIn("TRACK-100003", result)

        # TRACK-100004 (file_gone) — 应删除
        self.assertNotIn("TRACK-100004", result)

        # 后续章节应保留
        self.assertIn("## 其他", result)


class TestIdempotent(unittest.TestCase):
    """幂等性: 跑两次后第二次应无修改 (除空 backlog 段无变化)."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.src_root = Path(self.tmp.name) / "src"
        self.src_root.mkdir()
        self.roadmap_path = Path(self.tmp.name) / "roadmap.md"
        (self.src_root / "a.rs").write_text(
            "// 1\n" * 9 + "// TODO(TRACK-DEADBE): x\n", encoding="utf-8"
        )
        self.roadmap_path.write_text(
            "# T\n\n## Backlog: 过期 TODO 跟踪\n\n> m\n\n"
            "- [TRACK-DEADBE] `src/a.rs:10` TODO\n"
            "- [TRACK-C0FFEE] `src/none.rs:5` TODO\n"
            "\n## End\n",
            encoding="utf-8",
        )
        self._orig_roadmap = stb.ROADMAP
        self._orig_root = stb.ROOT
        stb.ROADMAP = self.roadmap_path
        stb.ROOT = self.tmp.name

    def tearDown(self) -> None:
        stb.ROADMAP = self._orig_roadmap
        stb.ROOT = self._orig_root
        self.tmp.cleanup()

    def test_idempotent(self) -> None:
        sys.argv = ["sync_track_backlog.py", "--apply"]
        stb.main()
        after_first = self.roadmap_path.read_text(encoding="utf-8")
        # 第二次跑应无变化
        stb.main()
        after_second = self.roadmap_path.read_text(encoding="utf-8")
        self.assertEqual(
            after_first, after_second,
            "第二次跑应幂等无修改"
        )
        # 验证第一次已删除 no_todo 项
        self.assertNotIn("TRACK-C0FFEE", after_first)


if __name__ == "__main__":
    unittest.main(verbosity=2)
