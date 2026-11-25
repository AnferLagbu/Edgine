# dynamic-musl-aarch64 fixture

D5/D6 aarch64 动态装载验收载荷。它不是自研程序，而是 Alpine 3.20.3 aarch64
minirootfs 里的真实外部程序：`/bin/busybox`。

- `busybox`（919,232 B）：动态 PIE，`PT_INTERP=/lib/ld-musl-aarch64.so.1`，
  `NEEDED=libc.musl-aarch64.so.1`；init 以 argv
  `["/bin/dyntest", "echo", "hello dynamic musl", NULL]` 执行，输出固定字符串后
  以 0 退出。
- `ld-musl-aarch64.so.1`（723,408 B）：上游 musl 动态解释器。
- `libc.musl-aarch64.so.1`（723,408 B）：Alpine 镜像中它是 loader 的符号链接；
  当前 cpio 解析器不支持 symlink，故归档同字节副本。

来源：

- `https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/alpine-minirootfs-3.20.3-aarch64.tar.gz`
- 归档 sha256：`041fa34a81788242df9e78fa69b97ab45b8ec47ddbf88864755610414a7bf3de`

校验：`sha256sum -c SHA256SUMS`。宿主交叉运行验证（构建机需 qemu-user）：

```sh
qemu-aarch64 -L /path/to/alpine-rootfs /path/to/busybox echo "hello dynamic musl"
```

`ELF_MAX_SIZE = 1 MiB`，busybox 919 KB、loader 723 KB 均 fit；busybox 的
`PT_LOAD` 按 64 KiB 对齐，内核按 4 KiB 页拆分映射。
