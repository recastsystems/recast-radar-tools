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
//!
//! The goldens were produced by the pre-FM301 renderer, so their labels use
//! the legacy moment names (`REF`, `VEL`, `CFP`, ...); [`legacy_label`] maps
//! each FM301 field name back. The pixels must be identical. The volumes come
//! from the native decoder; the base sweep of the derived products is the
//! lowest by tilt elevation (a Level II sweep's first-ray elevation, the
//! legacy cut elevation), so KTLX 2024's derived products lie on sweep 1,
//! whose first ray reads 0.483 deg against sweep 0's 0.582 deg, although
//! both have the 0.4834 deg VCP cut angle as their fixed angle. KLIX 2005's
//! dealiased velocity (`DVEL`) was re-recorded when the Message 1 Nyquist
//! velocity moved from spare bytes 46-47 to halfword 31 (bytes 60-61, ICD
//! Table III, as MetPy and Py-ART read it). The sample-cache lines
//! (`samples`, `sample_bytes`, `sample_cache`, `sample_cache_reuse`,
//! `geometry_resolved`, `geometry_cache`) were re-recorded when sample caches
//! began resolving pixels by colour-table visibility, as the direct render
//! does: 83 lines changed, all on those paths, and every field's cached render
//! now equals its direct render (`sample_cache` = `viewport`, `geometry_cache`
//! = `zoom`), which 36 cached/direct pairs did not before. No direct-render or
//! storm-relative line changed.

