//! International feed frames: their times, the frames nearest a time, and
//! the names their parts are saved under.
//!
//! A `recast_radar_data` frame plan carries an identity and part URLs but
//! no time. Every provider with an archive (SMHI, NCI Australia, EUMETNET
//! ORD) and most of the others put the scan time in the identity
//! (`radar_angelholm_qcvol_202606120625`, `australia-nci_2_2_20260625_000000.pvol.h5`,
//! `nlhrw_20260612T1455_p1_h...`), so [`frame_time`] reads it from there.
//! `fetch intl --date --time` in the command-line tool and
//! `recast_radar.fetch.intl(when=...)` in the Python package choose frames
//! with it.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};

/// How far either side of a requested time an archive is searched: an hour,
/// or ten minutes per requested frame when that is more.
pub fn archive_search_span(count: usize) -> Duration {
    // At most about a week either side, far inside chrono's range.
    let minutes = count.min(1000) as i64 * 10;
    Duration::minutes(minutes.max(60))
}

/// The scan time in a frame identity or file name: the first run of digits
/// that starts with a `YYYYMMDD` date and continues, directly or after one
/// `_`, `-` or `T`, with `HHMM` or `HHMMSS` (longer runs, such as ANM's
/// `YYYYMMDDHHMMSScc`, are read to the seconds). `None` when there is none.
pub fn frame_time(text: &str) -> Option<DateTime<Utc>> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() || (start > 0 && bytes[start - 1].is_ascii_digit()) {
            start += 1;
            continue;
        }
        let run = digit_run(bytes, start);
        if let Some(time) = stamp_at(bytes, start, run) {
            return Some(time);
        }
        start += run.max(1);
    }
    None
}

/// Length of the run of ASCII digits at `start`.
fn digit_run(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

fn digits(bytes: &[u8], start: usize, len: usize) -> Option<&str> {
    std::str::from_utf8(bytes.get(start..start + len)?).ok()
}

fn stamp_at(bytes: &[u8], start: usize, run: usize) -> Option<DateTime<Utc>> {
    if run < 8 {
        return None;
    }
    let date = NaiveDate::parse_from_str(digits(bytes, start, 8)?, "%Y%m%d").ok()?;
    let (time_start, time_len) = if run >= 12 {
        (start + 8, run - 8)
    } else if run == 8 && matches!(bytes.get(start + 8), Some(b'_' | b'-' | b'T')) {
        let time_start = start + 9;
        let len = bytes
            .get(time_start)
            .filter(|byte| byte.is_ascii_digit())
            .map_or(0, |_| digit_run(bytes, time_start));
        (time_start, len)
    } else {
        return None;
    };
    let time = match time_len {
        4 | 5 => NaiveTime::parse_from_str(digits(bytes, time_start, 4)?, "%H%M").ok()?,
        len if len >= 6 => {
            NaiveTime::parse_from_str(digits(bytes, time_start, 6)?, "%H%M%S").ok()?
        }
        _ => return None,
    };
    Some(date.and_time(time).and_utc())
}

/// Indices of the `count` entries of `times` nearest `target`, in their
/// original order. Entries without a time come after every timed one.
pub fn nearest(times: &[Option<DateTime<Utc>>], target: DateTime<Utc>, count: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..times.len()).collect();
    order.sort_by_key(|&index| {
        times[index].map_or(u64::MAX, |time| {
            (time - target).num_seconds().unsigned_abs()
        })
    });
    order.truncate(count);
    order.sort_unstable();
    order
}

/// Characters kept in saved file names; anything else becomes `_`.
fn safe_name(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._@-+".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_owned()
}

/// A file name from the last segment of a URL path (`%40` read as `@`),
/// restricted to safe characters.
pub fn url_file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    safe_name(&last.replace("%40", "@"))
}

