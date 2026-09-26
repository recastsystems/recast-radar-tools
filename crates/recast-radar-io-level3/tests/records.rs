//! Display packet records ([`Level3Product::display_records`], carried by
//! the volume as `level3_display_packets`) on every corpus product:
//!
//! - **Structure** against the ICD walker golden JSON
//!   (`testdata/level3/golden/<id>.json`, `tools/level3_golden.py`): one
//!   record per top-level packet of each symbology layer and graphic page and
//!   per packet nested in SCIT packets 23 and 24, with the walker's codes in
//!   the walker's order.
//! - **Values**: each record, split per the documented grammar (strings
//!   unescaped), gives back every value of its decoded packet, which the
//!   family tests compare with MetPy 1.7.1 and the separate byte readings
//!   (`symbols.rs`, `text_vectors.rs`, `radial_generic.rs`). A data array's
//!   record names the volume sweep that holds it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;

use common::Json;
use recast_radar_core::model::{AttrValue, FieldData};
use recast_radar_io_level3::packets::contour::Contour;
use recast_radar_io_level3::packets::generic::GenericComponent;
use recast_radar_io_level3::packets::irm::IrmPacket;
use recast_radar_io_level3::packets::vectors::Vectors;
use recast_radar_io_level3::{Packet, SymbolPacket};

/// Splits a record on spaces outside double quotes.
fn split_outside_quotes(text: &str, separators: &[char]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in text.chars() {
        if quoted {
            current.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            current.push(c);
        } else if separators.contains(&c) {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    parts.push(current);
    parts
}

/// Undoes Rust's `{:?}` escaping of a quoted string.
fn unescape(quoted: &str) -> String {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not a quoted string: {quoted}"));
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().unwrap() {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'u' => {
                assert_eq!(chars.next(), Some('{'));
                let digits: String = chars.by_ref().take_while(|c| *c != '}').collect();
                out.push(char::from_u32(u32::from_str_radix(&digits, 16).unwrap()).unwrap());
            }
            other => panic!("unexpected escape \\{other}"),
        }
    }
    out
}

/// A record's values after the place and code: numbers and unescaped
/// strings, splitting items on spaces, commas and colons, dropping `key=`
/// prefixes and list brackets.
fn values(fields: &str) -> Vec<String> {
    split_outside_quotes(fields, &[' ', ',', ':'])
        .into_iter()
        .filter(|t| !t.is_empty())
        .flat_map(|token| {
            let token = match token.find('=') {
                Some(at) if !token.starts_with('"') => token[at + 1..].to_owned(),
                _ => token,
            };
            let token = token
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned();
            split_outside_quotes(&token, &['='])
        })
        .filter(|t| !t.is_empty())
        .map(|t| if t.starts_with('"') { unescape(&t) } else { t })
        .collect()
}

/// The values a packet's record must give back, in record order.
fn expected(packet: &Packet) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut n = |x: &dyn ToString| v.push(x.to_string());
    match packet {
        Packet::Text(t) => {
            if let Some(color) = t.color_level {
                n(&color);
            }
            n(&t.i);
            n(&t.j);
            n(&t.text);
        }
        Packet::Vectors(p) => {
            if let Some(color) = p.color_level {
                n(&color);
            }
            push_vectors(&mut v, &p.vectors);
        }
        Packet::Contour(c) => match &c.contour {
            Contour::ColorLevel(level) => n(level),
            Contour::Vectors(vectors) => push_vectors(&mut v, vectors),
            other => panic!("contour {other:?}"),
        },
        Packet::Symbol(symbol) => match symbol {
            SymbolPacket::Mesocyclone(c)
            | SymbolPacket::CorrelatedShear(c)
            | SymbolPacket::StiCircles(c) => {
                for c in c {
                    v.extend([c.i, c.j, c.radius].map(|x| x.to_string()));
                }
            }
            SymbolPacket::WindBarbs(b) => {
                for b in b {
                    v.extend(
                        [b.color_level, b.x, b.y, b.direction_deg, b.speed_kt]
                            .map(|x| x.to_string()),
                    );
                }
            }
            SymbolPacket::VectorArrows(a) => {
                for a in a {
                    v.extend(
                        [a.i, a.j, a.direction_deg, a.arrow_length, a.head_length]
                            .map(|x| x.to_string()),
                    );
                }
            }
            SymbolPacket::Tvs(p)
            | SymbolPacket::HailPositive(p)
            | SymbolPacket::HailProbable(p)
            | SymbolPacket::Etvs(p) => {
                for p in p {
                    v.extend([p.i, p.j].map(|x| x.to_string()));
                }
            }
            SymbolPacket::StormIds(s) => {
                for s in s {
                    v.extend([s.i.to_string(), s.j.to_string(), s.id.clone()]);
                }
            }
            SymbolPacket::HdaHail(h) => {
                for h in h {
                    v.extend(
                        [
                            h.i,
                            h.j,
                            h.probability_of_hail,
                            h.probability_of_severe_hail,
                            h.max_hail_size_in,
                        ]
                        .map(|x| x.to_string()),
                    );
                }
            }
            SymbolPacket::PointFeatures(f) => {
                for f in f {
                    v.extend([f.i, f.j, f.feature_type, f.attribute].map(|x| x.to_string()));
                }
            }
            SymbolPacket::CellTrend(t) => {
                v.extend([t.id.clone(), t.i.to_string(), t.j.to_string()]);
                for trend in &t.trends {
                    v.push(trend.code.to_string());
                    v.push(trend.volumes.latest.to_string());
                    v.extend(trend.volumes.values.iter().map(ToString::to_string));
                }
            }
            SymbolPacket::CellTrendTimes(times) => {
                v.push(times.latest.to_string());
                v.extend(times.values.iter().map(ToString::to_string));
            }
            SymbolPacket::ScitPast(nested) | SymbolPacket::ScitForecast(nested) => {
                v.push(nested.len().to_string());
            }
            other => panic!("symbol packet {} not covered", other.code()),
        },
        Packet::Irm(IrmPacket::Parameters { values }) => {
            v.extend(values.iter().map(ToString::to_string));
        }
        Packet::Irm(IrmPacket::StormCount { count }) => n(count),
        other => panic!("packet {} has no value check here", other.code()),
    }
    v
}

