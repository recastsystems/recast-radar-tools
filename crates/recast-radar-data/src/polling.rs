//! GR2Analyst polling servers: `dir.list` and site configuration files, and
//! the client side of polling one.
//!
//! A polling server (GR2Analyst's own convention, used by the Iowa
//! Environmental Mesonet and North Dakota State Water Commission hosts in
//! [`crate::community_feeds`]) publishes one directory per site:
//!
//! ```text
//! {root}/config.cfg            "Site: XXXX" lines, one per site
//! {root}/grlevel2.cfg          same format
//! {root}/{SITE}/dir.list       "<size> <file name>" lines, oldest first
//! {root}/{SITE}/{file name}    Level II volumes
//! ```
//!
//! Two ways in:
//!
//! - By server root and site id, the way a polling client is configured:
//!   [`parse_dir_list`] and [`parse_site_config`] return what a client can
//!   use (entries and site ids that are plain file names, see "File names"),
//!   [`dir_list_url`], [`site_file_url`] and [`site_config_url`] build the
//!   URLs, and with `net`, `fetch_dir_list(root, site)` and
//!   `fetch_site_config(root)` fetch and parse.
//! - By site directory URL, for feeds published as one directory
//!   ([`crate::community_feeds`]): [`DirList::parse`] and
//!   [`SiteConfig::parse`] keep every line as listed (odd lines in
//!   [`DirList::skipped`]), [`newest_volume_entry`] picks the newest volume
//!   and [`entry_url`] builds its URL; with `net`, `fetch_dir_list_at`,
//!   `fetch_site_config_at`, `latest_volume` and
//!   `latest_volume_or_single_site` fetch. A poll URL can also be a root
//!   whose `grlevel2.cfg` names a single site (the Laredo EWR feed in
//!   [`crate::community_feeds`] is one): [`single_site_url`] and
//!   `latest_volume_or_single_site` find that site's directory.
//!
//! Observed in the feed survey of 2026-09-24 and 2026-09-25
//! (`docs/testdata/feeds-survey.md`): the Iowa Environmental Mesonet's and
//! North Dakota SWC's files end their lines in LF (the parsers take CRLF
//! too), and a site configuration can hold other keys (`ListFile: dir.list`)
//! beside its `Site:` lines. A listing may name entries that are not volumes
//! (a `.tmp` file still being written, a hidden state file whose name starts
//! with a dot), so [`newest_volume_entry`] skips those. File names do not
//! reliably say whether a file is compressed, so callers should sniff the
//! downloaded bytes, as `recast_radar_io::read_supported_volume_bytes` does.
//! The listed size is the server's number, not always a byte count: North
//! Dakota SWC listed 40,288 for a 20,616,906-byte volume.
//!
//! One odd line does not stop a site from being polled: a line that is not
//! `<size> <name>` is kept in [`DirList::skipped`] (and left out by
//! [`parse_dir_list`]) and the rest of the listing is read, in the same
//! spirit as [`newest_volume_entry`] skipping names that are not volumes.
//!
//! # Limits
//!
//! A listing holds at most [`MAX_DIR_LIST_ENTRIES`] entries, a site
//! configuration at most [`MAX_CONFIG_SITES`] sites, and a line of either at
//! most [`MAX_DIR_LIST_LINE_BYTES`] bytes. [`DirList::parse`] and
//! [`SiteConfig::parse`] refuse longer input with an error rather than
//! allocate without bound or return a silently shortened list;
//! [`parse_dir_list`] and [`parse_site_config`], which never fail, return
//! nothing for such input. Duplicate `Site:` lines are found with a hash set,
//! so a hostile configuration costs time in proportion to its length. With
//! `net`, the fetches are capped too: a `dir.list` is read to at most the
//! crate's listing limit (32 MiB), a site configuration to at most its
//! text-resource limit (16 MiB), and a response over the limit is refused
//! while it streams.
//!
//! # File names
//!
//! Listed names and site ids come from the server and go into request URLs
//! and, in a client, local paths. A name is used only when it is a plain file
//! name ([`is_safe_file_name`]): made of RFC 3986 unreserved characters
//! (ASCII letters and digits, `-`, `.`, `_`, `~`), not starting or ending
//! with a dot, and not a Windows device name (`CON`, `PRN`, `AUX`, `NUL`,
//! `COM0`-`COM9`, `LPT0`-`LPT9`, in any letter case, alone or before a
//! dot, as in `nul.ar2v`). Every name the feed survey read in IEM's and ND
//! SWC's listings of 2026-09-24 and 2026-09-25 is one. Other characters (`/`, `\`, `?`, `#`, `%`, `:`, spaces,
//! non-ASCII) could step out of the site directory or change the request,
//! since URL parsers treat `\` as `/` in `http(s)` URLs; a device name opens
//! a device on Windows rather than a file, and Windows drops a trailing dot,
//! so `x.ar2v.` would be stored as `x.ar2v`. So [`parse_dir_list`] and
//! [`parse_site_config`] leave out every name that is not plain,
//! [`newest_volume_entry`] skips it, and [`entry_url`] and
//! [`single_site_url`] refuse it. [`parse_dir_list`] and
//! [`parse_site_config`] also keep each name once whatever its letter case
//! (the first listed wins): `kxwa` and `KXWA` are two URLs on the server but
//! one directory on a case-insensitive file system, such as the default ones
//! of Windows and macOS. [`DirList::parse`] and [`SiteConfig::parse`] keep
//! every name as listed, so that a caller can report them.

use std::collections::HashSet;
use std::fmt;

/// Most entries a `dir.list` may have (entries and skipped lines together).
/// North Dakota SWC's KXWA listed 1,161 on 2026-09-25 (about 3.6 days of
/// volumes).
pub const MAX_DIR_LIST_ENTRIES: usize = 100_000;

