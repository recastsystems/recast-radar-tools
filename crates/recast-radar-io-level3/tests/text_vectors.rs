//! Text, vector and contour packets (codes 1, 2, 8, 6, 7, 9, 10, 0x0802, 0x0E03,
//! 0x3501) and tabular alphanumeric text, against the real Level III corpus
//! (`testdata/level3/manifest.toml`).
//!
//! Where the expected values come from:
//!
//! - **Golden JSON** (`testdata/level3/golden/<id>.json`, written by
//!   `tools/level3_golden.py`): the ICD walker's packet codes per layer and
//!   page, and for tabular data the page count, lines per page and SHA-256 of
//!   the page text; MetPy's tabular page count.
//! - **MetPy 1.7.1** `Level3File` for packet contents, which the golden JSON
//!   does not record. [`METPY_PACKETS`] lists, per file, how many top-level
//!   family packets MetPy decodes and the SHA-256 of their canonical rendering
//!   (format in [`render`]). The script that produced it is at the end of this
//!   file. MetPy does not decode packets 7, 9 or 0x3501 (none are in the corpus).
//! - **The file's own headers** for what MetPy cannot read (the 1995 product 82
//!   and the 1999 stand-alone message 102) and for the radar coded message
//!   record stamps, which MetPy does not check.
//!
//! Packets 2 and 6 nested inside SCIT packets 23 and 24 are reached only through
//! the symbol packet decoder (Task L3.3 `symbols` family); this file checks the
//! top-level packets of symbology layers and graphic pages.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use common::{Entry, Json};
use recast_radar_io_level3::packets::contour::Contour;
use recast_radar_io_level3::packets::text::SpecialSymbol;
use recast_radar_io_level3::packets::vectors::{Point, Segment, Vectors};
use recast_radar_io_level3::{
    Level3Error, Level3Product, Packet, TabularLayout, TextPacket, TextPage, decode_product,
};

/// Packet codes of this family.
const FAMILY: [u16; 10] = [1, 2, 8, 6, 7, 9, 10, 0x0802, 0x0E03, 0x3501];

/// Corpus files whose family packets MetPy cannot decode, checked instead by
/// `metpy_unsupported_files_match_their_headers`.
const HEADER_CHECKED: [&str; 2] = ["l3-fws-sup-19950517-2304", "l3-tlx-102-19990504-0052"];

