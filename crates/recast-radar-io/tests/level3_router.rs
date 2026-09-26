//! NEXRAD / TDWR Level III through the router: every real Level III file in
//! the corpus (`testdata/level3/manifest.toml`) sniffs as Level III, no file
//! of another format does, and the routed decode is exactly
//! `recast_radar_io_level3::read_level3_volume` (with the decoded product as
//! the format metadata).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use recast_radar_io::{
    FormatMetadata, IoError, SupportedVolumeFormat, read_supported_volume_bytes,
    read_supported_volume_with_metadata, sniff_supported_volume_format,
};
use recast_radar_io_level3::{
    Level3Error, Level3Message, decode_message, decode_product, read_level3_volume,
};
use recast_radar_testdata::Format;

/// Committed corpus entries of one format (or of every other format).
fn committed(level3: bool) -> Vec<(&'static str, Vec<u8>)> {
    recast_radar_testdata::manifest()
        .files
        .iter()
        .filter(|entry| entry.committed.is_some())
        // Fuzz regression inputs are malformed on purpose.
        .filter(|entry| !entry.tags.iter().any(|t| t == "fuzz-regression"))
        .filter(|entry| (entry.format == Format::NexradLevel3) == level3)
        .map(|entry| {
            let bytes = recast_radar_testdata::bytes(&entry.id)
                .unwrap_or_else(|err| panic!("{}: {err}", entry.id));
            (entry.id.as_str(), bytes)
        })
        .collect()
}

#[test]
fn every_level3_file_sniffs_as_level3_and_nothing_else_does() {
    let level3 = committed(true);
    // 269 files of testdata/level3 (products of 1993-2026, General Status
    // Messages and a free text message, with WMO/AWIPS, NOAAPort and zlib
    // framing) and 7 of testdata/other (VWP and storm tracking products, a
    // SAILS cut and a Radar Observation text bulletin).
    assert_eq!(level3.len(), 276);
    for (id, bytes) in &level3 {
        assert_eq!(
            sniff_supported_volume_format(bytes),
            SupportedVolumeFormat::NexradLevel3,
            "{id}"
        );
    }
    let others = committed(false);
    assert!(others.len() > 50, "{} other committed files", others.len());
    for (id, bytes) in &others {
        assert_ne!(
            sniff_supported_volume_format(bytes),
            SupportedVolumeFormat::NexradLevel3,
            "{id}"
        );
    }
}

/// Files that come close to the Level III rules and are not Level III.
#[test]
fn near_misses_do_not_sniff_as_level3() {
    // A headerless Level II LDM record of 162 169 bytes: its size word
    // starts 00 02, the General Status Message code. With bytes 18-19 set
    // to FF FF (the block divider, which a bzip2 stream holds once in 65 536
    // records) it still goes to Level II: its "time" at byte 4 is `BZh9`.
    let chunk = recast_radar_testdata::bytes("l2chunk-kiwa-307-20260917-003629-002-i").unwrap();
    assert_eq!(&chunk[..8], b"\x00\x02\x79\x79BZh9");
    assert_eq!(
        sniff_supported_volume_format(&chunk),
        SupportedVolumeFormat::NexradLevel2
    );
    let mut collision = chunk.clone();
    collision[18..20].copy_from_slice(&[0xFF, 0xFF]);
    assert_eq!(
        sniff_supported_volume_format(&collision),
        SupportedVolumeFormat::NexradLevel2
    );

    // A WMO text bulletin that is not radar (SRUS55 hydrological XML from
    // the NWS radar directory): plain text after a heading is Level III
    // only after a NOUS or SDUS heading.
    let text = recast_radar_testdata::bytes("wmo-text-kslc-20251012-0424-hmlslc").unwrap();
    assert!(text.starts_with(b"SRUS55 KSLC 120424\r\r\nHMLSLC\r\r\n<?xml"));
    assert_eq!(
        sniff_supported_volume_format(&text),
        SupportedVolumeFormat::NexradLevel2
    );
}

/// The message without its transmission header (the form some LDM and
/// GR2Analyst caches store) still sniffs and decodes as Level III.
#[test]
fn bare_messages_sniff_as_level3() {
    // KTLX N0V 2013: `SDUS54 KOUN 202016\r\r\nN0VTLX\r\r\n` (30 bytes) then
    // the message (golden framing: message_bytes 17444 of 17474).
    let framed = recast_radar_testdata::bytes("l3-tlx-n0v-20130520-2016").unwrap();
    assert_eq!(&framed[..30], b"SDUS54 KOUN 202016\r\r\nN0VTLX\r\r\n");
    let bare = &framed[30..];
    assert_eq!(bare.len(), 17_444);
    assert_eq!(
        sniff_supported_volume_format(bare),
        SupportedVolumeFormat::NexradLevel3
    );
    let routed = read_supported_volume_bytes(bare).unwrap();
    let framed_volume = read_level3_volume(&framed).unwrap();
    assert_eq!(routed.sweeps.len(), framed_volume.sweeps.len());
    assert_eq!(
        format!("{:?}", routed.sweeps),
        format!("{:?}", framed_volume.sweeps)
    );

    // KDDC General Status Message (NXUS63, 30-byte header): message code 2.
    let gsm = recast_radar_testdata::bytes("l3-ddc-gsm-20200817-1000").unwrap();
    assert_eq!(
        sniff_supported_volume_format(&gsm[30..]),
        SupportedVolumeFormat::NexradLevel3
    );
}

