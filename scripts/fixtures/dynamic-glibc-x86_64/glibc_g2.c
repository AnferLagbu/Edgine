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
