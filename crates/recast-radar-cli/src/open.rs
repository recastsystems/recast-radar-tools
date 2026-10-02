//! Opening input files: format detection and decoding.
//!
//! [`open_path`] reads a file and decodes it with the decoder its contents
//! call for:
//!
//! 1. ZIP files are tried as mobile-radar archives (DORADE sweep files and
//!    `.msg31` members, several volumes per archive), then as a single ZIP
//!    record through the router.
//! 2. JMA tars decode one station ([`OpenOptions::station`]), every station
//!    ([`OpenOptions::all_stations`]), or by default the first.
//! 3. Bytes the router would hand to the Level II decoder but that do not
//!    start like a Level II file (no `AR2V`/`ARCHIVE2` header, gzip, bzip2 or
//!    LDM record) are tried as a Level III product first.
//! 4. Everything else goes through `recast_radar_io`'s router: DORADE,
//!    ODIM_H5 (and netCDF-4 CfRadial), CfRadial 1, JMA, Level II, with gzip
//!    and single-record ZIP wrappers removed.

use std::fs;
use std::path::{Path, PathBuf};

use recast_radar_core::model::{SourceFormat, Volume, merge_volumes};
use recast_radar_io::{FormatMetadata, SupportedVolumeFormat};
use recast_radar_io_level3::{Level3Error, Level3Message};

use crate::records::{self, RecordSummary};
use crate::{CliError, InputArgs};

/// Largest input file `open_path` reads (the decoders expand at most
/// 512 MiB, so nothing larger is a radar file they accept).
pub const MAX_INPUT_BYTES: u64 = 1 << 30;

/// What to decode from a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenOptions {
    /// JMA tars: the station to decode (JMA id or station number).
    pub station: Option<String>,
    /// JMA tars: decode every station.
    pub all_stations: bool,
    /// Also decode the format's metadata (NEXRAD metadata messages).
    pub metadata: bool,
}

impl OpenOptions {
    /// Options from the command line, with metadata decoding on or off.
    pub fn from_args(args: &InputArgs, metadata: bool) -> Self {
        Self {
            station: args.station.clone(),
            all_stations: args.all_stations,
            metadata,
        }
    }
}

/// One decoded volume of an input.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// Name of the part of the input it came from (a mobile-archive member),
    /// when the input holds several.
    pub label: Option<String>,
    /// The volume.
    pub volume: Volume,
    /// Metadata decoded beside it.
    pub metadata: FormatMetadata,
}

/// A decoded Level III file.
#[derive(Clone, Debug, PartialEq)]
pub struct Level3Input {
    /// The product or message.
    pub message: Level3Message,
    /// The product's data array as a one-sweep volume, or why there is none
    /// (graphic, tabular and text products, and messages that are not
    /// products).
    pub volume: Result<Volume, String>,
}

/// What an input decoded to.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Contents {
    /// Radar volumes (one for most files).
    Volumes(Vec<Loaded>),
    /// A Level III product or message.
    Level3(Box<Level3Input>),
    /// Level II records that are not a volume: a real-time chunk, or a file
    /// with no radials.
    Level2Records(Box<RecordSummary>),
}

/// A decoded input file.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    /// The file.
    pub path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// What it decoded to.
    pub contents: Contents,
}

impl Input {
    /// The decoded volumes: every volume of [`Contents::Volumes`], or the
    /// Level III data array when there is one.
    pub fn volumes(&self) -> Vec<(Option<&str>, &Volume, &FormatMetadata)> {
        static NO_METADATA: FormatMetadata = FormatMetadata::None;
        match &self.contents {
            Contents::Volumes(volumes) => volumes
                .iter()
                .map(|loaded| (loaded.label.as_deref(), &loaded.volume, &loaded.metadata))
                .collect(),
            Contents::Level3(level3) => match &level3.volume {
                Ok(volume) => vec![(None, volume, &NO_METADATA)],
                Err(_) => Vec::new(),
            },
            Contents::Level2Records(_) => Vec::new(),
        }
    }

    /// Take the volumes out: see [`Self::volumes`].
    pub fn into_volumes(self) -> Vec<Loaded> {
        match self.contents {
            Contents::Volumes(volumes) => volumes,
            Contents::Level3(level3) => match level3.volume {
                Ok(volume) => vec![Loaded {
                    label: None,
                    volume,
                    metadata: FormatMetadata::None,
                }],
                Err(_) => Vec::new(),
            },
            Contents::Level2Records(_) => Vec::new(),
        }
    }

