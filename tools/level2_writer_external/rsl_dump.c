/*
 * Dump what RSL (NASA TRMM Radar Software Library) reads from a Level II
 * file, for tools/level2_writer_check.py.
 *
 *     rsl_dump IN SITE OUT
 *
 * SITE is the 4-letter call sign RSL looks up in its site table
 * (wsr88d_locations.dat); RSL refuses a file without one, and takes the
 * radar location from that table, never from the file.
 *
 * OUT is little-endian binary: the magic "RSLDUMP2", then one record per
 * (moment, sweep) RSL loaded, then the 4 bytes "END\0". A record is
 *
 *     char    name[4]      REF, VEL, SW, ZDR, PHI or RHO, NUL padded
 *     int32   sweep        index of the sweep in RSL's volume of the moment
 *                          (RSL drops a moment's empty sweeps, so the same
 *                          cut has other indices in other moments)
 *     int32   sweep_num    the cut's elevation number (sweep->h.sweep_num)
 *     int32   nrays        ray slots (RSL indexes rays by azimuth number)
 *     int32   nbins        the most gates of any ray
 *     float32 folded_as    what RSL's storage of the moment gives back for
 *                          a range-folded gate: -inf when it keeps RFVAL,
 *                          else the value it turns RFVAL into (its PHI and
 *                          RHO storage has no range-folded code)
 *     nrays x { float32 azimuth, elevation, nyquist; int32 range_bin1,
 *               gate_size, nbins, present }
 *     nrays x nbins float32 values
 *
 * Values are RSL's floats: NaN below threshold (BADVAL) and past a ray's
 * gates, -inf range folded (RFVAL). Split cuts are not merged.
 */
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <rsl.h>

static const struct {
    int index;
    const char *name;
} MOMENTS[] = {
    {DZ_INDEX, "REF"}, {VR_INDEX, "VEL"}, {SW_INDEX, "SW"},
    {DR_INDEX, "ZDR"}, {PH_INDEX, "PHI"}, {RH_INDEX, "RHO"},
};

static void put_i32(FILE *out, int32_t value) { fwrite(&value, 4, 1, out); }
static void put_f32(FILE *out, float value) { fwrite(&value, 4, 1, out); }

static void dump_sweep(FILE *out, const char *name, int index, Sweep *sweep) {
    int nrays = sweep->h.nrays;
    int nbins = 0;
    char tag[4] = {0};
    memcpy(tag, name, strlen(name) < sizeof tag ? strlen(name) : sizeof tag);
    for (int r = 0; r < nrays; r++) {
        Ray *ray = sweep->ray[r];
        if (ray != NULL && ray->h.nbins > nbins) nbins = ray->h.nbins;
    }
    fwrite(tag, 1, 4, out);
    put_i32(out, index);
    put_i32(out, sweep->h.sweep_num);
    put_i32(out, nrays);
    put_i32(out, nbins);
    float folded_as = NAN;
    for (int r = 0; r < nrays; r++) {
        Ray *ray = sweep->ray[r];
        if (ray == NULL) continue;
        float v = ray->h.f(ray->h.invf(RFVAL));
        folded_as = v == RFVAL ? -INFINITY : v;
        break;
    }
    put_f32(out, folded_as);
    for (int r = 0; r < nrays; r++) {
        Ray *ray = sweep->ray[r];
        put_f32(out, ray ? ray->h.azimuth : NAN);
        put_f32(out, ray ? ray->h.elev : NAN);
        put_f32(out, ray ? ray->h.nyq_vel : NAN);
        put_i32(out, ray ? ray->h.range_bin1 : 0);
        put_i32(out, ray ? ray->h.gate_size : 0);
        put_i32(out, ray ? ray->h.nbins : 0);
        put_i32(out, ray != NULL);
    }
    for (int r = 0; r < nrays; r++) {
        Ray *ray = sweep->ray[r];
        for (int g = 0; g < nbins; g++) {
            float value = NAN;
            if (ray != NULL && g < ray->h.nbins) {
                float v = ray->h.f(ray->range[g]);
                if (v == RFVAL) value = -INFINITY;
                else if (v != BADVAL && v != APFLAG && v != NOECHO) value = v;
            }
            put_f32(out, value);
        }
    }
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: rsl_dump IN SITE OUT\n");
        return 2;
    }
    RSL_wsr88d_merge_split_cuts_off();
    RSL_select_fields("all", NULL);
    Radar *radar = RSL_wsr88d_to_radar(argv[1], argv[2]);
    if (radar == NULL) {
        fprintf(stderr, "rsl_dump: RSL_wsr88d_to_radar failed on %s\n", argv[1]);
        return 1;
    }
    FILE *out = fopen(argv[3], "wb");
    if (out == NULL) {
        perror(argv[3]);
        return 1;
    }
    fwrite("RSLDUMP2", 1, 8, out);
    for (size_t m = 0; m < sizeof MOMENTS / sizeof MOMENTS[0]; m++) {
        Volume *volume = radar->v[MOMENTS[m].index];
        if (volume == NULL) continue;
        for (int s = 0; s < volume->h.nsweeps; s++) {
            Sweep *sweep = volume->sweep[s];
            if (sweep != NULL && sweep->h.nrays > 0) dump_sweep(out, MOMENTS[m].name, s, sweep);
        }
    }
    fwrite("END", 1, 4, out);
    if (fclose(out) != 0) {
        perror(argv[3]);
        return 1;
    }
    RSL_free_radar(radar);
    return 0;
}
