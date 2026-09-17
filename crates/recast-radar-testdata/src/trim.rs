//! Trim a real NEXRAD Level II archive volume into a small committed fixture.
//!
//! The output is an archive file made only of bytes taken from the source:
//!
//! - the 24-byte volume header, verbatim;
//! - the metadata record: every message before the first radial (Messages 15,
//!   13, 18, 3, 5, 2, 32, padding, ...), verbatim;
//! - the radials of the first split-cut pair (surveillance sweep followed by
//!   the Doppler sweep at the same elevation), or of the first sweep when
//!   sweeps 1 and 2 are not a split cut. [`TrimOptions::sweeps`] overrides the
//!   sweep count and [`TrimOptions::max_radials`] keeps only the first N
//!   radials of each kept sweep.
//!
//! Messages are never edited. They are grouped into records and each record is
//! re-encoded as an LDM record: a 4-byte big-endian control word holding the
//! compressed length (negative for the last record, as in complete archive
//! files) followed by one bzip2 stream at level 9 from the pure-Rust bzip2
//! implementation. The same input and options always give the same bytes.
//!
//! Records come from the source:
//!
//! - LDM-record sources (2016 onwards) keep their record boundaries. Records
//!   are kept or dropped whole, so a radial limit is applied at a record
//!   boundary (120 radials per record in real files).
//! - Sources of uncompressed messages (`ARCHIVE2`/`AR2V0001` tapes and the
//!   gzip archive objects up to 2016) are framed like LDM files: one record for
//!   the metadata, then records of at most [`RADIALS_PER_RECORD`] radials that
//!   never span two elevations. Non-radial messages stay in the record of the
//!   radial they follow.
//!
//! Whole-file gzip or bzip2 compression of the source is removed.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, Read, Write};

use bzip2::Compression;
use bzip2::write::BzEncoder;
use flate2::read::MultiGzDecoder;

/// Length of the archive volume header (`AR2V0006.123`, date, time, ICAO).
pub const VOLUME_HEADER_LEN: usize = 24;
/// Channel Terminal Manager bytes in front of every message.
pub const CTM_HEADER_LEN: usize = 12;
/// Length of the message header that follows the CTM bytes.
pub const MESSAGE_HEADER_LEN: usize = 16;
/// Length of a fixed-size message, including CTM bytes and frame check sequence.
pub const FIXED_MESSAGE_LEN: usize = 2432;
/// Radials per record when framing a source that has no LDM records.
pub const RADIALS_PER_RECORD: usize = 120;
/// bzip2 level (block size in units of 100 kB) of every output record.
pub const BZIP2_LEVEL: u32 = 9;
/// Name of the command-line tool; manifest `derivation` strings start with it.
pub const TOOL_NAME: &str = "trim-level2";

/// Most decompressed bytes read from one input (decompression-bomb guard).
const MAX_DECOMPRESSED_BYTES: u64 = 1 << 30;
/// Largest mean elevation difference between the two sweeps of a split cut.
const SPLIT_CUT_MAX_ELEVATION_DIFF_DEG: f64 = 0.25;
/// Message body offset of the start of the Message 1/31 fields read here.
const BODY: usize = CTM_HEADER_LEN + MESSAGE_HEADER_LEN;

/// What to keep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimOptions {
    /// Leading sweeps to keep. `None` keeps the first split-cut pair (2 sweeps)
    /// when sweeps 1 and 2 form one, else the first sweep.
    pub sweeps: Option<usize>,
    /// Keep at most this many radials of each kept sweep, rounded down to a
    /// record boundary. `None` keeps whole sweeps.
    pub max_radials: Option<usize>,
    /// Pick the largest record-aligned radial limit whose output fits in this
    /// many bytes. Only used when `max_radials` is `None`.
    pub max_bytes: Option<u64>,
}

