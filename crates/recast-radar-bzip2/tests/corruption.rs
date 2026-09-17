//! Corrupt input: seeded truncations, bit flips and byte bursts of real LDM
//! records must never panic, and must either fail or decode exactly as the
//! reference does. With CRC checks off, the bytes emitted before a failure
//! and the block where decoding stops must also match the reference
//! (`common::check_case`).

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::{Rng, Tally, VOLUMES, check_case, reference_decode, volume_records};
use recast_radar_bzip2::Decoder;

/// Records under test: the first two, the last, and every eighth record of
/// each volume, so both the short metadata records and full radial records
/// are covered.
fn picks(n: usize) -> Vec<usize> {
    (0..n)
        .filter(|&i| i < 2 || i == n - 1 || i % 8 == 0)
        .collect()
}

#[test]
fn seeded_truncations_never_panic_and_match_or_err() {
    let mut rng = Rng(0x5eed_b22b_0001);
    let mut dec = Decoder::new();
    let mut t = Tally::default();
    for &(id, count) in VOLUMES {
        let Some(records) = volume_records(id, count) else {
            continue;
        };
        for i in picks(records.len()) {
            let r = &records[i];
            let len = r.len();
            // Tiny prefixes, around the headers, random points, near the end.
            let mut cuts: Vec<usize> = vec![0, 1, 3, 4, 5, 10, 14, 20, 40, 100];
            for _ in 0..8 {
                cuts.push(rng.below(len));
            }
            for k in 1..=6 {
                cuts.push(len.saturating_sub(k));
            }
            for c in cuts {
                let c = c.min(len);
                check_case(
                    &mut dec,
                    &r[..c],
                    &mut t,
                    &format!("{id} record {i} cut to {c}"),
                );
            }
        }
    }
    eprintln!("truncations: {t:?}");
    assert_eq!(
        t.ours_err_ref_ok, 0,
        "we rejected inputs the reference accepts"
    );
    assert!(t.cases == 0 || t.cases > 200, "partial corpus: {t:?}");
}

#[test]
fn seeded_bit_flips_never_panic_and_match_or_err() {
    let mut rng = Rng(0x5eed_b22b_0002);
    let mut dec = Decoder::new();
    let mut t = Tally::default();
    for &(id, count) in VOLUMES {
        let Some(records) = volume_records(id, count) else {
            continue;
        };
        for i in picks(records.len()) {
            let r = &records[i];
            let len = r.len();
            // Single bit flips: header and table region, anywhere, last bytes.
            let mut flips: Vec<usize> = Vec::new();
            for _ in 0..8 {
                flips.push(rng.below(len.min(700) * 8));
            }
            for _ in 0..8 {
                flips.push(rng.below(len * 8));
            }
            for _ in 0..3 {
                flips.push((len - 1 - rng.below(len.min(12))) * 8 + rng.below(8));
            }
            for bit in flips {
                let mut m = r.clone();
                m[bit / 8] ^= 0x80 >> (bit % 8);
                check_case(
                    &mut dec,
                    &m,
                    &mut t,
                    &format!("{id} record {i} bit {bit} flipped"),
                );
            }
            // Burst corruption: 8 random bytes.
            for _ in 0..2 {
                let mut m = r.clone();
                let at = rng.below(len);
                for b in m[at..(at + 8).min(len)].iter_mut() {
                    *b = rng.next() as u8;
                }
                check_case(
                    &mut dec,
                    &m,
                    &mut t,
                    &format!("{id} record {i} burst at {at}"),
                );
            }
        }
    }
    eprintln!("bit flips: {t:?}");
    assert_eq!(
        t.ours_err_ref_ok, 0,
        "we rejected inputs the reference accepts"
    );
    if t.cases > 0 {
        assert!(t.cases > 200, "{t:?}");
        assert!(
            t.nocrc_equal > 50,
            "enough structurally valid corruptions: {t:?}"
        );
    }
}

/// The clean side of a paired call is unaffected by a corrupt other side,
/// and the corrupt side agrees with the reference.
#[test]
fn paired_decode_with_one_corrupt_side() {
    let mut rng = Rng(0x5eed_b22b_0003);
    let mut dec = Decoder::new();
    let mut cases = 0;
    for &(id, count) in VOLUMES {
        let Some(records) = volume_records(id, count) else {
            continue;
        };
        let n = records.len();
        for i in picks(n).into_iter().take(6) {
            let clean = &records[(i + 1) % n];
            let clean_expected = reference_decode(clean).expect("reference");
            let mut bad = records[i].clone();
            let bit = rng.below(bad.len() * 8);
            bad[bit / 8] ^= 0x80 >> (bit % 8);
            for corrupt_first in [true, false] {
                let (mut oa, mut ob) = (vec![1u8, 2, 3], vec![9u8]);
                let (ra, rb) = if corrupt_first {
                    catch_unwind(AssertUnwindSafe(|| {
                        dec.decode_two_into(&bad, &mut oa, clean, &mut ob)
                    }))
                } else {
                    catch_unwind(AssertUnwindSafe(|| {
                        dec.decode_two_into(clean, &mut ob, &bad, &mut oa)
                    }))
                    .map(|(rb, ra)| (ra, rb))
                }
                .unwrap_or_else(|_| panic!("panic in paired decode, {id} record {i} bit {bit}"));
                match (ra, reference_decode(&bad)) {
                    (Ok(()), Ok(v)) => assert!(oa[3..] == v[..], "{id} record {i}"),
                    (Ok(()), Err(e)) => {
                        panic!("paired: accepted what the reference rejects ({e}), {id} record {i}")
                    }
                    (Err(_), _) => assert_eq!(oa, [1, 2, 3], "output restored on error"),
                }
                rb.unwrap_or_else(|e| panic!("clean side of {id} record {i}: {e}"));
                assert!(ob[1..] == clean_expected[..], "{id}: clean side output");
                cases += 1;
            }
        }
    }
    eprintln!("paired corrupt-side cases: {cases}");
}