/// Longest `dir.list` or site configuration line accepted, in bytes.
pub const MAX_DIR_LIST_LINE_BYTES: usize = 1024;

/// Most distinct sites a site configuration may name.
pub const MAX_CONFIG_SITES: usize = 10_000;

/// One `dir.list` line: a file in the site directory.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirListEntry {
    /// Size as listed, not always bytes (see the module documentation).
    pub size: u64,
    /// File name relative to the site directory, as listed. From
    /// [`DirList::parse`] it may contain spaces or other characters that
    /// [`is_safe_file_name`] refuses; [`parse_dir_list`] leaves such entries
    /// out, [`newest_volume_entry`] skips them and [`entry_url`] refuses
    /// them.
    pub name: String,
}

impl DirListEntry {
    /// An entry naming `name`, listed at `size`.
    pub fn new(size: u64, name: impl Into<String>) -> Self {
        Self {
            size,
            name: name.into(),
        }
    }
}

/// A `dir.list` line that is not `<size> <name>` (a single word, or words
/// whose first is not a size).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkippedLine {
    /// 1-based line number.
    pub line: usize,
    /// The line, cut to 80 characters.
    pub text: String,
}

/// A `dir.list` as listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirList {
    /// The listed files, in listing order (oldest first by convention), names
    /// as listed.
    pub entries: Vec<DirListEntry>,
    /// Lines that could not be read as an entry, in listing order. They are
    /// kept so that a caller can report them; the entries are still usable.
    pub skipped: Vec<SkippedLine>,
}

impl DirList {
    /// Parse a `dir.list`: one `<size> <name>` entry per line, in listing
    /// order (oldest first by convention). Blank lines are ignored; in a line
    /// of two or more words whose first word is a size, the rest of the line
    /// (trimmed) is the name, so a name with a space is read as listed (and
    /// then refused as a URL component, see [`is_safe_file_name`]). Any
    /// other line is kept in [`DirList::skipped`] and does not stop the
    /// parse. CRLF and LF line ends are both accepted, and a UTF-8
    /// byte-order mark is ignored.
    ///
    /// # Errors
    ///
    /// [`PollingError::LimitExceeded`] when a line is longer than
    /// [`MAX_DIR_LIST_LINE_BYTES`] or the listing has more than
    /// [`MAX_DIR_LIST_ENTRIES`] entries and skipped lines together: the
    /// listing is refused rather than cut short.
    pub fn parse(text: &str) -> Result<Self, PollingError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut list = Self::default();
        for (index, line) in text.lines().enumerate() {
            if line.len() > MAX_DIR_LIST_LINE_BYTES {
                return Err(PollingError::LimitExceeded(format!(
                    "dir.list line {} is {} bytes (limit {MAX_DIR_LIST_LINE_BYTES})",
                    index + 1,
                    line.len()
                )));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if list.entries.len() + list.skipped.len() == MAX_DIR_LIST_ENTRIES {
                return Err(PollingError::LimitExceeded(format!(
                    "dir.list has more than {MAX_DIR_LIST_ENTRIES} entries"
                )));
            }
            let entry = trimmed
                .split_once(char::is_whitespace)
                .and_then(|(first, rest)| Some((first.parse::<u64>().ok()?, rest.trim_start())));
            match entry {
                Some((size, name)) => list.entries.push(DirListEntry::new(size, name)),
                None => list.skipped.push(SkippedLine {
                    line: index + 1,
                    text: line.chars().take(80).collect(),
                }),
            }
        }
        Ok(list)
    }

    /// The entries whose names are plain file names ([`is_safe_file_name`]),
    /// in listing order, each name once whatever its letter case (the first
    /// listed wins, see the module's "File names" section): the ones a
    /// client may request or store.
    pub fn into_safe_entries(self) -> Vec<DirListEntry> {
        let mut seen = HashSet::new();
        self.entries
            .into_iter()
            .filter(|entry| {
                is_safe_file_name(&entry.name) && seen.insert(entry.name.to_ascii_lowercase())
            })
            .collect()
    }
}

/// The site ids of a site configuration (`config.cfg` or `grlevel2.cfg`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SiteConfig {
    /// Every `Site:` value, in file order, each once, as listed.
    pub sites: Vec<String>,
}

impl SiteConfig {
    /// Parse a site configuration: every `Site: XXXX` line, in file order,
    /// without duplicates. Other lines are ignored; the key is matched
    /// without regard to case. CRLF and LF line ends are both accepted, and a
    /// UTF-8 byte-order mark is ignored.
    ///
    /// # Errors
    ///
    /// [`PollingError::LimitExceeded`] when the file names more than
    /// [`MAX_CONFIG_SITES`] distinct sites or has a line longer than
    /// [`MAX_DIR_LIST_LINE_BYTES`]: the list is refused rather than cut
    /// short.
    pub fn parse(text: &str) -> Result<Self, PollingError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut sites: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (index, line) in text.lines().enumerate() {
            if line.len() > MAX_DIR_LIST_LINE_BYTES {
                return Err(PollingError::LimitExceeded(format!(
                    "site configuration line {} is {} bytes (limit {MAX_DIR_LIST_LINE_BYTES})",
                    index + 1,
                    line.len()
                )));
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("site") {
                continue;
            }
            let site = value.trim();
            if site.is_empty() || seen.contains(site) {
                continue;
            }
            if sites.len() == MAX_CONFIG_SITES {
                return Err(PollingError::LimitExceeded(format!(
                    "site configuration names more than {MAX_CONFIG_SITES} sites"
                )));
            }
            seen.insert(site);
            sites.push(site.to_owned());
        }
        Ok(Self { sites })
    }