impl TrimOptions {
    /// Parse `--sweeps N`, `--max-radials N` and `--max-bytes N`. This is the
    /// form recorded (after [`TOOL_NAME`]) in manifest `derivation` strings.
    pub fn from_args<I, S>(args: I) -> Result<Self, TrimError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let flag = flag.as_ref();
            let slot = match flag {
                "--sweeps" => Slot::Sweeps,
                "--max-radials" => Slot::MaxRadials,
                "--max-bytes" => Slot::MaxBytes,
                other => {
                    return Err(TrimError::InvalidOption(format!(
                        "unknown argument `{other}`"
                    )));
                }
            };
            let value = args
                .next()
                .ok_or_else(|| TrimError::InvalidOption(format!("`{flag}` needs a value")))?;
            let value = value.as_ref();
            let number: u64 = value.parse().map_err(|_| {
                TrimError::InvalidOption(format!(
                    "`{flag}` needs a positive integer, got `{value}`"
                ))
            })?;
            if number == 0 {
                return Err(TrimError::InvalidOption(format!(
                    "`{flag}` must be at least 1"
                )));
            }
            let as_usize = || {
                usize::try_from(number)
                    .map_err(|_| TrimError::InvalidOption(format!("`{flag}` value is too large")))
            };
            match slot {
                Slot::Sweeps => options.sweeps = Some(as_usize()?),
                Slot::MaxRadials => options.max_radials = Some(as_usize()?),
                Slot::MaxBytes => options.max_bytes = Some(number),
            }
        }
        if options.max_radials.is_some() && options.max_bytes.is_some() {
            return Err(TrimError::InvalidOption(
                "`--max-radials` and `--max-bytes` cannot be combined".to_owned(),
            ));
        }
        Ok(options)
    }

    /// Canonical argument string, e.g. `--sweeps 2 --max-radials 240`.
    pub fn to_args(&self) -> String {
        let mut parts = Vec::new();
        if let Some(sweeps) = self.sweeps {
            parts.push(format!("--sweeps {sweeps}"));
        }
        if let Some(max_radials) = self.max_radials {
            parts.push(format!("--max-radials {max_radials}"));
        }
        if let Some(max_bytes) = self.max_bytes {
            parts.push(format!("--max-bytes {max_bytes}"));
        }
        parts.join(" ")
    }

    /// Options recorded in a manifest `derivation` string of the form
    /// `trim-level2 --sweeps 2 --max-radials 240: <explanation>`. Returns
    /// `None` when the string does not start with [`TOOL_NAME`].
    pub fn from_derivation(derivation: &str) -> Option<Result<Self, TrimError>> {
        let command = derivation.split(':').next()?.trim();
        let mut words = command.split_whitespace();
        (words.next()? == TOOL_NAME).then(|| Self::from_args(words))
    }
}

enum Slot {
    Sweeps,
    MaxRadials,
    MaxBytes,
}

/// Why a volume could not be trimmed.
#[derive(Debug)]
pub enum TrimError {
    /// Reading or decompressing the input failed.
    Io(io::Error),
    /// The input does not start with an `AR2V`/`ARCHIVE2` volume header.
    NotLevel2(String),
    /// A record or message is inconsistent.
    Malformed(String),
    /// The volume holds no Message 1 or Message 31 radials.
    NoRadials,
    /// Fewer sweeps than requested.
    NotEnoughSweeps {
        /// Sweeps requested.
        requested: usize,
        /// Sweeps in the volume.
        available: usize,
    },
    /// The radial limit leaves a kept sweep without radials.
    EmptySweep {
        /// 1-based sweep number.
        sweep: usize,
        /// Radials in the sweep's first record.
        first_record_radials: usize,
    },
    /// No record-aligned radial limit fits in the byte budget.
    DoesNotFit {
        /// Byte budget.
        max_bytes: u64,
        /// Size of the smallest candidate output.
        smallest: u64,
    },
    /// Invalid options.
    InvalidOption(String),
}

impl fmt::Display for TrimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "reading Level II input: {error}"),
            Self::NotLevel2(start) => write!(
                f,
                "not a Level II archive volume (no AR2V/ARCHIVE2 volume header; starts with {start})"
            ),
            Self::Malformed(reason) => write!(f, "malformed Level II input: {reason}"),
            Self::NoRadials => write!(f, "volume holds no Message 1 or Message 31 radials"),
            Self::NotEnoughSweeps {
                requested,
                available,
            } => write!(
                f,
                "requested {requested} sweep(s) but the volume has {available}"
            ),
            Self::EmptySweep {
                sweep,
                first_record_radials,
            } => write!(
                f,
                "the radial limit keeps no radials of sweep {sweep} (its first record holds {first_record_radials})"
            ),
            Self::DoesNotFit {
                max_bytes,
                smallest,
            } => write!(
                f,
                "no record-aligned radial limit fits in {max_bytes} bytes (smallest output is {smallest} bytes)"
            ),
            Self::InvalidOption(reason) => write!(f, "invalid trim option: {reason}"),
        }
    }
}

impl std::error::Error for TrimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TrimError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Whole-file compression of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// No whole-file compression.
    None,
    /// gzip (`.gz` archive objects).
    Gzip,
    /// bzip2.
    Bzip2,
}

/// How the source stores its messages after the volume header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// LDM records: control word + bzip2 stream.
    LdmRecords,
    /// Uncompressed messages.
    Messages,
}

