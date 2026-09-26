//! NEXRAD Level III products in the public `unidata-nexrad-level3` bucket
//! on AWS.
//!
//! The bucket is flat: one object per product, keyed
//! `<SSS>_<PPP>_<YYYY>_<MM>_<DD>_<hh>_<mm>_<ss>` with the 3-letter radar id,
//! the product mnemonic and the volume scan time, for example
//! `TLX_N0B_2024_03_15_00_02_17`. Listing a site, product and UTC date is one
//! prefix query.

#[cfg(feature = "net")]
use chrono::Duration;
use chrono::{DateTime, NaiveDate, Utc};

#[cfg(feature = "net")]
use crate::{DataSourceError, Result, S3Object};

/// The public Level III bucket.
pub const LEVEL3_ARCHIVE_BUCKET: &str = "unidata-nexrad-level3";

/// Most listing pages read for one site, product and date (a busy product
/// has about 300 objects a day; a page holds 1000).
#[cfg(feature = "net")]
const MAX_LISTING_PAGES: usize = 10;

/// The 3-letter id the bucket uses: `KTLX` and `TLX` both give `TLX`.
/// Four-letter ids drop their first letter (the ICAO region prefix); other
/// lengths are kept.
pub fn level3_site_id(site: &str) -> String {
    let site = site.trim().to_ascii_uppercase();
    if site.len() == 4 && site.is_ascii() {
        site[1..].to_owned()
    } else {
        site
    }
}

/// Key prefix of one site, product and UTC date:
/// `TLX_N0B_2024_03_15_`.
pub fn level3_key_prefix(site: &str, product: &str, date: NaiveDate) -> String {
    format!(
        "{}_{}_{}_",
        level3_site_id(site),
        product.trim().to_ascii_uppercase(),
        date.format("%Y_%m_%d")
    )
}

/// The volume scan time in a key, `None` when the key does not follow the
/// bucket's layout.
pub fn level3_key_time(key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    let mut parts = name.split('_');
    let _site = parts.next()?;
    let _product = parts.next()?;
    let fields: Vec<u32> = parts.map(|part| part.parse().ok()).collect::<Option<_>>()?;
    let [year, month, day, hour, minute, second] = fields[..] else {
        return None;
    };
    NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?
        .and_hms_opt(hour, minute, second)
        .map(|naive| naive.and_utc())
}

/// HTTPS URL of an object in the bucket.
pub fn level3_object_url(key: &str) -> String {
    format!("https://{LEVEL3_ARCHIVE_BUCKET}.s3.amazonaws.com/{key}")
}

/// Every product of `site` and `product` on one UTC date, oldest first.
#[cfg(feature = "net")]
pub fn level3_objects_for_date(
    site: &str,
    product: &str,
    date: NaiveDate,
) -> Result<Vec<S3Object>> {
    let prefix = level3_key_prefix(site, product, date);
    let listing = crate::list_s3_all_limited(
        LEVEL3_ARCHIVE_BUCKET,
        &prefix,
        None,
        None,
        MAX_LISTING_PAGES,
    )?;
    let mut objects: Vec<S3Object> = listing
        .contents
        .into_iter()
        .filter(|object| object.size > 0 && level3_key_time(&object.key).is_some())
        .collect();
    objects.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(objects)
}

/// Up to `max_count` of the newest products of `site` and `product`,
/// looking back `days_back` UTC dates before today; newest first.
#[cfg(feature = "net")]
pub fn recent_level3_objects(
    site: &str,
    product: &str,
    days_back: i64,
    max_count: usize,
) -> Result<Vec<S3Object>> {
    let today = Utc::now().date_naive();
    let mut recent = Vec::new();
    for offset in 0..=days_back.max(0) {
        if recent.len() >= max_count {
            break;
        }
        let date = today - Duration::days(offset);
        let mut objects = level3_objects_for_date(site, product, date)?;
        objects.reverse();
        recent.extend(objects.into_iter().take(max_count - recent.len()));
    }
    if recent.is_empty() && max_count > 0 {
        return Err(DataSourceError::NoObjects {
            bucket: LEVEL3_ARCHIVE_BUCKET.to_owned(),
            prefix: format!(
                "{}_{}",
                level3_site_id(site),
                product.trim().to_ascii_uppercase()
            ),
        });
    }
    Ok(recent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_ids_are_the_three_letter_form() {
        assert_eq!(level3_site_id("KTLX"), "TLX");
        assert_eq!(level3_site_id("tlx"), "TLX");
        assert_eq!(level3_site_id("PAHG"), "AHG");
    }

    /// Keys from a live listing of the bucket (2026-09-24), prefix
    /// `TLX_N0B_2024_03_15_00`.
    #[test]
    fn keys_carry_the_volume_scan_time() {
        let date = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        assert_eq!(
            level3_key_prefix("KTLX", "n0b", date),
            "TLX_N0B_2024_03_15_"
        );
        let time = level3_key_time("TLX_N0B_2024_03_15_00_02_17").unwrap();
        assert_eq!(time.to_rfc3339(), "2024-03-15T00:02:17+00:00");
        assert_eq!(level3_key_time("TLX_N0B_2024_03_15"), None);
        assert_eq!(level3_key_time("TLX_N0B_2024_13_15_00_02_17"), None);
        assert_eq!(
            level3_object_url("TLX_N0B_2024_03_15_00_02_17"),
            "https://unidata-nexrad-level3.s3.amazonaws.com/TLX_N0B_2024_03_15_00_02_17"
        );
    }
}
