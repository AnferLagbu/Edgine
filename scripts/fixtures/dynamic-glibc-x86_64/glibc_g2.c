#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/rseq.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef RSEQ_SIG
#define RSEQ_SIG 0x53053053u
#endif

static struct rseq *g_rseq_area;
static volatile int g_rseq_cleared;
static struct rseq_cs g_rseq_cs __attribute__((aligned(32)));

/* start_ip 必须落在不执行的地址, 使信号 IP 在临界区外; 内核据此走
 * 非重启用 clear 路径, 异步信号与宿主 Linux 语义一致. */
static void rseq_dummy_start(void) { __asm__ volatile(""); }

static void rseq_signal_handler(int sig) {
    (void)sig;
    g_rseq_cleared = (g_rseq_area->rseq_cs == 0);
}

struct rlimit64_area {
    unsigned long long cur;
    unsigned long long max;
};

int main(void) {
    int ok = 1;
    int tid_slot = 0;
    long tid = syscall(SYS_gettid);

    errno = 0;
    long rc = syscall(SYS_set_tid_address, &tid_slot);
    printf("[G2] set_tid_address rc=%ld errno=%d gettid=%ld\n", rc, errno, tid);
    if (rc == tid) {
        printf("[G2] SET_TID PASS\n");
    } else {
        printf("[G2] SET_TID FAIL\n");
        ok = 0;
    }

    struct rlimit64_area old = {0, 0};
    errno = 0;
    rc = syscall(SYS_prlimit64, 0, 7 /* RLIMIT_NOFILE */, NULL, &old);
    printf("[G2] prlimit64 rc=%ld errno=%d nofile=%llu/%llu\n",
           rc, errno, old.cur, old.max);
    if (rc == 0 && old.cur > 0 && old.max >= old.cur) {
        struct rlimit64_area same = {old.cur, old.max};
        errno = 0;
        rc = syscall(SYS_prlimit64, 0, 7, &same, NULL);
        printf("[G2] prlimit64 set rc=%ld errno=%d\n", rc, errno);
        if (rc == 0) {
            printf("[G2] PRLIMIT PASS\n");
        } else {
            printf("[G2] PRLIMIT FAIL\n");
            ok = 0;
        }
    } else {
        printf("[G2] PRLIMIT FAIL\n");
        ok = 0;
    }

    struct rlimit64_area legacy = {0, 0};
    int consistent = 1;
    for (int res = 0; res < 16; res++) {
        struct rlimit64_area via_pr = {0, 0};
        struct rlimit64_area via_get = {0, 0};
        errno = 0;
        rc = syscall(SYS_prlimit64, 0, res, NULL, &via_pr);
        if (rc != 0) {
            printf("[G2] prlimit64 read res=%d rc=%ld errno=%d\n", res, rc, errno);
            consistent = 0;
            break;
        }
        errno = 0;
        rc = syscall(SYS_getrlimit, res, &via_get);
        if (rc != 0) {
            printf("[G2] getrlimit res=%d rc=%ld errno=%d\n", res, rc, errno);
            consistent = 0;
            break;
        }
        if (via_pr.cur != via_get.cur || via_pr.max != via_get.max) {
            printf("[G2] rlimit mismatch res=%d get=%llu/%llu pr=%llu/%llu\n",
                   res, via_get.cur, via_get.max, via_pr.cur, via_pr.max);
            consistent = 0;
            break;
        }
        if (res == 7) {
            legacy = via_get;
        }
    }
    if (consistent && legacy.cur > 0 && legacy.max >= legacy.cur) {
        printf("[G2] getrlimit/prlimit64 resources=16 consistent nofile=%llu/%llu\n",
               legacy.cur, legacy.max);
        printf("[G2] GETRLIMIT PASS\n");
    } else {
        printf("[G2] GETRLIMIT FAIL\n");
        ok = 0;
    }

    /*
     * D7 R1: glibc 2.44 在启动时用 33 字节 feature 区注册 rseq；Edgine 支持
     * rseq 后 __rseq_size 应为非 0，且内核在返回用户态时写 cpu_id_start/cpu_id。
     * 这里先验证 glibc 自注册，再验证内核注册/注销/信号清理语义。
     */
    struct rseq *area =
        (struct rseq *)((char *)__builtin_thread_pointer() + __rseq_offset);
    unsigned int rseq_len = __rseq_size < 32 ? 32 : __rseq_size;
    if (area->cpu_id != RSEQ_CPU_ID_REGISTRATION_FAILED) {
        g_rseq_area = area;
        printf("[G2] glibc rseq feature=%u reg_len=%u offset=%td flags=%u cpu=%u/%u\n",
               __rseq_size, rseq_len, __rseq_offset, __rseq_flags,
               area->cpu_id_start, area->cpu_id);

        if (area->cpu_id_start != area->cpu_id || area->cpu_id >= 64) {
            printf("[G2] RSEQ CPU ID FAIL\n");
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, rseq_len, 0, RSEQ_SIG);
        if (rc != -1 || errno != EBUSY) {
            printf("[G2] RSEQ duplicate rc=%ld errno=%d (want EBUSY)\n", rc, errno);
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, rseq_len, 2, RSEQ_SIG);
        if (rc != -1 || (errno != EINVAL && errno != EBUSY)) {
            printf("[G2] RSEQ flags rc=%ld errno=%d (want EINVAL/EBUSY)\n", rc, errno);
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, 31, 0, RSEQ_SIG);
        if (rc != -1 || errno != EINVAL) {
            printf("[G2] RSEQ len rc=%ld errno=%d (want EINVAL)\n", rc, errno);
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, rseq_len, RSEQ_FLAG_UNREGISTER,
                     RSEQ_SIG ^ 1u);
        if (rc != -1 || errno != EPERM) {
            printf("[G2] RSEQ sig rc=%ld errno=%d (want EPERM)\n", rc, errno);
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, rseq_len, RSEQ_FLAG_UNREGISTER, RSEQ_SIG);
        if (rc != 0) {
            printf("[G2] RSEQ unregister rc=%ld errno=%d\n", rc, errno);
            ok = 0;
        } else if (area->cpu_id_start != 0 || area->cpu_id != 0xffffffffu) {
            printf("[G2] RSEQ unregister state cpu=%u/%u\n",
                   area->cpu_id_start, area->cpu_id);
            ok = 0;
        }

        errno = 0;
        rc = syscall(SYS_rseq, area, rseq_len, 0, RSEQ_SIG);
        if (rc != 0) {
            printf("[G2] RSEQ re-register rc=%ld errno=%d\n", rc, errno);
            ok = 0;
        } else if (area->cpu_id_start != area->cpu_id || area->cpu_id >= 64) {
            printf("[G2] RSEQ register state cpu=%u/%u\n",
                   area->cpu_id_start, area->cpu_id);
            ok = 0;
        }

        g_rseq_cleared = 0;
        if (signal(SIGALRM, rseq_signal_handler) == SIG_ERR) {
            printf("[G2] RSEQ signal install FAIL\n");
            ok = 0;
        } else {
            /* 构造合法 rseq_cs：abort_ip 前 4 字节必须是 RSEQ_SIG，
             * 否则内核 rseq_get_rseq_cs 校验失败，不会进入清理路径. */
            g_rseq_cs.version = 0;
            g_rseq_cs.flags = 0;
            g_rseq_cs.start_ip = (uint64_t)(uintptr_t)&rseq_dummy_start;
            g_rseq_cs.abort_ip = (uint64_t)(uintptr_t)&&rseq_signal_abort;
            g_rseq_cs.post_commit_offset = 4;
            area->rseq_cs = (uint64_t)(uintptr_t)&g_rseq_cs;
            ualarm(10000, 0);
            for (volatile int i = 0; i < 100000000 && !g_rseq_cleared; i++) {
            }
            goto rseq_signal_after;
            __asm__ volatile(".long 0x53053053");
        rseq_signal_abort:
        rseq_signal_after:
            if (!g_rseq_cleared || area->rseq_cs != 0) {
                printf("[G2] RSEQ signal cleanup FAIL cs=%llu\n",
                       (unsigned long long)area->rseq_cs);
                ok = 0;
            }
        }

        if (ok) {
            printf("[G2] RSEQ PASS\n");
        } else {
            printf("[G2] RSEQ FAIL\n");
        }
    } else {
        printf("[G2] RSEQ REGISTRATION FAIL feature=%u cpu=%u\n",
               __rseq_size, area->cpu_id);
        printf("[G2] RSEQ FAIL\n");
        ok = 0;
    }

    return ok ? 0 : 1;
}
