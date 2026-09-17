//! The router's gzip unwrapping decodes every member of a multi-member
//! `.gz` Level II volume, the same as the Level II decoder's own gzip path.
//! Real input: the KTLX 2013-05-20 archive (9.5 MB gzip), re-compressed as
//! back-to-back gzip members of the kind `pigz`, `bgzip` and appended files
//! produce. Whole-file gzip used to be inflated to its first member only,
//! which silently gave a partial volume.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use recast_radar_core::model::Volume;
use recast_radar_io::read_supported_volume_bytes;
use recast_radar_io_nexrad::{normalize_archive_bytes, read_volume_from_bytes};

/// `volume` with NaN per-ray values (rays without a Nyquist velocity or
/// unambiguous range) replaced, so that equal volumes compare equal.
fn without_nan(volume: &Volume) -> Volume {
    let mut volume = volume.clone();
    for sweep in &mut volume.sweeps {
        for values in [
            &mut sweep.ray_vars.nyquist_velocity_mps,
            &mut sweep.ray_vars.unambiguous_range_m,
        ]
        .into_iter()
        .flatten()
        {
            for value in values.iter_mut().filter(|value| value.is_nan()) {
                *value = f32::MAX;
            }
        }
    }
    volume
}

#[test]
fn router_decodes_every_member_of_a_regzipped_real_volume() {
    let original = match recast_radar_testdata::bytes("l2-ktlx-20130520-201643") {
        Ok(bytes) => bytes,
        Err(e) if e.is_offline() => {
            eprintln!("skipping: {e}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let expected = read_volume_from_bytes(&original).expect("real gzip volume decodes");
    assert_eq!(expected.sweeps.len(), 17);
    assert_eq!(expected.provenance.decode.decoded_ray_count, 8280);

    let (payload, _) = normalize_archive_bytes(&original).expect("real gzip volume inflates");
    let mut multi = Vec::new();
    for member in payload.chunks(4 << 20) {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(member).expect("gzip member");
        multi.extend(encoder.finish().expect("finish gzip member"));
    }
    // Trailing zero padding after the last member is ignored, as before.
    multi.extend_from_slice(&[0u8; 512]);

    let routed = read_supported_volume_bytes(&multi).expect("routed multi-member gzip decodes");
    assert_eq!(routed.sweeps.len(), expected.sweeps.len());
    assert_eq!(
        routed.provenance.decode.decoded_ray_count,
        expected.provenance.decode.decoded_ray_count
    );
    assert_eq!(routed.provenance.compression.as_deref(), Some("gzip"));
    assert!(
        without_nan(&routed) == without_nan(&expected),
        "routed volume differs from the original"
    );
}
