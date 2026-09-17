//! VAD Wind Profile (product 48): [`VadWindProfile`] against every product 48
//! file of the real corpora, the five of `testdata/level3/manifest.toml` and
//! the KBMX file of `testdata/other/manifest.toml` (carried over from BowEcho
//! with `recast_radar_io_nexrad::level3_vwp`).
//!
//! Expected values come from:
//!
//! 1. **`recast_radar_io_nexrad::level3_vwp`**, the decoder `vwp` replaced and
//!    which was then removed. In commit 9b105fc this test decoded each file
//!    with both implementations, rendered every field of `level3_vwp`'s
//!    `VwpProduct` (radar, scan times, source, metadata, profile labels and
//!    times, and every level) as text, compared that with
//!    [`render_new_as_old`] of the new output, and checked that
//!    `tests/level3_vwp/<id>.txt` held exactly `level3_vwp`'s rendering.
//!    [`matches_level3_vwp_output_on_every_product_48_file`] now compares the
//!    new output with those snapshots; they must not be regenerated from this
//!    crate. The rendering spells out the documented differences:
//!    - Heights: `level3_vwp` converted slant ranges with 6067.1/3281 km per
//!      nautical mile (6067.1 transposes the 6076.1 ft in a nautical mile) and
//!      rounded to metres; `height_above_radar_km` uses 1.852 km. The rendering
//!      applies `level3_vwp`'s formulas to the new decoder's slant range,
//!      elevation and altitude, and a separate check bounds the difference.
//!    - Display RMS: `level3_vwp` gave a barb's RMS as the middle of its color
//!      level's 4-knot range (18 kt for level 5); `VadWind` keeps the color
//!      level and its range.
//!    - Divergence: `level3_vwp` kept the printed 10^-3/s value.
//!    - `l3-mci-nvw-20160526-2154` is split into three zlib frames;
//!      `level3_vwp` inflated only the first (`flate2::read::ZlibDecoder`
//!      stops at the end of a stream), lost the Tabular Alphanumeric Block, and
//!      so reported the display winds and no metadata. The new decoder reads
//!      the table; for this file the old output is compared with the new
//!      display winds and empty metadata, and the table is checked separately.
//! 2. **The files themselves**: tabular rows spelled out from the page text,
//!    row counts from the golden page line counts, every wind barb assigned to
//!    a column, the Product Description Block's maximum wind halfwords (47-49)
//!    against the newest display column, and the adaptable parameter pages.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

use chrono::{DateTime, Datelike, Timelike, Utc};
use recast_radar_io_level3::packets::symbols::SymbolPacket;
use recast_radar_io_level3::vwp::{VWP_PRODUCT_CODE, VadWindProfile, VwpSource};
use recast_radar_io_level3::{
    Level3Error, Level3Product, OperationalMode, Packet, TabularLayout, decode_product,
};

/// The product 48 file of `testdata/other/manifest.toml`.
const KBMX_ID: &str = "l3-kbmx-19980416-0006-nvw";

/// The file whose zlib frames `level3_vwp` read only in part.
const FIRST_ZLIB_FRAME_ONLY: &str = "l3-mci-nvw-20160526-2154";

/// Winds of the newest display column in the four files with tabular winds.
const TABLE_CHECKED_DISPLAY_WINDS: usize = 10 + 29 + 27 + 29;

struct VwpFile {
    id: String,
    bytes: Vec<u8>,
}

/// Every product 48 file: the Level III manifest's, then the KBMX file.
fn vwp_files() -> Vec<VwpFile> {
    let mut files: Vec<VwpFile> = common::level3_manifest()
        .into_iter()
        .filter(|entry| entry.tag("product") == Some("48"))
        .map(|entry| VwpFile {
            bytes: entry.bytes(),
            id: entry.id,
        })
        .collect();
    assert_eq!(files.len(), 5, "product 48 files in the Level III manifest");
    files.push(other_manifest_file(KBMX_ID));
    files
}

