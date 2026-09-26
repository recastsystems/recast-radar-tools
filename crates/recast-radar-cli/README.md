# recast-radar-cli

The `recast-radar` command: one binary over the recast-radar-tools crates.

```text
recast-radar info KTLX20240315_000217_V06          # format, site, time, VCP, sweeps, fields
recast-radar dump --json --sweep 0 bejab.pvol.h5   # every decoded value as JSON
recast-radar dump --fm301 file                     # the FM301 (CfRadial 2) group tree
recast-radar render file -o dbz.png                # one sweep to PNG
recast-radar validate -r ./corpus                  # decode and check; exit 1 on failure
recast-radar bench --threads 1 file                # decode timing
recast-radar fetch level2 KTLX -n 3                # AWS Level II; also chunks, level3, intl, polling
recast-radar serve ./polling --bind 0.0.0.0:8080   # HTTP for a GR2Analyst polling directory
recast-radar convert in --to level2 -o out.ar2v    # needs a writer (see below)
recast-radar publish in --dir ./polling            # a GR2Analyst polling directory
```

It reads NEXRAD Level II (uncompressed, gzip, bzip2, LDM records, real-time
chunks), NEXRAD and TDWR Level III, ODIM_H5, CfRadial 1 and 2 (classic
netCDF and netCDF-4), DORADE sweep files and mobile archives, and JMA radar
GRIB2 tars, detecting the format from the file contents.

`convert` writes NEXRAD Level II (Archive II files or real-time chunks),
CfRadial 1, ODIM_H5 and FM301 (CfRadial 2), and `publish` places Level II
volumes in a GR2Analyst polling directory that follows the GRLevelX polling
conventions. Both go through the `backend` module's `VolumeWriter` and
`PollingPublisher` traits; `Backends::builtin` registers the writers of the
`writers` module.

The `net` feature (default) enables `fetch`. Without it the binary makes no
network requests and compiles no C.

Exit status: 0 success, 1 failure, 2 invalid arguments, 3 not available in
this build.

The guide is [`docs/guide/cli.md`](../../docs/guide/cli.md) in the repository.
