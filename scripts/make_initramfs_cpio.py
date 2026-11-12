#!/usr/bin/env python3
"""initramfs 载体生成器 —— 把用户态 ELF 打包成 cpio (newc) 归档 (K-07b).

背景: 内核的 initramfs 通路 (`kernel-functions` 的 `initramfs` feature) 把
`other/build/<arch>/user/initramfs.cpio` 编译期 `include_bytes!` 进镜像, 启动时
解压进 ramfs 根, 再从 `/init` 载入 PID 1 —— 即"用户态程序经文件系统载体进入",
是 `execve` 运行时射程的前提. 该 feature 默认关闭: 出货路径仍是内嵌 `init.bin`.

为什么自带打包器而不调用系统 `cpio`:
1. **确定性** —— ino 顺序编号、mtime/uid/gid 固定 0, 同一份输入两次得到同一份字节,
   便于镜像比对与跨架构排查; 系统 cpio 会把 mtime 写进头, 每次构建都变.
2. **无外部依赖** —— CI / 干净环境不必装 cpio.
3. **与内核解析器同源对齐** —— 内核侧 `functions/fs/initramfs.rs` 按 110 字节头 +
   `align4(name_offset + namesize)` + `align4(data)` 逐条前进, 本脚本按同一套对齐
   规则写出, 并由 `--self-test` 现场重解析自证.

归档布局 (条目名按 cpio 惯例带 `./` 前缀, 目录带独立条目):

    ./                    目录 040755
    ./init                init.bin   ⇒ 内核从 `/init` 载入 PID 1 (必需)
    ./bin                 目录 040755 ⇒ 校验解压器的嵌套路径处理 (逐级建目录)
    ./bin/<prog>          其余用户态程序 (缺失即跳过, 两架构产物集不同)

用法:
    python3 scripts/make_initramfs_cpio.py --arch x86_64
    python3 scripts/make_initramfs_cpio.py --arch aarch64 --out /tmp/initramfs.cpio
    python3 scripts/make_initramfs_cpio.py --self-test

退出码: 0 = 产出成功 / 自测通过, 1 = 参数或产物问题.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# cpio newc 头部长度: magic (6) + 13 个 8 位十六进制字段 (104) = 110
HEADER_SIZE = 110
# 归档结束条目的名字 (内核解析器以此停止)
TRAILER_NAME = "TRAILER!!!"
# TRAILER 条目定长: 头 (110) + 名字含 NUL (11) 填充到 4 ⇒ 124
TRAILER_BYTES = 124
# 字段顺序 = 头部偏移顺序 (与内核 initramfs.rs 的取数偏移一一对应)
FIELD_ORDER = (
    "ino",
    "mode",
    "uid",
    "gid",
    "nlink",
    "mtime",
    "filesize",
    "devmajor",
    "devminor",
    "rdevmajor",
    "rdevminor",
    "namesize",
    "check",
)

# POSIX 类型位 (cpio 头 mode 字段取八进制值)
S_IFDIR = 0o040_000
S_IFREG = 0o100_000
S_IFMT = 0o170_000
PERM_DIR = 0o0755
PERM_EXEC = 0o0755

# 归档布局: (条目名, 类型, 源文件名). 顺序即归档内顺序, 固定以保证确定性.
# 目录条目的源文件名为空串; 文件的源文件名缺失即跳过 (两架构产物集不同),
# 唯 `init` 必需 —— 它是 PID 1 的载体, 缺了整条 initramfs 启动路径无意义.
ARCHIVE_LAYOUT: tuple[tuple[str, str, str], ...] = (
    (".", "dir", ""),
    ("init", "file", "init.bin"),
    ("bin", "dir", ""),
    ("bin/eash", "file", "eash.bin"),
    ("bin/proctest", "file", "proctest.bin"),
    ("bin/exectest", "file", "exectest.bin"),
    ("bin/install", "file", "install.bin"),
    ("bin/fbterm", "file", "fbterm.bin"),
    ("bin/httpsrv", "file", "httpsrv.bin"),
    ("bin/wasmstub", "file", "wasmstub.bin"),
    ("bin/hello.wasm", "file", "hello.wasm"),
)
# 必需条目 (源文件缺失即失败)
REQUIRED_SRC = "init.bin"


def align4(n: int) -> int:
    """向上取整到 4 字节 (与内核 `align4` 同式)."""
    return (n + 3) & ~3


def _hex(value: int) -> str:
    return f"{value & 0xFFFF_FFFF:08X}"


def build_entry(name: str, mode: int, data: bytes, ino: int) -> bytes:
    """编码一个 cpio newc 条目 (名字与其后对齐填充、数据与其后填充)."""
    name_bytes = name.encode("utf-8") + b"\x00"
    namesize = len(name_bytes)
    fields = {
        "ino": ino,
        "mode": mode,
        "uid": 0,
        "gid": 0,
        "nlink": 2 if (mode & S_IFMT) == S_IFDIR else 1,
        "mtime": 0,
        "filesize": len(data),
        "devmajor": 0,
        "devminor": 0,
        "rdevmajor": 0,
        "rdevminor": 0,
        "namesize": namesize,
        "check": 0,
    }
    header = "070701" + "".join(_hex(fields[f]) for f in FIELD_ORDER)
    assert len(header) == HEADER_SIZE, f"头部长度非 110: {len(header)}"
    out = bytearray(header.encode("ascii"))
    out += name_bytes
    out += b"\x00" * (align4(HEADER_SIZE + namesize) - (HEADER_SIZE + namesize))
    out += data
    out += b"\x00" * (align4(len(data)) - len(data))
    return bytes(out)


def build_archive(entries: list[tuple[str, int, bytes]]) -> bytes:
    """按条目列表 (去前缀名, mode, 数据) 生成完整归档 (含 `./` 前缀与 TRAILER)."""
    out = bytearray()
    for idx, (name, mode, data) in enumerate(entries, start=1):
        out += build_entry(f"./{name}", mode, data, idx)
    out += build_entry(TRAILER_NAME, 0, b"", 0)
    return bytes(out)


def iter_entries(blob: bytes) -> list[tuple[str, int, bytes]]:
    """按内核解析器的同一套规则重解析归档 (自测用): 返回 (去 `./` 前缀名, mode, 数据)."""

    def parse_hex(field: bytes) -> int:
        return int(field.decode("ascii"), 16)

    entries: list[tuple[str, int, bytes]] = []
    offset = 0
    while offset + HEADER_SIZE <= len(blob):
        if blob[offset : offset + 6] != b"070701":
            break
        mode = parse_hex(blob[offset + 14 : offset + 22])
        filesize = parse_hex(blob[offset + 54 : offset + 62])
        namesize = parse_hex(blob[offset + 94 : offset + 102])
        name_start = offset + HEADER_SIZE
        raw_name = blob[name_start : name_start + namesize]
        name = raw_name.rstrip(b"\x00").decode("utf-8")
        if name == TRAILER_NAME:
            break
        data_start = align4(name_start + namesize)
        data = blob[data_start : data_start + filesize]
        assert len(data) == filesize, f"{name}: 数据长度不足 {filesize}"
        entries.append((name.removeprefix("./"), mode, data))
        offset = align4(data_start + filesize)
    return entries


def collect_entries(src_dir: Path) -> list[tuple[str, int, bytes]]:
    """按固定布局读产物; `init.bin` 缺失即报错, 其余缺失即跳过."""
    entries: list[tuple[str, int, bytes]] = []
    skipped: list[str] = []
    for name, kind, src in ARCHIVE_LAYOUT:
        if kind == "dir":
            entries.append((name, S_IFDIR | PERM_DIR, b""))
            continue
        path = src_dir / src
        if not path.is_file():
            if src == REQUIRED_SRC:
                raise FileNotFoundError(
                    f"必需产物缺失: {path} (先运行 `make ARCH=<arch> user` 生成用户态 ELF)"
                )
            skipped.append(name)
            continue
        entries.append((name, S_IFREG | PERM_EXEC, path.read_bytes()))
    for name in skipped:
        print(f"[cpio] 跳过缺失产物: {name}")
    return entries


def write_cpio(src_dir: Path, out_path: Path) -> int:
    """生成归档并落盘, 返回写出的条目数."""
    blob = build_archive(collect_entries(src_dir))
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_bytes(blob)
    print(f"[cpio] 写出 {out_path}: {len(blob)} 字节 (源目录 {src_dir})")
    return len(blob)


def self_test() -> int:
    """校验对齐规则、写-读往返、确定性与必需项失败路径. 全部通过返回 0."""
    failures: list[str] = []

    def check(cond: bool, msg: str) -> None:
        if not cond:
            failures.append(msg)

    check(align4(0) == 0 and align4(1) == 4 and align4(3) == 4, "align4 基本式")
    check(align4(HEADER_SIZE) == 112 and align4(111) == 112, "align4 跨界")

    fake = [
        ("init", S_IFREG | PERM_EXEC, b"\x7fELF" + bytes(range(256))),
        ("bin", S_IFDIR | PERM_DIR, b""),
        ("bin/eash", S_IFREG | PERM_EXEC, b"small"),
        # 非 4 倍数长度: 验数据段填充
        ("bin/odd", S_IFREG | PERM_EXEC, b"1234567"),
    ]
    blob = build_archive(fake)
    check(len(blob) % 4 == 0, "归档整体 4 字节对齐")
    parsed = iter_entries(blob)
    check([n for n, _, _ in parsed] == [n for n, _, _ in fake], f"条目名序列: {parsed}")
    check([d for _, _, d in parsed] == [d for _, _, d in fake], "条目数据逐字节往返一致")
    check([m for _, m, _ in parsed] == [m for _, m, _ in fake], "条目 mode 往返一致")
    # 内核解析器按 `mode & 0o170000` 分派类型, 故类型位必须原样可读回
    check(parsed[1][1] & S_IFMT == S_IFDIR, "目录类型位可读回")
    check(parsed[0][1] & S_IFMT == S_IFREG, "常规文件类型位可读回")

    # 名字长度决定头后填充量: 逐一构造 1..12 字节名字, 断言解析器仍能对齐前进
    for ln in range(1, 13):
        got = iter_entries(build_archive([("a" * ln, S_IFREG | PERM_EXEC, b"")]))
        check(len(got) == 1 and got[0][0] == "a" * ln, f"名字长度 {ln} 往返")

    check(build_archive(fake) == blob, "同输入两次写出字节一致 (确定性)")
    check(
        blob[-TRAILER_BYTES:-TRAILER_BYTES + 6] == b"070701",
        "TRAILER 条目落在末尾且头部对齐",
    )
    # TRAILER 条目 124 字节: 头 110 + 名字含 NUL 11 + 填充 3 ⇒ 名字起于倒数第 14 字节
    check(
        blob[-(TRAILER_BYTES - HEADER_SIZE) : -4] == b"TRAILER!!!",
        "TRAILER 名字段位置符合内核取数偏移",
    )

    with tempfile.TemporaryDirectory() as td:
        empty = Path(td)
        try:
            collect_entries(empty)
            failures.append("init.bin 缺失时 collect_entries 未报错")
        except FileNotFoundError:
            pass
        (empty / "init.bin").write_bytes(b"\x7fELF init")
        names = [n for n, _, _ in collect_entries(empty)]
        check(names[0] == "." and names[1] == "init", f"根目录条目先于 init: {names}")
        check("bin" in names and "bin/eash" not in names, "嵌套目录条目在、缺失产物跳过")
        rt = dict((n, d) for n, _, d in iter_entries(build_archive(collect_entries(empty))))
        check(rt.get("init") == b"\x7fELF init", "产物字节经归档写-读往返一致")

    if failures:
        for f in failures:
            print(f"[cpio] 自测失败: {f}", file=sys.stderr)
        return 1
    print(f"[cpio] 自测通过 ({len(fake)} 条目写-读往返 + 确定性 + 失败路径)")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="生成 initramfs cpio (newc) 归档")
    parser.add_argument("--arch", default="x86_64", choices=("x86_64", "aarch64"))
    parser.add_argument("--src", default="", help="用户态产物目录 (默认 other/build/<arch>/user)")
    parser.add_argument("--out", default="", help="输出路径 (默认 <src>/initramfs.cpio)")
    parser.add_argument("--self-test", action="store_true", help="跑生成器自测并退出")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    src_dir = Path(args.src) if args.src else PROJECT_ROOT / "other/build" / args.arch / "user"
    out_path = Path(args.out) if args.out else src_dir / "initramfs.cpio"
    try:
        write_cpio(src_dir, out_path)
    except FileNotFoundError as e:
        print(f"[cpio] 错误: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