    /// The site ids that are plain file names ([`is_safe_file_name`]), in
    /// file order, each once whatever its letter case (the first listed
    /// wins, see the module's "File names" section): the ones a client may
    /// put into a URL or a local path.
    pub fn into_safe_sites(self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.sites
            .into_iter()
            .filter(|site| is_safe_file_name(site) && seen.insert(site.to_ascii_lowercase()))
            .collect()
    }
}

/// Why a polling file could not be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum PollingError {
    /// The listing has more than [`MAX_DIR_LIST_ENTRIES`] lines that are not
    /// blank (entries and skipped lines together), the site
    /// configuration more than [`MAX_CONFIG_SITES`] sites, or a line is
    /// longer than [`MAX_DIR_LIST_LINE_BYTES`].
    LimitExceeded(String),
    /// A listed name or site id that is not a plain file name
    /// ([`is_safe_file_name`]), so no URL is built from it.
    UnsafeName {
        /// The name, cut to 80 characters.
        name: String,
    },
    /// The listing names no volume.
    NoVolume {
        /// The listing's URL or description.
        listing: String,
    },
    /// A polling root's site configuration does not name exactly one site,
    /// so it does not stand for one site directory ([`single_site_url`]).
    NotSingleSite {
        /// The root's URL.
        root: String,
        /// How many sites the configuration names.
        sites: usize,
    },
    /// The request for a polling file failed.
    #[cfg(feature = "net")]
    Fetch(crate::DataSourceError),
}

impl fmt::Display for PollingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded(what) => write!(f, "polling file limit exceeded: {what}"),
            Self::NoVolume { listing } => write!(f, "{listing} lists no volume"),
            Self::NotSingleSite { root, sites } => write!(
                f,
                "{} names {sites} sites, not one site directory",
                grlevel2_config_url(root)
            ),
            Self::UnsafeName { name } => {
                write!(f, "listed name {name:?} is not a plain file name")
            }
            #[cfg(feature = "net")]
            Self::Fetch(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PollingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(feature = "net")]
            Self::Fetch(error) => Some(error),
            _ => None,
        }
    }
}

/// The entries of a `dir.list` a client can use, in listing order (oldest
/// first by convention): [`DirList::parse`]'s entries whose names are plain
/// file names ([`is_safe_file_name`]), each name once whatever its letter
/// case ([`DirList::into_safe_entries`]). Other lines are left out. Never
/// fails:
/// a listing over the limits ([`DirList::parse`] refuses it) gives no
/// entries; call [`DirList::parse`] to learn why, or to see the lines left
/// out.
pub fn parse_dir_list(text: &str) -> Vec<DirListEntry> {
    DirList::parse(text)
        .map(DirList::into_safe_entries)
        .unwrap_or_default()
}

/// The site ids of a `config.cfg` or `grlevel2.cfg` a client can use, in
/// file order, each once whatever its letter case: [`SiteConfig::parse`]'s
/// sites that are plain file names ([`is_safe_file_name`],
/// [`SiteConfig::into_safe_sites`]). Never fails: a configuration over the
/// limits gives no sites; call [`SiteConfig::parse`] to learn why.
pub fn parse_site_config(text: &str) -> Vec<String> {
    SiteConfig::parse(text)
        .map(SiteConfig::into_safe_sites)
        .unwrap_or_default()
}

/// Whether a listed name is a plain file name that can go into a URL
/// unencoded and name a file inside its directory, on the server and in a
/// local directory: not empty, no leading or trailing `.`, only RFC 3986
/// unreserved characters (ASCII letters and digits, `-`, `.`, `_`, `~`), and
/// not a Windows device name, alone or before a dot (`CON`, `PRN`, `AUX`,
/// `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, in any letter case). See the module's
/// "File names" section.
pub fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
        && !is_windows_device_name(name)
}

/// Whether `name`'s part before its first dot is a name Windows reserves
/// for a device, in any letter case: `nul.ar2v` names the null device there,
/// not a file.
fn is_windows_device_name(name: &str) -> bool {
    const THREE: [&[u8; 3]; 4] = [b"CON", b"PRN", b"AUX", b"NUL"];
    const NUMBERED: [&[u8; 3]; 2] = [b"COM", b"LPT"];
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    match stem.as_bytes() {
        [a, b, c] => THREE
            .iter()
            .any(|device| device.eq_ignore_ascii_case(&[*a, *b, *c])),
        [a, b, c, digit] => {
            digit.is_ascii_digit()
                && NUMBERED
                    .iter()
                    .any(|device| device.eq_ignore_ascii_case(&[*a, *b, *c]))
        }
        _ => false,
    }
}

/// Whether a listed name can be a volume: a plain file name
/// ([`is_safe_file_name`], so not a hidden file with a leading `.`), not a
/// file still being written (`.tmp`, `.part`), and not a text or state file
/// (`.json`, `.txt`, `.cfg`, `.list`, `.html`).
pub fn is_volume_name(name: &str) -> bool {
    const NOT_VOLUMES: [&str; 7] = [".tmp", ".part", ".json", ".txt", ".cfg", ".list", ".html"];
    let lower = name.to_ascii_lowercase();
    is_safe_file_name(name) && !NOT_VOLUMES.iter().any(|suffix| lower.ends_with(suffix))
}

/// The newest volume of a listing: its last entry that
/// [`is_volume_name`] accepts (listings run oldest first).
pub fn newest_volume_entry(entries: &[DirListEntry]) -> Option<&DirListEntry> {
    entries
        .iter()
        .rev()
        .find(|entry| is_volume_name(&entry.name))
}

fn trim_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// `{root_url}/{site}`, a site's directory on the server at `root_url` (for
/// North Dakota SWC's KXWA, `https://level2.swc.nd.gov/raw/KXWA`). `site` is
/// not checked:
/// pass a plain file name ([`is_safe_file_name`]), such as one from
/// [`parse_site_config`].
pub fn site_url(root_url: &str, site: &str) -> String {
    format!("{}/{site}", trim_url(root_url))
}