fn push_vectors(v: &mut Vec<String>, vectors: &Vectors) {
    match vectors {
        Vectors::Linked(points) => {
            for p in points {
                v.extend([p.i, p.j].map(|x| x.to_string()));
            }
        }
        Vectors::Unlinked(segments) => {
            for s in segments {
                v.extend([s.begin.i, s.begin.j, s.end.i, s.end.j].map(|x| x.to_string()));
            }
        }
        other => panic!("vectors {other:?}"),
    }
}

/// The golden walker's places and codes: every top-level packet of each
/// layer and page, followed by the packets nested in it.
fn golden_places(golden: &Json) -> Vec<(String, u16)> {
    let blocks = golden.get("blocks");
    let mut places = Vec::new();
    let symbology = blocks.get("symbology");
    if !symbology.is_null() {
        let nested = symbology.get("nested").items();
        for (layer, entry) in symbology.get("layers").items().iter().enumerate() {
            for (index, code) in common::packet_codes(entry.get("packets"))
                .iter()
                .enumerate()
            {
                let place = format!("s{layer}:{index}");
                places.push((place.clone(), *code));
                let inner = nested.iter().find(|n| {
                    n.get("layer").as_i64() == Some(layer as i64)
                        && n.get("index").as_i64() == Some(index as i64)
                });
                if let Some(inner) = inner {
                    for (k, code) in common::packet_codes(inner.get("packets"))
                        .iter()
                        .enumerate()
                    {
                        places.push((format!("{place}.{k}"), *code));
                    }
                }
            }
        }
    }
    let graphic = blocks.get("graphic");
    if !graphic.is_null() {
        for page in graphic.get("pages").items() {
            let number = page.get("page").int("page");
            for (index, code) in common::packet_codes(page.get("packets")).iter().enumerate() {
                places.push((format!("g{number}:{index}"), *code));
            }
        }
    }
    places
}

fn code_of(text: &str) -> u16 {
    match text.strip_prefix("0x") {
        Some(hex) => u16::from_str_radix(hex, 16).unwrap(),
        None => text.parse().unwrap(),
    }
}

