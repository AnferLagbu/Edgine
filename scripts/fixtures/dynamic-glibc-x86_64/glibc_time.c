#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <sys/time.h>

/* 2025-01-01 00:00:00 UTC: 低于此值说明墙钟仍按自启动计数, 未使用真实 RTC epoch. */
#define MIN_PLAUSIBLE_EPOCH 1735689600LL

int main(void) {
    struct timespec mono = {0, 0}, real = {0, 0}, res = {0, 0};
    struct timeval tv = {0, 0};
    time_t now = (time_t)-1;
    int ok = 1;

    errno = 0;
    int rc = clock_gettime(CLOCK_MONOTONIC, &mono);
    printf("[TIME] clock_gettime mono rc=%d errno=%d sec=%lld nsec=%lld\n",
           rc, errno, (long long)mono.tv_sec, (long long)mono.tv_nsec);
    if (rc != 0) ok = 0;

    errno = 0;
    rc = clock_gettime(CLOCK_REALTIME, &real);
    printf("[TIME] clock_gettime real rc=%d errno=%d sec=%lld nsec=%lld\n",
           rc, errno, (long long)real.tv_sec, (long long)real.tv_nsec);
    if (rc != 0) ok = 0;

    errno = 0;
    rc = gettimeofday(&tv, NULL);
    printf("[TIME] gettimeofday rc=%d errno=%d sec=%lld usec=%lld\n",
           rc, errno, (long long)tv.tv_sec, (long long)tv.tv_usec);
    if (rc != 0) ok = 0;

    errno = 0;
    now = time(NULL);
    printf("[TIME] time rc=%lld errno=%d\n", (long long)now, errno);
    printf("[TIME] epoch now=%lld floor=%lld\n", (long long)now, (long long)MIN_PLAUSIBLE_EPOCH);
    if (now < MIN_PLAUSIBLE_EPOCH) ok = 0;

    long long delta_time = (long long)now - (long long)tv.tv_sec;
    long long delta_real = (long long)real.tv_sec - (long long)tv.tv_sec;
    printf("[TIME] consistency time-gettimeofday=%lld real-gettimeofday=%lld\n",
           delta_time, delta_real);
    if (delta_time < -1 || delta_time > 1 || delta_real < -1 || delta_real > 1) {
        ok = 0;
    }
    if (real.tv_sec < MIN_PLAUSIBLE_EPOCH) {
        ok = 0;
    }

    unsigned int cpu = 0xdeadbeefu;
    unsigned int node = 0xdeadbeefu;
    errno = 0;
    long cpu_rc = syscall(SYS_getcpu, &cpu, &node, NULL);
    printf("[TIME] getcpu rc=%ld errno=%d cpu=%u node=%u\n",
           cpu_rc, errno, cpu, node);
    if (cpu_rc != 0 || cpu > 64 || node > 64) {
        ok = 0;
    }

    errno = 0;
    rc = clock_getres(CLOCK_MONOTONIC, &res);
    printf("[TIME] clock_getres rc=%d errno=%d sec=%lld nsec=%lld\n",
           rc, errno, (long long)res.tv_sec, (long long)res.tv_nsec);
    if (rc != 0) ok = 0;

    if (now >= MIN_PLAUSIBLE_EPOCH && real.tv_sec >= MIN_PLAUSIBLE_EPOCH) {
        printf("[TIME] EPOCH PASS\n");
    }

    /* D7 T1: root 下验证 settimeofday 回写路径: 前进 2 秒再恢复, 避免污染后续腿. */
    if (geteuid() == 0) {
        struct timeval before, target, after;
        if (gettimeofday(&before, NULL) == 0) {
            target = before;
            target.tv_sec += 2;
            int set_rc = settimeofday(&target, NULL);
            int set_errno = errno;
            int get_rc = gettimeofday(&after, NULL);
            int restore_rc = settimeofday(&before, NULL);
            printf("[TIME] set rc=%d errno=%d target=%lld after=%lld restore=%d\n",
                   set_rc, set_errno, (long long)target.tv_sec,
                   (long long)after.tv_sec, restore_rc);
            if (set_rc == 0 && get_rc == 0
                && after.tv_sec >= target.tv_sec && restore_rc == 0) {
                printf("[TIME] SET PASS\n");
            } else {
                printf("[TIME] SET FAIL\n");
                ok = 0;
            }
        }
    }

    if (ok) {
        printf("[TIME] PASS\n");
    } else {
        printf("[TIME] FAIL\n");
    }
    return ok ? 0 : 1;
}
