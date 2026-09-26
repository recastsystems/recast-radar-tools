//! Symbol packet family (codes 3, 4, 5, 11-15, 19-26) against the real Level III
//! corpus (`testdata/level3/manifest.toml`).
//!
//! Expected values come from three sources:
//!
//! 1. **ICD walker golden JSON** (`testdata/level3/golden/<id>.json`): which
//!    packet codes each file holds, and the codes nested in SCIT packets 23/24.
//! 2. **MetPy 1.7.1** `Level3File`, which reads every corpus file holding symbol
//!    packets but the product 83 (`tests/rcm.rs`). The golden JSON carries no symbol values, so [`METPY`] records,
//!    per file and packet code, the packet count, item count and FNV-1a 64 hash
//!    of a canonical text (format at [`canonical`]) built from MetPy's decoded
//!    packets by the script at the end of this file. [`decoded_values_match_metpy_spot_checks`]
//!    spells out some of the same values.
//! 3. **ICD field semantics** checked on the decoded values and the product's
//!    own Product Description Block: symbol counts against Table V halfwords
//!    (products 48, 58, 61), cell trend times against the volume scan time
//!    (product 62), and value ranges from Figures 3-13 to 3-15a.
//!
//! Corpus coverage: 33 files (products 48, 58, 59, 60, 61, 62, 83 and 141, 1995
//! to 2026) hold packets 3, 4, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24 and 25.
//! No real sample was found for packets 5 and 26 (`docs/level3/reference.md`
//! section 7); their decoders are not exercised here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use common::{Entry, Json};
use recast_radar_io_level3::packets::symbols::{
    CellTrend, Circle, HdaHail, PointFeatureKind, Position, StormId, TrendKind, WindBarb,
};
use recast_radar_io_level3::{GraphicLayout, Level3Product, Packet, SymbolPacket};

/// Packet codes of this family.
const FAMILY: [u16; 16] = [3, 4, 5, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 25, 26];

/// Family codes present in the corpus (union of the golden `packet_codes`).
const CORPUS_CODES: [u16; 14] = [3, 4, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 25];

/// Number of corpus files holding packets of this family.
const CORPUS_FILES: usize = 35;

/// Number of those MetPy reads: all but the unedited Radar Coded Messages
/// (product 83, storm IDs in packet 15), whose packets `tests/rcm.rs` checks
/// against the radar coded message of the same volume, and the KLOT 1993 VAD
/// Wind Profile, whose tabular offset names the end of its message (MetPy
/// raises; `tests/vwp.rs` checks its wind barbs).
const METPY_FILES: usize = 32;

fn is_family(code: u16) -> bool {
    FAMILY.contains(&code)
}

/// A top-level packet and where it was found: `sym<layer>`, `page<number>`, or
/// `trend` for product 62 cell trend data.
struct Located<'a> {
    location: String,
    packet: &'a Packet,
}

fn top_level_packets(product: &Level3Product) -> Vec<Located<'_>> {
    let mut out = Vec::new();
    if let Some(symbology) = &product.symbology {
        for (layer, packets) in symbology.layers.iter().enumerate() {
            out.extend(packets.iter().map(|packet| Located {
                location: format!("sym{layer}"),
                packet,
            }));
        }
    }
    if let Some(graphic) = &product.graphic {
        for page in &graphic.pages {
            let location = match graphic.layout {
                GraphicLayout::CellTrend => "trend".to_string(),
                _ => format!("page{}", page.number),
            };
            out.extend(page.packets.iter().map(|packet| Located {
                location: location.clone(),
                packet,
            }));
        }
    }
    out
}

/// Decoded symbol packets with `code`, top level then nested SCIT contents, in file order.
fn symbols(product: &Level3Product, code: u16) -> Vec<&SymbolPacket> {
    let mut out = Vec::new();
    for Located { packet, .. } in top_level_packets(product) {
        if let Packet::Symbol(symbol) = packet {
            if symbol.code() == code {
                out.push(symbol);
            }
            if let SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested) = symbol {
                out.extend(nested.iter().filter_map(|p| match p {
                    Packet::Symbol(s) if s.code() == code => Some(s),
                    _ => None,
                }));
            }
        }
    }
    out
}

/// Family codes among the golden walker's `packet_codes` (nested ones included).
fn golden_family_codes(golden: &Json) -> Vec<u16> {
    common::golden_packet_codes(golden)
        .into_iter()
        .filter(|&c| is_family(c))
        .collect()
}

/// Corpus files holding symbol packets that MetPy reads, decoded, with their
/// golden JSON.
fn symbol_files() -> Vec<(Entry, Json, Level3Product)> {
    let files: Vec<_> = common::level3_manifest()
        .into_iter()
        .filter_map(|entry| {
            let golden = entry.golden();
            if golden_family_codes(&golden).is_empty()
                || golden.get("metpy").as_str() == Some("unsupported")
            {
                return None;
            }
            let product = common::decode_golden_product(&entry, &golden).unwrap();
            Some((entry, golden, product))
        })
        .collect();
    assert_eq!(
        files.len(),
        METPY_FILES,
        "corpus files holding symbol packets that MetPy reads"
    );
    files
}

fn file<'a>(files: &'a [(Entry, Json, Level3Product)], id: &str) -> &'a Level3Product {
    &files
        .iter()
        .find(|(entry, ..)| entry.id == id)
        .unwrap_or_else(|| panic!("{id} not in the corpus"))
        .2
}

