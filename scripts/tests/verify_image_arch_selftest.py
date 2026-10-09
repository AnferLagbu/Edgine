#!/usr/bin/env python3
"""`scripts/verify_image_arch.py` 自测 (S-6 防线③ 回归覆盖).

设计沿用 `scripts/tests/audit_selftest.py`: 统一入口
`python3 scripts/tests/verify_image_arch_selftest.py`, 用 tempfile 构造合成 ELF,
不依赖真实构建产物 (干净 checkout 也能跑).

覆盖:
- `read_e_machine`: 两架构合法头 + 4 类非法头 (过短 / 魔数 / class / 端序)
- `check_elf`: 架构相符 / 不符 / 文件缺失
- `check_image_contains`: 命中 / 未命中 / 镜像过小 / 产物为空 / 文件缺失
- CLI 退出码: 全通过 0, 任一不符 1
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(PROJECT_ROOT / "scripts"))

import verify_image_arch as via  # noqa: E402  (需先注入 sys.path)


def make_elf(machine: int, magic: bytes = b"\x7fELF", class_: int = 2, data: int = 1) -> bytes:
    """构造最小合法 ELF64 头 (64 字节) + 尾部标记, 只关心 e_machine."""
    head = bytearray(magic)
    head.append(class_)
    head.append(data)
    head.append(1)  # e_ident[EI_VERSION]
    head += b"\x00" * (18 - len(head))
    head += machine.to_bytes(2, "little")
    head += b"\x00" * (64 - len(head))
    return bytes(head) + b"PAYLOAD"


class ReadEMachineTest(unittest.TestCase):
    def test_both_arches(self) -> None:
        self.assertEqual(via.read_e_machine(self._write(make_elf(0x3E))), 0x3E)
        self.assertEqual(via.read_e_machine(self._write(make_elf(0xB7))), 0xB7)

    def test_rejects_malformed_header(self) -> None:
        cases = [
            (b"\x00" * 8, "过短"),
            (make_elf(0x3E, magic=b"\x00ELF"), "魔数"),
            (make_elf(0x3E, class_=1), "ELFCLASS64"),
            (make_elf(0x3E, data=2), "小端"),
        ]
        for blob, label in cases:
            with self.subTest(case=label):
                with self.assertRaises(via.ElfError):
                    via.read_e_machine(self._write(blob))

    @staticmethod
    def _write(blob: bytes) -> Path:
        tmp = tempfile.NamedTemporaryFile(delete=False, suffix=".bin")
        tmp.write(blob)
        tmp.close()
        return Path(tmp.name)


class CheckElfTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.x86 = self.dir / "init_x86.bin"
        self.x86.write_bytes(make_elf(0x3E))
        self.arm = self.dir / "init_arm.bin"
        self.arm.write_bytes(make_elf(0xB7))

    def test_matching_arch_ok(self) -> None:
        ok, msg = via.check_elf(self.x86, "x86_64")
        self.assertTrue(ok, msg)

    def test_mismatched_arch_fails(self) -> None:
        # 防线核心: 在 x86_64 门禁上喂 aarch64 用户态必须 FAIL (旧世界静默通过)
        ok, msg = via.check_elf(self.arm, "x86_64")
        self.assertFalse(ok)
        self.assertIn("0xB7", msg)
        self.assertIn("0x3E", msg)

    def test_missing_file_fails(self) -> None:
        ok, msg = via.check_elf(self.dir / "nope.bin", "aarch64")
        self.assertFalse(ok)
        self.assertIn("产物缺失", msg)


class CheckImageContainsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.init = self.dir / "init.bin"
        self.init.write_bytes(make_elf(0x3E))
        self.image = self.dir / "kernel.flat"

    def test_hit(self) -> None:
        blob = self.init.read_bytes()
        self.image.write_bytes(b"\x00" * 100 + blob + b"\xff" * 8)
        ok, msg = via.check_image_contains(self.image, self.init)
        self.assertTrue(ok, msg)
        self.assertIn("@0x64", msg)

    def test_contains_other_arch_build(self) -> None:
        # 镜像里嵌的是另一份 (aarch64) 产物 ⇒ 逐字节包含检查必须失败
        self.image.write_bytes(b"\x00" * 32 + make_elf(0xB7))
        ok, msg = via.check_image_contains(self.image, self.init)
        self.assertFalse(ok)
        self.assertIn("未包含", msg)

    def test_image_smaller_than_blob(self) -> None:
        self.image.write_bytes(b"\x00" * 4)
        ok, msg = via.check_image_contains(self.image, self.init)
        self.assertFalse(ok)
        self.assertIn("还小", msg)

    def test_empty_artifact(self) -> None:
        self.init.write_bytes(b"")
        self.image.write_bytes(b"\x00" * 4096)
        ok, msg = via.check_image_contains(self.image, self.init)
        self.assertFalse(ok)
        self.assertIn("内容为空", msg)

    def test_missing_files(self) -> None:
        ok, msg = via.check_image_contains(self.dir / "no-image", self.init)
        self.assertFalse(ok)
        self.assertIn("镜像缺失", msg)
        # 镜像存在但产物缺失 ⇒ 命中“产物缺失”分支
        self.image.write_bytes(b"\x00" * 4096)
        ok, msg = via.check_image_contains(self.image, self.dir / "no-init")
        self.assertFalse(ok)
        self.assertIn("产物缺失", msg)


class CliExitCodeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.init_x86 = self.dir / "init.bin"
        self.init_x86.write_bytes(make_elf(0x3E))
        self.init_arm = self.dir / "init_arm.bin"
        self.init_arm.write_bytes(make_elf(0xB7))
        self.image = self.dir / "kernel.flat"
        self.image.write_bytes(b"\x00" * 16 + self.init_x86.read_bytes())

    def test_pass(self) -> None:
        rc = via.main(
            ["--arch", "x86_64", "--image", str(self.image), "--init", str(self.init_x86)]
        )
        self.assertEqual(rc, 0)

    def test_fail_on_arch_mismatch(self) -> None:
        rc = via.main(
            ["--arch", "x86_64", "--image", str(self.image), "--init", str(self.init_arm)]
        )
        self.assertEqual(rc, 1)

    def test_elf_only_mode(self) -> None:
        self.assertEqual(via.main(["--arch", "aarch64", "--init", str(self.init_arm)]), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