#[test]
fn routed_level3_decode_is_the_direct_decode() {
    let mut volumes = 0;
    let mut no_data = 0;
    let mut messages = 0;
    for (id, bytes) in committed(true) {
        let direct = read_level3_volume(&bytes);
        let routed = read_supported_volume_with_metadata(&bytes);
        match (direct, routed) {
            (Ok(direct), Ok(routed)) => {
                volumes += 1;
                // Debug text compares NaN fills equal.
                assert_eq!(
                    format!("{:?}", routed.volume),
                    format!("{direct:?}"),
                    "{id}"
                );
                let FormatMetadata::Level3(product) = routed.metadata else {
                    panic!("{id}: no Level III metadata");
                };
                assert_eq!(*product, decode_product(&bytes).unwrap(), "{id}");
                let plain = read_supported_volume_bytes(&bytes).unwrap();
                assert_eq!(format!("{plain:?}"), format!("{direct:?}"), "{id}");
            }
            (Err(direct), Err(routed)) => {
                assert_eq!(routed.to_string(), direct.to_string(), "{id}");
                match (&direct, routed) {
                    // A product without a data array, a General Status
                    // Message and a text message come back decoded.
                    (
                        Level3Error::NoDataArray { .. }
                        | Level3Error::NotAProduct { code: 2 }
                        | Level3Error::TextOnly { .. },
                        IoError::Level3WithoutVolume(message),
                    ) => {
                        let expected = decode_message(&bytes).unwrap();
                        match (&direct, &*message) {
                            (Level3Error::NoDataArray { code }, Level3Message::Product(p)) => {
                                assert_eq!(p.description.product_code, *code, "{id}");
                            }
                            (Level3Error::NotAProduct { .. }, Level3Message::GeneralStatus(_))
                            | (Level3Error::TextOnly { .. }, Level3Message::Text(_)) => {
                                messages += 1;
                            }
                            (direct, message) => panic!("{id}: {direct} but {message:?}"),
                        }
                        assert_eq!(*message, expected, "{id}");
                    }
                    (_, IoError::Level3(_)) => {}
                    (_, routed) => panic!("{id}: routed {routed:?}"),
                }
                no_data += 1;
            }
            (direct, routed) => panic!(
                "{id}: direct {:?} but routed {:?}",
                direct.map(|_| ()),
                routed.map(|_| ())
            ),
        }
    }
    assert!(volumes >= 160, "{volumes} volumes");
    assert!(no_data >= 40, "{no_data} files without a data array");
    // Three General Status Messages, the free text message and the Radar
    // Observation bulletin.
    assert_eq!(messages, 5);
}

#[test]
fn products_without_a_data_array_are_level3_errors() {
    // KTLX NST 2013: storm tracking symbols and pages only. The error
    // carries the decoded product, whose storm table is readable.
    let nst = recast_radar_testdata::bytes("l3-tlx-nst-20130520-2016").unwrap();
    let Err(IoError::Level3WithoutVolume(message)) = read_supported_volume_bytes(&nst) else {
        panic!("NST is not a volume");
    };
    let Level3Message::Product(product) = &*message else {
        panic!("NST is a product");
    };
    assert_eq!(product.description.product_code, 58);
    assert!(!product.storm_tracking().unwrap().cells.is_empty());
    assert_eq!(
        IoError::Level3WithoutVolume(message).to_string(),
        Level3Error::NoDataArray { code: 58 }.to_string()
    );
    // KABR free text message (NOUS63): the text comes back decoded.
    let ftm = recast_radar_testdata::bytes("l3-abr-ftm-20110428-1331").unwrap();
    let Err(IoError::Level3WithoutVolume(message)) = read_supported_volume_bytes(&ftm) else {
        panic!("a free text message is not a volume");
    };
    let Level3Message::Text(text) = &*message else {
        panic!("expected a text message, got {message:?}");
    };
    assert_eq!(text.text_header.wmo_heading, "NOUS63 KABR 281331");
    assert!(!text.text.is_empty());
    assert_eq!(
        IoError::Level3WithoutVolume(message.clone()).to_string(),
        Level3Error::TextOnly {
            heading: "NOUS63 KABR 281331".into()
        }
        .to_string()
    );
    // KDDC General Status Message: the status comes back decoded.
    let gsm = recast_radar_testdata::bytes("l3-ddc-gsm-20200817-1000").unwrap();
    let Err(IoError::Level3WithoutVolume(message)) = read_supported_volume_bytes(&gsm) else {
        panic!("a General Status Message is not a volume");
    };
    let Level3Message::GeneralStatus(status) = &*message else {
        panic!("expected a General Status Message, got {message:?}");
    };
    assert_eq!(status.message_header.code, 2);
    assert_eq!(
        IoError::Level3WithoutVolume(message).to_string(),
        Level3Error::NotAProduct { code: 2 }.to_string()
    );
}