/// Product Description Block halfword `n` (1-based, ICD numbering) as INT*2.
fn halfword(product: &Level3Product, n: usize) -> i16 {
    product.description.halfword(n).unwrap() as i16
}

// ---------------------------------------------------------------------------------
// Decoding and structure
// ---------------------------------------------------------------------------------

#[test]
fn symbol_packets_decode_in_every_corpus_file() {
    let mut files_with_symbols = 0;
    let mut decoded_codes = BTreeMap::<u16, usize>::new();
    let mut failures = Vec::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = common::decode_golden_product(&entry, &golden) else {
            continue;
        };
        let mut problems = Vec::new();
        let mut found = false;
        for Located { location, packet } in top_level_packets(&product) {
            let code = packet.code();
            if !is_family(code) {
                continue;
            }
            found = true;
            let Packet::Symbol(symbol) = packet else {
                problems.push(format!("{location} packet {code} not decoded"));
                continue;
            };
            assert_eq!(symbol.code(), code);
            *decoded_codes.entry(code).or_default() += 1;
            if let SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested) = symbol {
                for inner in nested {
                    let inner_code = inner.code();
                    // Figure 3-14 sheet 3: SCIT data consists of packets 2, 6 and 25.
                    if ![2, 6, 25].contains(&inner_code) {
                        problems.push(format!("{location} SCIT packet holds packet {inner_code}"));
                    }
                    match inner {
                        Packet::Symbol(s) => *decoded_codes.entry(s.code()).or_default() += 1,
                        _ if is_family(inner_code) => problems
                            .push(format!("{location} nested packet {inner_code} not decoded")),
                        _ => {}
                    }
                }
            }
        }
        check_nested_codes(&product, &golden, &mut problems);
        if found == golden_family_codes(&golden).is_empty() {
            problems.push(format!("symbol packets found: {found}, golden disagrees"));
        }
        files_with_symbols += usize::from(found);
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", entry.id, problems.join("\n    ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(files_with_symbols, CORPUS_FILES);
    assert_eq!(
        decoded_codes.keys().copied().collect::<Vec<_>>(),
        CORPUS_CODES,
        "symbol packet codes decoded across the corpus"
    );
    eprintln!("{files_with_symbols} files; symbol packets decoded per code: {decoded_codes:?}");
}

/// Nested SCIT packet codes equal the golden walker's `blocks.symbology.nested`.
fn check_nested_codes(product: &Level3Product, golden: &Json, problems: &mut Vec<String>) {
    let mut decoded = Vec::new();
    if let Some(symbology) = &product.symbology {
        for (layer, packets) in symbology.layers.iter().enumerate() {
            for (index, packet) in packets.iter().enumerate() {
                if let Packet::Symbol(
                    SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested),
                ) = packet
                {
                    let codes: Vec<u16> = nested.iter().map(Packet::code).collect();
                    decoded.push((layer as i64, index as i64, packet.code(), codes));
                }
            }
        }
    }
    let golden_nested: Vec<(i64, i64, u16, Vec<u16>)> = golden
        .get("blocks")
        .get("symbology")
        .get("nested")
        .items()
        .iter()
        .map(|n| {
            (
                n.get("layer").int("layer"),
                n.get("index").int("index"),
                u16::try_from(n.get("code").int("code")).unwrap(),
                common::packet_codes(n.get("packets")),
            )
        })
        .collect();
    if decoded != golden_nested {
        problems.push(format!(
            "SCIT nested packets differ from golden:\n      decoded {decoded:?}\n      golden  {golden_nested:?}"
        ));
    }
}

// ---------------------------------------------------------------------------------
// Values against MetPy
// ---------------------------------------------------------------------------------

