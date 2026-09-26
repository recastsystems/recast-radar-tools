//! Corpus coverage: every file in `testdata/level3/manifest.toml` decodes with
//! no [`Packet::Unknown`] left anywhere (symbology layers, graphic pages,
//! product 62 cell trend data and the packets nested in SCIT packets 23/24),
//! and `docs/level3/coverage.md` is generated from the same walk.
//!
//! Values are not compared here; the family tests do that against the golden
//! JSON (`radial_generic.rs`, `raster.rs`, `symbols.rs`, `text_vectors.rs`).
//! This test checks that each packet code decodes to the variant of the family
//! that owns it, that the codes found (including nested ones) are the codes the
//! golden ICD walker found, and that the only files that do not decode to a
//! product are the ones the golden JSON marks as text-only or as having no
//! Product Description Block, which `decode_message` decodes to
//! `Level3Message::Text` and `Level3Message::GeneralStatus`
//! (`tests/messages.rs` compares their contents).
//!
//! `docs/level3/coverage.md` must match what this test renders. After a corpus
//! or decoder change, regenerate it with
//! `LEVEL3_WRITE_COVERAGE=1 cargo test -p recast-radar-io-level3 --test coverage`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::OnceLock;

use common::Entry;
use recast_radar_io_level3::levels::{DataLevels, LevelEncoding};
use recast_radar_io_level3::packets::generic::GenericComponent;
use recast_radar_io_level3::packets::symbols::SymbolPacket;
use recast_radar_io_level3::{
    GraphicLayout, Level3Message, Level3Product, Packet, TabularLayout, decode_message,
    product_info,
};

/// Environment variable that makes the test write `docs/level3/coverage.md`.
const WRITE_ENV: &str = "LEVEL3_WRITE_COVERAGE";

/// Every packet code the dispatcher routes to a family module, with the module
/// and the `Packet` variant its packets decode to.
///
/// All four L3.3 families (radial, raster, symbols, text) are merged, so no
/// packet code is exempt from the no-`Unknown` assertion. Packet 29 decodes
/// from the ICD (`packets/generic.rs`); no corpus file contains it.
const DISPATCH: &[(u16, &str, Option<&str>)] = &[
    (1, "text", Some("Text")),
    (2, "text", Some("Text")),
    (3, "symbols", Some("Symbol")),
    (4, "symbols", Some("Symbol")),
    (5, "symbols", Some("Symbol")),
    (6, "vectors", Some("Vectors")),
    (7, "vectors", Some("Vectors")),
    (8, "text", Some("Text")),
    (9, "vectors", Some("Vectors")),
    (10, "vectors", Some("Vectors")),
    (11, "symbols", Some("Symbol")),
    (12, "symbols", Some("Symbol")),
    (13, "symbols", Some("Symbol")),
    (14, "symbols", Some("Symbol")),
    (15, "symbols", Some("Symbol")),
    (16, "radial", Some("Radial")),
    (17, "raster", Some("DigitalPrecip")),
    (18, "raster", Some("Raster")),
    (19, "symbols", Some("Symbol")),
    (20, "symbols", Some("Symbol")),
    (21, "symbols", Some("Symbol")),
    (22, "symbols", Some("Symbol")),
    (23, "symbols", Some("Symbol")),
    (24, "symbols", Some("Symbol")),
    (25, "symbols", Some("Symbol")),
    (26, "symbols", Some("Symbol")),
    (28, "generic", Some("Generic")),
    (29, "generic", Some("Generic")),
    (30, "irm", Some("Irm")),
    (31, "irm", Some("Irm")),
    (32, "raster", Some("Raster")),
    (33, "raster", Some("Raster")),
    (0x0802, "contour", Some("Contour")),
    (0x0E03, "contour", Some("Contour")),
    (0x3501, "contour", Some("Contour")),
    (0xAF1F, "radial", Some("Radial")),
    (0xBA07, "raster", Some("Raster")),
    (0xBA0F, "raster", Some("Raster")),
];