    /// The format's display name.
    pub fn format_name(&self) -> &'static str {
        match &self.contents {
            Contents::Level3(_) => "NEXRAD Level III",
            Contents::Level2Records(_) => "NEXRAD Level II records",
            Contents::Volumes(volumes) => volumes
                .first()
                .map_or("unknown", |loaded| source_format_name(&loaded.volume)),
        }
    }
}

/// Display name of a volume's source format.
pub fn source_format_name(volume: &Volume) -> &'static str {
    match volume.provenance.source_format {
        SourceFormat::NexradLevel2 => "NEXRAD Level II",
        SourceFormat::NexradLevel3 => "NEXRAD Level III",
        SourceFormat::OdimH5 => "ODIM_H5",
        SourceFormat::CfRadial1 => "CfRadial 1",
        SourceFormat::CfRadial2 => "CfRadial 2",
        SourceFormat::Dorade => "DORADE",
        SourceFormat::JmaGrib2 => "JMA GRIB2",
        SourceFormat::MeteoFranceBufr => "Meteo-France BUFR",
        SourceFormat::Simulated => "simulated",
        _ => "unknown",
    }
}

/// Read and decode `path`.
pub fn open_path(path: &Path, options: &OpenOptions) -> Result<Input, CliError> {
    let metadata = fs::metadata(path).map_err(|err| CliError::io(path, err))?;
    if metadata.is_dir() {
        return Err(CliError::Usage(format!(
            "{} is a directory, not a radar file",
            path.display()
        )));
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(CliError::Decode {
            path: path.to_path_buf(),
            message: format!(
                "file is {} bytes; radar files larger than {MAX_INPUT_BYTES} bytes are not read",
                metadata.len()
            ),
        });
    }
    let bytes = fs::read(path).map_err(|err| CliError::io(path, err))?;
    let contents = decode(path, &bytes, options).map_err(|message| CliError::Decode {
        path: path.to_path_buf(),
        message,
    })?;
    let mut input = Input {
        path: path.to_path_buf(),
        size: bytes.len() as u64,
        contents,
    };
    if let Contents::Volumes(volumes) = &mut input.contents {
        for loaded in volumes {
            if loaded.volume.provenance.source_path.is_none() {
                loaded.volume.provenance.source_path = Some(path.display().to_string());
            }
        }
    }
    Ok(input)
}

/// Decode every input and, when `merge` is set, merge all their volumes
/// into one (parts of one scan). Without `merge`, exactly one input is
/// allowed and its volume `index` (default 0) is returned.
pub fn load_one(
    paths: &[PathBuf],
    options: &OpenOptions,
    merge: bool,
    index: Option<usize>,
) -> Result<Loaded, CliError> {
    if !merge {
        let [path] = paths else {
            return Err(CliError::Usage(format!(
                "{} inputs given: pass --merge to merge parts of one scan, or one file",
                paths.len()
            )));
        };
        let input = open_path(path, options)?;
        let mut volumes = input.into_volumes();
        let count = volumes.len();
        let index = index.unwrap_or(0);
        if index >= count {
            return Err(CliError::Usage(match count {
                0 => format!("{}: holds no radar volume", path.display()),
                _ => format!(
                    "{}: --volume {index} requested, but the file holds {count} volume(s)",
                    path.display()
                ),
            }));
        }
        return Ok(volumes.swap_remove(index));
    }
    if index.is_some() {
        return Err(CliError::Usage(
            "--volume cannot be combined with --merge".to_owned(),
        ));
    }
    let mut parts = Vec::new();
    for path in paths {
        let input = open_path(path, options)?;
        let volumes = input.into_volumes();
        if volumes.is_empty() {
            return Err(CliError::Decode {
                path: path.clone(),
                message: "holds no radar volume".to_owned(),
            });
        }
        parts.extend(volumes.into_iter().map(|loaded| loaded.volume));
    }
    let (volume, report) = merge_volumes(parts).map_err(|err| CliError::Failed(err.to_string()))?;
    if report.separate_sweeps > 0 || report.separate_fields > 0 || report.field_collisions > 0 {
        eprintln!(
            "note: merge moved {} field(s); kept {} sweep(s) of other rays or collection time and \
             {} field(s) of other gates as sweeps of their own; dropped {} field(s) whose name the \
             sweep already had",
            report.merged_fields,
            report.separate_sweeps,
            report.separate_fields,
            report.field_collisions
        );
    }
    Ok(Loaded {
        label: None,
        volume,
        metadata: FormatMetadata::None,
    })
}