// Test code: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use recast_radar_core::{Field, FieldData, FieldName, FloatCoding, Quantity, Volume};
use recast_radar_render::{
    ColorTableFamily, ColorTableSet, RasterOptions, StormMotion, ViewportFieldCache,
    ViewportRasterOptions, render_field_image, render_storm_relative_velocity_image,
    render_storm_relative_velocity_viewport_rgba, viewport_rgba_buffer_len,
    viewport_sample_cache_storage_upper_bound_for_field,
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

/// The pre-FM301 moment name the goldens were written with.
fn legacy_label(name: &FieldName) -> &str {
    match name {
        FieldName::Dbzh => "REF",
        FieldName::Vradh => "VEL",
        FieldName::Wradh => "SW",
        FieldName::Zdr => "ZDR",
        FieldName::Rhohv => "RHO",
        FieldName::Phidp => "PHI",
        FieldName::Kdp => "KDP",
        FieldName::Ccorh => "CFP",
        other => other.as_str(),
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
fn cache_paths(fp: &mut Fingerprints, label: &str, volume: &Volume, cache: &ViewportFieldCache) {
    let options = viewport();
    let mut direct = filled(options);
    cache
        .render_field_rgba_into(volume, options, &mut direct)
        .unwrap();
    fp.pixels(format!("{label}/viewport"), &direct);
    let mut close = filled(zoom());
    cache
        .render_field_rgba_into(volume, zoom(), &mut close)
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
        .render_field_rgba_with_sample_cache(volume, &sample_cache, &mut cached)
        .unwrap();
    fp.pixels(format!("{label}/sample_cache"), &cached);
    cache
        .render_field_rgba_with_sample_cache_reusing_transparency(
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
        .render_field_rgba_with_sample_cache(volume, &resolved, &mut from_geometry)
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
    volume: &Volume,
    cache: &ViewportFieldCache,
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

fn has_rows(field: &Field) -> bool {
    field.nrays as usize > field.absent_rows.len()
}

/// Lowest sweep (by tilt elevation, `Sweep::tilt_elevation_deg`, then index)
/// with rows of a `quantity` field.
fn lowest_sweep_with(volume: &Volume, quantity: Quantity) -> Option<usize> {
    let tilt = |index: usize| volume.tilt_elevation_deg(index).unwrap_or(f32::NAN);
    (0..volume.sweeps.len())
        .filter(|index| volume.sweeps[*index].find(quantity).is_some_and(has_rows))
        .min_by(|left, right| {
            tilt(*left)
                .total_cmp(&tilt(*right))
                .then_with(|| left.cmp(right))
        })
}

/// The real field re-encoded as physical `f32` (NaN for every sentinel),
/// same name, geometry and absent rows.
fn physical_copy(field: &Field) -> Field {
    let mut copy = field.clone();
    copy.data = FieldData::F32 {
        values: field.to_physical(),
        coding: FloatCoding::default(),
    };
    copy
}

fn fingerprint_volume(volume: &Volume) -> Fingerprints {
    let mut fp = Fingerprints::default();
    let tables = ColorTableSet::default();

    // Every field of every sweep: PNG raster plus the viewport cache paths.
    for (sweep_index, sweep) in volume.sweeps.iter().enumerate() {
        for field in &sweep.fields {
            if !has_rows(field) {
                continue;
            }
            let label = format!("cut{sweep_index}/{}", legacy_label(&field.name));
            let image = render_field_image(volume, sweep_index, &field.name, raster()).unwrap();
            fp.pixels(format!("{label}/image"), image.as_raw());
            fp.push(
                format!("{label}/grid_upper_bound"),
                viewport_sample_cache_storage_upper_bound_for_field(field, &sweep.range, zoom())
                    as u64,
            );
            let cache = ViewportFieldCache::new(volume, sweep_index, &field.name).unwrap();
            cache_paths(&mut fp, &label, volume, &cache);
        }
    }

    if let Some(sweep) = lowest_sweep_with(volume, Quantity::RadialVelocity) {
        let velocity = volume.sweeps[sweep].find(Quantity::RadialVelocity).unwrap();
        let label = format!("cut{sweep}/VEL");
        let image =
            render_storm_relative_velocity_image(volume, sweep, &velocity.name, storm(), raster())
                .unwrap();
        fp.pixels(format!("{label}/srv_image"), image.as_raw());
        let (_, _, pixels) = render_storm_relative_velocity_viewport_rgba(
            volume,
            sweep,
            &velocity.name,
            storm(),
            viewport(),
        )
        .unwrap();
        fp.pixels(format!("{label}/srv_free_viewport"), &pixels);
        let cache = ViewportFieldCache::new(volume, sweep, &velocity.name).unwrap();
        storm_relative_paths(&mut fp, &label, volume, &cache);

        // Dealiased velocity (u16 storage).
        let label = format!("cut{sweep}/DVEL");
        let dealiased =
            ViewportFieldCache::new_dealiased_velocity(volume, sweep, &velocity.name).unwrap();
        assert_eq!(dealiased.field_name(), &FieldName::Vraddh);
        cache_paths(&mut fp, &label, volume, &dealiased);
        storm_relative_paths(&mut fp, &label, volume, &dealiased);

        // Physical f32 velocity drawn as a derived field.
        let label = format!("cut{sweep}/VEL_F32");
        let derived = ViewportFieldCache::new_derived(
            volume,
            sweep,
            physical_copy(velocity),
            &volume.sweeps[sweep].range,
            ColorTableFamily::Velocity,
            &tables,
        )
        .unwrap();
        cache_paths(&mut fp, &label, volume, &derived);
        storm_relative_paths(&mut fp, &label, volume, &derived);
    }

    if let Some(sweep) = lowest_sweep_with(volume, Quantity::Reflectivity) {
        if let Some((composite, range)) = derived::composite_reflectivity(volume, sweep) {
            let label = format!("cut{sweep}/CREF");
            let derived = ViewportFieldCache::new_derived(
                volume,
                sweep,
                composite,
                &range,
                ColorTableFamily::Reflectivity,
                &tables,
            )
            .unwrap();
            cache_paths(&mut fp, &label, volume, &derived);
        }
        let label = format!("cut{sweep}/REF_SMOOTH");
        let (smoothed, range) = derived::smoothed_reflectivity(volume, sweep);
        let derived = ViewportFieldCache::new_derived(
            volume,
            sweep,
            smoothed,
            &range,
            ColorTableFamily::Reflectivity,
            &tables,
        )
        .unwrap();
        cache_paths(&mut fp, &label, volume, &derived);
        if let Some((field, range, row_azimuths_deg)) =
            derived::upsampled_reflectivity(volume, sweep)
        {
            let label = format!("cut{sweep}/REF_UPSAMPLED");
            let resampled = ViewportFieldCache::new_resampled(
                volume,
                sweep,
                field,
                &range,
                &row_azimuths_deg,
                ColorTableFamily::Reflectivity,
                &tables,
            )
            .unwrap();
            cache_paths(&mut fp, &label, volume, &resampled);
        }
    }
    fp
}

fn decode(id: &str) -> Option<Volume> {
    let path = match recast_radar_testdata::path(id) {
        Ok(path) => path,
        Err(err) if err.is_offline() => {
            eprintln!("skipping {id}: {err}");
            return None;
        }
        Err(err) => panic!("{err}"),
    };
    Some(recast_radar_io_nexrad::read_volume_from_path(&path).unwrap())
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
        let Some(volume) = decode(id) else {
            continue;
        };
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

/// The derived products of `recast-radar-map` and `recast-radar-filters`,
/// each with the range coordinate its gate mapping refers to.
mod derived {
    use recast_radar_core::{Field, FieldName, RangeCoord, Volume};

    /// The composite (column maximum) reflectivity, computed on the base
    /// reflectivity sweep; `sweep` must be that sweep.
    pub fn composite_reflectivity(volume: &Volume, sweep: usize) -> Option<(Field, RangeCoord)> {
        let composite = recast_radar_map::composite_reflectivity(volume)?;
        assert_eq!(composite.nrays as usize, volume.sweeps[sweep].nrays());
        Some((composite, volume.sweeps[sweep].range.clone()))
    }

    pub fn smoothed_reflectivity(volume: &Volume, sweep: usize) -> (Field, RangeCoord) {
        let model = &volume.sweeps[sweep];
        let field = model.field(&FieldName::Dbzh).unwrap();
        (
            recast_radar_filters::smooth_field(field),
            model.range.clone(),
        )
    }

    /// The display-upsampled reflectivity with its range and synthetic row
    /// azimuths.
    pub fn upsampled_reflectivity(
        volume: &Volume,
        sweep: usize,
    ) -> Option<(Field, RangeCoord, Vec<f32>)> {
        let model = &volume.sweeps[sweep];
        let field = model.field(&FieldName::Dbzh).unwrap();
        let up = recast_radar_filters::upsample_field(model, field)?;
        let mut sweep = up.sweep;
        let field = sweep.fields.swap_remove(0);
        Some((field, sweep.range, sweep.rays.azimuth_deg))
    }
}