/// The names the parts of one frame are saved under, in part order.
///
/// A part keeps its URL's file name when that name has an extension and a
/// scan time ([`frame_time`]) and no other part of the frame has it. Any
/// other part is named after the frame identity (`<identity>`, or
/// `<identity>-part<N>` when the frame has several parts), so a name
/// always stands for one upstream file: the identity is stable for a frame
/// by the provider contract, and a dated upstream name is too. That is what
/// lets a download keep a file that is already there under its name.
pub fn part_file_names(identity: &str, urls: &[&str]) -> Vec<String> {
    let base = safe_name(identity);
    let base = if base.is_empty() {
        "frame".to_owned()
    } else {
        base
    };
    let mut names: Vec<String> = Vec::with_capacity(urls.len());
    for (index, url) in urls.iter().enumerate() {
        let name = url_file_name(url);
        let keep = !name.is_empty()
            && name.contains('.')
            && frame_time(&name).is_some()
            && !names.contains(&name);
        names.push(if keep {
            name
        } else if urls.len() == 1 {
            base.clone()
        } else {
            format!("{base}-part{index}")
        });
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Option<String> {
        frame_time(text).map(|time| time.format("%Y-%m-%dT%H:%M:%S").to_string())
    }

    #[test]
    fn provider_identities_give_their_scan_times() {
        // SMHI, NCI Australia, EUMETNET ORD, DWD, Lombardia, ANM Romania,
        // GeoSphere identities (from the provider tests of recast-radar-data).
        assert_eq!(
            at("radar_angelholm_qcvol_202606120625").as_deref(),
            Some("2026-06-12T06:25:00")
        );
        assert_eq!(
            at("australia-nci_2_2_20260625_000000.pvol.h5").as_deref(),
            Some("2026-06-25T00:00:00")
        );
        assert_eq!(
            at("nlhrw_20260612T1455_p1_h0123").as_deref(),
            Some("2026-06-12T14:55:00")
        );
        assert_eq!(
            at("asb_20260612064402_p20_h99").as_deref(),
            Some("2026-06-12T06:44:02")
        );
        assert_eq!(
            at("des_20260626T230000Z_p5_h1").as_deref(),
            Some("2026-06-26T23:00:00")
        );
        assert_eq!(
            at("BUC_2026070718400200_p5_h1").as_deref(),
            Some("2026-07-07T18:40:02")
        );
        assert_eq!(
            at("WXRHOF_202606120635.hdf").as_deref(),
            Some("2026-06-12T06:35:00")
        );
        assert_eq!(at("kaia-104-1"), None);
        assert_eq!(at("20261399_0000"), None);
        assert_eq!(at("20260612"), None);
    }

    #[test]
    fn nearest_frames_come_back_in_their_order() {
        let t = |text| frame_time(text);
        let times = [
            t("x_202606120600"),
            None,
            t("x_202606120610"),
            t("x_202606120620"),
            t("x_202606120630"),
        ];
        let target = frame_time("x_202606120618").unwrap_or_default();
        assert_eq!(nearest(&times, target, 2), [2, 3]);
        assert_eq!(nearest(&times, target, 5), [0, 1, 2, 3, 4]);
        assert_eq!(nearest(&times, target, 0), Vec::<usize>::new());
    }

    #[test]
    fn part_names_are_dated_upstream_names_or_the_identity() {
        assert_eq!(
            part_file_names(
                "dkste_202606120635",
                &["https://opendataapi.dmi.dk/v1/radardata/download/dkste_202606120635.vol.h5"]
            ),
            ["dkste_202606120635.vol.h5"]
        );
        // KAIA's file URLs end in a file number: the identity names them.
        assert_eq!(
            part_file_names(
                "HAR_20260612T0635.h5",
                &["https://avaandmed.keskkonnaportaal.ee/api/lists/active/items/104/files/1"]
            ),
            ["HAR_20260612T0635.h5"]
        );
        // An undated name could stand for a different scan next time.
        assert_eq!(
            part_file_names("s_202606120635", &["https://x.invalid/latest.h5"]),
            ["s_202606120635"]
        );
        assert_eq!(
            part_file_names(
                "s:1/2",
                &[
                    "https://x.invalid/a_20260612T0635.h5",
                    "https://y.invalid/a_20260612T0635.h5"
                ]
            ),
            ["a_20260612T0635.h5", "s_1_2-part1"]
        );
        assert_eq!(
            url_file_name("https://example.org/a/..%2f..%2fx"),
            "_2f.._2fx"
        );
        assert_eq!(
            url_file_name("https://example.org/bejab%4020260612T1450%40DBZH.h5"),
            "bejab@20260612T1450@DBZH.h5"
        );
    }

    #[test]
    fn the_search_span_grows_with_the_count() {
        assert_eq!(archive_search_span(1), Duration::hours(1));
        assert_eq!(archive_search_span(12), Duration::minutes(120));
        assert_eq!(archive_search_span(usize::MAX), Duration::minutes(10_000));
    }
}