/// `{root_url}/{site}/dir.list`, a site's listing. `site` is not checked, as
/// in [`site_url`].
pub fn dir_list_url(root_url: &str, site: &str) -> String {
    format!("{}/{site}/dir.list", trim_url(root_url))
}

/// `{root_url}/{site}/{name}`, a file in a site's directory. Neither `site`
/// nor `name` is checked: pass plain file names ([`is_safe_file_name`]), such
/// as the entries of [`parse_dir_list`]; [`entry_url`] checks.
pub fn site_file_url(root_url: &str, site: &str, name: &str) -> String {
    format!("{}/{site}/{name}", trim_url(root_url))
}

/// `{root_url}/config.cfg`, the site configuration a polling root
/// publishes.
pub fn site_config_url(root_url: &str) -> String {
    format!("{}/config.cfg", trim_url(root_url))
}

/// `{root_url}/grlevel2.cfg`, the site configuration GR2Analyst reads (the
/// Laredo EWR root in [`crate::community_feeds`] has this one).
pub fn grlevel2_config_url(root_url: &str) -> String {
    format!("{}/grlevel2.cfg", trim_url(root_url))
}

/// `{site_url}/dir.list` for a site directory URL.
fn listing_url(site_url: &str) -> String {
    format!("{}/dir.list", trim_url(site_url))
}

/// The site directory `{root_url}/{site}` of a polling root whose site
/// configuration (`sites`, from [`SiteConfig::parse`]) names exactly one
/// site. A GR2Analyst-style client given such a root polls that site; the
/// Laredo EWR root in [`crate::community_feeds`] names `LARE` alone.
///
/// # Errors
///
/// [`PollingError::NotSingleSite`] when `sites` names no site or several,
/// and [`PollingError::UnsafeName`] when the one site id is not a plain file
/// name ([`is_safe_file_name`]), so it cannot go into a URL.
pub fn single_site_url(root_url: &str, sites: &[String]) -> Result<String, PollingError> {
    let [site] = sites else {
        return Err(PollingError::NotSingleSite {
            root: trim_url(root_url).to_owned(),
            sites: sites.len(),
        });
    };
    check_name(site)?;
    Ok(site_url(root_url, site))
}

/// URL of a listed file in the site directory `site_url`.
///
/// # Errors
///
/// [`PollingError::UnsafeName`] when the entry's name is not a plain file
/// name ([`is_safe_file_name`]).
pub fn entry_url(site_url: &str, entry: &DirListEntry) -> Result<String, PollingError> {
    check_name(&entry.name)?;
    Ok(format!("{}/{}", trim_url(site_url), entry.name))
}

fn check_name(name: &str) -> Result<(), PollingError> {
    if is_safe_file_name(name) {
        Ok(())
    } else {
        Err(PollingError::UnsafeName {
            name: name.chars().take(80).collect(),
        })
    }
}

/// Fetch a site's `dir.list` from the server at `root_url` and return the
/// entries a client can use ([`parse_dir_list`]'s entries), oldest first by
/// convention.
///
/// # Errors
///
/// [`PollingError::UnsafeName`] for a `site` that is not a plain file name,
/// before any request; otherwise those of [`fetch_dir_list_at`]: a failed
/// request, or a listing over the limits.
#[cfg(feature = "net")]
pub fn fetch_dir_list(root_url: &str, site: &str) -> Result<Vec<DirListEntry>, PollingError> {
    check_name(site)?;
    fetch_dir_list_at(&site_url(root_url, site)).map(DirList::into_safe_entries)
}

/// Fetch the site configuration `{root_url}/config.cfg` and return the site
/// ids a client can use ([`parse_site_config`]'s sites), in file order.
///
/// # Errors
///
/// Those of [`fetch_site_config_at`]: a failed request, or a configuration
/// over the limits.
#[cfg(feature = "net")]
pub fn fetch_site_config(root_url: &str) -> Result<Vec<String>, PollingError> {
    fetch_site_config_at(&site_config_url(root_url)).map(SiteConfig::into_safe_sites)
}

/// Fetch and parse `{site_url}/dir.list` ([`DirList::parse`]), where
/// `site_url` is a site directory.
///
/// # Errors
///
/// [`PollingError::Fetch`] when the request fails or the body is over the
/// crate's listing limit, and [`DirList::parse`]'s errors.
#[cfg(feature = "net")]
pub fn fetch_dir_list_at(site_url: &str) -> Result<DirList, PollingError> {
    let text = crate::fetch_listing_text(&listing_url(site_url)).map_err(PollingError::Fetch)?;
    DirList::parse(&text)
}

/// Fetch and parse a site configuration file ([`SiteConfig::parse`]); pass
/// its full URL (`{root}/config.cfg` or `{root}/grlevel2.cfg`).
///
/// # Errors
///
/// [`PollingError::Fetch`] when the request fails or the body is over the
/// crate's text-resource limit, and [`SiteConfig::parse`]'s errors.
#[cfg(feature = "net")]
pub fn fetch_site_config_at(url: &str) -> Result<SiteConfig, PollingError> {
    let text = crate::fetch_text(url).map_err(PollingError::Fetch)?;
    SiteConfig::parse(&text)
}

/// The newest volume in the site directory `site_url`: its listing entry
/// and URL. Fetches the listing only, never the volume. Skipped listing
/// lines ([`DirList::skipped`]) play no part here; call [`fetch_dir_list_at`]
/// to see them.
///
/// # Errors
///
/// Those of [`fetch_dir_list_at`], and [`PollingError::NoVolume`] when no
/// entry is a volume ([`is_volume_name`]).
#[cfg(feature = "net")]
pub fn latest_volume(site_url: &str) -> Result<(DirListEntry, String), PollingError> {
    let list = fetch_dir_list_at(site_url)?;
    let entry =
        newest_volume_entry(&list.entries)
            .cloned()
            .ok_or_else(|| PollingError::NoVolume {
                listing: listing_url(site_url),
            })?;
    let url = entry_url(site_url, &entry)?;
    Ok((entry, url))
}