/// Per file: number of top-level packets 1, 2, 6, 8, 10, 0x0802 and 0x0E03
/// MetPy 1.7.1 decodes, and SHA-256 of their canonical rendering ([`render`]).
#[rustfmt::skip]
const METPY_PACKETS: &[(&str, usize, &str)] = &[
    ("l3-fws-dpa-19950517-2304", 1, "3520e9438f1ef3b53172ff294fb71e9486c29843daeb9552d8a3861d556f0358"),
    ("l3-fws-ncz-19950517-2304", 7, "3fcf92e75a58cc4e6da2d1b950117427701c8235e90d16097b6c9338e1495c84"),
    ("l3-fws-nhi-19950517-1323", 7, "65b7f10a707ef5388fc3eb54b57cf5461c44285042532843b1d38016b34989a5"),
    ("l3-fws-nme-19950517-2316", 12, "4761fd6a4145b65ceb139cfa58901f4ba641d6f7ec45e4f4760df5f51521ac07"),
    ("l3-fws-nst-19950517-2304", 37, "85b7d85ca50eaeba7dd555081892d7648221e6b8ab5dd92bba75ec4b0756ce4f"),
    ("l3-fws-nvw-19950517-2322", 277, "57c83590a33f3d485a7a0d7dcbbebe66cd99199f3577631dc511b4e62e289b23"),
    ("l3-fws-nwp-19950517-2304", 10, "86d57ddc5c3efbcb65ed3435d6b3145c0103a66da3bfd4b8d280d764a9686736"),
    ("l3-mci-dhr-20160526-2154", 1, "16371df5ccc9be39b07bdf11aa187e53a84c15ceeaf67356b200f70ab0192fff"),
    ("l3-mci-dpa-20160526-2154", 1, "2cf562cf0788462fca59aad3e74c6d49cfc1efae21ba64dff8ade3fa9173a52d"),
    ("l3-mci-dsp-20160526-2154", 1, "16371df5ccc9be39b07bdf11aa187e53a84c15ceeaf67356b200f70ab0192fff"),
    ("l3-mci-ncr-20160526-2154", 56, "fa1cc0757b4c9568691591ec4504a04c00617a44b61a9ad9c1f82ac79fca089d"),
    ("l3-mci-nmd-20160526-2154", 18, "de406f944b5e18480b9413ed6a14e40a13b2281ed3663c722796f0478acfa75e"),
    ("l3-mci-nst-20160526-2154", 86, "327ffc6400a6141f60c9f30e79fb2d4bff4b6e9ca86047da05837d0a759cc19e"),
    ("l3-mci-nvw-20160526-2154", 245, "9e46ad494f6ab2e31245bc3344bb71f6a8c19f015c5e461464934fd74cb2d186"),
    ("l3-okc-ncr-20260622-080623", 56, "836a95bffc1c74abd2157e461c2c0ea55da9b636fb2242ce7d2e3bae9e4c93ec"),
    ("l3-okc-nhi-20220503-005210", 21, "c5e39730edcbb466192a2daa5215b1b02129bdd8572753343a5857f07fc9d03c"),
    ("l3-okc-nmd-20260622-080640", 8, "dc76a266f361b9ad4cbac985f84deaf83806478e10d888bd7b15eb5f1db09c2b"),
    ("l3-okc-nst-20260622-080640", 85, "e336b78e060bd6a3d3f74d7ddd6361ba4d8415408263e0768238693f231a858d"),
    ("l3-okc-ntv-20220503-005210", 7, "eb19ea71ffde8bbfbc98d59a81502d9aa460a9a79a144ab2ba6b9a57e0102b35"),
    ("l3-okc-nvw-20260622-080623", 63, "a54749db5b328e4a4f8abf84642717d92fb162197591e27f86844eb98919cc9b"),
    ("l3-rax-dta-20200818-0454", 8, "0e6e087be858a65556da630f029473f784c3134760eb5ec978e299219d3674c8"),
    ("l3-tlx-dhr-20130520-2016", 1, "061e99c23270bc46f35946200b26c18240c1e1353ca07547bb92754f49251da3"),
    ("l3-tlx-dhr-20260622-080623", 1, "7f1893c3a9cdeb8e829d3c35da50c3dada5a4e8e7e338df3a824d65a794ff21e"),
    ("l3-tlx-dpa-20130520-2016", 1, "2c1e3e6c60810efe29d7bf22dff1a47e4598cc5781d1254af34d6e1d04bc4496"),
    ("l3-tlx-dpa-20260629-173638", 1, "b7ef6c9e860256fe743bcd03556150838a64c37455baabd4ff8ba5325f586a3d"),
    ("l3-tlx-dsp-20130520-2016", 1, "061e99c23270bc46f35946200b26c18240c1e1353ca07547bb92754f49251da3"),
    ("l3-tlx-dsp-20260629-173638", 1, "fada108b6bc2bcf4d2211f2982c6164cea955e52299569f0714ff8bfc40b8562"),
    ("l3-tlx-dta-20130520-2016", 7, "5c31f377f3330e81c9475642a94ebce449da33ad3911f50fab3e48367c4d5f6b"),
    ("l3-tlx-dta-20260622-080623", 8, "dc68bbaa4539338238d94d4b802cb8aee7e1823480444975d01c877c9cae3e3c"),
    ("l3-tlx-n0m-20130520-2016", 8, "31378e081988147ff0d6d50595de29f869e8223e6297aa3b18cea2944a74311a"),
    ("l3-tlx-n0m-20260622-080623", 8, "56266444b0a87effc4416103bc31363316adb3c966533c6fee1f5a15951475b4"),
    ("l3-tlx-n1m-20130520-2016", 8, "3e4c51adaaa50af058fa63439ef56a81a27fb097bc9b6cf18f1c6e5987185c7b"),
    ("l3-tlx-n2m-20130520-2016", 8, "0935b68d629226057eff08d5cd974d307ff020db6dfc42249c871f044911881b"),
    ("l3-tlx-n3m-20130520-2016", 8, "e4b7e998d13f8fdcb7521ddb541f0d7be166f200fc7b79b357050162499da38f"),
    ("l3-tlx-nam-20130520-2016", 8, "5c3b7ae56b2e2bcc8407a347354ca60bf2ed45b8d6a543dde0276003f1526c99"),
    ("l3-tlx-nbm-20130520-2016", 8, "f3f10ae3ce54d70256c5fb1611f9a17a66491eded4a43ac96aa94e608366bc0e"),
    ("l3-tlx-nco-20130520-1816", 7, "12f0fabe33b01c5fcd49037a4152fee663fed595d561ffe010b542cc1a3ed403"),
    ("l3-tlx-ncr-20130520-2016", 42, "c6485c3abb2d501926be411b3fe597b2b5536c15d05a21d06245bf7eef22d041"),
    ("l3-tlx-ncr-20260622-080623", 56, "25da1ded59894a5e6f3afc300b65042d7a3b04fc3d48d74319fb2facf31f4b25"),
    ("l3-tlx-ncz-20130520-2016", 42, "c6485c3abb2d501926be411b3fe597b2b5536c15d05a21d06245bf7eef22d041"),
    ("l3-tlx-ncz-20220503-005231", 42, "128852e17d85682731b014b931991dd7badb8a9493a19895bd5b19b0835e282c"),
    ("l3-tlx-nhi-20130520-2016", 28, "f9cfad9c83ba493f61589aa4bceba5d8d3565d63c71cafdfbefca12dbf50660c"),
    ("l3-tlx-nhi-20220503-005231", 28, "b59b341169a86ec4828bb8325c127c93514bbad6b9610c93b9743e90e9aa1b61"),
    ("l3-tlx-nmd-20130520-2016", 13, "0259e71d28ef4550e225304b6051b68aa9b972f7a87d228d98298a04ebfbc235"),
    ("l3-tlx-nmd-20260622-080623", 35, "76de6ab6a2ba841503b4b6872bdff11093d6c328bb56d5a039e223747d9034e4"),
    ("l3-tlx-nst-20130520-2016", 50, "baa36b4023e5f4e2164ff287ed3e6de82572910146218ae0247bfc5ab7e3c55e"),
    ("l3-tlx-nst-20260622-080623", 116, "15fef74bfea35b46e08d51c5f4fd7a0b2e8547684d6d07cbf5afc3d1b3fffb6e"),
    ("l3-tlx-ntv-20130520-2016", 7, "55d0b6414b50ed9c59f1bfe683a3fba6aa7a65b60f49b7fd0fa9323ac40542a6"),
    ("l3-tlx-ntv-20220503-005231", 14, "7656f1ebd6d330ab567456cdc16b27d377def718192ea7a5b4afe7f5743db3cb"),
    ("l3-tlx-nvw-20130520-2016", 66, "941262abfa844b943b46152566ccdde509f66c91b735f73334edace670235c1e"),
    ("l3-tlx-nvw-20260622-080623", 77, "af4bcf377908abdaa9d0b17635df7dfb58af924d7f97735b719b36a518c846b5"),
    ("l3-tlx-pta-20200501-000023", 3, "5ec0aecd2a2e03ab6e12339f21031ce75a137acca3e2ac02e0bd3107e77073e8"),
];

