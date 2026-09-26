/* RSL (TRMM Radar Software Library 1.50) harness: RSL_wsr88d_to_radar for
 * Level II, RSL_dorade_to_radar for DORADE sweep files. Each sample is
 * path -> Radar (every moment of every ray decoded to RSL's Range codes);
 * RSL_free_radar runs outside the timed region. RSL inflates gzip input
 * through an external gzip process (its own design), which is timed.
 * RSL needs a WSR-88D call sign from its site table: RSL_SITE, else the
 * first four characters of the file name, else KTLX (the decode work does
 * not depend on it; only the reported location does). */
#include "bench_common.h"
#include "rsl.h"

static const char *site_for(const char *path) {
  static char site[5];
  const char *env = getenv("RSL_SITE"), *base = strrchr(path, '/');
  if (env && *env) return env;
  base = base ? base + 1 : path;
  if (strlen(base) >= 4 && base[0] == 'K') {
    memcpy(site, base, 4);
    site[4] = 0;
    return site;
  }
  return "KTLX";
}

int main(int argc, char **argv) {
  const char *fmt, *path;
  int iters, warmup, i, v, s, r;
  long sweeps = 0, fields = 0, gates = 0, rss_before;
  double *samples;
  XLIB_ARGS(argc, argv, fmt, path, iters, warmup);
  RSL_radar_verbose_off();
  samples = (double *)calloc(iters > 0 ? iters : 1, sizeof(double));
  rss_before = xlib_rss_kb();
  for (i = 0; i < warmup + iters; i++) {
    double t0 = xlib_now_ms(), t1;
    Radar *radar;
    if (strcmp(fmt, "l2") == 0)
      radar = RSL_wsr88d_to_radar((char *)path, (char *)site_for(path));
    else if (strcmp(fmt, "dorade") == 0)
      radar = RSL_dorade_to_radar((char *)path);
    else {
      fprintf(stderr, "rsl_bench: unsupported format %s\n", fmt);
      return 2;
    }
    t1 = xlib_now_ms();
    if (!radar) {
      fprintf(stderr, "rsl_bench: RSL could not read %s\n", path);
      return 1;
    }
    if (i >= warmup) samples[i - warmup] = t1 - t0;
    sweeps = fields = gates = 0;
    for (v = 0; v < radar->h.nvolumes; v++) {
      Volume *vol = radar->v[v];
      long vol_sweeps = 0;
      if (!vol) continue;
      fields++;
      for (s = 0; s < vol->h.nsweeps; s++) {
        Sweep *sw = vol->sweep[s];
        if (!sw) continue;
        vol_sweeps++;
        for (r = 0; r < sw->h.nrays; r++)
          if (sw->ray[r]) gates += sw->ray[r]->h.nbins;
      }
      if (vol_sweeps > sweeps) sweeps = vol_sweeps;
    }
    RSL_free_radar(radar);
  }
  xlib_report("rsl", fmt, samples, iters, sweeps, fields, gates, rss_before);
  return 0;
}