/// Item count and canonical text of one symbol packet. The canonical line of a
/// top-level packet is `<location> <code> <text>`, where text is:
///
/// - 3, 11, 25: `i,j,radius` per record, joined by `;`
/// - 4: `color_level,x,y,direction,speed` per record, joined by `;`
/// - 12, 13, 14, 26: `i,j` per record, joined by `;`
/// - 15: `i,j,id` per record, joined by `;`
/// - 19: `i,j,poh,posh,max_size` per record, joined by `;`
/// - 20: `i,j,type,attribute` per record (`i,j,type` for types 5-8, whose
///   attribute MetPy does not return), joined by `;`
/// - 21: `id i j`, then ` code:v,v,...` per trend with values oldest to latest
/// - 22: `t,t,...` oldest to latest
/// - 23, 24: the text of each nested packet 25, joined by `|` (nested packets 2
///   and 6 belong to the text and vector families)
///
/// Items: records (21: trend series; 22: times; 23, 24: nested circles).
/// Positions and radii are the stored integers (MetPy scales them by 1/4 in the
/// symbology block; the script divides the scale back out).
fn canonical(symbol: &SymbolPacket) -> (usize, String) {
    fn join<T>(items: &[T], f: impl Fn(&T) -> String) -> (usize, String) {
        let text: Vec<String> = items.iter().map(f).collect();
        (items.len(), text.join(";"))
    }
    fn circle(c: &Circle) -> String {
        format!("{},{},{}", c.i, c.j, c.radius)
    }
    fn position(p: &Position) -> String {
        format!("{},{}", p.i, p.j)
    }
    fn values(v: &[i16]) -> String {
        v.iter().map(i16::to_string).collect::<Vec<_>>().join(",")
    }
    match symbol {
        SymbolPacket::Mesocyclone(c)
        | SymbolPacket::CorrelatedShear(c)
        | SymbolPacket::StiCircles(c) => join(c, circle),
        SymbolPacket::WindBarbs(b) => join(b, |b| {
            format!(
                "{},{},{},{},{}",
                b.color_level, b.x, b.y, b.direction_deg, b.speed_kt
            )
        }),
        SymbolPacket::Tvs(p)
        | SymbolPacket::HailPositive(p)
        | SymbolPacket::HailProbable(p)
        | SymbolPacket::Etvs(p) => join(p, position),
        SymbolPacket::StormIds(s) => join(s, |s| format!("{},{},{}", s.i, s.j, s.id)),
        SymbolPacket::HdaHail(h) => join(h, |h| {
            format!(
                "{},{},{},{},{}",
                h.i, h.j, h.probability_of_hail, h.probability_of_severe_hail, h.max_hail_size_in
            )
        }),
        SymbolPacket::PointFeatures(f) => join(f, |f| match f.feature_type {
            5..=8 => format!("{},{},{}", f.i, f.j, f.feature_type),
            _ => format!("{},{},{},{}", f.i, f.j, f.feature_type, f.attribute),
        }),
        SymbolPacket::CellTrend(t) => {
            let mut text = format!("{} {} {}", t.id, t.i, t.j);
            for trend in &t.trends {
                write!(
                    text,
                    " {}:{}",
                    trend.code,
                    values(&trend.volumes.chronological())
                )
                .unwrap();
            }
            (t.trends.len(), text)
        }
        SymbolPacket::CellTrendTimes(times) => {
            let chronological = times.chronological();
            (chronological.len(), values(&chronological))
        }
        SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested) => {
            let circles: Vec<(usize, String)> = nested
                .iter()
                .filter_map(|p| match p {
                    Packet::Symbol(SymbolPacket::StiCircles(c)) => Some(join(c, circle)),
                    _ => None,
                })
                .collect();
            let count = circles.iter().map(|(n, _)| n).sum();
            let text: Vec<String> = circles.into_iter().map(|(_, t)| t).collect();
            (count, text.join("|"))
        }
        other => panic!("no canonical form for packet {}", other.code()),
    }
}