/// A trimmed volume.
#[derive(Debug, Clone)]
pub struct Trimmed {
    /// The trimmed archive file.
    pub bytes: Vec<u8>,
    /// What was kept.
    pub report: TrimReport,
}

/// What a trim kept.
#[derive(Debug, Clone, PartialEq)]
pub struct TrimReport {
    /// Resolved options that reproduce this output (`sweeps` always set,
    /// `max_radials` set only when it removed radials, `max_bytes` unset).
    pub options: TrimOptions,
    /// Sweeps 1 and 2 of the source form a split cut.
    pub split_cut: bool,
    /// Whole-file compression of the source.
    pub source_container: Container,
    /// Message framing of the source.
    pub source_framing: Framing,
    /// Records in the source that were read (reading stops after the kept sweeps).
    pub source_records_read: usize,
    /// Messages before the first radial (all kept).
    pub metadata_messages: usize,
    /// Kept sweeps, in order.
    pub sweeps: Vec<SweepReport>,
    /// Records in the output.
    pub records: usize,
    /// Message type -> count in the output.
    pub message_types: BTreeMap<u8, usize>,
    /// Index of the source record behind each output record (LDM record index
    /// for LDM sources, framed record index otherwise).
    pub source_record_indices: Vec<usize>,
}

/// One kept sweep.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepReport {
    /// Elevation number from the radial headers.
    pub elevation_number: u16,
    /// Radial message type (1 or 31).
    pub message_type: u8,
    /// Mean elevation angle of all radials of the sweep, degrees.
    pub mean_elevation_deg: f64,
    /// Radials kept.
    pub radials_kept: usize,
    /// Radials in the source sweep.
    pub radials_total: usize,
    /// Azimuth of the first kept radial, degrees.
    pub first_azimuth_deg: f64,
    /// Azimuth of the last kept radial, degrees.
    pub last_azimuth_deg: f64,
    /// Moment names present in the kept radials, in order of first appearance.
    pub moments: Vec<String>,
}

/// Trim a Level II archive volume (optionally gzip or bzip2 compressed as a
/// whole). See the module documentation for the rules.
pub fn trim_level2(input: &[u8], options: &TrimOptions) -> Result<Trimmed, TrimError> {
    if options.max_radials.is_some() && options.max_bytes.is_some() {
        return Err(TrimError::InvalidOption(
            "`max_radials` and `max_bytes` cannot be combined".to_owned(),
        ));
    }
    if options.sweeps == Some(0) || options.max_radials == Some(0) {
        return Err(TrimError::InvalidOption(
            "`sweeps` and `max_radials` must be at least 1".to_owned(),
        ));
    }
    let complete_sweeps = options.sweeps.unwrap_or(1).max(2);
    let volume = Volume::parse(input, complete_sweeps)?;
    let split_cut = volume.split_cut();
    let sweeps = options.sweeps.unwrap_or(if split_cut { 2 } else { 1 });
    if sweeps > volume.sweeps.len() {
        return Err(TrimError::NotEnoughSweeps {
            requested: sweeps,
            available: volume.sweeps.len(),
        });
    }

    let mut compressed: HashMap<usize, Vec<u8>> = HashMap::new();
    let max_radials = match (options.max_radials, options.max_bytes) {
        (Some(limit), _) => Some(limit),
        (None, None) => None,
        (None, Some(max_bytes)) => {
            let mut smallest = u64::MAX;
            let mut chosen = None;
            for limit in volume.radial_limit_candidates(sweeps) {
                let kept = match volume.select(sweeps, limit) {
                    Ok(kept) => kept,
                    Err(TrimError::EmptySweep { .. }) => continue,
                    Err(error) => return Err(error),
                };
                let size = volume.encoded_len(&kept, &mut compressed)?;
                smallest = smallest.min(size);
                if size <= max_bytes {
                    chosen = Some(limit);
                    break;
                }
            }
            match chosen {
                Some(limit) => limit,
                None => {
                    return Err(TrimError::DoesNotFit {
                        max_bytes,
                        smallest,
                    });
                }
            }
        }
    };

    let kept = volume.select(sweeps, max_radials)?;
    let bytes = volume.encode(&kept, &mut compressed)?;
    let report = volume.report(sweeps, max_radials, split_cut, &kept);
    Ok(Trimmed { bytes, report })
}

#[derive(Debug, Clone, Copy)]
struct Radial {
    elevation_number: u16,
    elevation_deg: f32,
    azimuth_deg: f32,
    has_velocity: bool,
}

