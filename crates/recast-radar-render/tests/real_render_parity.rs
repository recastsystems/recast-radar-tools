//! Pixel fingerprints of every CPU raster path on real Level II volumes.
//!
//! Each case decodes a real file and hashes (FNV-1a 64) the RGBA output of the
//! PNG raster, the viewport raster at two zooms, sample and geometry caches,
//! storm-relative velocity (with and without palette and sample caches),
//! dealiased velocity, float-valued derived fields and display-resampled
//! fields, plus the cache sizes. `goldens/render_fingerprints.txt` pins the
//! renderer's output, so a refactor that changes one pixel fails here.
//!
//! Cases: a modern dual-pol split cut (8- and 16-bit moments), a Message 1
//! split cut (1 km reflectivity, 250 m Doppler starting at -375 m) and a
//! legacy-resolution Message 31 volume whose sweeps carry 1 km reflectivity
//! and 250 m Doppler moments side by side.

// Test code: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::{MomentGrid, MomentStorage, MomentType, RadarVolume};
use recast_radar_render::{
    ColorTableFamily, ColorTableSet, RasterOptions, StormMotion, ViewportMomentCache,
    ViewportRasterOptions, render_moment_image, render_storm_relative_velocity_image,
    render_storm_relative_velocity_viewport_rgba, viewport_rgba_buffer_len,
    viewport_sample_cache_storage_upper_bound_for_grid,
};

const GOLDENS: &str = include_str!("goldens/render_fingerprints.txt");

const CASES: [&str; 3] = [
    "l2-ktlx-20240315-000217-trim",
    "l2-klix-20050829-130035-trim",
    "l2-kpah-20080415-235014",
];

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn raster() -> RasterOptions {
    RasterOptions {
        width: 160,
        height: 160,
        range_fraction: 94,
    }
}

/// Wide view: the whole sweep at 2 km per pixel, slightly rotated.
fn viewport() -> ViewportRasterOptions {
    ViewportRasterOptions {
        width: 200,
        height: 150,
        radar_x_px: 100.0,
        radar_y_px: 82.5,
        km_per_px_x: 2.0,
        km_per_px_y: 2.0,
        rotation_rad: 0.02,
    }
}

/// Close view: 0.2 km per pixel near the radar, where 1 km and 250 m gates
/// differ pixel by pixel.
fn zoom() -> ViewportRasterOptions {
    ViewportRasterOptions {
        width: 240,
        height: 180,
        radar_x_px: 150.0,
        radar_y_px: 60.0,
        km_per_px_x: 0.2,
        km_per_px_y: 0.2,
        rotation_rad: -0.035,
    }
}

fn storm() -> StormMotion {
    StormMotion {
        direction_deg: 225.0,
        speed_mps: 15.0,
    }
}

#[derive(Default)]
struct Fingerprints(Vec<(String, u64)>);

impl Fingerprints {
    fn push(&mut self, label: impl Into<String>, value: u64) {
        self.0.push((label.into(), value));
    }

    fn pixels(&mut self, label: impl Into<String>, pixels: &[u8]) {
        self.push(label, fnv(pixels));
    }
}

fn filled(options: ViewportRasterOptions) -> Vec<u8> {
    vec![255; viewport_rgba_buffer_len(options)]
}

/// Direct, sample-cache and geometry-cache viewport paths for one cache.
fn cache_paths(
    fp: &mut Fingerprints,
    label: &str,
    volume: &RadarVolume,
    cache: &ViewportMomentCache,
) {
    let options = viewport();
    let mut direct = filled(options);
    cache
        .render_moment_rgba_into(volume, options, &mut direct)
        .unwrap();
    fp.pixels(format!("{label}/viewport"), &direct);
    let mut close = filled(zoom());
    cache
        .render_moment_rgba_into(volume, zoom(), &mut close)
        .unwrap();
    fp.pixels(format!("{label}/zoom"), &close);

    let sample_cache = cache.build_sample_cache(volume, options).unwrap();
    fp.push(
        format!("{label}/samples"),
        sample_cache.sample_count() as u64,
    );
    fp.push(
        format!("{label}/sample_bytes"),
        sample_cache.storage_bytes() as u64,
    );
    let mut cached = filled(options);
    cache
        .render_moment_rgba_with_sample_cache(volume, &sample_cache, &mut cached)
        .unwrap();
    fp.pixels(format!("{label}/sample_cache"), &cached);
    cache
        .render_moment_rgba_with_sample_cache_reusing_transparency(
            volume,
            &sample_cache,
            &mut cached,
        )
        .unwrap();
    fp.pixels(format!("{label}/sample_cache_reuse"), &cached);

    let geometry_cache = cache.build_geometry_cache(volume, zoom()).unwrap();
    fp.push(
        format!("{label}/geometry_samples"),
        geometry_cache.sample_count() as u64,
    );
    let resolved = cache
        .build_sample_cache_from_geometry_cache(volume, &geometry_cache)
        .unwrap();
    fp.push(
        format!("{label}/geometry_resolved"),
        resolved.sample_count() as u64,
    );
    let mut from_geometry = filled(zoom());
    cache
        .render_moment_rgba_with_sample_cache(volume, &resolved, &mut from_geometry)
        .unwrap();
    fp.pixels(format!("{label}/geometry_cache"), &from_geometry);
    fp.push(
        format!("{label}/upper_bound"),
        cache
            .sample_cache_storage_upper_bound(volume, zoom())
            .unwrap() as u64,
    );
}

