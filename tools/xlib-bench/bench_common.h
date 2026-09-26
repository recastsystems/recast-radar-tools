/* Shared timing and JSON output for the C and C++ harnesses of
 * tools/xlib-bench (docs/perf/cross-library.md). Linux only. */
#ifndef XLIB_BENCH_COMMON_H
#define XLIB_BENCH_COMMON_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double xlib_now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

/* A /proc/self/status value in KiB ("VmRSS:", "VmHWM:"), 0 if unknown. */
static long xlib_status_kb(const char *key) {
  FILE *f = fopen("/proc/self/status", "r");
  char line[256];
  size_t len = strlen(key);
  long kb = 0;
  if (!f) return 0;
  while (fgets(line, sizeof line, f)) {
    if (strncmp(line, key, len) == 0) {
      kb = strtol(line + len, NULL, 10);
      break;
    }
  }
  fclose(f);
  return kb;
}

static long xlib_rss_kb(void) { return xlib_status_kb("VmRSS:"); }

static int xlib_cmp(const void *a, const void *b) {
  double x = *(const double *)a, y = *(const double *)b;
  return (x > y) - (x < y);
}

static void xlib_report(const char *lib, const char *format, const double *samples,
                        int iters, long sweeps, long fields, long gates, long rss_before) {
  double *sorted = (double *)malloc(sizeof(double) * (iters > 0 ? iters : 1));
  int i;
  if (iters > 0) memcpy(sorted, samples, sizeof(double) * iters);
  qsort(sorted, iters, sizeof(double), xlib_cmp);
  printf("{\"lib\":\"%s\",\"format\":\"%s\",\"iters\":%d,\"median_ms\":%.3f,"
         "\"min_ms\":%.3f,\"samples_ms\":[",
         lib, format, iters, iters ? sorted[iters / 2] : 0.0, iters ? sorted[0] : 0.0);
  for (i = 0; i < iters; i++) printf("%s%.3f", i ? "," : "", samples[i]);
  /* VmHWM: this process's own peak resident set since exec (the driver's
   * GNU time figure is the primary one; this is its cross-check). */
  printf("],\"sweeps\":%ld,\"fields\":%ld,\"gates\":%ld,\"rss_before_kb\":%ld,"
         "\"self_hwm_kb\":%ld}\n",
         sweeps, fields, gates, rss_before, xlib_status_kb("VmHWM:"));
  free(sorted);
}

/* argv: FORMAT FILE ITERS WARMUP [WAIT_STDIN] */
#define XLIB_ARGS(argc, argv, fmt, path, iters, warmup)                        \
  do {                                                                        \
    if ((argc) < 5) {                                                         \
      fprintf(stderr, "usage: %s FORMAT FILE ITERS WARMUP [1]\n", (argv)[0]); \
      return 2;                                                               \
    }                                                                         \
    fmt = (argv)[1];                                                          \
    path = (argv)[2];                                                         \
    iters = atoi((argv)[3]);                                                  \
    warmup = atoi((argv)[4]);                                                 \
    if ((argc) > 5 && atoi((argv)[5])) {                                      \
      char line_[16];                                                         \
      if (!fgets(line_, sizeof line_, stdin)) return 2;                       \
    }                                                                         \
  } while (0)

#endif
