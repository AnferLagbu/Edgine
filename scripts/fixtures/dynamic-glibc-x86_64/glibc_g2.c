#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

struct rseq_area {
    uint32_t cpu_id_start;
    uint32_t cpu_id;
    uint64_t rseq_cs;
    uint32_t flags;
    uint8_t pad[12];
};

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

    struct rseq_area area = {0};
    errno = 0;
    rc = syscall(SYS_rseq, &area, sizeof(area), 0, 0);
    printf("[G2] rseq rc=%ld errno=%d\n", rc, errno);
    if (rc == -1 && errno == ENOSYS) {
        printf("[G2] RSEQ ENOSYS\n");
    } else {
        printf("[G2] RSEQ UNEXPECTED\n");
    }

    return ok ? 0 : 1;
}
