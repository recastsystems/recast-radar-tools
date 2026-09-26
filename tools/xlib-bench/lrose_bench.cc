// LROSE Radx harness: RadxFile::readFromPath (format detected by Radx:
// NEXRAD Level II, NIDS Level III, ODIM HDF5, CfRadial, DORADE) followed by
// RadxVol::loadFieldsFromRays, so every field is one contiguous array as the
// Radx apps use it. Each sample is path -> RadxVol.
#include <vector>
#include <string>
#include <Radx/RadxFile.hh>
#include <Radx/RadxVol.hh>
#include <Radx/RadxField.hh>
#include "bench_common.h"

int main(int argc, char **argv) {
  const char *fmt, *path;
  int iters, warmup;
  XLIB_ARGS(argc, argv, fmt, path, iters, warmup);
  std::vector<double> samples;
  long sweeps = 0, fields = 0, gates = 0;
  long rss_before = xlib_rss_kb();
  for (int i = 0; i < warmup + iters; i++) {
    double t0 = xlib_now_ms();
    RadxFile file;
    RadxVol vol;
    if (file.readFromPath(path, vol)) {
      fprintf(stderr, "LROSE read failed: %s\n", file.getErrStr().c_str());
      return 1;
    }
    vol.loadFieldsFromRays();
    double t1 = xlib_now_ms();
    if (i >= warmup) samples.push_back(t1 - t0);
    sweeps = (long)vol.getNSweeps();
    const std::vector<RadxField *> &all = vol.getFields();
    fields = (long)all.size();
    gates = 0;
    for (const RadxField *field : all) gates += (long)field->getNPoints();
  }
  xlib_report("lrose", fmt, samples.data(), (int)samples.size(), sweeps, fields, gates,
              rss_before);
  return 0;
}