#[test]
fn records_carry_every_display_packet() {
    let mut failures = Vec::new();
    let mut per_code: BTreeMap<u16, usize> = BTreeMap::new();
    let mut products = 0;
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = common::decode_golden_product(&entry, &golden) else {
            continue;
        };
        products += 1;
        let records = product.display_records().unwrap();
        let mut problems = Vec::new();

        // Structure: places and codes of the golden walker, generic
        // components aside (they follow their packet as `.c<k>` records).
        let ours: Vec<(String, u16)> = records
            .iter()
            .filter(|r| !r.split(' ').next().unwrap().contains(".c"))
            .map(|r| {
                let mut parts = r.splitn(3, ' ');
                (
                    parts.next().unwrap().to_owned(),
                    code_of(parts.next().unwrap()),
                )
            })
            .collect();
        // Product 62's cell trend data is page 0 of the graphic block, which
        // the walker records apart (`cell_trend`).
        let theirs = golden_places(&golden);
        let ours_without_trends: Vec<_> = if golden.get("blocks").get("cell_trend").is_null() {
            ours.clone()
        } else {
            ours.iter()
                .filter(|(p, _)| !p.starts_with("g0:"))
                .cloned()
                .collect()
        };
        if ours_without_trends != theirs {
            problems.push(format!(
                "places differ from the walker: {} records vs {} packets",
                ours_without_trends.len(),
                theirs.len()
            ));
        }

        // Values: walk the decoded packets in record order.
        let mut packets: Vec<&Packet> = Vec::new();
        fn walk<'a>(packet: &'a Packet, out: &mut Vec<&'a Packet>) {
            out.push(packet);
            if let Packet::Symbol(SymbolPacket::ScitPast(n) | SymbolPacket::ScitForecast(n)) =
                packet
            {
                for inner in n {
                    walk(inner, out);
                }
            }
        }
        for packet in product
            .symbology
            .iter()
            .flat_map(|s| s.layers.iter().flatten())
            .chain(
                product
                    .graphic
                    .iter()
                    .flat_map(|g| g.pages.iter())
                    .flat_map(|p| &p.packets),
            )
        {
            walk(packet, &mut packets);
        }
        let top: Vec<&String> = records
            .iter()
            .filter(|r| !r.split(' ').next().unwrap().contains(".c"))
            .collect();
        if top.len() != packets.len() {
            problems.push(format!(
                "{} records for {} packets",
                top.len(),
                packets.len()
            ));
        }
        let volume = product.to_volume().ok();
        for (record, packet) in top.iter().zip(&packets) {
            let mut parts = record.splitn(3, ' ');
            let (_place, code) = (parts.next().unwrap(), code_of(parts.next().unwrap()));
            let fields = parts.next().unwrap_or("");
            *per_code.entry(code).or_default() += 1;
            if code != packet.code() {
                problems.push(format!(
                    "{record}: code {code} for packet {}",
                    packet.code()
                ));
                continue;
            }
            match packet {
                Packet::Radial(_) | Packet::Raster(_) | Packet::DigitalPrecip(_) => {
                    // `sweep <n>` names a sweep holding this packet's array.
                    let n: usize = fields.strip_prefix("sweep ").unwrap().parse().unwrap();
                    let sweep = volume.as_ref().and_then(|v| v.sweeps.get(n));
                    let held = sweep.is_some_and(|s| {
                        s.fields[0].attrs.other.iter().any(|(name, value)| {
                            &**name == "level3_packet_code"
                                && value.as_f64() == Some(f64::from(code))
                        })
                    });
                    if !held {
                        problems.push(format!("{record}: sweep {n} does not hold it"));
                    }
                }
                Packet::Generic(generic) => {
                    // Its components follow as `.c<k>` records; radial ones
                    // name their sweeps.
                    let place = record.split(' ').next().unwrap();
                    let components: Vec<&String> = records
                        .iter()
                        .filter(|r| r.starts_with(&format!("{place}.c")))
                        .collect();
                    if components.len() != generic.components.len() {
                        problems.push(format!("{record}: component records"));
                    }
                    for (component, text) in generic.components.iter().zip(components) {
                        let rest = text.splitn(3, ' ').nth(2).unwrap();
                        match component {
                            GenericComponent::Radial(_) => {
                                let n: usize =
                                    rest.strip_prefix("radial sweep ").unwrap().parse().unwrap();
                                let ok = volume.as_ref().and_then(|v| v.sweeps.get(n)).is_some_and(
                                    |s| {
                                        matches!(
                                            s.fields[0].data,
                                            FieldData::U16 { .. } | FieldData::I32 { .. }
                                        )
                                    },
                                );
                                if !ok {
                                    problems.push(format!("{text}: no generic sweep {n}"));
                                }
                            }
                            GenericComponent::Text {
                                text: t,
                                parameters,
                            } => {
                                let got = values(rest.strip_prefix("text ").unwrap());
                                let mut want: Vec<String> = Vec::new();
                                for p in parameters {
                                    want.push(p.id.clone());
                                    want.push(p.attributes.clone());
                                }
                                want.push(t.clone());
                                if got != want {
                                    problems.push(format!("{text}: {got:?} != {want:?}"));
                                }
                            }
                            GenericComponent::Grid(_) => {
                                problems.push("grid component in the corpus: check it".into());
                            }
                            other => problems.push(format!("unexpected component {other:?}")),
                        }
                    }
                }
                Packet::Unknown { .. } => problems.push(format!("{record}: unknown packet")),
                _ => {
                    let got = values(fields);
                    let want = expected(packet);
                    if got != want {
                        let at = got.iter().zip(&want).position(|(a, b)| a != b);
                        problems.push(format!(
                            "{}: {} values vs {} expected, first difference at {at:?}",
                            &record[..record.len().min(80)],
                            got.len(),
                            want.len()
                        ));
                    }
                }
            }
        }

        // The volume carries the records.
        if let Some(volume) = &volume {
            let carried = volume
                .attrs
                .other
                .iter()
                .find(|(name, _)| &**name == "level3_display_packets")
                .map(|(_, value)| value.clone());
            let want = (!records.is_empty()).then(|| {
                AttrValue::Array(recast_radar_core::model::ArrayBuf::Text(
                    records.iter().map(|r| r.as_str().into()).collect(),
                ))
            });
            if carried != want {
                problems.push("volume attribute level3_display_packets".into());
            }
        }
        if !problems.is_empty() {
            failures.push(format!("{}:\n    {}", entry.id, problems.join("\n    ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!("{products} products; records per code {per_code:?}");
    // Every display packet code of the corpus is rendered.
    for code in [
        1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 28,
        30, 31, 32, 0x0802, 0x0E03, 0xAF1F, 0xBA07,
    ] {
        assert!(
            per_code.get(&code).is_some_and(|&n| n > 0),
            "no record of packet {code}"
        );
    }
}

/// The record grammar on hand-made strings: quoting, escapes and splitting.
#[test]
fn record_values_split_and_unescape() {
    assert_eq!(
        values(r#"3 -4 "A B\0\u{80}\"""#),
        ["3", "-4", "A B\0\u{80}\""]
    );
    assert_eq!(values("12,34 56,78"), ["12", "34", "56", "78"]);
    assert_eq!(
        values(r#"parameters=["id"="a=b;c=d"] components=2"#),
        ["id", "a=b;c=d", "2"]
    );
    assert_eq!(values("5:1:10,20,30"), ["5", "1", "10", "20", "30"]);
}

/// The record size limit (a product past it is refused, not truncated) is
/// far above every corpus product.
#[test]
fn record_limit_is_far_above_the_corpus() {
    let mut largest = 0;
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = common::decode_golden_product(&entry, &golden) else {
            continue;
        };
        let bytes: usize = product
            .display_records()
            .unwrap()
            .iter()
            .map(|r| r.len() + 1)
            .sum();
        largest = largest.max(bytes);
    }
    assert!(largest > 0);
    assert!(
        largest * 100 < recast_radar_io_level3::records::MAX_RECORD_BYTES,
        "{largest}"
    );
}

/// Every value the Level III volume keeps without a typed slot (root
/// `attrs.other`, including the display packet records, and each sweep's
/// `other`) reaches the FM301 view verbatim with `Passthrough::All`, for
/// every corpus product that has a volume.
#[test]
fn fm301_view_carries_every_level3_attribute() {
    use recast_radar_core::fm301::{self, Passthrough, ViewOptions};

    let all = ViewOptions {
        passthrough: Passthrough::All,
        ..ViewOptions::XRADAR
    };
    let mut volumes = 0;
    for entry in common::level3_manifest() {
        let golden = entry.golden();
        let Some(product) = common::decode_golden_product(&entry, &golden) else {
            continue;
        };
        let Ok(volume) = product.to_volume() else {
            continue;
        };
        volumes += 1;
        let view = fm301::volume_view(&volume, all, None).unwrap();
        for (name, value) in &volume.attrs.other {
            assert_eq!(view.root.attr(name), Some(value), "{}: {name}", entry.id);
        }
        for (index, sweep) in volume.sweeps.iter().enumerate() {
            let group = view.group(&format!("sweep_{index}")).unwrap();
            for (name, value) in &sweep.other {
                assert_eq!(
                    group.attr(name),
                    Some(value),
                    "{} sweep {index}: {name}",
                    entry.id
                );
            }
        }
    }
    assert!(volumes > 190, "{volumes}");
}