#[derive(Debug, Clone)]
struct Message {
    offset: usize,
    len: usize,
    msg_type: u8,
    radial: Option<Radial>,
    /// (sweep index, radial index within the sweep) for radials.
    position: Option<(usize, usize)>,
}

#[derive(Debug, Default)]
struct Record {
    data: Vec<u8>,
    messages: Vec<Message>,
}

#[derive(Debug)]
struct Sweep {
    elevation_number: u16,
    message_type: u8,
    radials: usize,
    elevation_sum: f64,
    has_velocity: bool,
}

#[derive(Debug)]
struct Volume {
    header: Vec<u8>,
    container: Container,
    framing: Framing,
    records: Vec<Record>,
    sweeps: Vec<Sweep>,
}

impl Volume {
    /// Parse until the first radial of sweep `complete_sweeps + 1` (0-based
    /// index `complete_sweeps`) or the end of the input.
    fn parse(input: &[u8], complete_sweeps: usize) -> Result<Self, TrimError> {
        let (container, reader): (Container, Box<dyn Read + '_>) =
            if input.starts_with(&[0x1f, 0x8b]) {
                (Container::Gzip, Box::new(MultiGzDecoder::new(input)))
            } else if input.starts_with(b"BZh") {
                (
                    Container::Bzip2,
                    Box::new(bzip2::read::MultiBzDecoder::new(input)),
                )
            } else {
                (Container::None, Box::new(input))
            };
        let mut reader = reader.take(MAX_DECOMPRESSED_BYTES);

        let mut header = vec![0u8; VOLUME_HEADER_LEN];
        read_full(&mut reader, &mut header, "volume header")?;
        if !(header.starts_with(b"AR2V") || header.starts_with(b"ARCHIVE2")) {
            let start: String = header[..12]
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() {
                        char::from(b)
                    } else {
                        '.'
                    }
                })
                .collect();
            return Err(TrimError::NotLevel2(format!("{start:?}")));
        }

        let mut peek = [0u8; CTM_HEADER_LEN];
        let peeked = read_up_to(&mut reader, &mut peek)?;
        let framing = if peeked == CTM_HEADER_LEN && &peek[4..7] == b"BZh" {
            Framing::LdmRecords
        } else {
            Framing::Messages
        };
        let mut reader = (&peek[..peeked]).chain(reader);