/// A committed file of `testdata/other/manifest.toml`, checked against its
/// manifest size and SHA-256.
fn other_manifest_file(id: &str) -> VwpFile {
    let path = common::testdata_dir().join("other/manifest.toml");
    let text = fs::read_to_string(&path).unwrap();
    let block = text
        .split("[[file]]")
        .find(|block| block.lines().any(|l| l.trim() == format!("id = \"{id}\"")))
        .unwrap_or_else(|| panic!("{id} not in {}", path.display()));
    let value = |key: &str| {
        block
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key} = ")))
            .unwrap_or_else(|| panic!("{id}: no {key}"))
            .trim_matches('"')
            .to_string()
    };
    let bytes = fs::read(common::testdata_dir().join(value("committed"))).unwrap();
    assert_eq!(bytes.len().to_string(), value("size"), "{id}: size");
    assert_eq!(common::sha256_hex(&bytes), value("sha256"), "{id}: sha256");
    VwpFile {
        id: id.to_string(),
        bytes,
    }
}

fn decode(file: &VwpFile) -> (Level3Product, VadWindProfile) {
    let product = decode_product(&file.bytes).unwrap_or_else(|e| panic!("{}: {e}", file.id));
    let vwp = VadWindProfile::from_product(&product).unwrap();
    (product, vwp)
}