/// Gaps that the corpus walk cannot show. Rendered into the "Known gaps"
/// section of the document.
const KNOWN_GAPS: &[&str] = &[
    "**Packets and components without a real sample.** Packets 5 (vector \
     arrow), 26 (ETVS), 29 (generic data with external data description), 33 \
     (digital raster data array), 0xBA0F (raster data) and 0x3501 (unlinked \
     contour vectors), and generic grid (2), area (3), table (5) and event (6) \
     components, have no public sample, so they are not checked against \
     data. Their layouts are the ICD figures', and for the generic packets \
     that of the RPG's own serializer (`orpg_xdr.c` in the ORPG source of \
     CODE, `docs/level3/reference.md` sections 1 and 7), which settles the \
     XDR details the figures leave open (`packets::generic`); CODE's display \
     tool reads packets 5, 26, 0x3501 and 0xBA0F as the decoder does. \
     Packets 7 and 9 were found in the NCEI archive of 1993-1994 (products \
     50, 51, 53 and 84) and are checked. Searched (`docs/level3/reference.md` \
     section 7): the AWS bucket's 146 product IDs, MetPy's test data, 15 NCEI \
     daily archives (1995-2016, about 90 000 products) decoded with \
     `examples/level3_scan.rs`, the product IDs of every NCEI day archive of \
     1992-1994 (5 286 archives) and of 520 random archives of 1995-2007 \
     (`tools/level3_ncei_survey.py`), 20 archive days of 1992-1995 holding \
     product codes the corpus lacked decoded whole, Unidata's THREDDS \
     Level III catalog (the IDD feed: the AWS product IDs), the test data of \
     netcdf-java, Py-ART, wradlib, xradar and nexrad-level-3-data, and IEM's \
     archive (no Level III). Packet 26 does not occur in distributed \
     products: every TVS product examined (214 of KNQA 2008-02-05, the corpus \
     files, and one from each of the 203 sites with TVS products on AWS in \
     2022) lists the TDA adaptation \"Max # of Elevated TVSs\" as 0, so no \
     ETVS is detected. The others belong to products that were not \
     distributed or not found (packet 5 to the combined moment product 49 of \
     DSI-7000 Table III, area components to the DMD, MIGFA and AMDA products \
     149, 140 and 196), and the ORPG source uses packet 29 only for the model \
     data the RPG ingests. Only the fuzz target exercises these decoders, as \
     does TDWR product 184 below. Accepting this as a gap, or searching \
     further (for example by asking the Radar Operations Center for sample \
     products 49, 140, 143, 149 and 196), is the owner's decision.",
    "**Same-author goldens.** Where no third-party reader exists, the golden \
     is a second implementation by the decoder's author and shares its \
     reading of the ICD: the radar coded message groups and sub-box \
     lettering (`tools/level3_rcm_golden.py`; MetPy only splits the message \
     into its three parts), the IRM grid, packets 7 and 9 \
     (`tests/text_vectors.rs`; MetPy stops reading a layer at them), the \
     legacy storm table layouts (columns split from MetPy's page text, \
     `tools/level3_tables_golden.py`), and the placement of the DPA, rate \
     and radar coded message grids (`tools/level3_dpa_golden.py`, \
     `tools/level3_lfm_golden.py`: pyproj gives the projection only; the \
     placement is fitted to coverage masks and STI/TVS positions). The Weak \
     Echo Region window is taken from the raster MetPy reads, and the test \
     requires every nonzero raster cell to be in it.",
    "**Geometry.** Checked on real products: the windows 43-46 and 55 \
     (their data span the window of halfwords 27-28; the four KFTG 1994 \
     products of one window span the same ranges with three bin sizes), the \
     cross sections 50 and 51 (the products' own axes and end point \
     labels), the combined shear 87 (the maximum-shear cell sits at \
     halfwords 48-49), the Weak Echo Region 53 (placement fitted to the \
     base reflectivity of the same volume in two products, one on a storm \
     60 nmi out and one at the radar; no ICD describes it; the sweep keeps \
     52 columns of the 50-row raster shifted back, because the nonzero \
     cells of 7 of the 29 products of 1993-1994 reach one column beyond a \
     50-column window), and the legacy radial products 16-29 (bin count \
     times the Table III size is the Table III range). Not checked, for \
     lack of a real product: cross sections 52, 85 and 86 (nominal geometry \
     of 2620003AE section 14.2.3), the combined moment 49 (0.27 nmi cells) \
     and layer composite turbulence 68-72 (2.2 nmi cells) of DSI-7000 Table \
     III, 93, 156 and 157 (Table III resolutions) and the quasi-vertical \
     profiles 189-192 (2620001AD Table III). Not verified either way: the \
     bin size of product 34 (Clutter Filter Control) with 460 bins (PAKC \
     2021), read as 500 m bins over the 230 km of 2620003AE section 34.2.3; \
     460 bins of the section's 1 km (out to Level II's 460 km) fit the ICD \
     as well, and neither the packet's scale factor (a display scale: 999 \
     for the 1 km and 250 m products alike) nor, in review, the clutter \
     maps of the Level II volumes of the same site and hour settle it \
     (`volume::range_bin_size_m`). The higher Weak Echo Region slices match \
     the base reflectivity best 0.5-1 nmi along the storm motion, as if \
     moved to the time of the lowest slice; the sweeps are not moved.",
    "**Generic grid components** (type 2) do not become sweeps: 2620001AD \
     Figure E-5 gives the grid types (array, equally spaced, \
     latitude/longitude, polar) and leaves the origin and step sizes to \
     component parameters it does not name, and no real product with a grid \
     component was found (see Packets and components without a real \
     sample), so any placement would be a guess. The decoded product keeps \
     the component typed, and the volume keeps its dimensions, parameters \
     and every value as a display packet record (`level3_display_packets`).",
    "**TDWR product 184** is read as a 16-level threshold product (2620063E \
     Figure 3-6 sheet 6 Note 1), although its Table III row lists 256 levels; \
     no real product 184 exists in the NCEI archive or on AWS to settle it \
     (NWS distributes TDWR reflectivity and velocity, not spectrum width: \
     the NCEI TDWR days of 2009, 2012, 2014, 2018, 2019 and 2024 hold TR0-TR2, \
     TV0-TV2 and TZL or TZ0-TZ2 and none; `docs/level3/reference.md` \
     section 7). The packed codes are kept, so a 256-level product would \
     lose no data, but its levels from 16 up would read as missing. Which \
     reading of the ICD to follow is the owner's decision.",
    "**Storm attribute tables**: every table layout found in the corpus and \
     in five NCEI days (1996-2003) is typed, also on the stand-alone products \
     101-104 of 1993-2001 and the composite reflectivity products 35, 36 and \
     39; the adaptation data pages stay as page text. The legacy layouts \
     (1993-1997) follow their column headings, which no ICD obtained \
     documents; the meaning of the legacy TVS table's `ORI` and `ROT` and of \
     the 1995-1996 combined table's `MW VOL` is taken from the ICD acronym \
     list or the heading alone.",
    "**Radar Coded Message (Unedited)** (product 83, `IRM`): no ICD on the \
     ROC site defines it; DSI-7000 Figure 3-22 gives its layers and names \
     packet 30's values (LFM grid rotation, X and Y offsets to the radar's \
     `MM` box corner, 1/16 LFM box size, spare), but the products store the \
     offsets at about four times and the box size at twice the figure's \
     kilometres, and the rotation does not match the grid convergence \
     (`packets::irm`); the values are kept as stored.",
    "**Legacy codes from DSI-7000.** Codes 39, 40, 42, 49, 52, 53, 68-72, 83, \
     88 and 106 are spare in every Table III on the ROC site; their names \
     and Table V come from the 1990s Table III and Table V that NCDC \
     reproduces in its Level III documentation DSI-7000 (2005; `products`, \
     `params`). The corpus products 39, 42, 53 and 83 agree with them \
     (thresholds, contours against the echo tops product 41 of the same \
     volume, window, storm ID, elevation bit map, edit times); the Weak Echo \
     Region's geometry is observed. DSI-7000 also gives products 43-46 an \
     alert category in halfword 51 (for products generated by an alert); \
     the KFTG 1994 products carry their window azimuth there, so it is not \
     decoded by name.",
    "**Archive oddities seen in the NCEI walk** (not corpus files): 47 of \
     the 11 829 KTLX 2001-05-03 products have block offsets or dividers that \
     do not match their data, and 3 of its standalone product 102 messages \
     fail (the one examined carries 4 extra bytes before halfword 54); one of \
     its products 103 has zero offsets before a page block. Of the 69 010 \
     products of 20 archive days of 1992-1995 decoded, 45 fail: 32 have \
     block offsets that name no block (31 of the 7 600 KFTG 1994-09-30 \
     products and a KLOT 1994-11-06 product 100), 7 stand-alone page blocks \
     of products 100 and 101 (KLOT, KCAE, KIWA 1994) break off, some holding \
     another product's bytes, 5 rasters or radials of 1994 run past their \
     layer, and a KLWX 1992-05-08 product 56 has text for its offsets. Every \
     other product of these days decodes, including the 1993-1994 products \
     whose tabular offset names the end of the message.",
];