        let mut volume = Self {
            header,
            container,
            framing,
            records: Vec::new(),
            sweeps: Vec::new(),
        };
        match framing {
            Framing::LdmRecords => volume.read_ldm_records(&mut reader, complete_sweeps)?,
            Framing::Messages => volume.read_messages(&mut reader, complete_sweeps)?,
        }
        if volume.sweeps.is_empty() {
            return Err(TrimError::NoRadials);
        }
        Ok(volume)
    }

    fn read_ldm_records(
        &mut self,
        reader: &mut impl Read,
        complete_sweeps: usize,
    ) -> Result<(), TrimError> {
        loop {
            let mut control = [0u8; 4];
            let got = read_up_to(reader, &mut control)?;
            if got == 0 {
                return Ok(());
            }
            let index = self.records.len();
            if got < control.len() {
                return Err(TrimError::Malformed(format!(
                    "truncated control word of LDM record {index}"
                )));
            }
            let len = usize::try_from(i32::from_be_bytes(control).unsigned_abs())
                .map_err(|_| TrimError::Malformed("LDM record length overflow".to_owned()))?;
            if len == 0 {
                return Err(TrimError::Malformed(format!(
                    "LDM record {index} has a zero control word"
                )));
            }
            let mut payload = vec![0u8; len];
            read_full(reader, &mut payload, "LDM record")?;
            let data = bunzip_record(&payload, index)?;
            let messages = walk_messages(&data, &format!("LDM record {index}"))?;
            let mut record = Record { data, messages };
            let done = self.assign_positions(&mut record.messages, complete_sweeps);
            self.records.push(record);
            if done {
                return Ok(());
            }
        }
    }

    fn read_messages(
        &mut self,
        reader: &mut impl Read,
        complete_sweeps: usize,
    ) -> Result<(), TrimError> {
        let mut data = Vec::new();
        let mut messages = Vec::new();
        loop {
            let offset = data.len();
            let mut head = [0u8; BODY];
            let got = read_up_to(reader, &mut head)?;
            if got == 0 {
                break;
            }
            if got < BODY {
                return Err(TrimError::Malformed(format!(
                    "truncated message header at uncompressed offset {offset}"
                )));
            }
            let len = message_len(&head, offset)?;
            data.extend_from_slice(&head);
            let rest = len - BODY;
            let start = data.len();
            data.resize(start + rest, 0);
            read_full(reader, &mut data[start..], "message")?;
            let mut message = parse_message(&data[offset..], offset, len)?;
            let done = self.assign_positions(std::slice::from_mut(&mut message), complete_sweeps);
            messages.push(message);
            if done {
                break;
            }
        }
        self.frame_messages(data, messages);
        Ok(())
    }

    /// Assign sweep positions to the radials in `messages`; returns true once
    /// a radial of sweep index `complete_sweeps` has been seen.
    fn assign_positions(&mut self, messages: &mut [Message], complete_sweeps: usize) -> bool {
        let mut done = false;
        for message in messages {
            let Some(radial) = message.radial else {
                continue;
            };
            let new_sweep = self
                .sweeps
                .last()
                .is_none_or(|sweep| sweep.elevation_number != radial.elevation_number);
            if new_sweep {
                self.sweeps.push(Sweep {
                    elevation_number: radial.elevation_number,
                    message_type: message.msg_type,
                    radials: 0,
                    elevation_sum: 0.0,
                    has_velocity: false,
                });
            }
            let index = self.sweeps.len() - 1;
            if let Some(sweep) = self.sweeps.last_mut() {
                message.position = Some((index, sweep.radials));
                sweep.radials += 1;
                sweep.elevation_sum += f64::from(radial.elevation_deg);
                sweep.has_velocity |= radial.has_velocity;
            }
            if index >= complete_sweeps {
                done = true;
            }
        }
        done
    }

    /// Group uncompressed messages into records: the metadata, then at most
    /// [`RADIALS_PER_RECORD`] radials of one elevation per record.
    fn frame_messages(&mut self, data: Vec<u8>, messages: Vec<Message>) {
        let mut current: Option<(Record, usize, u16)> = None;
        let mut metadata = Record::default();
        for message in messages {
            let bytes = &data[message.offset..message.offset + message.len];
            match (message.radial, current.as_mut()) {
                (None, None) => push_message(&mut metadata, bytes, message),
                (None, Some((record, _, _))) => push_message(record, bytes, message),
                (Some(radial), slot) => {
                    let start_new = slot.is_none_or(|(_, radials, elevation)| {
                        *radials == RADIALS_PER_RECORD || *elevation != radial.elevation_number
                    });
                    if start_new {
                        if let Some((record, _, _)) = current.take() {
                            self.records.push(record);
                        } else if !metadata.messages.is_empty() {
                            self.records.push(std::mem::take(&mut metadata));
                        }
                        current = Some((Record::default(), 0, radial.elevation_number));
                    }
                    if let Some((record, radials, _)) = current.as_mut() {
                        push_message(record, bytes, message);
                        *radials += 1;
                    }
                }
            }
        }
        if let Some((record, _, _)) = current {
            self.records.push(record);
        } else if !metadata.messages.is_empty() {
            self.records.push(metadata);
        }
    }

    fn split_cut(&self) -> bool {
        match (self.sweeps.first(), self.sweeps.get(1)) {
            (Some(first), Some(second)) => {
                let mean = |s: &Sweep| s.elevation_sum / s.radials.max(1) as f64;
                !first.has_velocity
                    && second.has_velocity
                    && (mean(first) - mean(second)).abs() <= SPLIT_CUT_MAX_ELEVATION_DIFF_DEG
            }
            _ => false,
        }
    }

    /// Radial limits to try when fitting a byte budget, largest first: no
    /// limit, then every record boundary inside the kept sweeps.
    fn radial_limit_candidates(&self, sweeps: usize) -> Vec<Option<usize>> {
        let longest = self.sweeps.iter().take(sweeps).map(|s| s.radials).max();
        let mut ends: Vec<usize> = self
            .records
            .iter()
            .filter_map(|record| {
                let positions: Vec<_> = record.messages.iter().filter_map(|m| m.position).collect();
                let in_kept = !positions.is_empty() && positions.iter().all(|&(s, _)| s < sweeps);
                in_kept.then(|| positions.iter().map(|&(_, i)| i + 1).max())?
            })
            .filter(|&end| longest.is_some_and(|longest| end < longest))
            .collect();
        ends.sort_unstable_by(|a, b| b.cmp(a));
        ends.dedup();
        std::iter::once(None)
            .chain(ends.into_iter().map(Some))
            .collect()
    }

    /// Indices of the records to keep.
    fn select(&self, sweeps: usize, max_radials: Option<usize>) -> Result<Vec<usize>, TrimError> {
        let limit = max_radials.unwrap_or(usize::MAX);
        #[derive(Clone, Copy, PartialEq)]
        enum Kind {
            Metadata,
            Kept,
            Dropped,
            NoRadials,
        }
        let mut seen_radial = false;
        let kinds: Vec<Kind> = self
            .records
            .iter()
            .map(|record| {
                let mut positions = record.messages.iter().filter_map(|m| m.position).peekable();
                if positions.peek().is_none() {
                    return if seen_radial {
                        Kind::NoRadials
                    } else {
                        Kind::Metadata
                    };
                }
                seen_radial = true;
                if positions.all(|(sweep, index)| sweep < sweeps && index < limit) {
                    Kind::Kept
                } else {
                    Kind::Dropped
                }
            })
            .collect();
        let radial_kind = |range: &mut dyn Iterator<Item = usize>| {
            range
                .map(|i| kinds[i])
                .find(|k| matches!(k, Kind::Kept | Kind::Dropped))
        };
        let kept: Vec<usize> = (0..kinds.len())
            .filter(|&i| match kinds[i] {
                Kind::Metadata | Kind::Kept => true,
                Kind::Dropped => false,
                Kind::NoRadials => {
                    radial_kind(&mut (0..i).rev()) == Some(Kind::Kept)
                        && radial_kind(&mut (i + 1..kinds.len())) == Some(Kind::Kept)
                }
            })
            .collect();

        for sweep in 0..sweeps {
            let kept_radials = kept
                .iter()
                .flat_map(|&r| &self.records[r].messages)
                .filter(|m| m.position.is_some_and(|(s, _)| s == sweep))
                .count();
            if kept_radials == 0 {
                let first_record_radials = self
                    .records
                    .iter()
                    .map(|record| {
                        record
                            .messages
                            .iter()
                            .filter(|m| m.position.is_some_and(|(s, _)| s == sweep))
                            .count()
                    })
                    .find(|&n| n > 0)
                    .unwrap_or(0);
                return Err(TrimError::EmptySweep {
                    sweep: sweep + 1,
                    first_record_radials,
                });
            }
        }
        Ok(kept)
    }

    fn compressed<'a>(
        &self,
        record: usize,
        cache: &'a mut HashMap<usize, Vec<u8>>,
    ) -> Result<&'a [u8], TrimError> {
        match cache.entry(record) {
            Entry::Occupied(entry) => Ok(entry.into_mut().as_slice()),
            Entry::Vacant(entry) => {
                let mut encoder = BzEncoder::new(Vec::new(), Compression::new(BZIP2_LEVEL));
                encoder.write_all(&self.records[record].data)?;
                Ok(entry.insert(encoder.finish()?).as_slice())
            }
        }
    }

    fn encoded_len(
        &self,
        kept: &[usize],
        cache: &mut HashMap<usize, Vec<u8>>,
    ) -> Result<u64, TrimError> {
        let mut len = VOLUME_HEADER_LEN as u64;
        for &record in kept {
            len += 4 + self.compressed(record, cache)?.len() as u64;
        }
        Ok(len)
    }

    fn encode(
        &self,
        kept: &[usize],
        cache: &mut HashMap<usize, Vec<u8>>,
    ) -> Result<Vec<u8>, TrimError> {
        let mut out = self.header.clone();
        for (n, &record) in kept.iter().enumerate() {
            let payload = self.compressed(record, cache)?;
            let len = i32::try_from(payload.len()).map_err(|_| {
                TrimError::Malformed(format!("record {record} compresses beyond i32::MAX bytes"))
            })?;
            let control = if n + 1 == kept.len() { -len } else { len };
            out.extend_from_slice(&control.to_be_bytes());
            out.extend_from_slice(payload);
        }
        Ok(out)
    }

    fn report(
        &self,
        sweeps: usize,
        max_radials: Option<usize>,
        split_cut: bool,
        kept: &[usize],
    ) -> TrimReport {
        let mut message_types = BTreeMap::new();
        for &r in kept {
            for message in &self.records[r].messages {
                *message_types.entry(message.msg_type).or_insert(0) += 1;
            }
        }
        let metadata_messages = self
            .records
            .iter()
            .flat_map(|r| &r.messages)
            .take_while(|m| m.radial.is_none())
            .count();
        let sweep_reports = (0..sweeps)
            .filter_map(|index| {
                let sweep = self.sweeps.get(index)?;
                let mut radials_kept = 0;
                let mut azimuths = (f64::NAN, f64::NAN);
                let mut moments: Vec<String> = Vec::new();
                for &r in kept {
                    let record = &self.records[r];
                    for message in &record.messages {
                        if message.position.is_none_or(|(s, _)| s != index) {
                            continue;
                        }
                        let Some(radial) = message.radial else {
                            continue;
                        };
                        let azimuth = f64::from(radial.azimuth_deg);
                        if radials_kept == 0 {
                            azimuths.0 = azimuth;
                        }
                        azimuths.1 = azimuth;
                        radials_kept += 1;
                        let bytes = &record.data[message.offset..message.offset + message.len];
                        for name in moments_of(message.msg_type, bytes) {
                            if !moments.contains(&name) {
                                moments.push(name);
                            }
                        }
                    }
                }
                Some(SweepReport {
                    elevation_number: sweep.elevation_number,
                    message_type: sweep.message_type,
                    mean_elevation_deg: sweep.elevation_sum / sweep.radials.max(1) as f64,
                    radials_kept,
                    radials_total: sweep.radials,
                    first_azimuth_deg: azimuths.0,
                    last_azimuth_deg: azimuths.1,
                    moments,
                })
            })
            .collect();
        let limited = max_radials.filter(|&limit| {
            self.sweeps
                .iter()
                .take(sweeps)
                .any(|sweep| sweep.radials > limit)
        });
        TrimReport {
            options: TrimOptions {
                sweeps: Some(sweeps),
                max_radials: limited,
                max_bytes: None,
            },
            split_cut,
            source_container: self.container,
            source_framing: self.framing,
            source_records_read: match self.framing {
                Framing::LdmRecords => self.records.len(),
                Framing::Messages => 0,
            },
            metadata_messages,
            sweeps: sweep_reports,
            records: kept.len(),
            message_types,
            source_record_indices: kept.to_vec(),
        }
    }
}