fn iso(time: DateTime<Utc>) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        time.year(),
        time.month(),
        time.day(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// One level line of the rendering. Coordinates print with three decimals
/// (0.001 degree in the file), heights with three (`level3_vwp` rounded them to
/// metres) and divergence with four (as printed in the table); other numbers
/// print exactly.
#[allow(clippy::too_many_arguments)]
fn level_line(
    altitude_km_agl: f64,
    altitude_ft_msl: Option<i32>,
    direction_deg: f64,
    speed_kts: f64,
    rms_kts: Option<f64>,
    divergence: Option<f64>,
    slant_range_nm: Option<f64>,
    elevation_angle_deg: Option<f64>,
) -> String {
    format!(
        "  level altitude_km_agl={altitude_km_agl:.3} altitude_ft_msl={altitude_ft_msl:?} \
         direction_deg={direction_deg:?} speed_kts={speed_kts:?} rms_kts={rms_kts:?} \
         divergence={} slant_range_nm={slant_range_nm:?} \
         elevation_angle_deg={elevation_angle_deg:?}",
        divergence.map_or("None".to_string(), |d| format!("Some({d:.4})"))
    )
}

/// Which part of the new output corresponds to `level3_vwp`'s output.
#[derive(Clone, Copy)]
struct OldView {
    source: VwpSource,
    metadata: bool,
}

/// `level3_vwp`'s choice for a file (see the module documentation).
fn old_view(id: &str, vwp: &VadWindProfile) -> OldView {
    if id == FIRST_ZLIB_FRAME_ONLY {
        OldView {
            source: VwpSource::Symbology,
            metadata: false,
        }
    } else {
        OldView {
            source: vwp.source().unwrap(),
            metadata: true,
        }
    }
}

/// `level3_vwp`'s beam height: slant range times 6067.1/3281 km per nm, 4/3
/// earth radius, rounded to metres (its expression order is kept).
fn old_table_altitude_km(slant_range_nm: f64, elevation_deg: f64) -> f64 {
    const EFFECTIVE_EARTH_RADIUS_KM: f64 = (4.0 / 3.0) * 6_371.0;
    let slant_km = slant_range_nm * 6_067.1 / 3_281.0;
    let elevation_rad = elevation_deg.to_radians();
    let altitude_km_agl = (EFFECTIVE_EARTH_RADIUS_KM.powi(2)
        + slant_km.powi(2)
        + 2.0 * EFFECTIVE_EARTH_RADIUS_KM * slant_km * elevation_rad.sin())
    .sqrt()
        - EFFECTIVE_EARTH_RADIUS_KM;
    (altitude_km_agl * 1_000.0).round() / 1_000.0
}

/// `level3_vwp`'s display height: (kft - radar height in kft) / 3.281, rounded
/// to metres.
fn old_display_altitude_km(altitude_ft_msl: i32, radar_height_ft: i16) -> f64 {
    let altitude_km_agl =
        (f64::from(altitude_ft_msl / 1000) - f64::from(radar_height_ft) / 1_000.0) / 3.281;
    (altitude_km_agl * 1_000.0).round() / 1_000.0
}

/// The new output rendered as `level3_vwp`'s `VwpProduct` was in commit 9b105fc
/// (one line per radar, scan, source, metadata, profile and level), with
/// `level3_vwp`'s derived values computed from the new decoder's fields.
fn render_new_as_old(product: &Level3Product, vwp: &VadWindProfile, view: OldView) -> String {
    let d = &product.description;
    let p = &vwp.parameters;
    let (rms, symmetry, points, optimum) = if view.metadata {
        (
            p.rms_threshold_kt,
            p.symmetry_threshold_kt,
            p.data_points_threshold,
            p.optimum_slant_range_nm,
        )
    } else {
        (None, None, None, None)
    };
    let mut lines = vec![
        format!(
            "radar latitude_deg={:.3} longitude_deg={:.3} height_ft={} vcp={} mode={}",
            d.latitude_deg,
            d.longitude_deg,
            d.height_ft,
            d.vcp,
            // level3_vwp: mode 1 is ClearAir, anything else Precipitation.
            if d.mode() == OperationalMode::ClearAir {
                "ClearAir"
            } else {
                "Precipitation"
            }
        ),
        format!(
            "scan volume_time={} generation_time={}",
            iso(d.volume_scan_time),
            iso(d.generation_time)
        ),
        format!("source {:?}", view.source),
        format!(
            "metadata rms_threshold_kts={rms:?} symmetry_threshold_kts={symmetry:?} \
             data_points_threshold={points:?} optimum_slant_range_nm={optimum:?}"
        ),
    ];
    let profiles = match view.source {
        VwpSource::Tabular => &vwp.tabular,
        VwpSource::Symbology => &vwp.display,
    };
    for profile in profiles {
        lines.push(format!(
            "profile label_hhmm={} valid_time={}",
            profile.label_hhmm,
            iso(profile.valid_time)
        ));
        for wind in &profile.winds {
            let altitude_km_agl = match (wind.slant_range_nm, wind.elevation_deg) {
                (Some(range), Some(elevation)) => old_table_altitude_km(range, elevation),
                _ => old_display_altitude_km(wind.altitude_ft_msl, d.height_ft),
            };
            let rms_kts = wind
                .rms_kt
                .or_else(|| wind.rms_range_kt().map(|(low, _)| low + 2.0));
            lines.push(level_line(
                altitude_km_agl,
                Some(wind.altitude_ft_msl),
                wind.direction_deg,
                wind.speed_kt,
                rms_kts,
                wind.divergence_per_s.map(|d| d * 1e3),
                wind.slant_range_nm,
                wind.elevation_deg,
            ));
        }
    }
    lines.join("\n") + "\n"
}

fn snapshot_path(id: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/level3_vwp")
        .join(format!("{id}.txt"))
}

/// First differing line of two renderings, for failure messages.
fn first_difference(a: &str, b: &str) -> String {
    a.lines()
        .zip(b.lines())
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map_or_else(
            || {
                format!(
                    "line counts {} and {}",
                    a.lines().count(),
                    b.lines().count()
                )
            },
            |(n, (x, y))| format!("line {}:\n  {x}\n  {y}", n + 1),
        )
}

#[test]
fn matches_level3_vwp_output_on_every_product_48_file() {
    let mut winds = 0;
    for file in vwp_files() {
        let id = &file.id;
        let (product, vwp) = decode(&file);
        let path = snapshot_path(id);
        let snapshot = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .replace("\r\n", "\n");
        if id == FIRST_ZLIB_FRAME_ONLY {
            // Three zlib frames; the table and parameters are past the first.
            let framing = common::entry(id).golden().get("framing").clone();
            assert_eq!(framing.get("zlib_frames").as_i64(), Some(3));
            assert_eq!(vwp.source(), Some(VwpSource::Tabular));
            assert_eq!(vwp.tabular[0].winds.len(), 17);
            assert_eq!(vwp.parameters.rms_threshold_kt, Some(9.7));
            assert_eq!(snapshot.lines().nth(2), Some("source Symbology"));
            assert!(snapshot.contains("metadata rms_threshold_kts=None "));
        }
        let new_text = render_new_as_old(&product, &vwp, old_view(id, &vwp));
        assert!(
            new_text == snapshot,
            "{id}: new output differs from level3_vwp's at {}",
            first_difference(&new_text, &snapshot)
        );

        // Heights differ from level3_vwp's only by its nm conversion and rounding.
        let view = old_view(id, &vwp);
        let profiles = match view.source {
            VwpSource::Tabular => &vwp.tabular,
            VwpSource::Symbology => &vwp.display,
        };
        let old_heights: Vec<f64> = snapshot
            .lines()
            .filter_map(|line| line.strip_prefix("  level altitude_km_agl="))
            .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
            .collect();
        let new_winds: Vec<_> = profiles.iter().flat_map(|p| &p.winds).collect();
        assert_eq!(new_winds.len(), old_heights.len(), "{id}");
        for (wind, old_km) in new_winds.iter().zip(old_heights) {
            assert!(
                (wind.height_above_radar_km - old_km).abs() <= 0.0035 * old_km.abs() + 0.0006,
                "{id}: height {} km, level3_vwp {old_km} km",
                wind.height_above_radar_km
            );
            winds += 1;
        }
    }
    // 60 KBMX + 99 FWS + 119 MCI (display) + 48 OKC + 43 + 43 TLX levels.
    assert_eq!(winds, 60 + 99 + 119 + 48 + 43 + 43);
}

/// Ported from `level3_vwp`'s unit tests: the KBMX product of 1998-04-16 00:06Z
/// has no tabular winds, so its profiles are the 11 display columns, which
/// start at 23:14 the previous day.
#[test]
fn kbmx_display_profiles_cross_midnight() {
    let file = other_manifest_file(KBMX_ID);
    let (product, vwp) = decode(&file);
    let d = &product.description;

    assert!((d.latitude_deg - 33.172).abs() < 0.0001);
    assert!((d.longitude_deg + 86.770).abs() < 0.0001);
    assert_eq!(d.height_ft, 759);
    assert_eq!(d.vcp, 11);
    assert_eq!(d.mode(), OperationalMode::Precipitation);
    assert_eq!(vwp.source(), Some(VwpSource::Symbology));
    assert!(vwp.tabular.is_empty());

    // Julian day 1 is 1970-01-01: halfword 21 holds 10333, 1998-04-16.
    assert_eq!(d.halfword(21), Some(10333));
    assert_eq!(iso(d.volume_scan_time), "1998-04-16T00:06:45Z");
    assert_eq!(iso(d.generation_time), "1998-04-16T00:11:19Z");

    let profiles = vwp.profiles();
    let labels: Vec<&str> = profiles.iter().map(|p| p.label_hhmm.as_str()).collect();
    assert_eq!(
        labels,
        [
            "0006", "0001", "2355", "2350", "2345", "2340", "2335", "2330", "2325", "2319", "2314"
        ]
    );
    let counts: Vec<usize> = profiles.iter().map(|p| p.winds.len()).collect();
    assert_eq!(counts, [5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 6]);
    assert_eq!(iso(profiles[0].valid_time), "1998-04-16T00:06:00Z");
    assert_eq!(iso(profiles[1].valid_time), "1998-04-16T00:01:00Z");
    assert_eq!(iso(profiles[2].valid_time), "1998-04-15T23:55:00Z");
    assert_eq!(iso(profiles[10].valid_time), "1998-04-15T23:14:00Z");

    let p = &vwp.parameters;
    assert_eq!(p.analysis_slant_range_nm, Some(16.2));
    assert_eq!(p.beginning_azimuth_deg, Some(0.0));
    assert_eq!(p.ending_azimuth_deg, Some(0.0));
    assert_eq!(p.passes, Some(2));
    assert_eq!(p.rms_threshold_kt, Some(9.7));
    assert_eq!(p.symmetry_threshold_kt, Some(13.6));
    assert_eq!(p.data_points_threshold, Some(25));
    assert_eq!(p.optimum_slant_range_nm, Some(16.2));
    let selected: Vec<i32> = (1..=20)
        .chain([22, 24, 25, 26, 28, 30, 35, 40, 45, 50])
        .map(|kft| kft * 1000)
        .collect();
    assert_eq!(p.altitudes_selected_ft, selected);

    // Oldest column, lowest wind: barb at 1 kft, 182 deg, 12 kt, color level 2.
    let first = &profiles[10].winds[0];
    assert_eq!(first.altitude_ft_msl, 1000);
    assert!((first.height_above_radar_km - (1000.0 - 759.0) * 0.0003048).abs() < 1e-12);
    assert_eq!((first.direction_deg, first.speed_kt), (182.0, 12.0));
    assert_eq!(first.color_level, Some(2));
    assert_eq!(first.rms_range_kt(), Some((4.0, 8.0)));
    assert_eq!(first.rms_kt, None);
    assert_eq!(first.slant_range_nm, None);
}

/// Tabular rows as printed on the VAD Algorithm Output pages, and the row
/// counts from the golden page line counts (every page but the last two, the
/// parameter pages, holds 3 header lines and rows).
#[test]
fn tabular_winds_follow_the_page_rows() {
    let mut checked_against_table = 0;
    for file in vwp_files() {
        let id = &file.id;
        let (product, vwp) = decode(&file);
        let golden = if id == KBMX_ID {
            None
        } else {
            Some(common::entry(id).golden())
        };
        let tabular = product.tabular.as_ref().unwrap();
        assert_eq!(tabular.layout, TabularLayout::Block, "{id}");
        let lines: Vec<usize> = tabular.pages.iter().map(|p| p.lines.len()).collect();
        if let Some(golden) = &golden {
            let golden_lines: Vec<usize> = golden
                .get("blocks")
                .get("tabular")
                .get("lines_per_page")
                .items()
                .iter()
                .map(|n| usize::try_from(n.int("lines")).unwrap())
                .collect();
            assert_eq!(lines, golden_lines, "{id}");
        }
        let rows: usize = lines[..lines.len() - 2].iter().map(|n| n - 3).sum();
        let decoded: usize = vwp.tabular.iter().map(|p| p.winds.len()).sum();
        match id.as_str() {
            // 1990s products: two parameter pages, no wind pages.
            "l3-fws-nvw-19950517-2322" | KBMX_ID => {
                assert_eq!(lines, [17, 17], "{id}");
                assert_eq!(decoded, 0, "{id}");
            }
            _ => {
                assert_eq!(decoded, rows, "{id}");
                assert_eq!(vwp.tabular.len(), 1, "{id}");
                assert_eq!(vwp.source(), Some(VwpSource::Tabular), "{id}");
            }
        }
        for profile in &vwp.tabular {
            for wind in &profile.winds {
                assert!(wind.color_level.is_none() && wind.rms_kt.is_some(), "{id}");
            }
            assert!(
                profile
                    .winds
                    .windows(2)
                    .all(|w| w[0].height_above_radar_km <= w[1].height_above_radar_km),
                "{id}"
            );
            // The newest display column shows the table's winds at the
            // selected altitudes (the rows without divergence; constant slant
            // range rows carry it). Observed: the table prints north as 360
            // where the barb has 0 (KOKC 2026).
            let newest = &vwp.display[0];
            assert_eq!(newest.label_hhmm, profile.label_hhmm, "{id}");
            for shown in &newest.winds {
                assert!(
                    profile
                        .winds
                        .iter()
                        .any(|row| row.altitude_ft_msl == shown.altitude_ft_msl
                            && row.direction_deg % 360.0 == shown.direction_deg % 360.0
                            && row.speed_kt == shown.speed_kt
                            && row.divergence_per_s.is_none()),
                    "{id}: display wind {shown:?} not in the table"
                );
                checked_against_table += 1;
            }
        }
    }
    // Newest columns: 10 TMCI 2016, 29 TOKC 2026, 27 KTLX 2013, 29 KTLX 2026.
    assert_eq!(checked_against_table, TABLE_CHECKED_DISPLAY_WINDS);

    // KTLX 2013-05-20 20:16Z: first, last and a constant slant range row.
    let file = vwp_files()
        .into_iter()
        .find(|f| f.id == "l3-tlx-nvw-20130520-2016")
        .unwrap();
    let (product, vwp) = decode(&file);
    let pages = &product.tabular.as_ref().unwrap().pages;
    assert_eq!(
        pages[0].lines[0].trim(),
        "VAD Algorithm Output  05/20/13  20:16"
    );
    assert_eq!(
        pages[0].lines[3].trim(),
        "016    -5.5     3.7     NA    124   013   5.7      NA      5.67    0.5"
    );
    let profile = &vwp.tabular[0];
    assert_eq!(profile.label_hhmm, "2016");
    assert_eq!(iso(profile.valid_time), "2013-05-20T20:16:00Z");
    let first = &profile.winds[0];
    assert_eq!(first.altitude_ft_msl, 1600);
    assert_eq!(first.u_m_per_s, Some(-5.5));
    assert_eq!(first.v_m_per_s, Some(3.7));
    assert_eq!(first.w_cm_per_s, None);
    assert_eq!((first.direction_deg, first.speed_kt), (124.0, 13.0));
    assert_eq!(first.rms_kt, Some(5.7));
    assert_eq!(first.divergence_per_s, None);
    assert_eq!(first.slant_range_nm, Some(5.67));
    assert_eq!(first.elevation_deg, Some(0.5));
    // Row "024 -1.0 11.8 1.4 175 023 5.5 -0.0429 16.20 0.5".
    let row = profile
        .winds
        .iter()
        .find(|w| w.altitude_ft_msl == 2400)
        .unwrap();
    assert_eq!(row.w_cm_per_s, Some(1.4));
    assert!((row.divergence_per_s.unwrap() - -0.0429e-3).abs() < 1e-18);
    assert_eq!(row.slant_range_nm, Some(16.2));
    let last = profile.winds.last().unwrap();
    // Row "400 37.7 14.0 NA 250 078 8.2 NA 28.98 12.5": beam height at 28.98 nm, 12.5 deg.
    assert_eq!(last.altitude_ft_msl, 40_000);
    let r = 4.0 / 3.0 * 6371.0;
    let s = 28.98 * 1.852;
    let h = (r * r + s * s + 2.0 * r * s * 12.5_f64.to_radians().sin()).sqrt() - r;
    assert!((last.height_above_radar_km - h).abs() < 1e-9);
    assert!((last.height_above_radar_km - 11.78).abs() < 0.01);
    assert_eq!(profile.winds.len(), 43);
}

/// Every wind barb lands in a display column, and the newest column holds the
/// maximum wind of the Product Description Block (Table V: halfword 47 speed
/// in knots, 48 direction in degrees, 49 altitude in feet/10).
#[test]
fn display_winds_hold_every_barb_and_the_maximum_wind() {
    let mut north = Vec::new();
    let mut ties = Vec::new();
    for file in vwp_files() {
        let id = &file.id;
        let (product, vwp) = decode(&file);
        let barbs: usize = product
            .symbology
            .as_ref()
            .unwrap()
            .layers
            .iter()
            .flatten()
            .map(|packet| match packet {
                Packet::Symbol(SymbolPacket::WindBarbs(barbs)) => barbs.len(),
                _ => 0,
            })
            .sum();
        let winds: usize = vwp.display.iter().map(|p| p.winds.len()).sum();
        assert!(barbs > 0, "{id}");
        assert_eq!(winds, barbs, "{id}");

        let newest = &vwp.display[0];
        assert!(
            vwp.display
                .windows(2)
                .all(|w| w[0].valid_time > w[1].valid_time),
            "{id}"
        );
        let max_speed = newest.winds.iter().map(|w| w.speed_kt).fold(0.0, f64::max);
        let fastest: Vec<_> = newest
            .winds
            .iter()
            .filter(|w| w.speed_kt == max_speed)
            .collect();
        let d = &product.description;
        let hw = |n| d.halfword(n).unwrap();
        assert_eq!(max_speed, f64::from(hw(47)), "{id}: max speed");
        // Halfwords 48-49 are the direction and altitude of one of the fastest
        // winds. Observed: north prints as 360 in halfword 48 and 0 in the barb
        // (KOKC 2026); of equally fast winds the halfwords give the lower in
        // KBMX 1998 (27 kt at 4 and 5 kft) and the higher in KTLX 2026 (76 kt
        // at 12 and 13 kft).
        let reported: Vec<i32> = fastest
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                w.direction_deg % 360.0 == f64::from(hw(48)) % 360.0
                    && w.altitude_ft_msl == i32::from(hw(49)) * 10
            })
            .map(|(rank, w)| {
                if w.direction_deg != f64::from(hw(48)) {
                    north.push(id.clone());
                }
                i32::try_from(rank).unwrap()
            })
            .collect();
        assert_eq!(
            reported.len(),
            1,
            "{id}: halfwords 48-49 {} {}",
            hw(48),
            hw(49)
        );
        if fastest.len() > 1 {
            ties.push((id.clone(), fastest.len(), reported[0]));
        }
        for wind in vwp.display.iter().flat_map(|p| &p.winds) {
            assert!(matches!(wind.color_level, Some(1..=5)), "{id}");
            assert!(
                wind.rms_kt.is_none() && wind.slant_range_nm.is_none(),
                "{id}"
            );
            assert_eq!(wind.altitude_ft_msl % 1000, 0, "{id}");
        }
    }
    assert_eq!(north, ["l3-okc-nvw-20260622-080623"]);
    assert_eq!(
        ties,
        [
            ("l3-tlx-nvw-20260622-080623".to_string(), 2, 1),
            (KBMX_ID.to_string(), 2, 0)
        ]
    );
}