/// FNV-1a 64 of `lines` joined by `\n`.
fn fnv1a64(lines: &[String]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in lines.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// `(packet code, packets, items, FNV-1a 64 of the canonical lines)`.
type CodeSummary = (u16, usize, usize, u64);

/// Per file, the [`CodeSummary`] of each packet code in increasing code order,
/// generated from MetPy 1.7.1 by the script at the end of this file.
#[rustfmt::skip] // One generated row per file.
const METPY: &[(&str, &[CodeSummary])] = &[
    // GENERATED-METPY-START
    ("l3-fws-nhi-19950517-1323", &[(13, 1, 1, 0x7062ef27b35a1d71), (14, 1, 1, 0x136e89853c21f24c), (15, 2, 2, 0xe33a49e7f0a0ab75)]),
    ("l3-fws-nme-19950517-2316", &[(3, 1, 1, 0x1e84143bc2783014), (11, 1, 1, 0x1ad19b53447a0c5c), (15, 1, 1, 0xd7d1bf36f0bd19c8)]),
    ("l3-fws-nst-19950517-2304", &[(15, 3, 3, 0x97615f6289b2883f)]),
    ("l3-fws-nvw-19950517-2322", &[(4, 99, 99, 0xe13af5e1fc6aae71)]),
    ("l3-ilx-nhi-19960419-2303", &[(13, 8, 8, 0x3df7052542bc4eaa), (14, 1, 1, 0x1ff7389f74714f09), (15, 9, 9, 0x0307fa01936b9f73)]),
    ("l3-ilx-nme-19960419-2303", &[(3, 7, 7, 0x8eafdf2b7c0db64d), (11, 1, 1, 0x30982746cc3a13d3), (15, 7, 7, 0xca51e6fc47cc1d20)]),
    ("l3-ilx-nst-19960419-2303", &[(15, 15, 15, 0x88cf74d5ada3e76c)]),
    ("l3-ilx-ntv-19960419-2303", &[(12, 1, 1, 0x207c45f96b7c2083), (15, 1, 1, 0xa006838b37f15b9a)]),
    ("l3-lzk-nme-19970301-2027", &[(3, 3, 3, 0xf1b0b06575538441), (15, 3, 3, 0x41b8d0f80f95ddb4)]),
    ("l3-lzk-ntv-19970301-2027", &[(12, 1, 1, 0x0d8ca59220fb3686), (15, 1, 1, 0xd771ba2a7e664818)]),
    ("l3-mci-nmd-20160526-2154", &[(20, 10, 10, 0x588177e5581d61db), (23, 8, 0, 0xaa9baea93a2db7ad), (24, 8, 0, 0x6de40e17f3d2fdb5)]),
    ("l3-mci-nst-20160526-2154", &[(15, 44, 44, 0x4e36225b49b9b03d), (23, 37, 0, 0x9326e2f9481acc99), (24, 32, 0, 0x73acb7cbc1b2c06d)]),
    ("l3-mci-nvw-20160526-2154", &[(4, 119, 119, 0x31d41d4a18273500)]),
    ("l3-okc-nhi-20220503-005210", &[(15, 10, 10, 0xa43a973884a03b00), (19, 17, 17, 0xa2dae52b34039eaa)]),
    ("l3-okc-nmd-20260622-080640", &[(20, 2, 2, 0x9a8d1670da79fee1), (23, 2, 0, 0xf63045833b5f5925), (24, 2, 0, 0x19cd90341308d9ef)]),
    ("l3-okc-nst-20260622-080640", &[(15, 43, 43, 0x0b82d22f78625d01), (23, 27, 0, 0x0fbff79bcb834c81), (24, 20, 0, 0xc7d0d415c9bea051)]),
    ("l3-okc-ntv-20220503-005210", &[(12, 6, 6, 0xd451964a14fa2e38), (15, 6, 6, 0x53d714eac2577c2c)]),
    ("l3-okc-nvw-20260622-080623", &[(4, 301, 301, 0x830b8dcdf3ba2cdf)]),
    ("l3-sgf-nme-20030504-2332", &[(3, 8, 8, 0x37632f37af015058), (11, 1, 2, 0x7ae7a8e4374e2716), (15, 8, 8, 0xd059097cb1925e6d)]),
    ("l3-sgf-ntv-20030504-2352", &[(12, 5, 5, 0x9f8f944790edc004), (15, 5, 5, 0x0c7c413d4a27c53f)]),
    ("l3-tlx-nhi-20130520-2016", &[(15, 11, 11, 0xb72904dfbef2c4be), (19, 22, 22, 0x0641a140b850e5d9)]),
    ("l3-tlx-nhi-20220503-005231", &[(15, 12, 12, 0x6faa358c4ac6b393), (19, 22, 22, 0xd492ca24aac9fc8f)]),
    ("l3-tlx-nmd-20130520-2016", &[(20, 6, 6, 0xfb3188e12efde0cd), (23, 4, 0, 0x9505ecd6c6451a7d), (24, 4, 0, 0x0719764f11f9d9e1)]),
    ("l3-tlx-nmd-20260622-080623", &[(20, 14, 14, 0x76c572e28849321f), (23, 11, 0, 0x81973896dbfa8541), (24, 11, 0, 0xafda868e2d743d78)]),
    ("l3-tlx-nss-20130520-2016", &[(21, 22, 176, 0x30fdd51b23f723c2), (22, 1, 10, 0x6f4215496ed7a0ba)]),
    ("l3-tlx-nss-20220503-005231", &[(21, 22, 176, 0xfdaeeb21824aeb2e), (22, 1, 10, 0x70d20973daf06231)]),
    ("l3-tlx-nst-20130520-2016", &[(15, 22, 22, 0x22a8c2140519636b), (23, 18, 0, 0xfde1982022d4bb65), (24, 18, 0, 0x24de68623c26695f)]),
    ("l3-tlx-nst-20260622-080623", &[(15, 74, 74, 0x06103cc552525955), (23, 43, 0, 0xd593b2bb37ed48c1), (24, 43, 2, 0x8b7a929d5cb44865)]),
    ("l3-tlx-ntv-20130520-2016", &[(12, 4, 4, 0x793b2df4b3d8505a), (15, 4, 4, 0xa11036f85a5f1637)]),
    ("l3-tlx-ntv-20220503-005231", &[(12, 7, 7, 0x56016fc6831e80ff), (15, 7, 7, 0x373c7f2d1e485822)]),
    ("l3-tlx-nvw-20130520-2016", &[(4, 298, 298, 0x21129d82e9ad377c)]),
    ("l3-tlx-nvw-20260622-080623", &[(4, 287, 287, 0x18f118813102bf12)]),
    // GENERATED-METPY-END
];

#[test]
fn decoded_values_match_metpy() {
    let expected: BTreeMap<&str, &[CodeSummary]> = METPY.iter().copied().collect();
    assert_eq!(expected.len(), METPY.len(), "duplicate file in METPY");
    let files = symbol_files();
    let mut failures = Vec::new();
    for (entry, golden, product) in &files {
        let want = *expected
            .get(entry.id.as_str())
            .unwrap_or_else(|| panic!("{}: no MetPy values recorded", entry.id));
        assert_ne!(
            golden.get("metpy").as_str(),
            Some("unsupported"),
            "{}",
            entry.id
        );
        let mut by_code = BTreeMap::<u16, (usize, usize, Vec<String>)>::new();
        for Located { location, packet } in top_level_packets(product) {
            if let Packet::Symbol(symbol) = packet {
                let (items, text) = canonical(symbol);
                let slot = by_code.entry(symbol.code()).or_default();
                slot.0 += 1;
                slot.1 += items;
                slot.2.push(format!("{location} {} {text}", symbol.code()));
            }
        }
        let got: Vec<CodeSummary> = by_code
            .iter()
            .map(|(&code, (packets, items, lines))| (code, *packets, *items, fnv1a64(lines)))
            .collect();
        if got != want {
            let mut text = format!("{}:\n    decoded {got:x?}\n    MetPy   {want:x?}", entry.id);
            for ((code, (.., lines)), summary) in by_code.iter().zip(&got) {
                if !want.contains(summary) {
                    for line in lines.iter().take(10) {
                        write!(text, "\n      {code}: {line}").unwrap();
                    }
                }
            }
            failures.push(text);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(files.len(), METPY.len());
}

/// `(i, j, id)` of every storm ID record in file order.
fn storm_ids(product: &Level3Product) -> Vec<(i16, i16, String)> {
    symbols(product, 15)
        .into_iter()
        .flat_map(|s| match s {
            SymbolPacket::StormIds(ids) => ids.iter().map(|s| (s.i, s.j, s.id.clone())).collect(),
            _ => Vec::new(),
        })
        .collect()
}

/// Explicit values for a sample of packets, copied from MetPy 1.7.1's decoding
/// (positions divided by its 1/4 symbology-block scale).
#[test]
fn decoded_values_match_metpy_spot_checks() {
    let files = symbol_files();

    // Product 60 (1995): mesocyclone, storm ID and 3-D correlated shear.
    let nme = file(&files, "l3-fws-nme-19950517-2316");
    assert_eq!(
        nme.symbology.as_ref().unwrap().layers,
        [vec![
            Packet::Symbol(SymbolPacket::Mesocyclone(vec![Circle {
                i: -300,
                j: -732,
                radius: 22
            }])),
            Packet::Symbol(SymbolPacket::StormIds(vec![StormId {
                i: -300,
                j: -732,
                id: "21".into()
            }])),
            Packet::Symbol(SymbolPacket::CorrelatedShear(vec![Circle {
                i: 76,
                j: -376,
                radius: 26
            }])),
        ]]
    );

    // Product 59 (1995): hail positive and probable; 1995 storm labels are digits.
    let nhi_1995 = file(&files, "l3-fws-nhi-19950517-1323");
    assert_eq!(
        symbols(nhi_1995, 13),
        [&SymbolPacket::HailPositive(vec![Position {
            i: -10,
            j: 809
        }])]
    );
    assert_eq!(
        symbols(nhi_1995, 14),
        [&SymbolPacket::HailProbable(vec![Position {
            i: -93,
            j: 147
        }])]
    );
    assert_eq!(
        storm_ids(nhi_1995),
        [(-10, 809, "48".to_string()), (-93, 147, "97".to_string())]
    );

    // Product 58 (1995): storm labels with a leading space.
    let nst_1995 = file(&files, "l3-fws-nst-19950517-2304");
    assert_eq!(
        storm_ids(nst_1995),
        [
            (-348, -721, "12".to_string()),
            (0, -426, " W".to_string()),
            (354, 70, " N".to_string())
        ]
    );

    // Product 59 (2022): HDA hail.
    let nhi = file(&files, "l3-okc-nhi-20220503-005210");
    let SymbolPacket::HdaHail(hail) = symbols(nhi, 19)[0] else {
        panic!("packet 19 not HDA hail");
    };
    assert_eq!(
        hail[..],
        [HdaHail {
            i: 270,
            j: -15,
            probability_of_hail: 100,
            probability_of_severe_hail: 90,
            max_hail_size_in: 2
        }]
    );
    assert!(!hail[0].is_beyond_range());

    // Product 61 (2022): TVS.
    let ntv = file(&files, "l3-okc-ntv-20220503-005210");
    assert_eq!(
        symbols(ntv, 12)[..3],
        [
            &SymbolPacket::Tvs(vec![Position { i: 265, j: -30 }]),
            &SymbolPacket::Tvs(vec![Position { i: 258, j: -20 }]),
            &SymbolPacket::Tvs(vec![Position { i: 246, j: -38 }]),
        ]
    );

    // Product 141 (2016): MDA point features with radii.
    let nmd = file(&files, "l3-mci-nmd-20160526-2154");
    let features: Vec<_> = symbols(nmd, 20)
        .into_iter()
        .flat_map(|s| match s {
            SymbolPacket::PointFeatures(f) => f.clone(),
            _ => Vec::new(),
        })
        .collect();
    let first: Vec<_> = features[..3]
        .iter()
        .map(|f| (f.i, f.j, f.kind(), f.radius()))
        .collect();
    assert_eq!(
        first,
        [
            (180, -137, PointFeatureKind::MdaLowBase, Some(16)),
            (23, -149, PointFeatureKind::MdaLowBase, Some(8)),
            (222, -124, PointFeatureKind::MdaElevatedBase, Some(9)),
        ]
    );

    // Product 48 (2026): wind barbs.
    let nvw = file(&files, "l3-okc-nvw-20260622-080623");
    assert_eq!(
        symbols(nvw, 4)[..2],
        [
            &SymbolPacket::WindBarbs(vec![WindBarb {
                color_level: 2,
                x: 474,
                y: 454,
                direction_deg: 9,
                speed_kt: 34
            }]),
            &SymbolPacket::WindBarbs(vec![WindBarb {
                color_level: 2,
                x: 474,
                y: 439,
                direction_deg: 0,
                speed_kt: 62
            }]),
        ]
    );

    // Product 58 (2026): an STI circle nested in a SCIT forecast packet.
    let nst = file(&files, "l3-tlx-nst-20260622-080623");
    let forecast: Vec<_> = symbols(nst, 24)
        .into_iter()
        .filter_map(|s| match s {
            SymbolPacket::ScitForecast(nested) => Some(nested),
            _ => None,
        })
        .collect();
    assert_eq!(
        forecast[2][..],
        [Packet::Symbol(SymbolPacket::StiCircles(vec![Circle {
            i: 686,
            j: 537,
            radius: 6
        }]))]
    );

    // Product 62 (2022): volume scan times and the first cell's trends.
    let nss = file(&files, "l3-tlx-nss-20220503-005231");
    let SymbolPacket::CellTrendTimes(times) = symbols(nss, 22)[0] else {
        panic!("packet 22 not cell trend times");
    };
    assert_eq!(
        times.chronological(),
        [1432, 1439, 6, 12, 19, 26, 32, 39, 45, 52]
    );
    assert_eq!(times.latest_value(), Some(52));
    let SymbolPacket::CellTrend(CellTrend { id, i, j, trends }) = symbols(nss, 21)[0] else {
        panic!("packet 21 not cell trend data");
    };
    assert_eq!((id.as_str(), *i, *j, trends.len()), ("K2", 391, -98, 8));
    assert_eq!(trends[0].kind(), Some(TrendKind::CellTop));
    assert_eq!(
        trends[0].volumes.chronological(),
        [1254, 1304, 1349, 1357, 1392, 291, 1339, 379, 402, 446]
    );
    assert_eq!(trends[6].kind(), Some(TrendKind::MaxReflectivity));
    assert_eq!(
        trends[6].volumes.chronological(),
        [57, 62, 60, 61, 62, 58, 69, 66, 67, 64]
    );
}

// ---------------------------------------------------------------------------------
// ICD field semantics
// ---------------------------------------------------------------------------------

/// Symbol counts and maxima equal the product-dependent halfwords of Table V.
#[test]
fn symbols_agree_with_product_description_halfwords() {
    let files = symbol_files();
    let mut checked = BTreeMap::<i16, usize>::new();
    for (entry, _, product) in &files {
        let code = product.description.product_code;
        let count = |packet: u16| -> usize {
            symbols(product, packet)
                .into_iter()
                .map(|s| canonical(s).0)
                .sum()
        };
        match code {
            // STI: hw47 = total number of storms; one storm ID per storm.
            58 => assert_eq!(count(15), halfword(product, 47) as usize, "{}", entry.id),
            // TVS: hw47 = number of TVS, hw48 = number of ETVS. Observed: the
            // products of the legacy TVS algorithm (1996-1997) leave both 0;
            // their TVS count is the legacy table's.
            61 => match product.legacy_tvs_table() {
                Some(table) => {
                    assert_eq!(count(12), table.features.len(), "{}", entry.id);
                    assert_eq!(halfword(product, 47), 0, "{}", entry.id);
                    assert_eq!(halfword(product, 48), 0, "{}", entry.id);
                }
                None => {
                    assert_eq!(count(12), halfword(product, 47) as usize, "{}", entry.id);
                    assert_eq!(count(26), halfword(product, 48) as usize, "{}", entry.id);
                }
            },
            // VWP: hw47 = maximum wind speed (kt). Observed: of the latest profile,
            // the rightmost barb column.
            48 => {
                let barbs: Vec<WindBarb> = symbols(product, 4)
                    .into_iter()
                    .flat_map(|s| match s {
                        SymbolPacket::WindBarbs(b) => b.clone(),
                        _ => Vec::new(),
                    })
                    .collect();
                let latest_x = barbs.iter().map(|b| b.x).max().unwrap();
                let max_speed = barbs
                    .iter()
                    .filter(|b| b.x == latest_x)
                    .map(|b| b.speed_kt)
                    .max()
                    .unwrap();
                assert_eq!(max_speed, halfword(product, 47), "{}", entry.id);
            }
            _ => continue,
        }
        *checked.entry(code).or_default() += 1;
    }
    // Two of the six TVS products are of the legacy algorithm (1996, 1997).
    assert_eq!(checked, BTreeMap::from([(48, 5), (58, 6), (61, 6)]));
}

/// Product 62 cell trend data (Figures 3-15, 3-15a): circular lists with
/// in-range pointers, volume times increasing to the product's volume scan
/// time, trend codes 1-8 and values in their ICD ranges.
#[test]
fn cell_trend_data_follows_the_icd() {
    let files = symbol_files();
    let mut cells = 0;
    let mut products = 0;
    for (entry, _, product) in &files {
        if product.description.product_code != 62 {
            continue;
        }
        products += 1;
        let id = &entry.id;
        let times = symbols(product, 22);
        let [SymbolPacket::CellTrendTimes(times)] = times[..] else {
            panic!("{id}: expected one packet 22");
        };
        let count = times.values.len();
        assert!((1..=10).contains(&count), "{id}: {count} volume times");
        assert!(
            (1..=count).contains(&usize::from(times.latest)),
            "{id}: latest pointer"
        );
        // Minutes after midnight, increasing (modulo a day) and spanning less than a day.
        let chronological = times.chronological();
        assert!(
            chronological.iter().all(|t| (0..1440).contains(t)),
            "{id}: {chronological:?}"
        );
        let steps: Vec<i16> = chronological
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).rem_euclid(1440))
            .collect();
        assert!(
            steps.iter().all(|&s| s > 0),
            "{id}: volume times {chronological:?}"
        );
        assert!(
            steps.iter().map(|&s| i32::from(s)).sum::<i32>() < 1440,
            "{id}: {chronological:?}"
        );
        let scan_minute = (product
            .description
            .volume_scan_time
            .timestamp()
            .rem_euclid(86_400)
            / 60) as i16;
        assert_eq!(
            chronological.last(),
            Some(&scan_minute),
            "{id}: latest volume time"
        );
        assert_eq!(times.latest_value(), Some(scan_minute));

        for symbol in symbols(product, 21) {
            let SymbolPacket::CellTrend(cell) = symbol else {
                panic!("{id}: packet 21 not cell trend data");
            };
            cells += 1;
            let what = format!("{id} cell {}", cell.id);
            assert!(
                cell.id.chars().next().unwrap().is_ascii_uppercase(),
                "{what}"
            );
            assert!(cell.id.chars().nth(1).unwrap().is_ascii_digit(), "{what}");
            assert!((-4096..=4095).contains(&cell.i) && (-4096..=4095).contains(&cell.j));
            let codes: Vec<i16> = cell.trends.iter().map(|t| t.code).collect();
            assert_eq!(codes, [1, 2, 3, 4, 5, 6, 7, 8], "{what}: trend codes");
            for trend in &cell.trends {
                let volumes = &trend.volumes;
                let n = volumes.values.len();
                assert!((1..=count).contains(&n), "{what}: {n} volumes");
                assert!(
                    (1..=n).contains(&usize::from(volumes.latest)),
                    "{what}: latest pointer"
                );
                let range = match trend.kind().unwrap() {
                    // Heights in hundreds of feet; 1000 added when the top (base) is
                    // on the highest (lowest) elevation.
                    TrendKind::CellTop | TrendKind::CellBase => 0..=1700,
                    TrendKind::MaxReflectivityHeight | TrendKind::CentroidHeight => 0..=700,
                    TrendKind::ProbabilityOfHail | TrendKind::ProbabilityOfSevereHail => 0..=100,
                    TrendKind::CellVil => 0..=100,
                    TrendKind::MaxReflectivity => 0..=75,
                    other => panic!("{what}: trend kind {other:?} has no value range"),
                };
                for &v in &volumes.values {
                    assert!(
                        range.contains(&v) || (trend.code == 4 || trend.code == 5) && v == -999,
                        "{what}: trend {} value {v}",
                        trend.code
                    );
                }
            }
        }
    }
    assert_eq!((products, cells), (2, 44));
}

/// Record fields within the ranges of Figures 3-13 and 3-14.
#[test]
fn symbol_values_are_within_icd_ranges() {
    let files = symbol_files();
    let in_range = |v: i16| (-2048..=2047).contains(&v);
    let mut records = 0;
    for (entry, _, product) in &files {
        let id = &entry.id;
        // Storm IDs before the SCIT algorithm (1995-1996) are digits or
        // letters, not A0-Z9.
        let modern = !(entry.id.contains("-1995") || entry.id.contains("-1996"));
        for code in FAMILY {
            for symbol in symbols(product, code) {
                match symbol {
                    SymbolPacket::WindBarbs(barbs) => {
                        for b in barbs {
                            assert!((1..=5).contains(&b.color_level), "{id}: {b:?}");
                            assert!((0..=359).contains(&b.direction_deg), "{id}: {b:?}");
                            assert!((0..=195).contains(&b.speed_kt), "{id}: {b:?}");
                            assert!(in_range(b.x) && in_range(b.y), "{id}: {b:?}");
                            records += 1;
                        }
                    }
                    SymbolPacket::StormIds(ids) => {
                        for s in ids {
                            assert!(in_range(s.i) && in_range(s.j), "{id}: {s:?}");
                            assert_eq!(s.id.chars().count(), 2, "{id}: {s:?}");
                            if modern {
                                // A0 through Z9 (Table VII).
                                let mut c = s.id.chars();
                                assert!(c.next().unwrap().is_ascii_uppercase(), "{id}: {s:?}");
                                assert!(c.next().unwrap().is_ascii_digit(), "{id}: {s:?}");
                            }
                            records += 1;
                        }
                    }
                    SymbolPacket::HdaHail(hail) => {
                        for h in hail {
                            let probability =
                                |p: i16| (0..=100).contains(&p) || p == HdaHail::BEYOND_RANGE;
                            assert!(probability(h.probability_of_hail), "{id}: {h:?}");
                            assert!(probability(h.probability_of_severe_hail), "{id}: {h:?}");
                            assert!((0..=4).contains(&h.max_hail_size_in), "{id}: {h:?}");
                            assert!(in_range(h.i) && in_range(h.j), "{id}: {h:?}");
                            records += 1;
                        }
                    }
                    SymbolPacket::PointFeatures(features) => {
                        for f in features {
                            assert!(
                                !matches!(f.kind(), PointFeatureKind::Other(_)),
                                "{id}: {f:?}"
                            );
                            if let Some(radius) = f.radius() {
                                assert!(radius > 0, "{id}: {f:?}");
                            }
                            assert!(in_range(f.i) && in_range(f.j), "{id}: {f:?}");
                            records += 1;
                        }
                    }
                    SymbolPacket::Mesocyclone(c)
                    | SymbolPacket::CorrelatedShear(c)
                    | SymbolPacket::StiCircles(c) => {
                        for c in c {
                            assert!(in_range(c.i) && in_range(c.j), "{id}: {c:?}");
                            assert!((0..=2047).contains(&c.radius), "{id}: {c:?}");
                            records += 1;
                        }
                    }
                    SymbolPacket::Tvs(p)
                    | SymbolPacket::Etvs(p)
                    | SymbolPacket::HailPositive(p)
                    | SymbolPacket::HailProbable(p) => {
                        for p in p {
                            assert!(in_range(p.i) && in_range(p.j), "{id}: {p:?}");
                            records += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    // 1104 wind barbs, 288 storm IDs, 61 HDA hail, 32 point features, 24 TVS,
    // 19 mesocyclones, 9 hail positive, 4 correlated shear, 2 STI circles and
    // 2 hail probable.
    assert_eq!(records, 1545, "records checked");
}

/* Generator for METPY (MetPy 1.7.1, Python 3.11+). Save between the markers as
   symbols_metpy.py and run from the workspace root with the golden-value venv:
   `python symbols_metpy.py` prints the rows between GENERATED-METPY-START/END;
   `python symbols_metpy.py --lines` prints the canonical lines as JSON.

--- symbols_metpy.py ---
import json
import sys
from pathlib import Path

sys.path.insert(0, 'tools')
import level3_golden  # noqa: E402  (MetPy date shim for 1990s products)

level3_golden.install_metpy_shim()
from metpy.io import Level3File  # noqa: E402

FAMILY = {3, 4, 5, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 25, 26}
POINT_KIND = {'Mesocyclone (ext.)': 1, 'Mesocyclone': 3, 'TVS (Ext.)': 5, 'ETVS (Ext.)': 6,
              'TVS': 7, 'ETVS': 8, 'MDA': 9, 'MDA (Elev.)': 10, 'MDA (Weak)': 11}
TREND = ['Cell Top', 'Cell Base', 'Max Reflectivity Height', 'Probability of Hail',
         'Probability of Severe Hail', 'Cell-based VIL', 'Maximum Reflectivity', 'Centroid Height']
TREND_SCALE = [100, 100, 100, 1, 1, 1, 1, 100]


def as_list(v):
    return v if isinstance(v, list) else [v]


def unscale(v, scale):
    r = v / scale
    assert r == int(r), (v, scale)
    return int(r)


def records(code, p, scale):
    xs = [unscale(v, scale) for v in as_list(p['x'])]
    ys = [unscale(v, scale) for v in as_list(p['y'])]
    out = []
    if code == 20:
        radii = iter(as_list(p.get('radius', [])))
        for x, y, name in zip(xs, ys, as_list(p['type']), strict=True):
            t = POINT_KIND[name]
            out.append(f'{x},{y},{t}' if 5 <= t <= 8 else f'{x},{y},{t},{unscale(next(radii), scale)}')
        assert next(radii, None) is None
        return out
    for n, (x, y) in enumerate(zip(xs, ys, strict=True)):
        f = [x, y]
        if code in (3, 11, 25):
            f.append(unscale(as_list(p['radius'])[n], scale))
        elif code == 15:
            f.append(as_list(p['id'])[n])
        elif code == 19:
            f += [as_list(p['POH'])[n], as_list(p['POSH'])[n], as_list(p['Max Size'])[n]]
        out.append(','.join(str(v) for v in f))
    return out


def canonical(code, p, scale):
    """(items, text) of one packet as MetPy decoded it."""
    if code == 4:
        recs = [f'{c},{unscale(x, scale)},{unscale(y, scale)},{d},{s}' for c, x, y, d, s in
                zip(p['color'], p['x'], p['y'], p['direc'], p['speed'], strict=True)]
        return len(recs), ';'.join(recs)
    if code in (3, 11, 12, 13, 14, 15, 19, 20, 25, 26):
        recs = records(code, p, scale)
        return len(recs), ';'.join(recs)
    if code == 22:
        return len(p['times']), ','.join(str(t) for t in p['times'])
    if code == 21:
        parts = [f"{p['id']} {unscale(p['x'], scale)} {unscale(p['y'], scale)}"]
        trends = 0
        for name in p:
            if name not in TREND:
                continue
            k = TREND.index(name)
            if k in (0, 1):
                raw = [unscale(v, TREND_SCALE[k]) + (1000 if flag else 0)
                       for v, flag in zip(p[name], p[name + ' Limited'], strict=True)]
            else:
                raw = [unscale(v, TREND_SCALE[k]) for v in p[name]]
            parts.append(f'{k + 1}:' + ','.join(str(v) for v in raw))
            trends += 1
        return trends, ' '.join(parts)
    if code in (23, 24):
        circles = [canonical(25, c, scale) for c in as_list(p.get('STI Circle', []))]
        return sum(n for n, _ in circles), '|'.join(t for _, t in circles)
    raise ValueError(f'no canonical form for packet {code}')


def fnv1a64(lines):
    h = 0xcbf29ce484222325
    for b in '\n'.join(lines).encode('latin-1'):
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def main():
    out = {}
    for path in sorted(Path('testdata/level3/golden').glob('*.json')):
        golden = json.loads(path.read_text(encoding='utf-8'))
        if not set(golden['packet_codes'] or []) & FAMILY:
            continue
        file_id = path.stem
        f = Level3File(f'testdata/files/level3/{file_id}')
        blocks, lines = golden['blocks'], []
        if blocks['symbology']:  # the walker's codes give each MetPy packet dict its code
            for n, layer in enumerate(f.sym_block):
                codes = blocks['symbology']['layers'][n]['packets']
                for code, p in zip(codes, layer, strict=True):
                    if code in FAMILY:
                        items, text = canonical(code, p, 0.25)
                        lines.append((code, items, f'sym{n} {code} {text}'))
        if blocks['cell_trend']:
            (page,) = f.graph_pages
            for code, p in zip(blocks['cell_trend']['packets'], page, strict=True):
                items, text = canonical(code, p, 1)
                lines.append((code, items, f'trend {code} {text}'))
        assert not blocks['graphic'] or not any(
            set(pg['packets']) & FAMILY for pg in blocks['graphic']['pages'])
        out[file_id] = lines
    if '--lines' in sys.argv:
        json.dump(out, sys.stdout, indent=1)
        return
    for file_id, lines in out.items():
        groups = {}
        for code, items, text in lines:
            g = groups.setdefault(code, [0, 0, []])
            g[0] += 1
            g[1] += items
            g[2].append(text)
        rows = ', '.join(f'({c}, {g[0]}, {g[1]}, 0x{fnv1a64(g[2]):016x})'
                         for c, g in sorted(groups.items()))
        print(f'    ("{file_id}", &[{rows}]),')


main()
--- end ---
*/