/// Storm-relative paths of a velocity cache.
fn storm_relative_paths(
    fp: &mut Fingerprints,
    label: &str,
    volume: &RadarVolume,
    cache: &ViewportMomentCache,
) {
    let options = viewport();
    let mut direct = filled(options);
    cache
        .render_storm_relative_velocity_rgba_into(volume, storm(), options, &mut direct)
        .unwrap();
    fp.pixels(format!("{label}/srv_viewport"), &direct);
    let mut close = filled(zoom());
    cache
        .render_storm_relative_velocity_rgba_into(volume, storm(), zoom(), &mut close)
        .unwrap();
    fp.pixels(format!("{label}/srv_zoom"), &close);

    let sample_cache = cache.build_sample_cache(volume, options).unwrap();
    let mut cached = filled(options);
    cache
        .render_storm_relative_velocity_rgba_with_sample_cache(
            volume,
            storm(),
            &sample_cache,
            &mut cached,
        )
        .unwrap();
    fp.pixels(format!("{label}/srv_sample_cache"), &cached);
    cache
        .render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency(
            volume,
            StormMotion {
                direction_deg: 40.0,
                speed_mps: 22.0,
            },
            &sample_cache,
            &mut cached,
        )
        .unwrap();
    fp.pixels(format!("{label}/srv_sample_cache_reuse"), &cached);

    match cache
        .build_storm_relative_velocity_palette_cache(volume, storm())
        .unwrap()
    {
        Some(palette_cache) => {
            let mut palette = filled(options);
            cache
                .render_storm_relative_velocity_rgba_into_with_palette_cache(
                    volume,
                    storm(),
                    &palette_cache,
                    options,
                    &mut palette,
                )
                .unwrap();
            fp.pixels(format!("{label}/srv_palette"), &palette);
            let mut both = filled(options);
            cache
                .render_storm_relative_velocity_rgba_with_sample_cache_and_palette_cache(
                    volume,
                    storm(),
                    &palette_cache,
                    &sample_cache,
                    &mut both,
                )
                .unwrap();
            fp.pixels(format!("{label}/srv_sample_palette"), &both);
            cache
                .render_storm_relative_velocity_rgba_with_sample_cache_reusing_transparency_and_palette_cache(
                    volume,
                    storm(),
                    &palette_cache,
                    &sample_cache,
                    &mut both,
                )
                .unwrap();
            fp.pixels(format!("{label}/srv_sample_palette_reuse"), &both);
        }
        None => fp.push(format!("{label}/srv_palette"), 0),
    }
}

fn lowest_cut_with(volume: &RadarVolume, moment: &MomentType) -> Option<usize> {
    volume
        .cuts
        .iter()
        .enumerate()
        .filter(|(_, cut)| {
            cut.moments
                .get(moment)
                .is_some_and(|grid| !grid.radial_indices.is_empty())
        })
        .min_by(|(li, lc), (ri, rc)| {
            lc.elevation_deg
                .total_cmp(&rc.elevation_deg)
                .then_with(|| li.cmp(ri))
        })
        .map(|(index, _)| index)
}

/// Physical `f32` copy of a grid (NaN for every sentinel).
fn physical_copy(grid: &MomentGrid) -> MomentGrid {
    let rows = grid.radial_count();
    let gates = grid.gate_range.gate_count;
    let mut values = vec![f32::NAN; rows * gates];
    for row in 0..rows {
        for gate in 0..gates {
            if let Some(value) = grid.scaled_value(row, gate) {
                values[row * gates + gate] = value;
            }
        }
    }
    MomentGrid {
        moment: grid.moment.clone(),
        gate_range: grid.gate_range.clone(),
        scale: 1.0,
        offset: 0.0,
        nodata: None,
        range_folded: None,
        radial_indices: grid.radial_indices.clone(),
        storage: MomentStorage::F32(values),
    }
}