/// Where decoded physical values follow the ICD and differ from MetPy 1.7.1
/// `map_data`, as asserted in `tests/radial_generic.rs`. Rendered into the
/// "Differences from MetPy" section of the document.
const METPY_DIFFERENCES: &[&str] = &[
    "**Product 138 (DSP).** ICD 2620001AD Figure 3-6 sheet 6 Note 1: data \
     level 0 is no accumulation and levels 1-255 are accumulations in even \
     increments, level 1 being the first non-zero one; halfword 31 is the \
     minimum (0) and halfword 32 the increment in 0.01 in. The decoder gives \
     level `N` the value `(hw31 + N * hw32) / 100` in (level 0 is 0 in). \
     MetPy's `DigitalStormPrecipMapper` masks levels 0 and 1 and maps level \
     `N >= 2` to `(N - 2) * hw32 / 100`, two increments lower. In the three \
     corpus files halfword 47 (maximum accumulation: 4.38, 2.89 and 0 in) is \
     within one increment of the decoded maximum (4.38, 2.90 and 0 in) and not \
     of MetPy's (4.34 in, 2.86 in, and no value with every bin masked). The test \
     `product_138_follows_the_icd_and_differs_from_metpy_as_documented` \
     asserts MetPy's summary is exactly that shift of the decoded values.",
    "**Categorical products** (34, 113, 165, 177). The decoder returns \
     `Level::Class` for each class and NaN from `values`; MetPy returns \
     numbers: the class index `N / 10` (165, 177), the level (113, read from \
     threshold halfwords that hold it) or 0 (34, whose threshold halfwords are \
     all zero). The tests reproduce MetPy's numbers from the decoded classes.",
    "**Correlation coefficient halfwords** (161, 167 halfwords 47-48). The ICD \
     scales them by 300 (`params` gives `x / 300`); MetPy multiplies by \
     0.00333. `tests/params.rs` compares through that factor.",
    "**Supplemental scan** (halfword 50 bits 0-4). Table V Note 24 gives 1 = \
     SAILS and 2 = MRLE; real products carry 2 on SAILS cuts (KTLX \
     2026-06-22: code 2 on the three extra 0.5 degree cuts of each VCP 212 \
     volume; KDDC 2020-08-17, whose General Status Message lists SAILS and \
     not MRLE). The decoder follows the products, as MetPy does.",
    "**Cross section end points** (products 50 and 51). Table V (2620001H) \
     puts them in halfwords 47-50, where the KLOT and KMLB 1994 products \
     print them; MetPy reads them from halfwords 27, 28, 30 and 47. \
     `tests/params.rs` asserts MetPy's values are those halfwords.",
];

/// Where the volume differs from Py-ART 2.3.0 `read_nexrad_level3`, as
/// asserted in `tests/pyart.rs`. Rendered into the "Differences from Py-ART"
/// section of the document.
const PYART_DIFFERENCES: &[&str] = &[
    "**Azimuth, range, elevation, time.** Py-ART reports the radial start \
     angle (the volume the centre, and the start as `level3_start_angle`), \
     starts range at 0 and steps it by the packet's display scale factor \
     (999 for a 1 km bin), gives elevation 0 to volume products and TDWR \
     products, and ray time 0 where the volume adds the halfword 50 delay.",
    "**Odd bin counts.** Py-ART keeps the pad byte of packet 16 radials with \
     an odd number of bins as one more gate (KTLX 2013 N1Q, N2Q, NBQ, N3U).",
    "**Values.** Every value equal (the SHA-256 of Py-ART's `float32` field) \
     for 94 of the 112 files Py-ART reads (including DSP, \
     where Py-ART agrees with the ICD reading and not with MetPy). Py-ART's \
     DHR (32) values are one increment above the ICD's; it masks a DPR (176) \
     rate of 0; its spectrum width 155 maps levels from 2 like reflectivity; \
     and it gives 170 and 172-175 in inches, where the ICD unit is 0.01 in \
     (and 174, 175 a value at every gate).",
];

/// Decode outcome of one manifest file.
struct FileOutcome {
    id: String,
    /// Product code: the golden product code, or the `product:` tag for files
    /// without a Product Description Block.
    product_code: Option<i16>,
    /// Three-character AWIPS product ID from the `awips:` tag (e.g. `N0Q`).
    awips_product: Option<String>,
    /// MetPy 1.7.1 status from the golden JSON: `ok`, `partial` or `unsupported`.
    metpy: String,
    result: Outcome,
}

enum Outcome {
    Product(ProductSummary),
    Text,
    GeneralStatus { message_code: i16 },
    Failed(String),
}

#[derive(Default)]
struct ProductSummary {
    /// Decoded packets per code, including nested SCIT contents.
    packets: BTreeMap<u16, usize>,
    /// Codes left as `Packet::Unknown`.
    unknown: BTreeSet<u16>,
    /// Generic component types left as `GenericComponent::Undecoded`.
    undecoded_components: BTreeSet<i32>,
    /// Packets whose variant is not the one its code dispatches to.
    wrong_variants: Vec<String>,
    /// Data level encoding name, when the product has one.
    levels: Option<&'static str>,
    /// Blocks other than the Product Symbology Block.
    blocks: BTreeSet<&'static str>,
    /// Golden check failures (product code, packet codes present).
    golden_mismatches: Vec<String>,
}

