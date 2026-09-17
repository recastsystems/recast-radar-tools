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
//! Product Description Block.
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
    GraphicLayout, Level3Error, Level3Product, Packet, TabularLayout, decode_product, product_info,
};

/// Environment variable that makes the test write `docs/level3/coverage.md`.
const WRITE_ENV: &str = "LEVEL3_WRITE_COVERAGE";

/// Every packet code the dispatcher routes to a family module, with the module
/// and the `Packet` variant its packets decode to.
///
/// All four L3.3 families (radial, raster, symbols, text) are merged, so no
/// packet code is exempt from the no-`Unknown` assertion. Packet 29 has no
/// decoder (no real sample exists and its XDR layout is ambiguous, see
/// `packets/generic.rs`); no corpus file contains it, so adding one makes this
/// test fail until it is decoded.
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
    (29, "generic", None),
    (33, "raster", Some("Raster")),
    (0x0802, "contour", Some("Contour")),
    (0x0E03, "contour", Some("Contour")),
    (0x3501, "contour", Some("Contour")),
    (0xAF1F, "radial", Some("Radial")),
    (0xBA07, "raster", Some("Raster")),
    (0xBA0F, "raster", Some("Raster")),
];

/// Gaps that the corpus walk cannot show, from the L3.3 family reports and
/// spec section 4.5. Rendered into the "Known gaps" section of the document.
const KNOWN_GAPS: &[&str] = &[
    "**Core data model.** Spec section 4.5 asks for radial and raster products \
     in the core `Volume`/`Sweep` model (one sweep). That conversion does not \
     exist yet. Products decode to the packet structs listed above; \
     `levels::DataLevels::for_packet` gives a data packet's level mapping, and \
     `RadialPacket::values`, `RasterGrid::values` and \
     `GenericRadialComponent::values` return its physical values as `f32` \
     (NaN without a value), with `level_at` and `DataLevels::levels` giving each \
     cell's `Level` (value, class, or flag such as missing or range folded).",
    "**Packets without a real sample.** 5 (vector arrow), 7 (unlinked vector, \
     no value), 9 (linked vector, uniform value), 26 (ETVS), 33 (digital \
     raster data array), 0xBA0F (raster data) and 0x3501 (unlinked contour \
     vectors) have decoders written from the ICD figures, but no public file \
     containing them was found (`docs/level3/reference.md` section 7), so they \
     are not tested against data.",
    "**Packet 29** (generic data with external data description) has no \
     decoder and stays `Packet::Unknown`: no real sample exists and the spare \
     fields of Figure E-1b are ambiguous in XDR.",
    "**Generic components.** Packet 28 decodes radial (type 1) and text \
     (type 4) components. Grid (2), area (3), table (5) and event (6) \
     components have no real sample and are kept as \
     `GenericComponent::Undecoded`, which also holds any components after \
     them. Parameter lists with two or more entries follow a reading of real \
     bytes, but no corpus file has one.",
    "**Data levels.** TDWR product 184 has no data level mapping \
     (`DataLevels::from_description` returns `None`): 2620063E does not give \
     its 256-level encoding. No corpus file has product 184. Packet 18 \
     (precipitation rate data array, products 81 and 82) has no mapping either \
     (`DataLevels::for_packet` returns `None`): no halfword describes its 4-bit \
     levels, and MetPy maps none.",
    "**Text-only messages.** Plain-text messages (WMO heading `NOUS..`, e.g. \
     the Free Text Message) return `Level3Error::TextOnly` without their text.",
    "**General Status Message** (message code 2) returns \
     `Level3Error::NotAProduct`; its contents are not decoded.",
    "**Radar coded message** (product 74) is split into 70-character records \
     (`TabularAlphanumeric::pages`); the coded groups inside them (legacy ICD \
     Appendix B) are not decoded.",
    "**Graphic alphanumeric tables.** Storm attribute tables in Graphic \
     Alphanumeric Block pages decode as text packets 8 and vector packets 10 \
     in screen coordinates; they are not parsed into typed storm cell structs \
     (their layout differs by product and ICD build).",
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
    TextOnly,
    NotAProduct { message_code: i16 },
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
    let result = match decode_product(&entry.bytes()) {
        Ok(product) => {
            let mut summary = summarize(&product);
            if golden_code != Some(i64::from(product.description.product_code)) {
                summary.golden_mismatches.push(format!(
                    "product code {} decoded, golden {golden_code:?}",
                    product.description.product_code
                ));
            }
            let found: Vec<i64> = summary.packets.keys().map(|&c| i64::from(c)).collect();
            let golden_codes: Vec<i64> = golden
                .get("packet_codes")
                .items()
                .iter()
                .map(|c| c.int("packet code"))
                .collect();
            if found != golden_codes {
                summary.golden_mismatches.push(format!(
                    "packet codes found (including nested) {found:?}, golden {golden_codes:?}"
                ));
            }
            Outcome::Product(summary)
        }
        Err(Level3Error::TextOnly { .. }) if framing.get("text_only").as_bool() == Some(true) => {
            Outcome::TextOnly
        }
        Err(Level3Error::NotAProduct { code }) if golden_code.is_none() => {
            Outcome::NotAProduct { message_code: code }
        }
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
            Outcome::TextOnly | Outcome::NotAProduct { .. } => {}
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
            Outcome::TextOnly => "text only (`Level3Error::TextOnly`)".to_string(),
            Outcome::NotAProduct { .. } => "not a product (`Level3Error::NotAProduct`)".to_string(),
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
            (Outcome::NotAProduct { message_code }, None) => {
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
         codes. Messages without a Product Description Block: {}. Text-only \
         messages: {}.",
        outcomes.len(),
        by_product
            .values()
            .filter(|files| files
                .iter()
                .any(|f| matches!(f.result, Outcome::Product(_))))
            .count(),
        outcomes
            .iter()
            .filter(|f| matches!(f.result, Outcome::NotAProduct { .. }))
            .count(),
        outcomes
            .iter()
            .filter(|f| matches!(f.result, Outcome::TextOnly))
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
        "- **text only** / **not a product**: the golden JSON marks the file as a \
         plain-text message or a message without a Product Description Block, \
         and `decode_product` returns the matching error.",
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
         product 197.",
        "- **Data levels**: the encoding `levels::DataLevels::from_description` \
         selects for the product's files (— when the product has no data levels).",
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

    writeln!(w, "## Known gaps").unwrap();
    writeln!(w).unwrap();
    for gap in KNOWN_GAPS {
        writeln!(w, "- {gap}").unwrap();
    }
    out
}