/// Records a mismatch between a decoded value and its expected value.
macro_rules! check_eq {
    ($problems:expr, $what:expr, $decoded:expr, $expected:expr $(,)?) => {{
        let (decoded, expected) = (&$decoded, &$expected);
        if decoded != expected {
            $problems.push(format!(
                "{}: decoded {:?}, expected {:?}",
                $what, decoded, expected
            ));
        }
    }};
}

/// The decoded product for a manifest entry the golden JSON marks as a product;
/// `None` for text-only messages and messages without a Product Description Block.
fn decode_entry(entry: &Entry, golden: &Json) -> Option<Level3Product> {
    if golden.get("product_code").is_null() {
        return None;
    }
    Some(
        decode_product(&entry.bytes())
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", entry.id)),
    )
}

/// Top-level packets of the symbology layers (`s<layer index>`) and graphic
/// pages (`g<page number>`) in file order.
fn located_packets(product: &Level3Product) -> Vec<(String, &Packet)> {
    let mut out = Vec::new();
    if let Some(symbology) = &product.symbology {
        for (index, layer) in symbology.layers.iter().enumerate() {
            out.extend(layer.iter().map(|p| (format!("s{index}"), p)));
        }
    }
    if let Some(graphic) = &product.graphic {
        for page in &graphic.pages {
            out.extend(
                page.packets
                    .iter()
                    .map(|p| (format!("g{}", page.number), p)),
            );
        }
    }
    out
}

/// Counts of family packet codes the golden walker found at top level.
fn golden_family_counts(golden: &Json) -> BTreeMap<u16, usize> {
    let blocks = golden.get("blocks");
    let mut lists: Vec<&Json> = Vec::new();
    lists.extend(
        blocks
            .get("symbology")
            .get("layers")
            .items()
            .iter()
            .map(|l| l.get("packets")),
    );
    lists.extend(
        blocks
            .get("graphic")
            .get("pages")
            .items()
            .iter()
            .map(|p| p.get("packets")),
    );
    lists.push(blocks.get("cell_trend").get("packets"));
    let mut counts = BTreeMap::new();
    for code in lists.into_iter().flat_map(Json::items) {
        let code = u16::try_from(code.int("packet code")).unwrap();
        if FAMILY.contains(&code) {
            *counts.entry(code).or_default() += 1;
        }
    }
    counts
}

fn hex(text: &str) -> String {
    text.chars().map(|c| format!("{:02x}", c as u8)).collect()
}

fn points(points: &[Point]) -> String {
    let parts: Vec<String> = points.iter().map(|p| format!("{},{}", p.i, p.j)).collect();
    parts.join(";")
}

fn segments(segments: &[Segment]) -> String {
    let parts: Vec<String> = segments
        .iter()
        .map(|s| format!("{},{},{},{}", s.begin.i, s.begin.j, s.end.i, s.end.j))
        .collect();
    parts.join(";")
}

fn color(level: Option<u16>) -> String {
    level.map_or_else(|| "-".to_owned(), |l| l.to_string())
}

fn symbol_name(symbol: SpecialSymbol) -> &'static str {
    match symbol {
        SpecialSymbol::PastStormCell => "past-storm",
        SpecialSymbol::CurrentStormCell => "current-storm",
        SpecialSymbol::ForecastStormCell => "forecast-storm",
        SpecialSymbol::PastMda => "past-mda",
        SpecialSymbol::ForecastMda => "forecast-mda",
        _ => "other",
    }
}

