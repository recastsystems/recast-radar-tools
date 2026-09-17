//! F.3 behaviour-identity probe over the LEGACY API of the five algorithm
//! crates: prints one line per product, `<file> <product> <digest>`, for
//! seven real volumes. Run before and after a migration and diff the
//! output. Names the FM301 functions shadow at the crate root are reached
//! through `legacy_api::`. Deleted with the shim.
//!
//! `CORPUS` points at the directory holding the two bench Level II files;
//! the others come from `recast-radar-testdata`. `ONLY=<label>` limits the
//! run to one input.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(deprecated)]

use std::path::PathBuf;

use chrono::Duration;
use recast_radar_core::{ElevationCut, MomentGrid, MomentType, RadarVolume};
use recast_radar_correct as correct;
use recast_radar_filters as filters;
use recast_radar_map as map;
use recast_radar_retrieve as retrieve;
use recast_radar_track as track;

struct H(u64);
impl H {
    fn new() -> Self {
        H(0xcbf29ce484222325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
    fn f32(&mut self, v: Option<f32>) {
        let bits = match v {
            Some(v) if !v.is_nan() => v.to_bits(),
            _ => 0x7fc0_0000,
        };
        self.bytes(&bits.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}

fn values_hash(values: &[f32]) -> String {
    let mut h = H::new();
    h.u64(values.len() as u64);
    let mut finite = 0usize;
    for v in values {
        if v.is_finite() {
            finite += 1;
        }
        h.f32(Some(*v));
    }
    format!("n={} finite={} {:016x}", values.len(), finite, h.0)
}

fn grid_hash(g: &MomentGrid) -> String {
    let rows = g.radial_count();
    let gates = g.gate_range.gate_count;
    let mut h = H::new();
    h.u64(rows as u64);
    h.u64(gates as u64);
    let mut finite = 0usize;
    for r in 0..rows {
        for gate in 0..gates {
            let v = g.scaled_value(r, gate);
            if v.is_some_and(f32::is_finite) {
                finite += 1;
            }
            h.f32(v);
        }
    }
    format!(
        "{}x{} first={} sp={} finite={} {:016x}",
        rows, gates, g.gate_range.first_gate_m, g.gate_range.gate_spacing_m, finite, h.0
    )
}

fn opt_grid(g: Option<&MomentGrid>) -> String {
    g.map_or_else(|| "None".to_owned(), grid_hash)
}

/// Debug text with the names the migration renamed mapped back (values
/// unchanged), so pre- and post-migration output compare.
fn dbg_text<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
        .replace("SweepDerivationReport", "CutDerivationReport")
        .replace("sweep_index", "cut_index")
        .replace("velocity_sweep_count", "velocity_cut_count")
        .replace("sweeps_processed", "cuts_processed")
}

fn dbg_hash<T: std::fmt::Debug>(v: &T) -> String {
    let s = dbg_text(v);
    let mut h = H::new();
    h.bytes(s.as_bytes());
    format!("len={} {:016x}", s.len(), h.0)
}

fn lowest(volume: &RadarVolume, pred: impl Fn(&ElevationCut) -> bool) -> Option<usize> {
    volume
        .cuts
        .iter()
        .enumerate()
        .filter(|(_, c)| pred(c))
        .min_by(|a, b| a.1.elevation_deg.total_cmp(&b.1.elevation_deg))
        .map(|(i, _)| i)
}

fn main() {
    let corpus = PathBuf::from(std::env::var("CORPUS").expect("CORPUS"));
    let mut inputs: Vec<(String, PathBuf, bool)> = vec![
        (
            "ktlx2024".into(),
            corpus.join("KTLX20240315_000217_V06"),
            true,
        ),
        (
            "ktlx2013".into(),
            corpus.join("KTLX20130520_201643_V06.gz"),
            true,
        ),
    ];
    for (label, id) in [
        ("klix2005", "l2-klix-20050829-130035"),
        ("dkrom", "odim-dkrom-20260820-1130-pvol"),
        ("iesha", "odim-iesha-20260305-0115-pvol"),
        ("dow8rhi", "cfrad1-dow8-20211011-223602-rhi-trim3-classic"),
        ("irene", "cfrad1-irene-sr2-20110827-120420-sur-sweeps01"),
    ] {
        inputs.push((
            label.into(),
            recast_radar_testdata::path(id).expect(id),
            false,
        ));
    }
    let only = std::env::var("ONLY").ok();
    for (label, path, _) in inputs {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read");
        let volume = recast_radar_io::decode_supported_volume_bytes(&bytes).expect("decode");
        probe(&label, &volume);
    }
}

fn probe(label: &str, volume: &RadarVolume) {
    let out = |name: &str, digest: String| println!("{label} {name} {digest}");
    use MomentType as M;
    let vel_cuts: Vec<usize> = (0..volume.cuts.len())
        .filter(|&i| volume.cuts[i].moments.contains_key(&M::Velocity))
        .collect();
    out("cuts", format!("{} vel={:?}", volume.cuts.len(), vel_cuts));

    // ---------------- correct ----------------
    let mut dealiased: Vec<Option<MomentGrid>> = vec![None; volume.cuts.len()];
    for &i in &vel_cuts {
        let cut = &volume.cuts[i];
        let grid = &cut.moments[&M::Velocity];
        let d = correct::dealias_velocity_grid(cut, grid);
        out(&format!("dealias_region[{i}]"), grid_hash(&d));
        // Shadowed at the crate root by the FM301 signatures.
        out(
            &format!("skipped_no_nyquist[{i}]"),
            format!(
                "{}",
                correct::legacy_api::dealias_skipped_no_nyquist(cut, grid)
            ),
        );
        out(
            &format!("radial_azimuths[{i}]"),
            values_hash(&correct::legacy_api::radial_azimuths(cut, grid)),
        );
        dealiased[i] = Some(d);
    }
    for &i in vel_cuts.iter().take(2) {
        let cut = &volume.cuts[i];
        let grid = &cut.moments[&M::Velocity];
        out(
            &format!("dealias_pyart[{i}]"),
            grid_hash(&correct::dealias_velocity_grid_pyart_region(cut, grid)),
        );
        let reference = correct::fit_range_band_reference(cut, grid);
        out(
            &format!("range_band_ref[{i}]"),
            dbg_hash(&(reference.band_gates, &reference.fits)),
        );
        if let Some(&j) = vel_cuts.iter().next_back() {
            let cutj = &volume.cuts[j];
            let r = correct::fit_range_band_reference(cutj, &cutj.moments[&M::Velocity]);
            out(
                &format!("dealias_with_ref[{i}<-{j}]"),
                grid_hash(&correct::dealias_velocity_grid_with_reference(
                    cut,
                    grid,
                    Some(&r),
                )),
            );
        }
    }
    let env = correct::EnvironmentalWindProfile {
        levels: vec![
            correct::EnvWindLevel {
                height_m_arl: 0.0,
                u_mps: 5.0,
                v_mps: 10.0,
            },
            correct::EnvWindLevel {
                height_m_arl: 6000.0,
                u_mps: 25.0,
                v_mps: 20.0,
            },
        ],
        valid_time: volume.volume_time - Duration::minutes(30),
    };
    if let Some(&i) = vel_cuts.first() {
        let cut = &volume.cuts[i];
        out(
            "env_projection",
            values_hash(&correct::project_environmental_winds(
                &env,
                cut,
                &cut.moments[&M::Velocity],
            )),
        );
    }
    if !vel_cuts.is_empty() {
        for (tag, profile) in [("v4_noenv", None), ("v4_env", Some(&env))] {
            let solution = correct::dealias_volume_v4(volume, None, profile);
            out(&format!("{tag}.diag"), dbg_hash(solution.diagnostics()));
            for i in 0..volume.cuts.len() {
                if let Some(g) = solution.tilt_grid(i) {
                    out(&format!("{tag}.grid[{i}]"), grid_hash(g));
                }
                if let Some(c) = solution.tilt_confidence(i) {
                    let mut h = H::new();
                    h.bytes(c.values());
                    out(
                        &format!("{tag}.conf[{i}]"),
                        format!("{}x{} {:016x}", c.rows(), c.gates(), h.0),
                    );
                }
            }
            let prior = correct::TemporalPrior::Solution(&solution);
            let mut later = volume.clone();
            later.volume_time += Duration::minutes(5);
            let with_prior = correct::dealias_volume_v4(&later, Some(prior), profile);
            out(
                &format!("{tag}.prior.diag"),
                dbg_hash(with_prior.diagnostics()),
            );
            for i in 0..volume.cuts.len() {
                if let Some(g) = with_prior.tilt_grid(i) {
                    out(&format!("{tag}.prior.grid[{i}]"), grid_hash(g));
                }
            }
        }
        let i = vel_cuts[0];
        out(
            "v4_single",
            opt_grid(correct::dealias_velocity_grid_v4(volume, i, None, None).as_ref()),
        );
    }

    // ---------------- filters ----------------
    let ref_cut = lowest(volume, |c| c.moments.contains_key(&M::Reflectivity));
    if let Some(i) = ref_cut {
        let cut = &volume.cuts[i];
        let grid = &cut.moments[&M::Reflectivity];
        out("smooth_ref", grid_hash(&filters::smooth_moment_grid(grid)));
        for (name, m) in [
            ("ref", M::Reflectivity),
            ("vel", M::Velocity),
            ("rho", M::CorrelationCoefficient),
        ] {
            if let Some(g) = cut.moments.get(&m) {
                match filters::upsample_moment_grid(cut, g) {
                    Some(up) => {
                        out(&format!("upsample_{name}"), grid_hash(&up.grid));
                        out(
                            &format!("upsample_{name}.az"),
                            values_hash(&up.row_azimuths_deg),
                        );
                        out(
                            &format!("upsample_{name}.idx"),
                            dbg_hash(&up.grid.radial_indices),
                        );
                    }
                    None => out(&format!("upsample_{name}"), "None".into()),
                }
            }
        }
        for (i, cut) in volume.cuts.iter().enumerate() {
            if let Some(g) = cut.moments.get(&M::Velocity) {
                out(
                    &format!("gate_filter_vel[{i}]"),
                    grid_hash(&filters::legacy_api::apply_reflectivity_gate_filter(
                        cut, g, 10.0,
                    )),
                );
            }
        }
    }
    out(
        "upsample_factors",
        dbg_hash(&[
            filters::upsample_factors(1.0, 1000, 360, 1000),
            filters::upsample_factors(0.5, 250, 720, 1832),
        ]),
    );

    // ---------------- map ----------------
    out(
        "cref",
        opt_grid(map::composite_reflectivity_grid(volume).as_ref()),
    );
    out(
        "et",
        opt_grid(map::echo_top_grid(volume, map::ECHO_TOP_THRESHOLD_DBZ).as_ref()),
    );
    out("vil", opt_grid(map::vil_grid(volume).as_ref()));
    out("vild", opt_grid(map::vil_density_grid(volume).as_ref()));
    out(
        "mehs",
        opt_grid(map::mehs_grid(volume, 3200.0, 6400.0).as_ref()),
    );
    out("poh", opt_grid(map::poh_grid(volume, 3200.0).as_ref()));
    for cal in [
        map::MeshCalibration::Witt1998,
        map::MeshCalibration::MurilloHomeyer2019P95,
    ] {
        match map::hail_grids(volume, 3200.0, 6400.0, cal) {
            Some(h) => {
                out(&format!("hail.{cal:?}.shi"), grid_hash(&h.shi));
                out(&format!("hail.{cal:?}.mesh"), grid_hash(&h.mesh_mm));
                out(&format!("hail.{cal:?}.posh"), grid_hash(&h.posh_pct));
            }
            None => out(&format!("hail.{cal:?}"), "None".into()),
        }
    }
    let xs = |x: Option<map::CrossSection>| {
        x.map_or_else(
            || "None".to_owned(),
            |x| {
                format!(
                    "{}x{} len={} {}",
                    x.width,
                    x.height,
                    x.length_m,
                    values_hash(&x.values)
                )
            },
        )
    };
    out(
        "xs_ref",
        xs(map::reflectivity_cross_section(
            volume,
            (5.0, 10.0),
            (60.0, 80.0),
            96,
            48,
            15_000.0,
        )),
    );
    out(
        "xs_ref_native",
        xs(map::reflectivity_cross_section_with_smoothing(
            volume,
            (5.0, 10.0),
            (60.0, 80.0),
            96,
            48,
            15_000.0,
            map::CrossSectionSmoothing::Native,
        )),
    );
    out(
        "xs_rho",
        xs(map::moment_cross_section(
            volume,
            M::CorrelationCoefficient,
            map::InterpPolicy::CcGuard,
            (-40.0, 10.0),
            (30.0, 50.0),
            80,
            40,
            12_000.0,
        )),
    );
    out(
        "xs_vel",
        xs(map::velocity_cross_section(
            volume,
            (5.0, 10.0),
            (60.0, 80.0),
            96,
            48,
            15_000.0,
        )),
    );
    let mut cache = map::legacy_api::LegacyVolumeDealiasCache::new();
    out(
        "xs_vel_cached_native",
        xs(map::velocity_cross_section_cached_with_smoothing(
            volume,
            &mut cache,
            (-20.0, -30.0),
            (40.0, 20.0),
            64,
            40,
            12_000.0,
            map::CrossSectionSmoothing::Native,
        )),
    );
    out(
        "box_ref",
        map::volume_box_resample(volume, 20.0, 30.0, 60.0, 32, 8, 12_000.0)
            .map_or("None".into(), |v| values_hash(&v)),
    );
    out(
        "box_zdr",
        map::volume_box_resample_moment(
            volume,
            &M::DifferentialReflectivity,
            map::InterpPolicy::LinearAngle,
            20.0,
            30.0,
            60.0,
            32,
            8,
            12_000.0,
        )
        .map_or("None".into(), |v| values_hash(&v)),
    );
    for (i, cut) in volume.cuts.iter().enumerate() {
        let looks = map::cut_looks_like_rhi(cut);
        if !looks && i > 0 {
            continue;
        }
        out(
            &format!("rhi[{i}]"),
            format!("looks={} az={}", looks, map::rhi_fixed_azimuth_deg(cut)),
        );
        if let Some(g) = cut.moments.get(&M::Reflectivity) {
            out(
                &format!("rhi_cov[{i}]"),
                format!(
                    "{} {}",
                    map::rhi_coverage_top_m(cut, g),
                    map::rhi_coverage_range_m(cut, g)
                ),
            );
            out(
                &format!("rhi_section[{i}]"),
                xs(map::rhi_section(cut, g, 128, 64, 15_000.0, 60_000.0)),
            );
        }
    }

    // ---------------- retrieve ----------------
    let config = retrieve::DerivationConfig::all_supported();
    let dual = lowest(volume, |c| {
        c.moments.contains_key(&M::DifferentialPhase) && c.moments.contains_key(&M::Reflectivity)
    });
    let mut derive_targets: Vec<usize> = dual.into_iter().collect();
    if let Some(&v) = vel_cuts.first() {
        derive_targets.push(v);
    }
    if let Some(i) = ref_cut {
        derive_targets.push(i);
    }
    derive_targets.sort_unstable();
    derive_targets.dedup();
    for &i in &derive_targets {
        let mut cut = volume.cuts[i].clone();
        let report = retrieve::derive_cut_in_place(&mut cut, &config);
        out(&format!("derive[{i}].report"), dbg_text(&report));
        for (moment, grid) in &cut.moments {
            out(&format!("derive[{i}].{moment}"), grid_hash(grid));
        }
        let mut s_band = retrieve::DerivationConfig::with_products(
            retrieve::RadarBand::Unknown,
            retrieve::DerivedSweepProduct::ALL.iter().copied(),
        );
        s_band.overwrite_existing = true;
        let mut cut2 = volume.cuts[i].clone();
        out(
            &format!("derive_unknown_band[{i}].report"),
            dbg_text(&retrieve::derive_cut_in_place(&mut cut2, &s_band)),
        );
        out(
            &format!("derive_product[{i}].TDS"),
            opt_grid(
                retrieve::legacy_api::derive_product(
                    &volume.cuts[i],
                    retrieve::DerivedSweepProduct::TdsConfidence,
                    &config,
                )
                .as_ref(),
            ),
        );
    }
    let mut avail = String::new();
    for cut in &volume.cuts {
        for &p in retrieve::DerivedSweepProduct::ALL {
            avail.push(if retrieve::cut_has_advanced_product_sources(cut, p) {
                '1'
            } else {
                '0'
            });
        }
        for m in [
            M::Reflectivity,
            M::Velocity,
            M::Unknown("REFC".into()),
            M::SpecificDifferentialPhase,
        ] {
            avail.push(if retrieve::cut_can_materialize_moment(cut, &m) {
                '1'
            } else {
                '0'
            });
            avail.push(if retrieve::cut_has_moment_source(cut, &m) {
                '1'
            } else {
                '0'
            });
        }
        avail.push('|');
    }
    out("availability", dbg_hash(&avail));
    out(
        "volume_availability",
        format!(
            "{:?}",
            retrieve::DerivedSweepProduct::ALL
                .iter()
                .map(|p| retrieve::legacy_api::volume_has_advanced_product_sources(volume, *p))
                .collect::<Vec<_>>()
        ),
    );
    if let Some(&i) = vel_cuts.first() {
        let cut = &volume.cuts[i];
        let g = &cut.moments[&M::Velocity];
        out(
            "azshear",
            grid_hash(&retrieve::azimuthal_shear_grid(cut, g)),
        );
        out(
            "divergence",
            grid_hash(&retrieve::radial_divergence_grid(cut, g)),
        );
        let d = dealiased[i].as_ref().unwrap();
        let field = retrieve::PolarVelocityField::from_dealiased_velocity_grid(cut, d);
        out(
            "polar_field",
            format!(
                "{} {} {} {} {}",
                values_hash(&field.azimuths_deg),
                field.first_gate_m,
                field.gate_spacing_m,
                field.gate_count,
                values_hash(&field.values)
            ),
        );
        let radii: Vec<f32> = (10..=60).step_by(10).map(|r| r as f32).collect();
        out(
            "gbvtd_axisym",
            dbg_hash(&retrieve::retrieve_axisymmetric(
                &field,
                (20.0, 80.0),
                &radii,
                72,
                (0.0, 0.0),
            )),
        );
    }
    out(
        "rotation_sites",
        dbg_hash(&retrieve::legacy_api::detect_rotation_sites(volume)),
    );
    out(
        "rotation_features",
        dbg_hash(&retrieve::legacy_api::rotation_features_per_tilt(volume)),
    );
    out(
        "rotation_cut_indices",
        format!("{:?}", retrieve::rotation_velocity_cut_indices(volume)),
    );
    let borrowed: Vec<Option<&MomentGrid>> = dealiased.iter().map(Option::as_ref).collect();
    match retrieve::legacy_api::compute_vwp(volume, &borrowed, retrieve::VwpConfig::default()) {
        Ok(profile) => {
            out("vwp.levels", dbg_hash(&profile.levels));
            out(
                "vwp.meta",
                format!(
                    "{} {} {:?} {}",
                    profile.site_id,
                    profile.valid_time,
                    profile.radar_elevation_m,
                    profile.velocity_sweep_count
                ),
            );
        }
        Err(e) => out("vwp", format!("Err({e})")),
    }
    out("marc", opt_grid(retrieve::marc_grid(volume).as_ref()));
    out("gust", opt_grid(retrieve::gust_proxy_grid(volume).as_ref()));
    for interp in [
        retrieve::CappiInterpolation::Nearest,
        retrieve::CappiInterpolation::LinearElevation,
    ] {
        out(
            &format!("cappi.{interp:?}"),
            opt_grid(retrieve::cappi_grid(volume, M::Reflectivity, 3000.0, interp).as_ref()),
        );
    }
    out(
        "cappi_zdr",
        opt_grid(
            retrieve::cappi_grid(
                volume,
                M::DifferentialReflectivity,
                2000.0,
                retrieve::CappiInterpolation::LinearElevation,
            )
            .as_ref(),
        ),
    );
    out(
        "cmax",
        opt_grid(retrieve::column_max_grid(volume, M::Reflectivity).as_ref()),
    );
    out(
        "cmin",
        opt_grid(retrieve::column_min_grid(volume, M::Reflectivity).as_ref()),
    );
    out(
        "cmean",
        opt_grid(retrieve::column_mean_grid(volume, M::Velocity).as_ref()),
    );
    out(
        "llcref",
        opt_grid(retrieve::low_level_composite_reflectivity_grid(volume, 3000.0).as_ref()),
    );
    out(
        "ebase",
        opt_grid(retrieve::echo_base_grid(volume, 18.0).as_ref()),
    );
    out(
        "etop",
        opt_grid(retrieve::echo_top_height_grid(volume, 18.0).as_ref()),
    );
    out(
        "edepth",
        opt_grid(retrieve::echo_depth_grid(volume, 18.0).as_ref()),
    );
    out(
        "hmax",
        opt_grid(retrieve::height_of_max_reflectivity_grid(volume).as_ref()),
    );

    // ---------------- track ----------------
    let cells = track::legacy_api::identify_storm_cells(volume);
    out("cells", dbg_hash(&cells));
    let mut tracker = track::StormTracker::default();
    tracker.associate(volume.volume_time, &cells, None);
    tracker.associate(volume.volume_time + Duration::minutes(5), &cells, None);
    out("tracker", format!("{}", tracker.tracks.len()));
    for (m, agg) in [
        (M::Reflectivity, track::SwathAggregation::Max),
        (M::Velocity, track::SwathAggregation::MaxMagnitude),
    ] {
        out(
            &format!("base_tilt.{m}"),
            format!("{:?}", track::base_tilt_cut(volume, &m)),
        );
        match track::max_value_swath(&[volume, volume], m.clone(), agg) {
            Some(swath) => {
                let cut = &swath.cuts[0];
                out(&format!("swath.{m}"), grid_hash(&cut.moments[&m]));
                out(
                    &format!("swath.{m}.meta"),
                    format!(
                        "{} {} {} {}",
                        swath.site.id,
                        swath.volume_time,
                        cut.elevation_deg,
                        values_hash(
                            &cut.radials
                                .iter()
                                .map(|r| r.azimuth_deg)
                                .collect::<Vec<_>>()
                        )
                    ),
                );
            }
            None => out(&format!("swath.{m}"), "None".into()),
        }
    }
    if let Some(i) = ref_cut {
        let a = &volume.cuts[i].moments[&M::Reflectivity];
        let b = a.clone();
        let o = M::Unknown("OUT".into());
        out(
            "diff",
            opt_grid(track::difference_grid(a, &b, o.clone()).as_ref()),
        );
        out(
            "trend",
            opt_grid(track::trend_grid(a, &b, 600.0, o.clone()).as_ref()),
        );
        out(
            "maxswath",
            opt_grid(track::maximum_swath_grid(&[a, &b], o.clone()).as_ref()),
        );
        out(
            "minswath",
            opt_grid(track::minimum_swath_grid(&[a, &b], o.clone()).as_ref()),
        );
        out(
            "mean",
            opt_grid(track::mean_grid(&[a, &b], o.clone()).as_ref()),
        );
        out(
            "accum",
            opt_grid(track::accumulate_rate_grids(&[(a, 0.0), (&b, 600.0)], o.clone()).as_ref()),
        );
        out(
            "duration",
            opt_grid(
                track::exceedance_duration_grid(&[(a, 0.0), (&b, 600.0)], 30.0, o.clone()).as_ref(),
            ),
        );
        out(
            "prob",
            opt_grid(track::exceedance_probability_grid(&[a, &b], 30.0, o).as_ref()),
        );
    }
    let spec = track::tracks::TracksGridSpec {
        half_extent_km: 100.0,
        cell_km: 1.0,
    };
    out(
        "azshear_cart",
        values_hash(&track::legacy_api::low_level_azshear_cartesian(
            volume, &spec,
        )),
    );
    out(
        "azshear_cut_indices",
        format!(
            "{:?}",
            track::legacy_api::low_level_azshear_cut_indices(volume)
        ),
    );
    let sites = retrieve::legacy_api::detect_rotation_sites(volume);
    out(
        "tds",
        dbg_hash(&track::legacy_api::detect_tds_gates(volume, &sites)),
    );
    let fake_sites: Vec<retrieve::RotationSite> = cells
        .iter()
        .take(4)
        .map(|c| retrieve::RotationSite {
            azimuth_deg: (c.east_km.atan2(c.north_km).to_degrees().rem_euclid(360.0)) as f32,
            ground_range_m: c.east_km.hypot(c.north_km) * 1000.0,
            vrot_mps: 20.0,
            gate_to_gate_dv_mps: 30.0,
            rank: 5,
            depth_tilts: 3,
            depth_m: 4000.0,
            base_elevation_deg: 0.5,
            strength: retrieve::RotationStrength::Mesocyclone,
        })
        .collect();
    out(
        "tds_fake",
        dbg_hash(&track::legacy_api::detect_tds_gates(volume, &fake_sites)),
    );
}
