/*
 * WASM P3 WASI 覆盖探针 (wasi_snapshot_preview1)。
 *
 * 与 P2 载荷（P2 已验证 stdio/文件/链接/目录核心路径）互补，本探针专打 P2
 * 未覆盖或只在代码层实现、没在真机载荷里跑过的入口：
 *   - sched_yield
 *   - fd_allocate / fd_advise（posix_fallocate / posix_fadvise）
 *   - fd_renumber（dup2）
 *   - fd_dup（wasmrun 非标准直接 fd 语义，需自定义 import）
 *   - path_filestat_set_times
 *   - fd_prestat_get / fd_prestat_dir_name（枚举 preopen）
 *   - fd_seek SEEK_END / fd_readdir 多条目
 *   - 错误分支：ENOENT / EEXIST / ENOTEMPTY / ENOTDIR / EBADF
 *
 * 输出契约（由 scripts/wasmi_wasi_test.sh 消费）：
 *   `P3 start`        探针进入 _start
 *   `OK <name>`       单项通过
 *   `FAIL <name> errno=<n>` 单项失败（n 为 wasi-libc errno 值）
 *   `P3 done fails=<n>`    总判定行，n 必为 0
 *
 * 编译（wasi-sdk 34，架构无关）：
 *   clang --target=wasm32-wasip1 -O2 wasi_p3_probe.c -o wasi_p3_probe.wasm
 * 生成器见 scripts/make_wasi_p3_probe.py。
 */

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wasi/api.h>

static int fails;

static void check(int cond, const char *what) {
    if (cond) {
        printf("OK %s\n", what);
    } else {
        printf("FAIL %s errno=%d\n", what, errno);
        fails++;
    }
    /* 逐项 flush: wasi-libc 的 stdout 在非 tty 下是全缓冲, 挂死时看不到进度. */
    fflush(stdout);
}

/* wasmrun 特有：fd_dup 直接返回新 fd（正数），失败返回正 errno。
 * 标准 preview1 没有这个名字，故用自定义 import 显式声明。 */
__attribute__((import_module("wasi_snapshot_preview1"), import_name("fd_dup")))
int32_t wasi_fd_dup(int32_t fd);

/* wasi-libc 的 dup2() 在 wasip1 上直接返回 ENOTSUP(58) 而不会转发 fd_renumber,
 * 所以 G4 必须自定义 import 直接打宿主实现, 否则测的是 libc 的桩而不是 wasmrun. */
__attribute__((import_module("wasi_snapshot_preview1"), import_name("fd_renumber")))
int32_t wasi_fd_renumber(int32_t from, int32_t to);

static const char MSG[] = "p3data";