/// Canonical one-line rendering of a packet MetPy decodes, in unscaled file
/// units (MetPy multiplies symbology coordinates by 0.25; the script divides
/// them back):
///
/// - `<loc> text<1|8> <color|-> <i> <j> <hex of the character bytes>`
/// - `<loc> symbol <i> <j> <sorted distinct symbol names>`; MetPy keeps only
///   the set of symbols with one position, so a packet without symbols renders
///   as `<loc> symbol - - `
/// - `<loc> linked6 <color|-> <i,j;...>` (starting point first)
/// - `<loc> unlinked10 <color> <ib,jb,ie,je;...>`
/// - `<loc> color <level>` (0x0802) and `<loc> contour <i,j;...>` (0x0E03)
///
/// `None` for packets outside that set.
fn render(loc: &str, packet: &Packet) -> Option<String> {
    Some(match packet {
        Packet::Text(t) if matches!(t.code, 1 | 8) => format!(
            "{loc} text{} {} {} {} {}",
            t.code,
            color(t.color_level),
            t.i,
            t.j,
            hex(&t.text)
        ),
        Packet::Text(t) if t.code == 2 => {
            let mut names: Vec<&str> = t.special_symbols().map(symbol_name).collect();
            names.sort_unstable();
            names.dedup();
            if names.is_empty() {
                format!("{loc} symbol - - ")
            } else {
                format!("{loc} symbol {} {} {}", t.i, t.j, names.join(","))
            }
        }
        Packet::Vectors(v) => match (&v.vectors, v.code) {
            (Vectors::Linked(p), 6) => {
                format!("{loc} linked6 {} {}", color(v.color_level), points(p))
            }
            (Vectors::Unlinked(s), 10) => {
                format!("{loc} unlinked10 {} {}", color(v.color_level), segments(s))
            }
            _ => return None,
        },
        Packet::Contour(c) => match (&c.contour, c.code) {
            (Contour::ColorLevel(level), 0x0802) => format!("{loc} color {level}"),
            (Contour::Vectors(Vectors::Linked(p)), 0x0E03) => {
                format!("{loc} contour {}", points(p))
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// ICD field ranges (Figures 3-7, 3-8, 3-8a, 3-8b) and the variant each code
/// must decode to.
fn icd_problems(loc: &str, packet: &Packet) -> Vec<String> {
    let mut problems = Vec::new();
    let code = packet.code();
    let mut coords: Vec<Point> = Vec::new();
    let mut level = None;
    match packet {
        Packet::Text(t) => {
            level = t.color_level;
            check_eq!(problems, "color level presence", level.is_some(), code == 8);
            if code == 2
                && !t
                    .text
                    .chars()
                    .all(|c| c == ' ' || SpecialSymbol::from_char(c).is_some())
            {
                problems.push(format!("special symbol characters {:?}", t.text));
            }
            coords.push(Point { i: t.i, j: t.j });
        }
        Packet::Vectors(v) => {
            level = v.color_level;
            check_eq!(
                problems,
                "color level presence",
                level.is_some(),
                matches!(code, 9 | 10)
            );
            match &v.vectors {
                Vectors::Linked(p) if matches!(code, 6 | 9) && p.len() >= 2 => coords.extend(p),
                Vectors::Unlinked(s) if matches!(code, 7 | 10) && !s.is_empty() => {
                    coords.extend(s.iter().flat_map(|s| [s.begin, s.end]));
                }
                other => problems.push(format!("vectors {other:?}")),
            }
        }
        Packet::Contour(c) => match (&c.contour, code) {
            (Contour::ColorLevel(l), 0x0802) => level = Some(*l),
            (Contour::Vectors(Vectors::Linked(p)), 0x0E03) if p.len() >= 2 => coords.extend(p),
            (Contour::Vectors(Vectors::Unlinked(s)), 0x3501) if !s.is_empty() => {
                coords.extend(s.iter().flat_map(|s| [s.begin, s.end]));
            }
            (other, _) => problems.push(format!("contour {other:?}")),
        },
        other => problems.push(format!("decoded as {other:?}")),
    }
    if level.is_some_and(|l| l > 15) {
        problems.push(format!("color level {level:?} outside 0-15"));
    }
    if let Some(p) = coords
        .iter()
        .find(|p| !(-2048..=2047).contains(&p.i) || !(-2048..=2047).contains(&p.j))
    {
        problems.push(format!("coordinate {p:?} outside -2048..=2047"));
    }
    problems
        .into_iter()
        .map(|p| format!("{loc} packet {code}: {p}"))
        .collect()
}

#[test]
fn family_packets_match_golden_and_metpy() {
    let mut failures = Vec::new();
    let mut matched_metpy = Vec::new();
    let mut decoded: BTreeMap<u16, usize> = BTreeMap::new();
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = decode_entry(&entry, &golden) else {
            continue;
        };
        let mut problems = Vec::new();
        let located = located_packets(&product);

        let mut counts = BTreeMap::new();
        for (loc, packet) in &located {
            let code = packet.code();
            if !FAMILY.contains(&code) {
                continue;
            }
            *counts.entry(code).or_default() += 1;
            problems.extend(icd_problems(loc, packet));
        }
        check_eq!(
            problems,
            "family packet counts",
            counts,
            golden_family_counts(&golden)
        );
        for (code, n) in &counts {
            *decoded.entry(*code).or_default() += n;
        }

        let rendered: Vec<String> = located
            .iter()
            .filter_map(|(loc, packet)| render(loc, packet))
            .collect();
        let text: String = rendered.iter().map(|line| format!("{line}\n")).collect();
        match METPY_PACKETS.iter().find(|(id, ..)| *id == entry.id) {
            Some((id, count, digest)) => {
                matched_metpy.push(*id);
                check_eq!(problems, "packets decoded by MetPy", rendered.len(), *count);
                if sha256_hex(text.as_bytes()) != *digest {
                    let head: Vec<&str> = text.lines().take(6).collect();
                    problems.push(format!(
                        "rendering differs from MetPy's; first lines:\n      {}",
                        head.join("\n      ")
                    ));
                }
            }
            None if rendered.is_empty() => {}
            None if golden.get("metpy").as_str() == Some("unsupported")
                && HEADER_CHECKED.contains(&entry.id.as_str()) => {}
            None => problems.push(format!(
                "{} family packets but no METPY_PACKETS row",
                rendered.len()
            )),
        }
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", entry.id, problems.join("\n    ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let missing: Vec<&str> = METPY_PACKETS
        .iter()
        .map(|(id, ..)| *id)
        .filter(|id| !matched_metpy.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "METPY_PACKETS rows without a corpus file: {missing:?}"
    );
    // The corpus exercises every family packet code it holds (reference.md section 7).
    for code in [1, 2, 6, 8, 10, 0x0802, 0x0E03] {
        assert!(
            decoded.get(&code).is_some_and(|&n| n > 0),
            "no top-level packet {code} decoded"
        );
    }
    eprintln!(
        "decoded top-level family packets {decoded:?}; {} files matched MetPy",
        matched_metpy.len()
    );
}

/// Page text as the golden walker hashes it: lines joined by `\n`, pages by
/// form feed, one byte per character.
fn page_text_bytes(pages: &[TextPage]) -> Vec<u8> {
    let pages: Vec<String> = pages.iter().map(|p| p.lines.join("\n")).collect();
    pages.join("\u{c}").chars().map(|c| c as u8).collect()
}

#[test]
fn tabular_pages_match_golden() {
    let mut failures = Vec::new();
    let (mut paged, mut rcm) = (0, 0);
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = decode_entry(&entry, &golden) else {
            continue;
        };
        let blocks = golden.get("blocks");
        let mut problems = Vec::new();
        let tab = product.tabular.as_ref();
        let paged_golden = [blocks.get("tabular"), blocks.get("standalone_tabular")]
            .into_iter()
            .find(|g| !g.is_null());
        if let Some(g) = paged_golden {
            paged += 1;
            let Some(tab) = tab else {
                failures.push(format!("{}: no tabular data decoded", entry.id));
                continue;
            };
            let lines: Vec<i64> = tab.pages.iter().map(|p| p.lines.len() as i64).collect();
            let golden_lines: Vec<i64> = g
                .get("lines_per_page")
                .items()
                .iter()
                .map(|n| n.int("lines"))
                .collect();
            check_eq!(
                problems,
                "page count",
                tab.pages.len() as i64,
                g.get("num_pages").int("num_pages")
            );
            check_eq!(problems, "lines per page", lines, golden_lines);
            check_eq!(
                problems,
                "page text sha256",
                sha256_hex(&page_text_bytes(&tab.pages)),
                g.get("text_sha256").as_str().unwrap()
            );
            if let Some(metpy_pages) = golden.get("metpy_detail").get("tab_pages").as_i64() {
                check_eq!(
                    problems,
                    "page count (MetPy)",
                    tab.pages.len() as i64,
                    metpy_pages
                );
            }
            // ICD limits (Figure 3-6 sheet 10, Figure 3-16).
            if !(1..=48).contains(&tab.pages.len()) {
                problems.push(format!("{} pages, ICD allows 1-48", tab.pages.len()));
            }
            for (n, page) in tab.pages.iter().enumerate() {
                if page.lines.len() > 17 {
                    problems.push(format!(
                        "page {n}: {} lines, ICD allows 17",
                        page.lines.len()
                    ));
                }
                if let Some(line) = page.lines.iter().find(|l| l.chars().count() > 80) {
                    problems.push(format!(
                        "page {n}: line longer than 80 characters: {line:?}"
                    ));
                }
            }
        }
        let rcm_golden = blocks.get("rcm");
        if !rcm_golden.is_null() {
            rcm += 1;
            match tab {
                Some(tab) if tab.layout == TabularLayout::RadarCodedMessage => {
                    check_eq!(problems, "radar coded message pages", tab.pages.len(), 1);
                    let text: String = tab
                        .pages
                        .iter()
                        .flat_map(|p| p.lines.iter().map(String::as_str))
                        .collect();
                    let bytes: Vec<u8> = text.chars().map(|c| c as u8).collect();
                    check_eq!(problems, "radar coded message bytes", bytes, tab.data);
                    check_eq!(
                        problems,
                        "radar coded message sha256",
                        sha256_hex(&bytes),
                        rcm_golden.get("text_sha256").as_str().unwrap()
                    );
                    let lines = &tab.pages[0].lines;
                    if let Some(short) = lines.iter().find(|l| l.len() != 70) {
                        problems.push(format!("record of {} characters: {short:?}", short.len()));
                    }
                }
                other => problems.push(format!("expected radar coded message, got {other:?}")),
            }
        }
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", entry.id, problems.join("\n    ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(paged > 0 && rcm > 0);
    eprintln!("{paged} files with tabular pages, {rcm} radar coded messages");
}

/// `/NEXR..` record stamp: the volume scan time rounded to the nearest minute as
/// `ddmmyyHHMM` (observed in all three corpus messages).
fn rcm_stamp(volume_scan_time: DateTime<Utc>) -> String {
    let rounded =
        DateTime::from_timestamp((volume_scan_time.timestamp() + 30) / 60 * 60, 0).unwrap();
    format!(
        "{:02}{:02}{:02}{:02}{:02}",
        rounded.day(),
        rounded.month(),
        rounded.year() % 100,
        rounded.hour(),
        rounded.minute()
    )
}

fn record(text: &str) -> String {
    format!("{text:<70}")
}

/// Radar coded message records carry the radar ID (Message Header Block source
/// ID) and the volume scan time from the Product Description Block (legacy ICD
/// 2620001P Appendix B: header `cccc ROBUU sidd`, parts A-C each opening with a
/// `/NEXR..` record and parts A and B closing with `/END..`).
#[test]
fn radar_coded_messages_match_their_headers() {
    let mut checked = 0;
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        if golden.get("blocks").get("rcm").is_null() {
            continue;
        }
        let product = decode_entry(&entry, &golden).unwrap();
        let id = product.message_header.source_id;
        let stamp = rcm_stamp(product.description.volume_scan_time);
        let lines = &product.tabular.as_ref().unwrap().pages[0].lines;
        assert_eq!(
            lines[0],
            record(&format!("1234 ROBUU {id:04}")),
            "{}",
            entry.id
        );
        assert_eq!(
            lines[1],
            record(&format!("/NEXRAA {id:04} {stamp} UNEDITED")),
            "{}",
            entry.id
        );
        let position = |text: &str| {
            lines
                .iter()
                .position(|l| *l == record(text))
                .unwrap_or_else(|| panic!("{}: no record {text:?}", entry.id))
        };
        let order = [
            position("/ENDAA"),
            position(&format!("/NEXRBB {id:04} {stamp}")),
            position("/ENDBB"),
            position(&format!("/NEXRCC {id:04} {stamp}")),
        ];
        assert!(
            order.is_sorted() && order[0] > 1,
            "{}: section order {order:?}",
            entry.id
        );
        checked += 1;
    }
    assert_eq!(checked, 3);
}

/// Files MetPy 1.7.1 cannot read, checked against values from their own headers.
#[test]
fn metpy_unsupported_files_match_their_headers() {
    let entries = common::level3_manifest();
    let find = |id: &str| {
        let entry = entries.iter().find(|e| e.id == id).unwrap();
        let golden = entry.golden();
        assert_eq!(golden.get("metpy").as_str(), Some("unsupported"), "{id}");
        decode_entry(entry, &golden).unwrap()
    };

    // 1995 product 82 (SUP, version 0): a symbology block whose second layer is
    // one packet 1 at the origin holding 15 space-padded 80-character records.
    let sup = find(HEADER_CHECKED[0]);
    assert_eq!(
        (sup.description.product_code, sup.description.version),
        (82, 0)
    );
    let layers = &sup.symbology.as_ref().unwrap().layers;
    assert_eq!(layers.len(), 2);
    let [Packet::Text(text)] = layers[1].as_slice() else {
        panic!("layer 1: {:?}", layers[1]);
    };
    assert_eq!(
        (text.code, text.color_level, text.i, text.j),
        (1, None, 0, 0)
    );
    assert_eq!(text.text.len(), 15 * 80);
    let records: Vec<&str> = (0..15)
        .map(|k| text.text[k * 80..(k + 1) * 80].trim_end())
        .collect();
    // The VCP record repeats Product Description Block halfword 18.
    let vcp = format!("VOLUME COVERAGE PATTERN......:{:>9}", sup.description.vcp);
    assert_eq!(sup.description.vcp, 21);
    assert_eq!(
        records,
        [
            "NO.OF ISOLATED BINS..........:       72",
            "NO.OF OUTLIERS INTERPOLATED..:        0",
            "NO.OF OUTLIERS REPLACED......:        0",
            "PERCENT AREA REDUCTION.......:    27.36",
            "BI-SCAN RATIO................:     0.94",
            "DATA QUALITY FLAG............:        0",
            "NUMBER OF HOURLY OUTLIERS....:        0",
            "BIAS ESTIMATE................:     1.00",
            "BIAS ERROR VARIANCE..........:     0.50",
            "GAGE BIAS APPLIED............:  YES",
            "MISSING PERIOD BEGINNING TIME:        0",
            "MISSING PERIOD ENDING TIME...:        0",
            vcp.as_str(),
            "OPERATIONAL (WEATHER) MODE...:        1",
            "THERE ARE NO GAGES IN THE DATABASE",
        ]
    );

    // 1999 alphanumeric message 102 (hail index table) on its own: three
    // 16-line pages. Each table page names the radar (Message Header Block
    // source ID) and the volume scan time (Product Description Block halfwords
    // 21-23, as mm:dd:yy/hh:mm:ss) and counts the storm cells it lists.
    let hail = find(HEADER_CHECKED[1]);
    assert_eq!(
        (hail.message_header.code, hail.description.product_code),
        (102, 102)
    );
    let tab = hail.tabular.as_ref().unwrap();
    assert_eq!(tab.layout, TabularLayout::StandAlone);
    let lines: Vec<usize> = tab.pages.iter().map(|p| p.lines.len()).collect();
    assert_eq!(lines, [16, 16, 16]);
    let t = hail.description.volume_scan_time;
    let stamp = format!(
        "{:02}:{:02}:{:02}/{:02}:{:02}:{:02}",
        t.month(),
        t.day(),
        t.year() % 100,
        t.hour(),
        t.minute(),
        t.second()
    );
    assert_eq!(stamp, "05:04:99/00:52:04");
    let mut cells = Vec::new();
    for page in &tab.pages[..2] {
        assert_eq!(page.lines[0].trim(), "HAIL");
        assert_eq!(
            page.lines[1].trim_end(),
            format!(
                "     RADAR ID {:>3}   DATE/TIME {stamp}   NUMBER OF STORM CELLS {:>3}",
                hail.message_header.source_id, 20
            )
        );
        cells.extend(
            page.lines[6..]
                .iter()
                .map(|l| l.split_whitespace().next().unwrap()),
        );
    }
    assert_eq!(cells.len(), 20);
    assert!(cells.iter().all(|id| {
        let id = id.as_bytes();
        id.len() == 2 && id[0].is_ascii_uppercase() && id[1].is_ascii_digit()
    }));
    assert_eq!(
        tab.pages[2].lines[0].trim(),
        "HAIL DETECTION ADAPTATION DATA"
    );
}

/// A few packets spelled out through the public API (values as MetPy 1.7.1
/// decodes them; MetPy's symbology coordinates are these divided by 4).
#[test]
fn packet_values_read_as_documented() {
    let entries = common::level3_manifest();
    let product = |id: &str| {
        let entry = entries.iter().find(|e| e.id == id).unwrap();
        decode_product(&entry.bytes()).unwrap()
    };

    // Product 166 melting layer: color level, then a linked contour.
    let melting = product("l3-tlx-n0m-20130520-2016");
    let layer = &melting.symbology.as_ref().unwrap().layers[0];
    let Packet::Contour(level) = &layer[0] else {
        panic!("{:?}", layer[0])
    };
    assert_eq!(
        (level.code, &level.contour),
        (0x0802, &Contour::ColorLevel(1))
    );
    let Packet::Contour(contour) = &layer[1] else {
        panic!("{:?}", layer[1])
    };
    let Contour::Vectors(Vectors::Linked(p)) = &contour.contour else {
        panic!("{contour:?}")
    };
    assert_eq!(contour.code, 0x0E03);
    assert_eq!(
        p[..3],
        [
            Point { i: 652, j: -665 },
            Point { i: 641, j: -675 },
            Point { i: 629, j: -686 }
        ]
    );

    // Product 37 composite reflectivity: storm attribute table in graphic page 1.
    let ncr = product("l3-tlx-ncr-20260622-080623");
    let page = &ncr.graphic.as_ref().unwrap().pages[0];
    assert_eq!(page.number, 1);
    let texts: Vec<&TextPacket> = page
        .packets
        .iter()
        .filter_map(|p| match p {
            Packet::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(
        (texts[0].code, texts[0].color_level, texts[0].i, texts[0].j),
        (8, Some(1), 0, 1)
    );
    assert_eq!(
        texts[0].text,
        " STM ID  AZ/RAN TVS  MDA  POSH/POH/MX SIZE VIL DBZM  HT  TOP  FCST MVMT "
    );
    assert_eq!(
        texts[1].text,
        "    W1   97/ 44 TVS     7    0/100/ 0.50     7  49 24.8  32.3  289/ 52  "
    );
    let Packet::Vectors(grid) = &page.packets[5] else {
        panic!("{:?}", page.packets[5])
    };
    assert_eq!((grid.code, grid.color_level), (10, Some(6)));
    let segment = |ib, jb, ie, je| Segment {
        begin: Point { i: ib, j: jb },
        end: Point { i: ie, j: je },
    };
    assert_eq!(
        grid.vectors,
        Vectors::Unlinked(vec![
            segment(4, 0, 501, 0),
            segment(4, 10, 501, 10),
            segment(4, 20, 501, 20),
            segment(4, 30, 501, 30),
            segment(4, 40, 501, 40),
            segment(4, 50, 501, 50),
        ])
    );
    assert_eq!(grid.vectors.segments().len(), 6);

    // Product 48 VAD wind profile: frame and labels in screen coordinates.
    let vwp = product("l3-tlx-nvw-20260622-080623");
    let layer = &vwp.symbology.as_ref().unwrap().layers[0];
    let Packet::Vectors(frame) = &layer[0] else {
        panic!("{:?}", layer[0])
    };
    let Vectors::Unlinked(s) = &frame.vectors else {
        panic!("{frame:?}")
    };
    assert_eq!(
        (frame.code, frame.color_level, s[0]),
        (10, Some(6), segment(0, 0, 511, 0))
    );
    let Packet::Text(label) = &layer[3] else {
        panic!("{:?}", layer[3])
    };
    assert_eq!(
        (
            label.code,
            label.color_level,
            label.i,
            label.j,
            label.text.as_str()
        ),
        (8, Some(6), 11, 490, "TIME")
    );

    // Product 58 storm tracking: special symbols and a linked track (1995 FWS).
    let sti = product("l3-fws-nst-19950517-2304");
    let layer = &sti.symbology.as_ref().unwrap().layers[0];
    let Packet::Text(past) = &layer[0] else {
        panic!("{:?}", layer[0])
    };
    assert_eq!(
        (past.code, past.i, past.j, past.text.as_str()),
        (2, -400, -737, "! ")
    );
    assert_eq!(
        past.special_symbols().collect::<Vec<_>>(),
        [SpecialSymbol::PastStormCell]
    );
    let Packet::Vectors(track) = &layer[8] else {
        panic!("{:?}", layer[8])
    };
    assert_eq!(
        (track.code, track.color_level, &track.vectors),
        (
            6,
            None,
            &Vectors::Linked(vec![Point { i: -348, j: -721 }, Point { i: -400, j: -737 }])
        )
    );
}

/// Message byte offset of an unwrapped, uncompressed corpus file's Message Header Block.
fn message_start(entry: &Entry, bytes: &[u8]) -> usize {
    let golden = entry.golden();
    let framing = golden.get("framing");
    assert_eq!(framing.get("zlib_frames").int("zlib_frames"), 0);
    assert_eq!(
        golden.get("compression").get("bzip2").as_bool(),
        Some(false)
    );
    let trailer = if framing.get("trailer").is_null() {
        0
    } else {
        4
    };
    bytes.len()
        - trailer
        - usize::try_from(framing.get("message_bytes").int("message_bytes")).unwrap()
}

fn halfword_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

/// Real files with one ICD-fixed field changed decode to errors.
#[test]
fn corrupted_family_fields_are_errors() {
    let entries = common::level3_manifest();
    let entry = |id: &str| entries.iter().find(|e| e.id == id).unwrap();
    let with = |bytes: &[u8], at: usize, value: u16| {
        let mut changed = bytes.to_vec();
        changed[at..at + 2].copy_from_slice(&value.to_be_bytes());
        decode_product(&changed)
    };

    // Contours: layer 0 of this product 166 opens with 0x0802 then 0x0E03.
    let melting = entry("l3-tlx-n0m-20130520-2016");
    let bytes = melting.bytes();
    let product = decode_product(&bytes).unwrap();
    let layer0 =
        message_start(melting, &bytes) + 2 * product.description.symbology_offset as usize + 16;
    assert_eq!(
        [halfword_at(&bytes, layer0), halfword_at(&bytes, layer0 + 2)],
        [0x0802, 0x0002]
    );
    assert_eq!(
        [
            halfword_at(&bytes, layer0 + 6),
            halfword_at(&bytes, layer0 + 8)
        ],
        [0x0E03, 0x8000]
    );
    assert!(matches!(
        with(&bytes, layer0 + 2, 0x0003),
        Err(Level3Error::InvalidPacket { code: 0x0802, .. })
    ));
    assert!(matches!(
        with(&bytes, layer0 + 8, 0x0000),
        Err(Level3Error::InvalidPacket { code: 0x0E03, .. })
    ));

    // Tabular pages of this product 59: block divider and ID, second headers,
    // then the page block divider, page count and first line's character count.
    let hail = entry("l3-tlx-nhi-20130520-2016");
    let bytes = hail.bytes();
    let product = decode_product(&bytes).unwrap();
    let block = message_start(hail, &bytes) + 2 * product.description.tabular_offset as usize;
    let pages = block + 8 + 120;
    assert_eq!(
        [halfword_at(&bytes, block), halfword_at(&bytes, block + 2)],
        [0xFFFF, 3]
    );
    assert_eq!(
        [halfword_at(&bytes, pages), halfword_at(&bytes, pages + 2)],
        [0xFFFF, 4]
    );
    assert!(halfword_at(&bytes, pages + 4) <= 80);
    assert!(matches!(
        with(&bytes, pages, 0),
        Err(Level3Error::BadBlockHeader {
            what: "tabular page block divider",
            found: 0,
            ..
        })
    ));
    assert!(matches!(
        with(&bytes, pages + 2, 0x7FFF),
        Err(Level3Error::Truncated {
            what: "tabular line character count",
            ..
        })
    ));
    assert!(matches!(
        with(&bytes, pages + 4, 0xFFFE),
        Err(Level3Error::BadBlockHeader {
            what: "tabular end of page flag",
            found: -2,
            ..
        })
    ));
    assert!(matches!(
        with(&bytes, pages + 4, 0x7FFF),
        Err(Level3Error::Truncated {
            what: "tabular line",
            ..
        })
    ));
}

/// The SHA-256 below reproduces every manifest checksum.
#[test]
fn sha256_matches_manifest_checksums() {
    for entry in common::level3_manifest() {
        assert_eq!(sha256_hex(&entry.bytes()), entry.sha256, "{}", entry.id);
    }
}

/// SHA-256 (FIPS 180-4) as lowercase hex; the crate has no dev-dependencies.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (t, word) in block.chunks_exact(4).enumerate() {
            w[t] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for t in 16..64 {
            let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
            let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16]
                .wrapping_add(s0)
                .wrapping_add(w[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for t in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(sigma1)
                .wrapping_add(choose)
                .wrapping_add(K[t])
                .wrapping_add(w[t]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = sigma0.wrapping_add(majority);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(value);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

// Script that produced METPY_PACKETS (run from the workspace root with the
// golden venv: Python 3.11+, MetPy 1.7.1; prints one row per file):
//
//     import hashlib, io, json, sys, tomllib, warnings
//     from pathlib import Path
//     sys.path.insert(0, 'tools')
//     import level3_golden as lg
//     lg.install_metpy_shim()
//     from metpy.io import Level3File
//
//     SYMBOLS = {'past storm position': 'past-storm', 'current storm position': 'current-storm',
//                'forecast storm position': 'forecast-storm', 'past MDA position': 'past-mda',
//                'forecast MDA position': 'forecast-mda'}
//     FAMILY = {1, 2, 6, 8, 10, 0x0802, 0x0E03}
//
//     def unscale(v, scale):
//         q = v / scale
//         assert q == int(q)
//         return int(q)
//
//     def render(p, code, loc, scale):
//         pts = lambda vs: ';'.join(f'{unscale(x, scale)},{unscale(y, scale)}' for x, y in vs)
//         color = lambda c: '-' if c is None else str(c)
//         if code in (1, 8):
//             return (f"{loc} text{code} {color(p['color'])} {unscale(p['x'], scale)} "
//                     f"{unscale(p['y'], scale)} {p['text'].encode('latin-1').hex()}")
//         if code == 2:
//             where = set(p.values())
//             assert len(where) <= 1
//             if not where:
//                 return f'{loc} symbol - - '
//             x, y = where.pop()
//             names = ','.join(sorted(SYMBOLS[k] for k in p))
//             return f'{loc} symbol {unscale(x, scale)} {unscale(y, scale)} {names}'
//         if code == 6:
//             return f"{loc} linked6 {color(p['color'])} {pts(p['vectors'])}"
//         if code == 0x0E03:
//             return f"{loc} contour {pts(p['vectors'])}"
//         if code == 10:
//             segs = ';'.join(','.join(str(unscale(v, scale)) for v in s) for s in p['vectors'])
//             return f"{loc} unlinked10 {p['color']} {segs}"
//         return f"{loc} color {p['color']}"  # 0x0802
//
//     manifest = tomllib.loads(Path('testdata/level3/manifest.toml').read_text(encoding='utf-8'))
//     for entry in manifest['file']:
//         golden = json.loads(Path(f"testdata/level3/golden/{entry['id']}.json").read_text())
//         blocks = golden['blocks']
//         if not blocks or golden['metpy'] == 'unsupported':
//             continue
//         with warnings.catch_warnings():
//             warnings.simplefilter('ignore')
//             f = Level3File(io.BytesIO(lg.committed_path(entry).read_bytes()))
//         lines = []
//         for li, (layer, packets) in enumerate(zip((blocks['symbology'] or {}).get('layers', []),
//                                                  f.sym_block if blocks['symbology'] else [])):
//             lines += [render(p, c, f's{li}', 0.25) for c, p in zip(layer['packets'], packets)
//                       if c in FAMILY]
//         for page, packets in zip((blocks['graphic'] or {}).get('pages', []),
//                                  f.graph_pages if blocks['graphic'] else []):
//             lines += [render(p, c, f"g{page['page']}", 1) for c, p in zip(page['packets'], packets)
//                       if c in FAMILY]
//         if lines:
//             text = ''.join(line + '\n' for line in lines)
//             digest = hashlib.sha256(text.encode('ascii')).hexdigest()
//             print(f'    ("{entry["id"]}", {len(lines)}, "{digest}"),')
