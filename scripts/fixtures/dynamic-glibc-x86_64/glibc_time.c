#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

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
    if (now < 0) ok = 0;

    long long delta_time = (long long)now - (long long)tv.tv_sec;
    long long delta_real = (long long)real.tv_sec - (long long)tv.tv_sec;
    printf("[TIME] consistency time-gettimeofday=%lld real-gettimeofday=%lld\n",
           delta_time, delta_real);
    if (delta_time < -1 || delta_time > 1 || delta_real < -1 || delta_real > 1) {
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

    if (ok) {
        printf("[TIME] PASS\n");
    } else {
        printf("[TIME] FAIL\n");
    }
    return ok ? 0 : 1;
}