/// The newest volume found from a poll URL that is either a site directory
/// or a polling root naming a single site.
#[cfg(feature = "net")]
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SiteVolume {
    /// The site directory whose `dir.list` was read: the poll URL itself,
    /// or the one site its `grlevel2.cfg` names.
    pub site_url: String,
    /// The newest volume's listing entry.
    pub entry: DirListEntry,
    /// The newest volume's URL.
    pub url: String,
}

/// Like [`latest_volume`], for a poll URL that may be a polling root rather
/// than a site directory: when `{poll_url}/dir.list` answers HTTP 404, read
/// `{poll_url}/grlevel2.cfg` and, if it names exactly one site, poll that
/// site's directory ([`single_site_url`]). This is the convention
/// [`crate::community_feeds`] documents for its poll URLs. At most three
/// requests, one after another: the root's `dir.list`, its `grlevel2.cfg`
/// and the site's `dir.list`.
///
/// # Errors
///
/// The error of the first `dir.list` request when it is not a 404, or when
/// the root has no `grlevel2.cfg` either (a 404 too); otherwise the errors
/// of [`fetch_site_config_at`], [`single_site_url`] and [`latest_volume`].
#[cfg(feature = "net")]
pub fn latest_volume_or_single_site(poll_url: &str) -> Result<SiteVolume, PollingError> {
    let listing_error = match latest_volume(poll_url) {
        Ok((entry, url)) => {
            return Ok(SiteVolume {
                site_url: trim_url(poll_url).to_owned(),
                entry,
                url,
            });
        }
        Err(PollingError::Fetch(error)) if error.is_not_found() => PollingError::Fetch(error),
        Err(error) => return Err(error),
    };
    let config = match fetch_site_config_at(&grlevel2_config_url(poll_url)) {
        Ok(config) => config,
        Err(PollingError::Fetch(error)) if error.is_not_found() => return Err(listing_error),
        Err(error) => return Err(error),
    };
    let site_url = single_site_url(poll_url, &config.sites)?;
    let (entry, url) = latest_volume(&site_url)?;
    Ok(SiteVolume {
        site_url,
        entry,
        url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A polling capture, as text, or `None` when it is not in the testdata
    /// cache (the captures are not redistributed).
    fn capture(id: &str) -> Option<String> {
        let bytes = recast_radar_testdata::bytes_if_available(id)?;
        Some(String::from_utf8(bytes).unwrap_or_else(|error| panic!("{id}: {error}")))
    }

    /// North Dakota SWC's KXWA `dir.list`, 2026-09-25 03:18Z: 1,161 LF lines.
    const KXWA: &str = "polling-ndswc-kxwa-dir-list-20260925";
    /// The Iowa Environmental Mesonet root's `config.cfg`, 2026-09-26
    /// 02:13Z: `ListFile: dir.list`, then 220 sites.
    const IEM: &str = "polling-iem-config-cfg-20260926";
    /// The Laredo EWR root's `grlevel2.cfg`, 2026-09-25 03:01Z.
    const LAREDO: &str = "polling-ewr-laredo-grlevel2-cfg-20260925";
    /// North Dakota SWC's polling root.
    const ND_SWC: &str = "https://level2.swc.nd.gov/raw";

    #[test]
    fn a_real_dir_list_parses_oldest_first() {
        let Some(text) = capture(KXWA) else {
            return;
        };
        let list = DirList::parse(&text).unwrap();
        assert!(list.skipped.is_empty());
        // Every LF line of the capture is an entry, and every name is usable.
        assert_eq!(list.entries.len(), 1161);
        assert_eq!(list.entries.len(), text.lines().count());
        assert_eq!(parse_dir_list(&text), list.entries);
        let entries = list.entries;
        assert_eq!(
            entries.first(),
            Some(&DirListEntry::new(31_040, "KXWA20260921_105810_V06.ar2v"))
        );
        assert!(entries.windows(2).all(|pair| pair[0].name < pair[1].name));
        assert!(
            entries.iter().all(
                |entry| entry.name.starts_with("KXWA2026") && entry.name.ends_with("_V06.ar2v")
            )
        );
        // The listed sizes are the server's units, not bytes: the surveyed
        // 20,616,906-byte volume is listed at 40,288.
        assert!(entries.iter().all(|e| (27_392..=66_880).contains(&e.size)));
        assert!(entries.contains(&DirListEntry::new(40_288, "KXWA20260924_214316_V06.ar2v")));
        let newest = newest_volume_entry(&entries).unwrap();
        assert_eq!(
            newest,
            &DirListEntry::new(33_472, "KXWA20260925_031115_V06.ar2v")
        );
        assert_eq!(
            entry_url(&format!("{ND_SWC}/KXWA/"), newest).unwrap(),
            "https://level2.swc.nd.gov/raw/KXWA/KXWA20260925_031115_V06.ar2v"
        );
        assert_eq!(
            site_file_url(&format!("{ND_SWC}/"), "KXWA", &newest.name),
            entry_url(&site_url(ND_SWC, "KXWA"), newest).unwrap()
        );
    }

    #[test]
    fn a_file_still_being_written_is_not_the_newest_volume() {
        // A listing whose last line names a volume still being written.
        let text = "31040 KXWA20260921_105810_V06.ar2v\n\
                    30976 KXWA20260921_110243_V06.ar2v\n\
                    1024 KXWA20260921_110716_V06.ar2v.tmp\n";
        let entries = parse_dir_list(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            newest_volume_entry(&entries).map(|e| e.name.as_str()),
            Some("KXWA20260921_110243_V06.ar2v")
        );
        for name in [
            "a.part",
            "a.json",
            "a.txt",
            "grlevel2.cfg",
            "dir.list",
            "index.html",
        ] {
            assert!(!is_volume_name(name), "{name}");
        }
    }

    #[test]
    fn hidden_state_files_are_kept_as_listed_but_not_used() {
        // A state file whose name starts with a dot, listed first.
        let text = "6240 .seen.json\r\n31040 KXWA20260921_105810_V06.ar2v\r\n";
        let listed = DirList::parse(text).unwrap().entries;
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0], DirListEntry::new(6240, ".seen.json"));
        assert!(!is_volume_name(&listed[0].name));
        assert_eq!(
            newest_volume_entry(&listed).map(|e| e.name.as_str()),
            Some("KXWA20260921_105810_V06.ar2v")
        );
        // A client given the usable entries never sees the hidden file.
        let usable = parse_dir_list(text);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].name, "KXWA20260921_105810_V06.ar2v");
    }

    #[test]
    fn a_real_site_config_lists_every_site_once() {
        let Some(text) = capture(IEM) else {
            return;
        };
        let sites = SiteConfig::parse(&text).unwrap().sites;
        assert_eq!(sites.len(), 220);
        // Every line but the first (`ListFile: dir.list`) names a site.
        assert_eq!(text.lines().next(), Some("ListFile: dir.list"));
        assert_eq!(
            sites.len(),
            text.lines().filter(|l| l.starts_with("Site:")).count()
        );
        assert_eq!(sites.len(), text.lines().count() - 1);
        assert_eq!(sites.first().map(String::as_str), Some("FUSA"));
        assert_eq!(sites.last().map(String::as_str), Some("WILU"));
        for site in ["FWLX", "GAWX", "KTLX", "FOP1", "MZZU"] {
            assert!(sites.iter().any(|s| s == site), "{site}");
        }
        assert_eq!(parse_site_config(&text), sites);
    }

    #[test]
    fn odd_lines_are_kept_aside_and_do_not_stop_the_listing() {
        let list = DirList::parse(
            "1 KXWA_1.ar2v\r\nbig a.ar2v\r\n12 x y.ar2v\r\nalone.ar2v\r\n2 KXWA_2.ar2v\r\n",
        )
        .unwrap();
        assert_eq!(
            list.skipped,
            [
                SkippedLine {
                    line: 2,
                    text: "big a.ar2v".to_owned()
                },
                SkippedLine {
                    line: 4,
                    text: "alone.ar2v".to_owned()
                }
            ]
        );
        let names: Vec<&str> = list.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["KXWA_1.ar2v", "x y.ar2v", "KXWA_2.ar2v"]);
        assert_eq!(list.entries[1].size, 12);
        // The name with a space is listed as is, but not usable.
        let usable: Vec<String> = list
            .into_safe_entries()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(usable, ["KXWA_1.ar2v", "KXWA_2.ar2v"]);
        // A newest name with a space is never requested.
        let list = DirList::parse("1 KXWA_1.ar2v\r\n2   KXWA 2.ar2v  \r\n").unwrap();
        assert_eq!(list.entries[1].name, "KXWA 2.ar2v");
        assert_eq!(
            newest_volume_entry(&list.entries).map(|e| e.name.as_str()),
            Some("KXWA_1.ar2v")
        );
    }

    #[test]
    fn oversized_listings_are_errors_and_give_no_usable_entries() {
        let long = format!("1 {}\n", "a".repeat(MAX_DIR_LIST_LINE_BYTES));
        assert!(matches!(
            DirList::parse(&long),
            Err(PollingError::LimitExceeded(_))
        ));
        assert!(parse_dir_list(&long).is_empty());
        let many = "1 a.ar2v\n".repeat(MAX_DIR_LIST_ENTRIES + 1);
        assert!(matches!(
            DirList::parse(&many),
            Err(PollingError::LimitExceeded(_))
        ));
        assert!(parse_dir_list(&many).is_empty());
        // Skipped lines count towards the limit too.
        let many = "a b\n".repeat(MAX_DIR_LIST_ENTRIES + 1);
        assert!(matches!(
            DirList::parse(&many),
            Err(PollingError::LimitExceeded(_))
        ));
        let at_limit: String = (0..MAX_DIR_LIST_ENTRIES)
            .map(|index| format!("1 a{index}.ar2v\n"))
            .collect();
        assert_eq!(
            DirList::parse(&at_limit).unwrap().entries.len(),
            MAX_DIR_LIST_ENTRIES
        );
        assert_eq!(parse_dir_list(&at_limit).len(), MAX_DIR_LIST_ENTRIES);
    }

    #[test]
    fn blank_lines_and_a_byte_order_mark_are_ignored() {
        let list = DirList::parse("\u{feff}4 a.ar2v\n\n  \n5 b.ar2v\n").unwrap();
        assert!(list.skipped.is_empty());
        assert_eq!(
            list.entries,
            [
                DirListEntry::new(4, "a.ar2v"),
                DirListEntry::new(5, "b.ar2v")
            ]
        );
        assert_eq!(DirList::parse("").unwrap(), DirList::default());
        assert!(parse_dir_list("").is_empty());
        assert!(newest_volume_entry(&[]).is_none());
    }

    #[test]
    fn a_site_configuration_may_start_with_a_byte_order_mark() {
        // A single-site root must still name its one site.
        let config = SiteConfig::parse("\u{feff}Site: KXWA\r\nSite: KBPP\r\n").unwrap();
        assert_eq!(config.sites, ["KXWA", "KBPP"]);
        // The Laredo capture is not redistributed: checked when cached.
        let root = "http://offsitevpn.ewradar.com/Laredo/archive2.trans";
        if let Some(laredo) = capture(LAREDO) {
            let bom = format!("\u{feff}{laredo}");
            let sites = SiteConfig::parse(&bom).unwrap().sites;
            assert_eq!(
                single_site_url(root, &sites).unwrap(),
                format!("{root}/LARE")
            );
            assert_eq!(parse_site_config(&bom), ["LARE"]);
        }
    }

    #[test]
    fn oversized_site_configurations_are_errors_not_cut_short() {
        let many: String = (0..=MAX_CONFIG_SITES)
            .map(|index| format!("Site: S{index}\r\n"))
            .collect();
        assert!(matches!(
            SiteConfig::parse(&many),
            Err(PollingError::LimitExceeded(_))
        ));
        assert!(parse_site_config(&many).is_empty());
        let at_limit: String = (0..MAX_CONFIG_SITES)
            .map(|index| format!("Site: S{index}\r\n"))
            .collect();
        assert_eq!(
            SiteConfig::parse(&at_limit).unwrap().sites.len(),
            MAX_CONFIG_SITES
        );
        let long = format!("Site: {}\r\n", "A".repeat(MAX_DIR_LIST_LINE_BYTES));
        assert!(matches!(
            SiteConfig::parse(&long),
            Err(PollingError::LimitExceeded(_))
        ));
    }

    #[test]
    fn duplicate_sites_cost_linear_time() {
        // The worst case for a linear duplicate scan: the most distinct
        // sites, then duplicates up to the 16 MiB a fetch may read. With the
        // scan this took 38 s in a release build; with the hash set, 0.1 s.
        let mut text: String = (0..MAX_CONFIG_SITES)
            .map(|index| format!("Site: S{index}\r\n"))
            .collect();
        let duplicate = format!("Site: S{}\r\n", MAX_CONFIG_SITES - 1);
        while text.len() + duplicate.len() <= 16 << 20 {
            text.push_str(&duplicate);
        }
        let started = std::time::Instant::now();
        let sites = SiteConfig::parse(&text).unwrap().sites;
        let elapsed = started.elapsed();
        assert_eq!(sites.len(), MAX_CONFIG_SITES);
        assert_eq!(sites.last().map(String::as_str), Some("S9999"));
        assert!(elapsed.as_secs() < 20, "{elapsed:?}");
    }

    #[test]
    fn a_root_naming_one_site_stands_for_that_site() {
        let root = "http://offsitevpn.ewradar.com/Laredo/archive2.trans";
        assert_eq!(
            grlevel2_config_url(&format!("{root}/")),
            format!("{root}/grlevel2.cfg")
        );
        // The captures are not redistributed: each is checked when cached.
        if let Some(laredo) = capture(LAREDO) {
            let sites = SiteConfig::parse(&laredo).unwrap().sites;
            assert_eq!(sites, ["LARE"]);
            assert_eq!(
                single_site_url(&format!("{root}/"), &sites).unwrap(),
                format!("{root}/LARE")
            );
        }
        // The Iowa Environmental Mesonet's root names 220 sites: it is not
        // one site directory.
        if let Some(iem) = capture(IEM) {
            let iem_root = "https://mesonet-nexrad.agron.iastate.edu/level2/raw";
            let iem = SiteConfig::parse(&iem).unwrap().sites;
            let error = single_site_url(iem_root, &iem).unwrap_err();
            assert!(
                matches!(error, PollingError::NotSingleSite { sites: 220, .. }),
                "{error}"
            );
            assert_eq!(
                error.to_string(),
                format!("{iem_root}/grlevel2.cfg names 220 sites, not one site directory")
            );
        }
        assert!(matches!(
            single_site_url(root, &[]),
            Err(PollingError::NotSingleSite { sites: 0, .. })
        ));
        assert!(matches!(
            single_site_url(root, &["../x".to_owned()]),
            Err(PollingError::UnsafeName { .. })
        ));
    }

    #[test]
    fn names_that_could_leave_the_site_directory_are_refused() {
        // Every volume name family the feed survey downloaded from the
        // community feeds' polling directories is a plain file name, and so
        // is the name the Laredo host's read-me gives.
        for name in [
            "KXWA20260924_214316_V06.ar2v",
            "KBPP_20260924_2145.gz",
            "FOP1_20260924_215145.bz2",
            "FUSA_20260924_214900.msg31",
            "GAWX_20260924_2147",
            "KULM_20240526_002644",
            "LARE_23032416_0559",
        ] {
            assert!(is_volume_name(name), "{name}");
        }
        let site = "https://level2.swc.nd.gov/raw/KXWA";
        for name in [
            "a\\..\\..\\x.ar2v",
            "a/../x.ar2v",
            "x.ar2v?y=1",
            "x.ar2v#frag",
            "x%2e.ar2v",
            "c:x.ar2v",
            "x y.ar2v",
            "ü.ar2v",
            "..",
            "",
        ] {
            assert!(!is_volume_name(name), "{name:?}");
            assert!(
                matches!(
                    entry_url(site, &DirListEntry::new(1, name)),
                    Err(PollingError::UnsafeName { .. })
                ),
                "{name:?}"
            );
        }
        // An unsafe newest name is skipped, not requested, and a client
        // given the usable entries never sees it.
        let text = "1 KXWA_1.ar2v\r\n2 a\\..\\..\\x.ar2v\r\n10 ../x\n10 a/b\n";
        let listed = DirList::parse(text).unwrap().entries;
        assert_eq!(listed.len(), 4);
        assert_eq!(
            newest_volume_entry(&listed).map(|e| e.name.as_str()),
            Some("KXWA_1.ar2v")
        );
        assert_eq!(parse_dir_list(text), [DirListEntry::new(1, "KXWA_1.ar2v")]);
        assert_eq!(
            parse_site_config("Site: ..\nSite: OK_\nSite: a/b\n"),
            ["OK_"]
        );
    }

    #[test]
    fn names_windows_would_store_elsewhere_are_refused() {
        // Device names, alone or before a dot and in any case, and a
        // trailing dot, which Windows drops.
        for name in [
            "CON",
            "con",
            "aux",
            "PRN.ar2v",
            "NUL.ar2v",
            "nul.tar.gz",
            "com1.ar2v",
            "COM0",
            "Lpt9",
            "lpt1.x",
            "x.ar2v.",
            "x.",
        ] {
            assert!(!is_safe_file_name(name), "{name:?}");
            assert!(!is_volume_name(name), "{name:?}");
            assert!(
                matches!(
                    entry_url(
                        "https://level2.swc.nd.gov/raw/KXWA",
                        &DirListEntry::new(1, name)
                    ),
                    Err(PollingError::UnsafeName { .. })
                ),
                "{name:?}"
            );
        }
        // Names that only start like one are plain.
        for name in [
            "CONS",
            "CON_x.ar2v",
            "NULL.ar2v",
            "com10.ar2v",
            "COMA",
            "LPT",
            "AUXX",
            "x.con",
            "x.ar2v.gz",
            "a.b",
        ] {
            assert!(is_safe_file_name(name), "{name:?}");
        }
        let text = "1 NUL.ar2v\r\n2 x.ar2v.\r\n3 x.ar2v\r\n";
        assert_eq!(parse_dir_list(text), [DirListEntry::new(3, "x.ar2v")]);
        assert_eq!(
            newest_volume_entry(&DirList::parse(text).unwrap().entries).map(|e| e.size),
            Some(3)
        );
        assert_eq!(
            parse_site_config("Site: CON\nSite: aux\nSite: KXWA\n"),
            ["KXWA"]
        );
    }

    #[test]
    fn names_that_differ_only_in_case_are_kept_once() {
        // Two URLs on the server, one file on a case-insensitive file system:
        // a client keeps the first listed.
        let config = SiteConfig::parse("Site: kxwa\r\nSite: KXWA\r\nSite: KBPP\r\n").unwrap();
        assert_eq!(config.sites, ["kxwa", "KXWA", "KBPP"]);
        assert_eq!(config.into_safe_sites(), ["kxwa", "KBPP"]);
        let text = "1 X.ar2v\r\n2 x.AR2V\r\n3 y.ar2v\r\n4 X.ar2v\r\n";
        assert_eq!(DirList::parse(text).unwrap().entries.len(), 4);
        assert_eq!(
            parse_dir_list(text),
            [
                DirListEntry::new(1, "X.ar2v"),
                DirListEntry::new(3, "y.ar2v")
            ]
        );
        // An unsafe name does not hold a place: the safe one after it is kept.
        assert_eq!(
            parse_site_config("Site: A B\nSite: a_b\nSite: A_B\n"),
            ["a_b"]
        );
    }

    #[test]
    fn urls_join_with_one_slash() {
        for root in [ND_SWC, "https://level2.swc.nd.gov/raw/"] {
            assert_eq!(
                dir_list_url(root, "KXWA"),
                "https://level2.swc.nd.gov/raw/KXWA/dir.list"
            );
            assert_eq!(
                site_file_url(root, "KXWA", "a.ar2v"),
                "https://level2.swc.nd.gov/raw/KXWA/a.ar2v"
            );
            assert_eq!(
                site_config_url(root),
                "https://level2.swc.nd.gov/raw/config.cfg"
            );
            assert_eq!(
                grlevel2_config_url(root),
                "https://level2.swc.nd.gov/raw/grlevel2.cfg"
            );
            assert_eq!(
                listing_url(&site_url(root, "KXWA")),
                dir_list_url(root, "KXWA")
            );
        }
        assert_eq!(
            listing_url("https://level2.swc.nd.gov/raw/KXWA/"),
            "https://level2.swc.nd.gov/raw/KXWA/dir.list"
        );
    }

    /// Serve one response on a local port and return its base URL: `head`,
    /// then the real KXWA `dir.list` capture `copies` times over (a runaway
    /// listing made of real lines).
    #[cfg(feature = "net")]
    fn serve_listing_copies(head: &'static str, copies: usize) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listing = recast_radar_testdata::bytes(KXWA).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut stream = stream;
            let _ = stream.write_all(head.as_bytes());
            for _ in 0..copies {
                if stream.write_all(&listing).is_err() {
                    return;
                }
            }
        });
        format!("http://{address}")
    }

    #[cfg(feature = "net")]
    #[test]
    fn oversized_listings_are_refused_while_streaming() {
        // The KXWA capture is not redistributed: skipped unless cached.
        let _ = recast_radar_testdata::require_file!(KXWA);
        // No Content-Length: the body is cut off at the 32 MiB listing limit,
        // not read whole.
        let copies = (32 << 20) / 40_635 + 2;
        let base = serve_listing_copies("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", copies);
        let err = fetch_dir_list(&base, "KXWA").unwrap_err();
        assert!(err.to_string().contains("limit"), "{err}");
        // A Content-Length over the limit is refused before the body.
        let base = serve_listing_copies(
            "HTTP/1.1 200 OK\r\nContent-Length: 300000000\r\nConnection: close\r\n\r\n",
            0,
        );
        let err = fetch_site_config(&base).unwrap_err();
        assert!(err.to_string().contains("limit"), "{err}");
        // One copy is a real listing, well under the limit, and parses.
        let base = serve_listing_copies("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n", 1);
        assert_eq!(fetch_dir_list(&base, "KXWA").unwrap().len(), 1161);
        // A site id that would leave the root is refused before any request.
        assert!(matches!(
            fetch_dir_list("http://127.0.0.1:9", "../KXWA"),
            Err(PollingError::UnsafeName { .. })
        ));
    }
}