/// The adaptable parameter pages of every file.
#[test]
fn parameters_match_the_parameter_pages() {
    let modern: Vec<i32> = (2..=20)
        .chain([22, 24, 25, 26, 28, 30, 35, 40, 45, 50])
        .map(|kft| kft * 1000)
        .chain([-666])
        .collect();
    let old: Vec<i32> = (1..=20)
        .chain([22, 24, 25, 26, 28, 30, 35, 40, 45, 50])
        .map(|kft| kft * 1000)
        .collect();
    for file in vwp_files() {
        let id = &file.id;
        let (_, vwp) = decode(&file);
        let p = &vwp.parameters;
        let (range, altitudes) = match id.as_str() {
            "l3-fws-nvw-19950517-2322" => (13.5, &old),
            KBMX_ID => (16.2, &old),
            _ => (16.2, &modern),
        };
        assert_eq!(p.analysis_slant_range_nm, Some(range), "{id}");
        assert_eq!(p.optimum_slant_range_nm, Some(range), "{id}");
        assert_eq!(p.beginning_azimuth_deg, Some(0.0), "{id}");
        assert_eq!(p.ending_azimuth_deg, Some(0.0), "{id}");
        assert_eq!(p.passes, Some(2), "{id}");
        assert_eq!(p.rms_threshold_kt, Some(9.7), "{id}");
        assert_eq!(p.symmetry_threshold_kt, Some(13.6), "{id}");
        assert_eq!(p.data_points_threshold, Some(25), "{id}");
        assert_eq!(&p.altitudes_selected_ft, altitudes, "{id}");
    }
}

/// Only product 48 is read; ported from `level3_vwp`'s rejection test with a
/// real product instead of zero bytes.
#[test]
fn other_products_are_rejected() {
    let entry = common::entry("l3-byx-n0q-20150124-2106");
    let product = decode_product(&entry.bytes()).unwrap();
    match VadWindProfile::from_product(&product) {
        Err(Level3Error::UnexpectedProduct { expected, found }) => {
            assert_eq!((expected, found), (VWP_PRODUCT_CODE, 94));
        }
        other => panic!("{other:?}"),
    }
}
