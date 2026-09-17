//! Live end-to-end probe of the split-volume international ODIM providers
//! (SHMU Slovakia, DWD Germany, CHMI Czechia, EUMETNET ORD): plan ->
//! download -> decode -> merge, against the real open-data endpoints.
//!
//! For each requested provider this fetches the newest [`FramePlan`] for
//! one site, downloads every part with `recast_radar_data::fetch_volume_bytes`,
//! decodes each through the shared `recast_radar_io::read_supported_volume_bytes`
//! router (ODIM_H5 per the EUMETNET OPERA Data Information Model; Michelson
//! et al., OPERA WP 2.1/2.2, v2.2-2.3), assembles the parts with
//! `recast_radar_core::merge_volumes`, and prints the merged volume's site,
//! sweep count, fields per sweep, and the `MergeReport` counters (including
//! `skipped_geometry`, which is expected to fire on CHMI's supplemental
//! 1.5-degree task sweep whose gate spacing differs from the full volume's
//! same-elevation sweep).
//!
//! Usage:
//!   cargo run -p recast-radar-io --example intl_assembly_probe -- [shmu|dwd|dwd-full|chmi|all] [site]
//!
//! `dwd-full` probes `DwdProvider::with_dual_pol()` (ZDR/RhoHV/PhiDP
//! sweeps included, ~50 parts); it is not part of `all`. Default sites:
//! shmu=skjav, dwd=asb, chmi=brd, ord=nohur (Norway's split PVOL feed —
//! pass another ORD site, e.g. frtou, to probe the per-sweep SCAN shape).
//! Exit code 1 when any requested probe fails.

use recast_radar_core::model::{Sweep, Volume};
use recast_radar_data::international::{
    ChmiProvider, DwdProvider, IntlProvider, OrdProvider, ShmuProvider,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let selection = args.next().unwrap_or_else(|| "all".to_owned());
    let site_override = args.next();

    let providers: Vec<(&str, Box<dyn IntlProvider>, &str)> = vec![
        ("shmu", Box::new(ShmuProvider::new()), "skjav"),
        ("dwd", Box::new(DwdProvider::new()), "asb"),
        ("dwd-full", Box::new(DwdProvider::with_dual_pol()), "asb"),
        ("chmi", Box::new(ChmiProvider::new()), "brd"),
        ("ord", Box::new(OrdProvider::new()), "nohur"),
    ];

    let mut ran = 0usize;
    let mut failures = 0usize;
    for (key, provider, default_site) in &providers {
        let included = if selection == "all" {
            *key != "dwd-full"
        } else {
            selection == *key
        };
        if !included {
            continue;
        }
        ran += 1;
        let site = site_override.as_deref().unwrap_or(default_site);
        println!("==== {} ({key}) site={site}", provider.label());
        if let Err(err) = probe(provider.as_ref(), site) {
            eprintln!("PROBE FAILED [{key}/{site}]: {err}");
            failures += 1;
        }
        println!();
    }

    if ran == 0 {
        eprintln!(
            "unknown provider '{selection}' (expected shmu, dwd, dwd-full, chmi, ord, or all)"
        );
        std::process::exit(2);
    }
    if failures > 0 {
        std::process::exit(1);
    }
}

fn probe(provider: &dyn IntlProvider, site: &str) -> Result<(), String> {
    let plan = provider.latest(site)?;
    println!("identity: {}", plan.identity);
    println!("parts: {} (merge={})", plan.parts.len(), plan.merge);

    let mut volumes: Vec<Volume> = Vec::with_capacity(plan.parts.len());
    for part in &plan.parts {
        let bytes = recast_radar_data::fetch_volume_bytes(&part.url)
            .map_err(|err| format!("download {}: {err}", part.url))?;
        let volume = recast_radar_io::read_supported_volume_bytes(&bytes)
            .map_err(|err| format!("decode {}: {err}", part.url))?;
        println!(
            "  part {} -> {} bytes, site={}, sweeps={}, fields[sweep0]={}",
            short_name(&part.url),
            bytes.len(),
            volume.attrs.instrument_name,
            volume.sweeps.len(),
            volume
                .sweeps
                .first()
                .map_or_else(String::new, sweep_field_names),
        );
        volumes.push(volume);
    }

    let (merged, report) =
        recast_radar_core::merge_volumes(volumes).map_err(|err| err.to_string())?;
    println!(
        "merged: site={} ({}) time={} sweeps={}",
        merged.attrs.instrument_name,
        merged.attrs.site_name.as_deref().unwrap_or("-"),
        merged.time_reference.format("%Y-%m-%dT%H:%M:%SZ"),
        merged.sweeps.len()
    );
    for (index, sweep) in merged.sweeps.iter().enumerate() {
        println!(
            "  sweep {index:2} fixed={:6.2} rays={:4} fields: {}",
            sweep.fixed_angle_deg,
            sweep.nrays(),
            sweep_field_names(sweep)
        );
    }
    println!(
        "merge report: merged_fields={} skipped_geometry={} field_collisions={}",
        report.merged_fields, report.skipped_geometry, report.field_collisions
    );
    Ok(())
}

fn sweep_field_names(sweep: &Sweep) -> String {
    sweep
        .fields
        .iter()
        .map(|field| field.name.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn short_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}