int main(void) {
    printf("P3 start argc=%d\n", 0);
    fflush(stdout);

    /* ---- G1 进程控制：sched_yield ---- */
    errno = 0;
    check(sched_yield() == 0, "sched_yield");

    /* ---- G4 prestat：枚举 preopen ---- */
    {
        int found = 0;
        for (int fd = 3; fd < 12; fd++) {
            __wasi_prestat_t ps;
            if (__wasi_fd_prestat_get((__wasi_fd_t)fd, &ps) == 0
                && ps.tag == __WASI_PREOPENTYPE_DIR && ps.u.dir.pr_name_len >= 1) {
                char name[64];
                name[0] = 0;
                __wasi_errno_t e = __wasi_fd_prestat_dir_name(
                    (__wasi_fd_t)fd, (uint8_t *)name, ps.u.dir.pr_name_len);
                if (e == __WASI_ERRNO_SUCCESS && name[0] != 0) {
                    found++;
                }
            }
        }
        check(found >= 1, "prestat_enum");
    }

    /* ---- G5 基础文件：open + write ---- */
    errno = 0;
    int fd = open("/p3main.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
    check(fd >= 0, "open_create");
    if (fd >= 0) {
        check(write(fd, MSG, sizeof(MSG) - 1) == (ssize_t)(sizeof(MSG) - 1), "write");

        {
            /* 写后必须立刻读到新 size: 钉住 icache 失效语义 (旧实现 stat 恒回 0,
             * 而 lseek(SEEK_END) 正确, 两条元数据路径给出互相矛盾的答案). */
            __wasi_filestat_t fs0;
            memset(&fs0, 0, sizeof fs0);
            int r0 = (int)__wasi_fd_filestat_get((__wasi_fd_t)fd, &fs0);
            check(r0 == 0 && fs0.size == sizeof(MSG) - 1, "size_after_write");
            struct stat ps;
            int r1 = stat("/p3main.txt", &ps);
            check(r1 == 0 && ps.st_size == (off_t)(sizeof(MSG) - 1), "size_by_path");
        }

        /* ---- G5 fd_allocate ---- */
        errno = 0;
        check(posix_fallocate(fd, 0, 4096) == 0, "fd_allocate");
        struct stat st;
        int fr = fstat(fd, &st);
        check(fr == 0 && st.st_size >= 4096, "allocate_size");

        /* ---- G5 fd_advise ---- */
        errno = 0;
        check(posix_fadvise(fd, 0, 0, POSIX_FADV_NORMAL) == 0, "fd_advise");
        errno = 0;
        check(posix_fadvise(fd, 0, 0, 9) == EINVAL, "fd_advise_bad");

        /* ---- G4 fd_renumber：把 /p3main.txt 的 fd 挪到另一个 fd ---- */
        errno = 0;
        int other = open("/p3dup.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
        check(other >= 0, "open_dup_target");
        if (other >= 0) {
            errno = 0;
            check(wasi_fd_renumber(fd, other) == 0, "fd_renumber");
            /* other 现在指向 /p3main.txt：回到头读回 MSG */
            check(lseek(other, 0, SEEK_SET) == 0, "renumber_seek");
            char buf[16] = {0};
            check(read(other, buf, sizeof(MSG) - 1) == (ssize_t)(sizeof(MSG) - 1)
                      && memcmp(buf, MSG, sizeof(MSG) - 1) == 0,
                  "renumber_read");
            /* fd 已在 renumber 中让位，不再引用任何文件 */
            errno = 0;
            check(close(fd) != 0, "close_renumbered");
            fd = -1;
        }

        /* ---- G7 fd_dup（wasmrun 扩展）：复用 other ---- */
        if (other >= 0) {
            int dup_fd = wasi_fd_dup(other);
            struct stat dst;
            int stok = dup_fd > 2 ? fstat(dup_fd, &dst) : -1;
            int ok = dup_fd > 2 && stok == 0;
            check(ok, "fd_dup");
            if (ok) {
                close(dup_fd);
            }
        }
    }

    /* ---- G5 fd_seek SEEK_END ---- */
    errno = 0;
    int sf = open("/p3seek.txt", O_CREAT | O_RDWR | O_TRUNC, 0644);
    check(sf >= 0, "open_seek");
    if (sf >= 0) {
        char ten[10];
        memset(ten, 'x', sizeof(ten));
        check(write(sf, ten, sizeof(ten)) == (ssize_t)sizeof(ten), "seek_write");
        check(lseek(sf, 0, SEEK_END) == (off_t)sizeof(ten), "seek_end");
        struct stat sst;
        int sr = stat("/p3seek.txt", &sst);
        check(sr == 0 && sst.st_size == (off_t)sizeof(ten), "seek_stat");
        check(lseek(sf, 0, SEEK_SET) == 0, "seek_rewind");
        close(sf);
    }

    /* ---- G6 path_filestat_set_times ---- */
    {
        const char *p = "/p3main.txt";
        __wasi_errno_t e = __wasi_path_filestat_set_times(
            (__wasi_fd_t)3, 0, p, 1000000000ull, 2000000000ull,
            __WASI_FSTFLAGS_ATIM | __WASI_FSTFLAGS_MTIM);
        check(e == __WASI_ERRNO_SUCCESS, "path_filestat_set_times");
        const char *q = "/bin/wasi_p3.wasm";
        __wasi_errno_t e2 = __wasi_path_filestat_set_times(
            (__wasi_fd_t)3, 0, q, 1000000000ull, 2000000000ull,
            __WASI_FSTFLAGS_ATIM | __WASI_FSTFLAGS_MTIM);
        /* cpio 里的现成文件同样要成功: 证明 ramfs 真的实现了 fs_utimensat,
         * 而不是只对本次新建的节点生效. */
        check(e2 == __WASI_ERRNO_SUCCESS, "path_filestat_set_times_cpio");
    }

    /* ---- G6 mkdir + 多条目 readdir ---- */
    errno = 0;
    int mk = mkdir("/p3dir", 0755);
    check(mk == 0, "mkdir");
    if (mk == 0) {
        struct stat est;
        check(stat("/p3dir", &est) == 0 && S_ISDIR(est.st_mode), "mkdir_stat");
        for (int i = 0; i < 3; i++) {
            char name[32];
            snprintf(name, sizeof(name), "/p3dir/f%d.txt", i);
            int f = open(name, O_CREAT | O_RDWR, 0644);
            check(f >= 0, "mkdir_child_open");
            if (f >= 0) close(f);
        }
        /* libc DIR* 一次 fd_readdir 要拿到多条: host 只回 1 条时这里会数到 1
         * (目录实际有 `.` `..` + 3 个文件, 共 5 条). */
        DIR *d = opendir("/p3dir");
        int n = 0;
        if (d) {
            while (readdir(d)) n++;
            closedir(d);
        }
        check(n >= 5, "readdir_multi");

        /* rewinddir 后重数一遍: host 必须在 cookie==0 时把目录游标复位. */
        d = opendir("/p3dir");
        int n2 = 0;
        if (d) {
            while (readdir(d)) n2++;
            rewinddir(d);
            while (readdir(d)) n2++;
            closedir(d);
        }
        check(n2 == 2 * n, "readdir_rewind");
    }

    /* ---- 错误分支 ---- */
    errno = 0;
    check(open("/p3missing.txt", O_RDONLY) < 0 && errno == ENOENT, "err_open_enoent");

    errno = 0;
    check(unlink("/p3missing.txt") != 0 && errno == ENOENT, "err_unlink_enoent");

    errno = 0;
    check(mkdir("/p3dir", 0755) != 0 && errno == EEXIST, "err_mkdir_eexist");

    errno = 0;
    check(rmdir("/p3dir") != 0 && errno == ENOTEMPTY, "err_rmdir_notempty");

    errno = 0;
    check(open("/p3main.txt", O_RDONLY | O_DIRECTORY) < 0 && errno == ENOTDIR,
          "err_open_notdir");

    errno = 0;
    check(write(999, MSG, 1) < 0 && errno == EBADF, "err_write_badf");

    /* ---- 清理 ---- */
    for (int i = 0; i < 3; i++) {
        char name[32];
        snprintf(name, sizeof(name), "/p3dir/f%d.txt", i);
        unlink(name);
    }
    rmdir("/p3dir");
    unlink("/p3main.txt");
    unlink("/p3dup.txt");
    unlink("/p3seek.txt");

    printf("P3 done fails=%d\n", fails);
    return fails == 0 ? 0 : 3;
}