/// True when the bytes start like a Level II file or one of its wrappers:
/// an `AR2V`/`ARCHIVE2` volume header, gzip, whole-file bzip2, or an LDM
/// record (4-byte length, then bzip2).
fn starts_like_level2(bytes: &[u8]) -> bool {
    bytes.starts_with(b"AR2V")
        || bytes.starts_with(b"ARCHIVE2")
        || bytes.starts_with(&[0x1f, 0x8b])
        || bytes.starts_with(b"BZh")
        || bytes.get(4..7) == Some(b"BZh".as_slice())
}

fn decode(path: &Path, bytes: &[u8], options: &OpenOptions) -> Result<Contents, String> {
    if recast_radar_io_dorade::mobile_archive::looks_like_zip_bytes(bytes)
        && let Ok(volumes) = recast_radar_io::read_mobile_archive_from_path(path)
        && !volumes.is_empty()
    {
        return Ok(Contents::Volumes(
            volumes
                .into_iter()
                .map(|member| Loaded {
                    label: Some(member.member_label),
                    volume: member.volume,
                    metadata: FormatMetadata::None,
                })
                .collect(),
        ));
    }
    open_bytes(bytes, options)
}

/// Decode the bytes of one radar file, as [`open_path`] decodes a file's
/// contents, except that ZIP files are not tried as mobile-radar archives
/// (that reader needs a path): a ZIP holding one record still goes through
/// the router. Errors are the decoders' messages, without a file name.
pub fn open_bytes(bytes: &[u8], options: &OpenOptions) -> Result<Contents, String> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(format!(
            "input is {} bytes; radar files larger than {MAX_INPUT_BYTES} bytes are not read",
            bytes.len()
        ));
    }
    let sniffed = recast_radar_io::sniff_supported_volume_format(bytes);
    if sniffed == SupportedVolumeFormat::JmaGrib2Tar
        && (options.station.is_some() || options.all_stations)
    {
        let filter = if options.all_stations {
            None
        } else {
            options.station.as_deref()
        };
        let volumes = recast_radar_io_jma::read_jma_tar_volumes(bytes, filter)
            .map_err(|err| err.to_string())?;
        if volumes.is_empty() {
            return Err(format!(
                "no station {} in the tar",
                options.station.as_deref().unwrap_or("")
            ));
        }
        return Ok(Contents::Volumes(
            volumes
                .into_iter()
                .map(|volume| Loaded {
                    label: Some(volume.attrs.instrument_name.clone()),
                    volume,
                    metadata: FormatMetadata::None,
                })
                .collect(),
        ));
    }

    // Level III (sniffed as such, or anything the router would take for
    // Level II that does not start like it) is decoded as a message, so a
    // product without a data array, a status or a text message still opens.
    if sniffed == SupportedVolumeFormat::NexradLevel3
        || (sniffed == SupportedVolumeFormat::NexradLevel2
            && !starts_like_level2(bytes)
            && !recast_radar_io_dorade::mobile_archive::looks_like_zip_bytes(bytes))
    {
        return match recast_radar_io_level3::decode_message(bytes) {
            Ok(message) => Ok(Contents::Level3(Box::new(level3_input(message)))),
            Err(level3_err) => route_or_records(bytes, options).map_err(|level2_err| {
                format!(
                    "not a recognised radar file (as Level II: {level2_err}; as Level III: {level3_err})"
                )
            }),
        };
    }

    route_or_records(bytes, options)
}

/// Route the bytes; Level II input that holds no radials (a start chunk) or
/// has no volume header (an intermediate chunk, a bare record) is described
/// record by record instead.
fn route_or_records(bytes: &[u8], options: &OpenOptions) -> Result<Contents, String> {
    let routed = route(bytes, options);
    let records_instead = match &routed {
        Ok(loaded) => {
            loaded.volume.sweeps.is_empty()
                && loaded.volume.provenance.source_format == SourceFormat::NexradLevel2
        }
        Err(_) => starts_like_level2(bytes),
    };
    if records_instead
        && let Ok(summary) = records::summarize(bytes)
        && !summary.messages.is_empty()
    {
        return Ok(Contents::Level2Records(Box::new(summary)));
    }
    routed.map(|loaded| Contents::Volumes(vec![loaded]))
}

fn route(bytes: &[u8], options: &OpenOptions) -> Result<Loaded, String> {
    if options.metadata {
        let decoded = recast_radar_io::read_supported_volume_with_metadata(bytes)
            .map_err(|e| e.to_string())?;
        Ok(Loaded {
            label: None,
            volume: decoded.volume,
            metadata: decoded.metadata,
        })
    } else {
        let volume =
            recast_radar_io::read_supported_volume_bytes(bytes).map_err(|e| e.to_string())?;
        Ok(Loaded {
            label: None,
            volume,
            metadata: FormatMetadata::None,
        })
    }
}