fn fingerprint_volume(volume: &RadarVolume) -> Fingerprints {
    let mut fp = Fingerprints::default();
    let tables = ColorTableSet::default();

    // Every moment of every cut: PNG raster plus the viewport cache paths.
    for (cut_index, cut) in volume.cuts.iter().enumerate() {
        for (moment, grid) in &cut.moments {
            if grid.radial_indices.is_empty() {
                continue;
            }
            let label = format!("cut{cut_index}/{moment}");
            let image = render_moment_image(volume, cut_index, moment.clone(), raster()).unwrap();
            fp.pixels(format!("{label}/image"), image.as_raw());
            fp.push(
                format!("{label}/grid_upper_bound"),
                viewport_sample_cache_storage_upper_bound_for_grid(grid, zoom()) as u64,
            );
            let cache = ViewportMomentCache::new(volume, cut_index, moment.clone()).unwrap();
            cache_paths(&mut fp, &label, volume, &cache);
        }
    }

    if let Some(cut) = lowest_cut_with(volume, &MomentType::Velocity) {
        let label = format!("cut{cut}/VEL");
        let image = render_storm_relative_velocity_image(volume, cut, storm(), raster()).unwrap();
        fp.pixels(format!("{label}/srv_image"), image.as_raw());
        let (_, _, pixels) =
            render_storm_relative_velocity_viewport_rgba(volume, cut, storm(), viewport()).unwrap();
        fp.pixels(format!("{label}/srv_free_viewport"), &pixels);
        let cache = ViewportMomentCache::new(volume, cut, MomentType::Velocity).unwrap();
        storm_relative_paths(&mut fp, &label, volume, &cache);

        // Dealiased velocity (u16 storage).
        let label = format!("cut{cut}/DVEL");
        let dealiased = ViewportMomentCache::new_dealiased_velocity(volume, cut).unwrap();
        cache_paths(&mut fp, &label, volume, &dealiased);
        storm_relative_paths(&mut fp, &label, volume, &dealiased);

        // Physical f32 velocity drawn as a derived field.
        let label = format!("cut{cut}/VEL_F32");
        let grid = physical_copy(&volume.cuts[cut].moments[&MomentType::Velocity]);
        let derived = ViewportMomentCache::new_derived(
            volume,
            cut,
            grid,
            ColorTableFamily::Velocity,
            &tables,
        )
        .unwrap();
        cache_paths(&mut fp, &label, volume, &derived);
        storm_relative_paths(&mut fp, &label, volume, &derived);
    }

    if let Some(cut) = lowest_cut_with(volume, &MomentType::Reflectivity) {
        let reflectivity = &volume.cuts[cut].moments[&MomentType::Reflectivity];
        if let Some(composite) = recast_radar_map::composite_reflectivity_grid(volume) {
            let label = format!("cut{cut}/CREF");
            let derived = ViewportMomentCache::new_derived(
                volume,
                cut,
                composite,
                ColorTableFamily::Reflectivity,
                &tables,
            )
            .unwrap();
            cache_paths(&mut fp, &label, volume, &derived);
        }
        let smoothed = recast_radar_filters::smooth_moment_grid(reflectivity);
        let label = format!("cut{cut}/REF_SMOOTH");
        let derived = ViewportMomentCache::new_derived(
            volume,
            cut,
            smoothed,
            ColorTableFamily::Reflectivity,
            &tables,
        )
        .unwrap();
        cache_paths(&mut fp, &label, volume, &derived);
        if let Some(up) =
            recast_radar_filters::upsample_moment_grid(&volume.cuts[cut], reflectivity)
        {
            let label = format!("cut{cut}/REF_UPSAMPLED");
            let resampled = ViewportMomentCache::new_resampled(
                volume,
                cut,
                up.grid,
                &up.row_azimuths_deg,
                ColorTableFamily::Reflectivity,
                &tables,
            )
            .unwrap();
            cache_paths(&mut fp, &label, volume, &resampled);
        }
    }
    fp
}

fn decode(id: &str) -> Option<RadarVolume> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            return None;
        }
        Err(err) => panic!("{err}"),
    };
    Some(recast_radar_io_nexrad::decode_volume_from_path(&path).unwrap())
}

/// Golden lines `<case id> <label> 0x<hash>` for one case.
fn golden_lines(id: &str) -> Vec<&'static str> {
    GOLDENS
        .lines()
        .filter(|line| line.split_whitespace().next() == Some(id))
        .collect()
}

#[test]
fn raster_paths_match_pinned_fingerprints() {
    let mut failures = String::new();
    for id in CASES {
        let Some(volume) = decode(id) else { continue };
        let actual: Vec<String> = fingerprint_volume(&volume)
            .0
            .iter()
            .map(|(label, value)| format!("{id} {label} 0x{value:016x}"))
            .collect();
        let expected = golden_lines(id);
        if actual
            .iter()
            .map(String::as_str)
            .ne(expected.iter().copied())
        {
            failures.push_str(&format!(
                "{id}: {} fingerprints, {} golden; actual:\n{}\n",
                actual.len(),
                expected.len(),
                actual.join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{failures}");
}