fn push_message(record: &mut Record, bytes: &[u8], mut message: Message) {
    message.offset = record.data.len();
    record.data.extend_from_slice(bytes);
    record.messages.push(message);
}

fn read_up_to(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn read_full(reader: &mut impl Read, buf: &mut [u8], what: &str) -> Result<(), TrimError> {
    let got = read_up_to(reader, buf)?;
    if got == buf.len() {
        Ok(())
    } else {
        Err(TrimError::Malformed(format!(
            "truncated {what}: expected {} bytes, got {got}",
            buf.len()
        )))
    }
}

fn bunzip_record(payload: &[u8], index: usize) -> Result<Vec<u8>, TrimError> {
    let mut decoder = bzip2::bufread::BzDecoder::new(payload);
    let mut data = Vec::new();
    (&mut decoder)
        .take(MAX_DECOMPRESSED_BYTES)
        .read_to_end(&mut data)
        .map_err(|e| TrimError::Malformed(format!("LDM record {index}: bzip2: {e}")))?;
    if decoder.total_in() != payload.len() as u64 {
        return Err(TrimError::Malformed(format!(
            "LDM record {index}: bzip2 stream ends after {} of {} bytes",
            decoder.total_in(),
            payload.len()
        )));
    }
    Ok(data)
}

/// Byte length of the message whose CTM bytes start `head` (at least
/// [`BODY`] bytes), following the ICD rules used by Py-ART and MetPy.
fn message_len(head: &[u8], offset: usize) -> Result<usize, TrimError> {
    let size_hw = usize::from(u16::from_be_bytes([head[12], head[13]]));
    let msg_type = head[15];
    let segments = usize::from(u16::from_be_bytes([head[24], head[25]]));
    let segment = usize::from(u16::from_be_bytes([head[26], head[27]]));
    let len = match size_hw {
        // Padding: a zero-filled fixed-size message.
        0 => FIXED_MESSAGE_LEN,
        // Size in bytes carried by the segment fields (Message 29, large messages).
        0xFFFF => CTM_HEADER_LEN + ((segments << 16) | segment),
        _ if matches!(msg_type, 29 | 31) => CTM_HEADER_LEN + 2 * size_hw,
        _ => FIXED_MESSAGE_LEN,
    };
    if len < BODY {
        return Err(TrimError::Malformed(format!(
            "message type {msg_type} at offset {offset} is {len} bytes, shorter than its header"
        )));
    }
    Ok(len)
}

fn walk_messages(data: &[u8], context: &str) -> Result<Vec<Message>, TrimError> {
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let head = data.get(offset..offset + BODY).ok_or_else(|| {
            TrimError::Malformed(format!(
                "{context}: {} trailing bytes at offset {offset}",
                data.len() - offset
            ))
        })?;
        let len = message_len(head, offset)?;
        let bytes = data.get(offset..offset + len).ok_or_else(|| {
            TrimError::Malformed(format!(
                "{context}: message at offset {offset} needs {len} bytes, {} remain",
                data.len() - offset
            ))
        })?;
        messages.push(parse_message(bytes, offset, len)?);
        offset += len;
    }
    Ok(messages)
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_f32(bytes: &[u8], at: usize) -> Option<f32> {
    Some(f32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// `bytes` starts at the message's CTM bytes and holds at least `len` bytes.
fn parse_message(bytes: &[u8], offset: usize, len: usize) -> Result<Message, TrimError> {
    let size_hw = u16::from_be_bytes([bytes[12], bytes[13]]);
    let msg_type = bytes[15];
    let body = &bytes[BODY..len];
    let malformed = |what: &str| {
        TrimError::Malformed(format!(
            "Message {msg_type} at offset {offset}: {what} outside the message"
        ))
    };
    let radial = match (msg_type, size_hw) {
        (_, 0) => None,
        (1, _) => {
            let angle = |raw: u16| f32::from(raw) * 180.0 / 32768.0;
            let doppler_gates = be_u16(body, 28).ok_or_else(|| malformed("Doppler gates"))?;
            let velocity_pointer = be_u16(body, 38).ok_or_else(|| malformed("velocity pointer"))?;
            Some(Radial {
                elevation_number: be_u16(body, 16).ok_or_else(|| malformed("elevation number"))?,
                elevation_deg: angle(be_u16(body, 14).ok_or_else(|| malformed("elevation"))?),
                azimuth_deg: angle(be_u16(body, 8).ok_or_else(|| malformed("azimuth"))?),
                has_velocity: doppler_gates != 0 && velocity_pointer != 0,
            })
        }
        (31, _) => Some(Radial {
            elevation_number: u16::from(
                *body.get(22).ok_or_else(|| malformed("elevation number"))?,
            ),
            elevation_deg: be_f32(body, 24).ok_or_else(|| malformed("elevation"))?,
            azimuth_deg: be_f32(body, 12).ok_or_else(|| malformed("azimuth"))?,
            has_velocity: moments_of(msg_type, bytes).iter().any(|m| m == "VEL"),
        }),
        _ => None,
    };
    Ok(Message {
        offset,
        len,
        msg_type,
        radial,
        position: None,
    })
}

/// Moment names of a Message 1 or 31 (`bytes` starts at the CTM bytes).
fn moments_of(msg_type: u8, bytes: &[u8]) -> Vec<String> {
    let Some(body) = bytes.get(BODY..) else {
        return Vec::new();
    };
    match msg_type {
        1 => {
            let surveillance_gates = be_u16(body, 26).unwrap_or(0);
            let doppler_gates = be_u16(body, 28).unwrap_or(0);
            [
                ("REF", surveillance_gates, 36),
                ("VEL", doppler_gates, 38),
                ("SW", doppler_gates, 40),
            ]
            .into_iter()
            .filter(|&(_, gates, at)| gates != 0 && be_u16(body, at).is_some_and(|p| p != 0))
            .map(|(name, _, _)| name.to_owned())
            .collect()
        }
        31 => {
            let blocks = usize::from(be_u16(body, 30).unwrap_or(0));
            (0..blocks)
                .filter_map(|i| be_u32(body, 32 + 4 * i))
                .filter_map(|pointer| {
                    let start = usize::try_from(pointer).ok().filter(|&p| p != 0)?;
                    let tag = body.get(start..start + 4)?;
                    (tag[0] == b'D').then(|| String::from_utf8_lossy(&tag[1..]).trim().to_owned())
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_through_args() {
        let parsed = TrimOptions::from_args(["--sweeps", "2", "--max-radials", "240"])
            .map_err(|e| e.to_string());
        let expected = TrimOptions {
            sweeps: Some(2),
            max_radials: Some(240),
            max_bytes: None,
        };
        assert_eq!(parsed, Ok(expected));
        assert_eq!(expected.to_args(), "--sweeps 2 --max-radials 240");
        let from_derivation = TrimOptions::from_derivation(
            "trim-level2 --sweeps 2 --max-radials 240: volume header, metadata record, ...",
        )
        .map(|r| r.map_err(|e| e.to_string()));
        assert_eq!(from_derivation, Some(Ok(expected)));
        assert!(TrimOptions::from_derivation("gzip -d").is_none());
    }

    #[test]
    fn options_reject_bad_arguments() {
        for args in [
            &["--sweeps"][..],
            &["--sweeps", "0"],
            &["--sweeps", "two"],
            &["--radials", "3"],
            &["--max-radials", "240", "--max-bytes", "1000000"],
        ] {
            assert!(TrimOptions::from_args(args).is_err(), "{args:?}");
        }
    }
}