fn level3_input(message: Level3Message) -> Level3Input {
    let volume = match &message {
        Level3Message::Product(product) => product.to_volume().map_err(|err| match err {
            Level3Error::NoDataArray { .. } => {
                "the product has no radial, raster or generic data array".to_owned()
            }
            other => other.to_string(),
        }),
        Level3Message::GeneralStatus(_) => Err("a General Status Message has no data".to_owned()),
        Level3Message::Text(_) => Err("a text message has no data".to_owned()),
        _ => Err("this Level III message has no data".to_owned()),
    };
    Level3Input { message, volume }
}

/// Decode the bytes of one file (no mobile archives, which need a path) for
/// timing: the volumes' ray count, or the Level III product code.
pub(crate) fn decode_for_bench(bytes: &[u8], metadata: bool) -> Result<usize, String> {
    let sniffed = recast_radar_io::sniff_supported_volume_format(bytes);
    if sniffed == SupportedVolumeFormat::NexradLevel2
        && !starts_like_level2(bytes)
        && let Ok(product) = recast_radar_io_level3::decode_product(bytes)
    {
        return Ok(match product.to_volume() {
            Ok(volume) => volume.sweeps.iter().map(|sweep| sweep.nrays()).sum(),
            Err(_) => 0,
        });
    }
    let options = OpenOptions {
        metadata,
        ..OpenOptions::default()
    };
    route(bytes, &options).map(|loaded| loaded.volume.sweeps.iter().map(|s| s.nrays()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(id: &str) -> PathBuf {
        recast_radar_testdata::path(id).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn opens_each_committed_format_with_the_right_decoder() {
        for (id, format) in [
            ("l2-ktlx-20240315-000217-trim", "NEXRAD Level II"),
            ("odim-bejab-20190606-0000-pvol", "ODIM_H5"),
            ("cfrad1-xsapr-sgp-20110520-ppi-classic", "CfRadial 1"),
            ("dorade-noxp-20090501-190244-ppi", "DORADE"),
            ("jma-n5-20191012-090000-rs47773", "JMA GRIB2"),
            ("l3-byx-n0q-20150124-2106", "NEXRAD Level III"),
            ("odim-au24-20260610-000300-nci-zip-member", "ODIM_H5"),
        ] {
            // The JMA file is not redistributed: skipped unless cached.
            let Some(path) = recast_radar_testdata::path_if_available(id) else {
                continue;
            };
            let input = open_path(&path, &OpenOptions::default())
                .unwrap_or_else(|err| panic!("{id}: {err}"));
            assert_eq!(input.format_name(), format, "{id}");
            assert!(!input.volumes().is_empty(), "{id}");
        }
    }

    #[test]
    fn level3_products_without_a_data_array_still_open() {
        // Product 58, Storm Tracking Information: symbols and tables only.
        let input = open_path(
            &committed("l3-kdvn-20200810-1804-nst"),
            &OpenOptions::default(),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let Contents::Level3(level3) = &input.contents else {
            panic!("NST should open as Level III");
        };
        assert!(level3.volume.is_err());
        assert!(input.volumes().is_empty());
    }

    #[test]
    fn a_jma_station_is_selected_by_id_or_number() {
        let path = recast_radar_testdata::require_file!("jma-n5-20191012-090000-rs47773");
        let options = OpenOptions {
            station: Some("47773".to_owned()),
            ..OpenOptions::default()
        };
        let input = open_path(&path, &options).unwrap_or_else(|err| panic!("{err}"));
        let volumes = input.volumes();
        assert_eq!(volumes.len(), 1);
        let first = open_path(&path, &OpenOptions::default()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            volumes[0].1.attrs.instrument_name,
            first.volumes()[0].1.attrs.instrument_name
        );
    }

    #[test]
    fn a_mobile_archive_opens_as_several_volumes() {
        let input = open_path(
            &committed("dorade-noxp-20090610-003210-heads-zip"),
            &OpenOptions::default(),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let volumes = input.volumes();
        assert!(!volumes.is_empty());
        assert!(volumes.iter().all(|(label, _, _)| label.is_some()));
    }

    #[test]
    fn unrecognised_bytes_report_both_decoders() {
        // The Cargo manifest of this crate: text, not radar data.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let err = match open_path(&path, &OpenOptions::default()) {
            Ok(_) => panic!("a manifest is not a radar file"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("as Level II"), "{err}");
        assert!(err.contains("as Level III"), "{err}");
    }
}