fn outcomes() -> &'static [FileOutcome] {
    static OUTCOMES: OnceLock<Vec<FileOutcome>> = OnceLock::new();
    OUTCOMES.get_or_init(|| common::level3_manifest().iter().map(decode_entry).collect())
}

fn decode_entry(entry: &Entry) -> FileOutcome {
    let golden = entry.golden();
    let framing = golden.get("framing");
    let golden_code = golden.get("product_code").as_i64();
    let product_code = golden_code
        .or_else(|| entry.tag("product").and_then(|t| t.parse().ok()))
        .map(|c| i16::try_from(c).unwrap());
    let result = match decode_message(&entry.bytes()) {
        Ok(Level3Message::Product(product)) => {
            let mut summary = summarize(&product);
            if golden_code != Some(i64::from(product.description.product_code)) {
                summary.golden_mismatches.push(format!(
                    "product code {} decoded, golden {golden_code:?}",
                    product.description.product_code
                ));
            }
            let found: Vec<u16> = summary.packets.keys().copied().collect();
            let golden_codes = common::golden_packet_codes(&golden);
            if found != golden_codes {
                summary.golden_mismatches.push(format!(
                    "packet codes found (including nested) {found:?}, golden {golden_codes:?}"
                ));
            }
            Outcome::Product(summary)
        }
        Ok(Level3Message::Text(_)) if framing.get("text_only").as_bool() == Some(true) => {
            Outcome::Text
        }
        Ok(Level3Message::GeneralStatus(status)) if golden_code.is_none() => {
            Outcome::GeneralStatus {
                message_code: status.message_header.code,
            }
        }
        Ok(_) => Outcome::Failed("message kind differs from the golden JSON".to_string()),
        Err(e) => Outcome::Failed(e.to_string()),
    };
    FileOutcome {
        id: entry.id.clone(),
        product_code,
        awips_product: entry
            .tag("awips")
            .map(|a| a.chars().take(3).collect::<String>()),
        metpy: golden.get("metpy").as_str().unwrap_or("?").to_string(),
        result,
    }
}

fn summarize(product: &Level3Product) -> ProductSummary {
    let mut summary = ProductSummary {
        levels: DataLevels::from_description(&product.description)
            .map(|levels| encoding_name(levels.encoding())),
        ..ProductSummary::default()
    };
    if let Some(symbology) = &product.symbology {
        for layer in &symbology.layers {
            walk(layer, &mut summary);
        }
    }
    if let Some(graphic) = &product.graphic {
        summary.blocks.insert(match graphic.layout {
            GraphicLayout::Pages => "graphic alphanumeric block",
            GraphicLayout::CellTrend => "cell trend data",
            other => panic!("graphic layout {other:?} has no summary name"),
        });
        for page in &graphic.pages {
            walk(&page.packets, &mut summary);
        }
    }
    if let Some(tabular) = &product.tabular {
        summary.blocks.insert(match tabular.layout {
            TabularLayout::Block => "tabular alphanumeric block",
            TabularLayout::StandAlone => "stand-alone tabular pages",
            TabularLayout::RadarCodedMessage => "radar coded message",
            other => panic!("tabular layout {other:?} has no summary name"),
        });
    }
    summary
}

/// Records `packets` and, recursively, the packets nested in SCIT packets.
fn walk(packets: &[Packet], summary: &mut ProductSummary) {
    for packet in packets {
        let code = packet.code();
        *summary.packets.entry(code).or_default() += 1;
        let variant = variant(packet);
        let expected = DISPATCH
            .iter()
            .find(|(c, ..)| *c == code)
            .and_then(|(_, _, v)| *v);
        if variant == "Unknown" {
            summary.unknown.insert(code);
        } else if Some(variant) != expected {
            summary.wrong_variants.push(format!(
                "packet {} decoded as {variant}, expected {expected:?}",
                code_label(code)
            ));
        }
        match packet {
            Packet::Symbol(SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested)) => {
                walk(nested, summary);
            }
            Packet::Generic(generic) => {
                for component in &generic.components {
                    if let GenericComponent::Undecoded { kind, .. } = component {
                        summary.undecoded_components.insert(*kind);
                    }
                }
            }
            _ => {}
        }
    }
}

fn variant(packet: &Packet) -> &'static str {
    match packet {
        Packet::Radial(_) => "Radial",
        Packet::Raster(_) => "Raster",
        Packet::DigitalPrecip(_) => "DigitalPrecip",
        Packet::Generic(_) => "Generic",
        Packet::Text(_) => "Text",
        Packet::Symbol(_) => "Symbol",
        Packet::Vectors(_) => "Vectors",
        Packet::Contour(_) => "Contour",
        Packet::Irm(_) => "Irm",
        Packet::Unknown { .. } => "Unknown",
        _ => "other",
    }
}

fn encoding_name(encoding: &LevelEncoding) -> &'static str {
    match encoding {
        LevelEncoding::Thresholds(_) => "16 thresholds",
        LevelEncoding::Linear(_) => "linear",
        LevelEncoding::ScaleOffset { .. } => "scale/offset",
        LevelEncoding::Vil { .. } => "VIL linear/log",
        LevelEncoding::EchoTops { .. } => "echo tops",
        LevelEncoding::Classes(_) => "classes",
        LevelEncoding::Edr { .. } => "EDR",
        _ => "other",
    }
}

fn code_label(code: u16) -> String {
    if code > 0xFF {
        format!("0x{code:04X}")
    } else {
        code.to_string()
    }
}

#[test]
fn every_corpus_file_decodes_without_unknown_packets() {
    let outcomes = outcomes();
    assert!(
        !outcomes.is_empty(),
        "the Level III manifest lists no files"
    );
    let mut failures = Vec::new();
    let (mut products, mut packets) = (0, 0);
    for file in outcomes {
        let mut problems = Vec::new();
        match &file.result {
            Outcome::Product(summary) => {
                products += 1;
                packets += summary.packets.values().sum::<usize>();
                if !summary.unknown.is_empty() {
                    let codes: Vec<String> =
                        summary.unknown.iter().map(|&c| code_label(c)).collect();
                    problems.push(format!(
                        "Packet::Unknown left for codes {}",
                        codes.join(", ")
                    ));
                }
                if !summary.undecoded_components.is_empty() {
                    problems.push(format!(
                        "generic components left undecoded: types {:?}",
                        summary.undecoded_components
                    ));
                }
                problems.extend(summary.wrong_variants.iter().cloned());
                problems.extend(summary.golden_mismatches.iter().cloned());
            }
            Outcome::Text | Outcome::GeneralStatus { .. } => {}
            Outcome::Failed(error) => problems.push(format!("decode failed: {error}")),
        }
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", file.id, problems.join("\n    ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} files are not fully decoded:\n{}",
        failures.len(),
        outcomes.len(),
        failures.join("\n")
    );
    assert!(products > 0 && packets > 0);
    eprintln!(
        "{} files: {products} products with {packets} packets decoded, no Packet::Unknown",
        outcomes.len()
    );
}

#[test]
fn coverage_document_is_current() {
    let rendered = render(outcomes());
    let path = common::testdata_dir().join("../docs/level3/coverage.md");
    if std::env::var_os(WRITE_ENV).is_some() {
        std::fs::write(&path, &rendered).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n");
    if committed != rendered {
        let line = committed
            .lines()
            .zip(rendered.lines())
            .position(|(a, b)| a != b)
            .map_or_else(
                || committed.lines().count().min(rendered.lines().count()) + 1,
                |i| i + 1,
            );
        panic!(
            "docs/level3/coverage.md is out of date (first difference at line {line}); \
             regenerate it with \
             `{WRITE_ENV}=1 cargo test -p recast-radar-io-level3 --test coverage`"
        );
    }
}

/// Status of a set of files of one product code.
fn status(files: &[&FileOutcome]) -> String {
    let mut statuses = BTreeSet::new();
    for file in files {
        statuses.insert(match &file.result {
            Outcome::Product(s)
                if s.unknown.is_empty()
                    && s.undecoded_components.is_empty()
                    && s.wrong_variants.is_empty()
                    && s.golden_mismatches.is_empty() =>
            {
                "decoded".to_string()
            }
            Outcome::Product(s) if !s.unknown.is_empty() => {
                let codes: Vec<String> = s.unknown.iter().map(|&c| code_label(c)).collect();
                format!("**Unknown packets {}**", codes.join(", "))
            }
            Outcome::Product(_) => "**differs from golden**".to_string(),
            Outcome::Text => "decoded text (`Level3Message::Text`)".to_string(),
            Outcome::GeneralStatus { .. } => {
                "decoded status (`Level3Message::GeneralStatus`)".to_string()
            }
            Outcome::Failed(error) => format!("**error: {error}**"),
        });
    }
    statuses.into_iter().collect::<Vec<_>>().join("; ")
}

/// A file id, marked when MetPy could not fully read it.
fn file_label(file: &FileOutcome) -> String {
    match file.metpy.as_str() {
        "ok" => format!("`{}`", file.id),
        "partial" => format!("`{}` (MetPy: default metadata)", file.id),
        "unsupported" => format!("`{}` (MetPy: unsupported)", file.id),
        other => format!("`{}` (MetPy: {other})", file.id),
    }
}

fn join_set<T: Ord>(items: impl IntoIterator<Item = T>, label: impl Fn(&T) -> String) -> String {
    let set: BTreeSet<T> = items.into_iter().collect();
    if set.is_empty() {
        return "—".to_string();
    }
    set.iter().map(label).collect::<Vec<_>>().join(", ")
}

fn render(outcomes: &[FileOutcome]) -> String {
    let mut by_product: BTreeMap<i16, Vec<&FileOutcome>> = BTreeMap::new();
    let mut messages: BTreeMap<i16, Vec<&FileOutcome>> = BTreeMap::new();
    for file in outcomes {
        match (&file.result, file.product_code) {
            (Outcome::GeneralStatus { message_code }, None) => {
                messages.entry(*message_code).or_default().push(file);
            }
            (_, Some(code)) => by_product.entry(code).or_default().push(file),
            (_, None) => messages.entry(0).or_default().push(file),
        }
    }
    let decoded_products = outcomes
        .iter()
        .filter(|f| matches!(f.result, Outcome::Product(_)))
        .count();
    let mut packet_files: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    for file in outcomes {
        if let Outcome::Product(summary) = &file.result {
            for (&code, &count) in &summary.packets {
                let entry = packet_files.entry(code).or_default();
                entry.0 += 1;
                entry.1 += count;
            }
        }
    }

    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "# Level III corpus coverage").unwrap();
    writeln!(w).unwrap();
    writeln!(
        w,
        "Generated by `crates/recast-radar-io-level3/tests/coverage.rs` from \
         `testdata/level3/manifest.toml` and `testdata/level3/golden/`; do not edit \
         by hand. The test fails when this file is out of date. Regenerate with \
         `{WRITE_ENV}=1 cargo test -p recast-radar-io-level3 --test coverage`."
    )
    .unwrap();
    writeln!(w).unwrap();
    writeln!(
        w,
        "Corpus files: {}. Decoded products: {decoded_products} files, {} product \
         codes. General Status Messages (no Product Description Block): {}. \
         Plain-text messages: {}.",
        outcomes.len(),
        by_product
            .values()
            .filter(|files| files
                .iter()
                .any(|f| matches!(f.result, Outcome::Product(_))))
            .count(),
        outcomes
            .iter()
            .filter(|f| matches!(f.result, Outcome::GeneralStatus { .. }))
            .count(),
        outcomes
            .iter()
            .filter(|f| matches!(f.result, Outcome::Text))
            .count(),
    )
    .unwrap();
    writeln!(w).unwrap();
    writeln!(w, "## Status and checks").unwrap();
    writeln!(w).unwrap();
    for line in [
        "- **decoded**: `decode_product` returns the product and every display \
         packet (symbology layers, graphic pages, product 62 cell trend data and \
         packets nested in SCIT packets 23/24) decodes to its family's typed \
         variant: no `Packet::Unknown`, no `GenericComponent::Undecoded`, and \
         the packet codes found equal the golden ICD walker's.",
        "- **decoded text** / **decoded status**: the golden JSON marks the file \
         as a plain-text message or a message without a Product Description \
         Block, and `decode_message` returns `Level3Message::Text` or \
         `Level3Message::GeneralStatus` (`decode_product` returns \
         `Level3Error::TextOnly` or `Level3Error::NotAProduct`). \
         `tests/messages.rs` compares every General Status Message field and \
         the text with the files' bytes and with MetPy 1.7.1.",
        "- **Values**: `tests/framing.rs` compares headers, blocks and packet \
         codes with the golden JSON for every file. The family tests \
         (`radial_generic.rs`, `raster.rs`, `symbols.rs`, `text_vectors.rs`) \
         compare decoded values with MetPy 1.7.1 `Level3File` output where \
         MetPy reads the file (raw-level SHA-256 and histograms, physical \
         min/max/mean within 1e-4 relative, packet contents), and check files \
         MetPy cannot read against their own header fields per the ICD. Files \
         MetPy reads only with default product metadata or not at all are \
         marked in the Files column.",
        "- **Physical values**: for every data packet MetPy maps (radial 16 and \
         0xAF1F, raster 0xBA07, packet 17 and generic 28 radial components), \
         the `f32` values from `DataLevels::for_packet` and the packets' \
         `values` methods have MetPy's finite and masked counts and \
         min/max/mean within 1e-4 relative, except where the ICD and MetPy \
         differ (see Differences from MetPy), where the documented difference \
         is asserted instead. MetPy maps no physical values for packet 18 and \
         product 197; packet 18 levels are checked level by level against the \
         8-level rate code of ICD 2620003AE section 30.2.1 (`tests/raster.rs`).",
        "- **Volume** (`read_level3_volume`, `tests/volume.rs`): every data \
         array of the 194 files MetPy reads becomes one sweep (248 sweeps: 93 \
         packet 16, 58 0xAF1F, 51 0xBA07, 4 packet 17, 40 packet 18, 2 \
         generic), packed in its stored width with the golden raw SHA-256 \
         (for the Weak Echo Region, the SHA-256 of its window taken from \
         MetPy's raster, which holds every nonzero raster cell), every gate resolving to its level's meaning; 16-level, VIL and echo \
         top fields keep range folding and the topped bit through a level \
         table. The DPA arrays sit on the HRAP grid and the rate arrays on the \
         national 1/4 LFM grid (`hrap`): box latitudes and longitudes equal \
         pyproj's, the accumulation placement fits the coverage mask of 47 \
         sites and the rate placement the ND boxes of 28 arrays from 26 sites \
         (`tools/level3_dpa_golden.py`). The radar coded message's intensity \
         grid is a sweep on the national 1/16 LFM grid, whose placement six \
         messages fix against the STI, TVS and hybrid scan products of the \
         same volume (`tools/level3_lfm_golden.py`). The products of 1993-1995 \
         check the geometry of the windows (43-46), the cross sections (50, \
         51), combined shear (87) and the Weak Echo Region (53) against their \
         own annotation or the base reflectivity of the same volume (see \
         Known gaps, Geometry). `tests/pyart.rs` compares 112 files with \
         Py-ART 2.3.0: every start azimuth of all 112 and every value of 94 \
         (see Differences from Py-ART).",
        "- **Product dependent halfwords** (`ProductDescription::parameters`, \
         `tests/params.rs`): 1035 values of 69 products equal MetPy's \
         `Level3File.metadata`, the Velocity Azimuth Display (84) wind \
         equals the fit the product prints, and the halfwords of products 39, \
         42, 53, 74 and 83, whose Table V is the 1990s one of DSI-7000, agree \
         with the products' own content (maximum reflectivity and echo top, \
         contour interval, window centre, storm ID, elevation bit map, edit \
         times).",
        "- **Radar coded message** (`Level3Product::radar_coded_message`, \
         `tests/rcm.rs`): every group of the 6 corpus messages (4 products 74 \
         and the messages inside 2 products 83) equals a separate Python \
         reading of Appendix B by the same author (`tools/level3_rcm_golden.py`; \
         see Known gaps, Same-author goldens); `/NI` equals \
         the boxes above level 0. The volume's intensity grid equals that \
         reading's grid (`tests/volume.rs`).",
        "- **Radar Coded Message (Unedited)** (product 83, `IRM`, DSI-7000 \
         Figure 3-22; `packets::irm`, `tests/rcm.rs`): its Tabular \
         Alphanumeric Block holds the radar coded message of its volume, \
         packet 32 is that message's Part A grid, packets 31, 15 and 2 its \
         storm centroids, and packet 30 its LFM grid parameters (box size and \
         offsets checked against the national grid; see Known gaps). In 188 \
         IRM/RCM pairs of four NCEI days (1996-2000) the text is the product \
         74's and the grid equals the separate reading of its Part A; all \
         829 IRM products of those days decode. The KLOT 1994 product 83 \
         carries no message (its tabular offset names the end of the \
         message); its grid equals the separate reading of its packet 32.",
        "- **Storm attribute tables** (`tables`, `tests/tables.rs`): the 520 rows \
         of the corpus STI, hail, mesocyclone (60), TVS, MDA and combined \
         attribute tables of the SCIT-era layouts (33 tables), and the 84 rows \
         of the 1993-1997 layouts (12 legacy STI, hail, TVS and combined \
         attribute tables), on products 35-39, 58-61, 101-104 and 141, equal \
         a separate same-author reading of MetPy's page text, or of the page \
         block of the stand-alone products MetPy cannot read \
         (`tools/level3_tables_golden.py`). Every table of five NCEI days \
         (KILX 1996, KLZK 1997, KTLX 1999, KFWS 2000, KSGF 2003) parses with \
         no unparsed line (`examples/level3_tables.rs`).",
        "- **Display packet records** (`Level3Product::display_records`, \
         `records`, `tests/records.rs`): every packet of every symbology layer \
         and graphic page that is not a data array (text, symbols, vectors, \
         contours, cell trends, SCIT data, generic components, IRM packets) \
         becomes one text record the volume carries as \
         `level3_display_packets`; on the 265 corpus products the records \
         follow the golden walker's packets and nesting and give back every \
         value of the decoded packets, and a data array's record names the \
         sweep holding it. The FM301 view with `Passthrough::All` writes \
         every untyped volume and sweep value of every corpus volume \
         verbatim.",
        "- **Router**: `recast_radar_io` sniffs all 276 Level III corpus files \
         as Level III and routes them to `read_level3_volume`, and sniffs \
         neither a Level II LDM record whose size word starts like a General \
         Status Message nor the WMO text bulletin tested (`SRUS55`, XML) as \
         Level III. A text bulletin under a `NOUS` heading sniffs as Level III \
         whatever its content, by MetPy's rule, and comes back as a decoded \
         text message; a product without a data array, a General Status \
         Message and a text message come back decoded in \
         `IoError::Level3WithoutVolume` (`recast-radar-io` \
         `tests/level3_router.rs`).",
        "- **Allocation limits** (`tests/product_budget.rs`): decompressed \
         data is capped at 16 MiB, and everything decoding a product \
         allocates from it (data levels at one byte a cell, packets, list \
         elements, strings) is charged, before it is allocated, to one \
         budget of `MAX_PRODUCT_DECODED_BYTES` (32 MiB) for the whole \
         product. `to_volume` refuses data arrays whose sweeps an estimate \
         made before building any puts above `volume::MAX_VOLUME_BYTES` (64 \
         MiB), or more than 1 024 sweeps. The tests repeat real layers \
         (DPA, NCR, N0R, VWP) past each limit, and refuse at the budget a \
         bzip2 product of under 64 KiB that expands to 5.5 MB of packet 17 \
         arrays. Measured with a counting allocator on generated products \
         (2026-09-25; the program is not committed): the products of the \
         review, 1 462 bytes of bzip2 holding 113 packet 17 arrays of 4096 \
         x 4096 and 246 bytes holding 14 0xBA07 rasters, which allocated \
         1.9 GB and 251 MB, and those found since, 764 bytes of empty \
         packets (923 MB) and 367 bytes of 1 800 DPA arrays (571 MB as a \
         volume), now peak at 33-57 MB in `read_supported_volume_bytes`. \
         The largest peak found for the fuzz target's whole path (decode, \
         volume, FM301 view) is 187 MB, for 140 rasters of 464 x 464 cells \
         in a 264-byte product. No committed real product allocates more \
         than 2.7 MB, besides the bzip2 decoder's reused working memory \
         (about 7 MB). Parsing a radar coded message's text \
         (`Level3Product::radar_coded_message`, which `to_volume` calls for \
         product 74) is charged the same way to a limit of its own, \
         `rcm::MAX_RCM_PARSED_BYTES` (4 MiB), starting with its copy of the \
         text, because each comma-separated group, however short, becomes \
         several allocations: before it, the volume of a 760-byte bzip2 \
         product 74 holding 13.8 MB of one-character groups allocated 504 \
         MB, and one of 1 005 bytes holding 14 MB of intensity groups 430 \
         MB (the review's probes, built from the real BOX 2022-05-16 20:16 \
         product 74); both are now refused at the text copy and peak at 47 \
         MB in `read_supported_volume_bytes`, as their decode alone does \
         (the decompressed message, and the 14 MB of text held as data and \
         as records). Texts of 1 and 3.9 MB built the same way from \
         intensity groups, centroids, TVS groups, remarks, words or \
         slashes, which exercise each list of the parse (bzip2 products of \
         275 to 567 bytes), peak at 16.3 MB at most. These peaks are \
         measured after an earlier product has allocated the bzip2 \
         decoder's reused working memory; a fresh process adds it (7.4 MB: \
         54.1 and 54.6 MB for the review's probes, 23.7 MB at most for the \
         others). Replayed through the \
         AddressSanitizer build of the `level3` fuzz target with \
         `-rss_limit_mb=512` (its whole path: decode, volume, FM301 view, \
         radar coded message, VAD wind profile and every table reader), the \
         review's probes, which ran out of memory there before (651 and 600 \
         MB), peak at 142 MB of resident memory, and the others at 100 MB \
         at most. The test repeats the KTLX 2013 message's intensity groups \
         in place: 200 copies parse (every copy's runs equal the message's, \
         and the grid is the message's), 2 000 copies (1.4 MB of text) are \
         refused while parsing and 6 053 at the text copy. The 2 798 real \
         messages found (products 74 and 83 of 16 NCEI days of 1994-2008, \
         28 products 74 of 2022 from AWS, the committed ones) hold at most \
         4 016 bytes of text and allocate at most 25 KB while they parse; a \
         message reporting each of the 10 000 fine boxes as its own group, \
         the most Appendix B allows, allocates 1.0 MB.",
        "- **Fuzzing** (`fuzz/`, targets `level3` and `io-router`, \
         `fuzz/README.md`): two AddressSanitizer campaigns in the nexbench \
         container on 2026-09-24, seeded with every Level III corpus file, \
         45 minutes each (`level3` with three workers: 675 434 and 671 000 \
         inputs; `io-router`: 692 994 and 389 683), the second over the \
         decoder with the legacy tables and product 83: no crash, sanitizer \
         report, out-of-memory or timeout. A third, of 40 minutes on \
         2026-09-25 over the decoder with the products of 1993-2001 and the \
         display packet records (`level3` with three workers: 1 103 561 \
         inputs; `io-router`: 279 843), found no crash, sanitizer report or \
         out-of-memory; its one timeout input replays in under 70 ms (a \
         load spike in the shared container). A fourth, of 45 minutes on \
         2026-09-25 over the decoder with the DSI-7000 tables and the named \
         IRM grid parameters (`level3` with three workers: 1 260 592 \
         inputs; `io-router`: 392 505), started from the third's corpus, \
         found no crash, sanitizer report, out-of-memory or timeout. A \
         fifth, of 30 minutes on 2026-09-25 over the review fixes (Weak \
         Echo Region window, zlib framing values, surplus generic values, \
         sniff and router), started from the fourth's corpus with the SRUS \
         text bulletin and the Level II chunks as router inputs (`level3` \
         with three workers: 581 455 inputs; `io-router`: 209 252), found \
         no crash, sanitizer report or out-of-memory; its two timeout \
         inputs replay in under 30 ms with AddressSanitizer and in 3 ms on \
         the stable replay (the shared container was loaded, and its clock \
         stepped back during the replay: one run reported -1629 ms). A \
         sixth, of 30 minutes on 2026-09-25 over the decode budget, the \
         volume byte limit and the ORPG generic layout, started from the \
         fifth's corpus plus seven generated hostile products (not \
         committed), ran `level3` with `-rss_limit_mb=512` \
         (AddressSanitizer quarantine 32 MB; three workers: 616 264 inputs; \
         `io-router`: 227 467) and found no crash, sanitizer report or \
         out-of-memory; its two timeout inputs replay in under 160 ms with \
         AddressSanitizer and 9 ms on the stable replay. A seventh, of 20 \
         minutes on 2026-09-25 over the radar coded message parse limit and \
         the enums marked `non_exhaustive`, started from the sixth's corpus \
         with 19 generated radar coded message products among the seeds \
         (the review's three probes and 16 built the same way: bzip2 \
         products of 275 to 1 527 bytes expanding to 1-14 MB of groups; not \
         committed), ran `level3` with `-rss_limit_mb=512` (three workers: \
         501 487 inputs; `io-router`: 182 714) and found no crash, \
         sanitizer report, out-of-memory or timeout. An earlier run without \
         sanitizer found one crash (regression \
         `fuzz-level3-rcm-centroid-non-ascii`).",
        "- **Data levels**: the encoding `levels::DataLevels::from_description` \
         selects for the product's files (— when the product has no data levels).",
        "- **VAD Wind Profile** (product 48): `vwp::VadWindProfile` gives the \
         tabular winds, the time-height display winds and the adaptable \
         parameters. `tests/vwp.rs` checks them on every product 48 file (the 6 \
         here and KBMX 1998 in `testdata/other`; the KLOT 1993 one has no \
         tabular block and only its display winds are checked) against the page text, every \
         display barb, Product Description Block halfwords 47-49, and the \
         recorded output of `recast_radar_io_nexrad::level3_vwp`, the decoder it \
         replaced (`tests/level3_vwp/`).",
    ] {
        writeln!(w, "{line}").unwrap();
    }
    writeln!(w).unwrap();

    writeln!(w, "## Products").unwrap();
    writeln!(w).unwrap();
    writeln!(
        w,
        "| Code | Mnemonic | AWIPS IDs | Name | Status | Contents | Data levels | Files |"
    )
    .unwrap();
    writeln!(w, "|---:|---|---|---|---|---|---|---|").unwrap();
    for (code, files) in &by_product {
        let info = product_info(*code);
        let mnemonic = info
            .map(|i| i.mnemonic)
            .filter(|m| !m.is_empty())
            .unwrap_or("—");
        let name = info.map_or("(not in the product table)", |i| i.name);
        let awips = join_set(
            files.iter().filter_map(|f| f.awips_product.clone()),
            String::clone,
        );
        let summaries: Vec<&ProductSummary> = files
            .iter()
            .filter_map(|f| match &f.result {
                Outcome::Product(s) => Some(s),
                _ => None,
            })
            .collect();
        let mut contents = join_set(
            summaries.iter().flat_map(|s| s.packets.keys().copied()),
            |&c| code_label(c),
        );
        let blocks = join_set(
            summaries.iter().flat_map(|s| s.blocks.iter().copied()),
            |b| (*b).to_string(),
        );
        if contents == "—" {
            contents = blocks;
        } else if blocks != "—" {
            contents = format!("{contents}; {blocks}");
        }
        let levels = join_set(summaries.iter().filter_map(|s| s.levels), |l| {
            (*l).to_string()
        });
        let file_list = files
            .iter()
            .map(|f| file_label(f))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            w,
            "| {code} | {mnemonic} | {awips} | {name} | {} | {contents} | {levels} | {file_list} |",
            status(files)
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    writeln!(w, "## Messages without a product code").unwrap();
    writeln!(w).unwrap();
    writeln!(w, "| Message code | AWIPS IDs | Status | Files |").unwrap();
    writeln!(w, "|---:|---|---|---|").unwrap();
    for (code, files) in &messages {
        let awips = join_set(
            files.iter().filter_map(|f| f.awips_product.clone()),
            String::clone,
        );
        let file_list = files
            .iter()
            .map(|f| file_label(f))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(w, "| {code} | {awips} | {} | {file_list} |", status(files)).unwrap();
    }
    writeln!(w).unwrap();

    writeln!(w, "## Packets").unwrap();
    writeln!(w).unwrap();
    writeln!(
        w,
        "Every packet code the dispatcher routes (`src/packets/mod.rs`), with the \
         corpus files containing it and the packets decoded (nested SCIT contents \
         included)."
    )
    .unwrap();
    writeln!(w).unwrap();
    writeln!(w, "| Code | Module | Decodes to | Files | Packets |").unwrap();
    writeln!(w, "|---:|---|---|---:|---:|").unwrap();
    for (code, module, variant) in DISPATCH {
        let (files, packets) = packet_files.get(code).copied().unwrap_or_default();
        let decodes_to = match (variant, files) {
            (Some(v), 0) => format!("`Packet::{v}` (no real sample)"),
            (Some(v), _) => format!("`Packet::{v}`"),
            (None, _) => "`Packet::Unknown` (no decoder)".to_string(),
        };
        writeln!(
            w,
            "| {} | `{module}` | {decodes_to} | {files} | {packets} |",
            code_label(*code)
        )
        .unwrap();
    }
    for (code, (files, packets)) in &packet_files {
        if !DISPATCH.iter().any(|(c, ..)| c == code) {
            writeln!(
                w,
                "| {} | — | **not dispatched** | {files} | {packets} |",
                code_label(*code)
            )
            .unwrap();
        }
    }
    writeln!(w).unwrap();

    writeln!(w, "## Differences from MetPy").unwrap();
    writeln!(w).unwrap();
    for difference in METPY_DIFFERENCES {
        writeln!(w, "- {difference}").unwrap();
    }
    writeln!(w).unwrap();

    writeln!(w, "## Differences from Py-ART").unwrap();
    writeln!(w).unwrap();
    for difference in PYART_DIFFERENCES {
        writeln!(w, "- {difference}").unwrap();
    }
    writeln!(w).unwrap();

    writeln!(w, "## Known gaps").unwrap();
    writeln!(w).unwrap();
    for gap in KNOWN_GAPS {
        writeln!(w, "- {gap}").unwrap();
    }
    out
}
