//! Output backends: the format writers behind `convert` and the GR2Analyst
//! polling-directory publisher behind `publish`.
//!
//! The command-line front end does not write radar formats itself. It opens
//! and decodes the input, picks a [`VolumeWriter`] or the
//! [`PollingPublisher`] from a [`Backends`] registry, and handles everything
//! around the bytes: output paths, atomic replacement, an optional gzip
//! wrapper and error reporting.
//!
//! # Integration
//!
//! The writers (Level II, CfRadial 1, ODIM_H5, FM301) and the publisher live
//! in other crates; [`crate::writers`] adapts them, and [`Backends::builtin`]
//! registers every one. [`Backends::stubs`] holds the stubs
//! [`UnavailableWriter`] and [`UnavailablePublisher`] instead, which fail
//! with [`BackendError::WriterUnavailable`] and
//! [`BackendError::PublisherUnavailable`] (exit status 3); a caller that
//! wants only some formats starts from it:
//!
//! ```text
//! Backends::stubs().with_writer(Box::new(Level2Writer))
//! ```
//!
//! A [`VolumeWriter`] encodes [`WriteInput::volume`] (and, for Level II
//! round trips, [`WriteInput::metadata`]) into `out`. A writer that can also
//! cut a volume into NEXRAD real-time chunks (the `S`, `I` and `E` files of
//! the `unidata-nexrad-level2-chunks` bucket) implements
//! [`VolumeWriter::write_chunks`] and returns `true` from
//! [`VolumeWriter::supports_chunks`]; `convert --chunks` and
//! `recast_radar.write_chunks` then use it.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use recast_radar_core::model::{Volume, split_scan_cycles};
use recast_radar_io::FormatMetadata;
use thiserror::Error;

/// A radar file format that `convert` can be asked to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
#[non_exhaustive]
pub enum OutputFormat {
    /// NEXRAD Archive II (AR2V0006) with Message 31 radials.
    #[value(name = "level2", alias = "nexrad", alias = "ar2v")]
    Level2,
    /// CfRadial 1.x in classic netCDF.
    #[value(name = "cfradial1", alias = "cfradial")]
    CfRadial1,
    /// ODIM_H5 polar volume (PVOL) in HDF5.
    #[value(name = "odim", alias = "odim-h5", alias = "odim_h5")]
    OdimH5,
    /// WMO FM301 (CfRadial 2) in netCDF-4.
    #[value(name = "fm301", alias = "cfradial2")]
    Fm301,
}

impl OutputFormat {
    /// Every format, in the order `--help` lists them.
    pub const ALL: [OutputFormat; 4] = [
        OutputFormat::Level2,
        OutputFormat::CfRadial1,
        OutputFormat::OdimH5,
        OutputFormat::Fm301,
    ];

    /// Display name, for example `NEXRAD Level II`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Level2 => "NEXRAD Level II",
            Self::CfRadial1 => "CfRadial 1",
            Self::OdimH5 => "ODIM_H5",
            Self::Fm301 => "FM301 (CfRadial 2)",
        }
    }

    /// The `--to` value, for example `level2`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Level2 => "level2",
            Self::CfRadial1 => "cfradial1",
            Self::OdimH5 => "odim",
            Self::Fm301 => "fm301",
        }
    }

    /// Conventional file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Level2 => "ar2v",
            Self::CfRadial1 | Self::Fm301 => "nc",
            Self::OdimH5 => "h5",
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// How a Level II writer packs its records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, clap::ValueEnum)]
#[non_exhaustive]
pub enum Level2Compression {
    /// bzip2-compressed LDM records, as the NWS and most polling servers
    /// distribute Level II.
    #[default]
    Bzip2,
    /// Uncompressed records.
    None,
}

/// How a Level II writer codes field values as Message 31 gate codes
/// (`recast_radar_io_nexrad::write::Quantization`, `docs/level2/writer.md`,
/// "Quantisation"). No policy clips a value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, clap::ValueEnum)]
#[non_exhaustive]
pub enum Level2Quantization {
    /// Never coarser than the source: 16-bit moments for 16-bit and float
    /// sources (xradar 0.12 misreads 16-bit REF, VEL and SW).
    #[default]
    Precise,
    /// NEXRAD's word sizes (what xradar 0.12 reads); coarser than a 16-bit
    /// or float source, with the error reported.
    Compatible,
    /// NOAA's current codings where they hold every value, else as
    /// `compatible`.
    Standard,
}

/// Options passed to a [`VolumeWriter`].
///
/// The gzip wrapper is not an option here: the front end applies it to any
/// format after the writer returns. Start from [`WriteOptions::new`] and
/// set the fields you need.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Level II record packing.
    pub level2_compression: Level2Compression,
    /// Radar identifier to write in place of the volume's
    /// `instrument_name` (for Level II, the 4-character ICAO of the volume
    /// header and Message 31).
    pub site_id: Option<String>,
    /// Level II value coding.
    pub level2_quantization: Level2Quantization,
    /// Level II: the radar's Nyquist velocity (m/s), written in every
    /// radial whose source has none (JMA volumes, for example); `None`
    /// writes 0, which readers take as unknown.
    pub nyquist_velocity_mps: Option<f32>,
    /// Level II: the radar's unambiguous range (m), written in every radial
    /// whose source has none.
    pub unambiguous_range_m: Option<f32>,
    /// Level II: leave out gates centred before the radar (Message 1
    /// volumes place their Doppler gates from -375 m) instead of refusing
    /// the field.
    pub drop_negative_range_gates: bool,
    /// Refuse, writing nothing, when the output would leave out a field or
    /// a sweep of the volume ([`WriteReport::left_out`]).
    pub strict: bool,
}

impl WriteOptions {
    /// Options with the given Level II packing and site override, and the
    /// defaults for the rest.
    pub fn new(level2_compression: Level2Compression, site_id: Option<String>) -> Self {
        Self {
            level2_compression,
            site_id,
            ..Self::default()
        }
    }
}

/// What a writer reports beside the bytes it wrote: what the output leaves
/// out of the volume, and remarks such as values coded more coarsely than
/// the source stores them. The front ends show both (the command on
/// standard error, the Python package as warnings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteReport {
    /// Fields and sweeps of the volume the output does not hold, each with
    /// the reason ([`WriteOptions::strict`] refuses the write instead).
    pub left_out: Vec<String>,
    /// Other remarks: codings coarser than the source, radials reordered or
    /// left out for having no data, a missing Nyquist velocity.
    pub notes: Vec<String>,
}

impl WriteReport {
    /// A report of `left_out` and `notes`.
    pub fn new(left_out: Vec<String>, notes: Vec<String>) -> Self {
        Self { left_out, notes }
    }

    /// Whether there is nothing to report.
    pub fn is_empty(&self) -> bool {
        self.left_out.is_empty() && self.notes.is_empty()
    }
}

/// A radar site position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SitePosition {
    /// Latitude, degrees north.
    pub latitude_deg: f64,
    /// Longitude, degrees east.
    pub longitude_deg: f64,
    /// Height of the antenna above mean sea level, metres.
    pub altitude_m: f64,
}

impl SitePosition {
    /// The position of `volume`, when it has a latitude, longitude and
    /// height.
    pub fn of(volume: &Volume) -> Option<Self> {
        let location = volume.location;
        Some(Self {
            latitude_deg: location.latitude_deg?,
            longitude_deg: location.longitude_deg?,
            altitude_m: location.altitude_m?,
        })
        .filter(|position| position.is_valid())
    }

    /// Whether every coordinate is finite and the latitude and longitude
    /// are within 90 and 360 degrees.
    pub fn is_valid(&self) -> bool {
        self.latitude_deg.is_finite()
            && self.longitude_deg.is_finite()
            && self.altitude_m.is_finite()
            && self.latitude_deg.abs() <= 90.0
            && self.longitude_deg.abs() <= 360.0
    }
}

impl std::str::FromStr for SitePosition {
    type Err = String;

    /// `LAT,LON,HEIGHT`: degrees north, degrees east, metres above sea
    /// level.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let values: Vec<f64> = text
            .split(',')
            .map(|part| part.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| format!("`{text}` is not LAT,LON,HEIGHT (three numbers)"))?;
        let [latitude_deg, longitude_deg, altitude_m] = values[..] else {
            return Err(format!("`{text}` is not LAT,LON,HEIGHT (three numbers)"));
        };
        let position = Self {
            latitude_deg,
            longitude_deg,
            altitude_m,
        };
        if position.is_valid() {
            Ok(position)
        } else {
            Err(format!(
                "`{text}`: latitude within 90 degrees, longitude within 360 and a finite height"
            ))
        }
    }
}

/// Changes a front end makes to a decoded volume before writing it: which
/// sweeps, in what order, and the site position. Every format's writer
/// gets the edited volume.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct VolumeEdits {
    /// Keep only these sweeps (0-based indices into the decoded volume), in
    /// this order; each at most once.
    pub sweeps: Option<Vec<usize>>,
    /// Put the sweeps in the order their first rays were collected (a JMA
    /// cycle is collected from its top sweep down).
    pub sweeps_in_time_order: bool,
    /// Make one volume of each scan cycle ([`split_scan_cycles`]) after the
    /// other edits: [`VolumeEdits::apply_each`].
    pub split_scan_cycles: bool,
    /// Site position to set (a Message 1 volume has none).
    pub position: Option<SitePosition>,
}

impl VolumeEdits {
    /// Whether the edits leave a volume as it is.
    pub fn is_empty(&self) -> bool {
        self.sweeps.is_none()
            && !self.sweeps_in_time_order
            && !self.split_scan_cycles
            && self.position.is_none()
    }

    /// The volumes to write: `volume` with the edits made ([`VolumeEdits::apply`]),
    /// then, with [`VolumeEdits::split_scan_cycles`], one volume per scan
    /// cycle in the order the cycles were collected.
    pub fn apply_each<'v>(&self, volume: &'v Volume) -> Result<Vec<Cow<'v, Volume>>, String> {
        let edited = self.apply(volume)?;
        if !self.split_scan_cycles {
            return Ok(vec![edited]);
        }
        Ok(split_scan_cycles(edited.into_owned())
            .into_iter()
            .map(Cow::Owned)
            .collect())
    }

    /// `volume` with the edits made (the sweep selection, order and site
    /// position; not the split, which [`VolumeEdits::apply_each`] makes);
    /// borrowed when there are none. A sweep index past the volume's sweeps,
    /// or given twice, is refused.
    pub fn apply<'v>(&self, volume: &'v Volume) -> Result<Cow<'v, Volume>, String> {
        if self.is_empty() {
            return Ok(Cow::Borrowed(volume));
        }
        let mut edited = volume.clone();
        if let Some(indices) = &self.sweeps {
            let mut taken = vec![false; volume.sweeps.len()];
            let mut sweeps = Vec::with_capacity(indices.len());
            for &index in indices {
                match taken.get_mut(index) {
                    Some(seen) if !*seen => *seen = true,
                    Some(_) => return Err(format!("sweep {index} is selected twice")),
                    None => {
                        return Err(format!(
                            "sweep {index} is not in the volume, which has {} sweeps (0 to {})",
                            volume.sweeps.len(),
                            volume.sweeps.len().saturating_sub(1)
                        ));
                    }
                }
                sweeps.push(volume.sweeps[index].clone());
            }
            edited.sweeps = sweeps;
        }
        if self.sweeps_in_time_order {
            let start = |sweep: &recast_radar_core::model::Sweep| {
                sweep
                    .rays
                    .time_s
                    .first()
                    .copied()
                    .filter(|time| time.is_finite())
                    .unwrap_or(f64::INFINITY)
            };
            edited.sweeps.sort_by(|a, b| start(a).total_cmp(&start(b)));
        }
        if let Some(position) = self.position {
            edited.location.latitude_deg = Some(position.latitude_deg);
            edited.location.longitude_deg = Some(position.longitude_deg);
            edited.location.altitude_m = Some(position.altitude_m);
        }
        Ok(Cow::Owned(edited))
    }
}

/// What a writer or the publisher is given: one decoded volume.
#[derive(Clone, Copy, Debug)]
pub struct WriteInput<'a> {
    /// The volume to write.
    pub volume: &'a Volume,
    /// The source format's metadata decoded beside the volume (NEXRAD
    /// metadata messages for Level II input; [`FormatMetadata::None`] for
    /// other formats and for merged volumes).
    pub metadata: &'a FormatMetadata,
    /// Name of the input file, for messages and provenance.
    pub source_name: Option<&'a str>,
}

/// Kind of a real-time chunk: the letter that ends its object name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChunkKind {
    /// `S`: the volume header and the metadata record.
    Start,
    /// `I`: one record of radials.
    Intermediate,
    /// `E`: the last record of radials.
    End,
}

impl ChunkKind {
    /// The object-name letter: `S`, `I` or `E`.
    pub fn letter(self) -> char {
        match self {
            Self::Start => 'S',
            Self::Intermediate => 'I',
            Self::End => 'E',
        }
    }
}

/// One real-time chunk of a volume.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutputChunk {
    /// The chunk's kind.
    pub kind: ChunkKind,
    /// The chunk's number in the volume, from 1.
    pub number: u16,
    /// The chunk file's bytes.
    pub bytes: Vec<u8>,
}

impl OutputChunk {
    /// A chunk of `kind` numbered `number`.
    pub fn new(kind: ChunkKind, number: u16, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            number,
            bytes,
        }
    }
}

/// A volume written as NEXRAD real-time chunks, in the layout of the
/// `unidata-nexrad-level2-chunks` bucket. Concatenated in order, the chunks
/// are one Archive II file.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChunkedOutput {
    /// Site identifier: the first component of the chunk keys.
    pub site: String,
    /// Volume number (1 to 999): the second component.
    pub volume_number: u16,
    /// Volume start time: the time in the chunk names.
    pub volume_time: DateTime<Utc>,
    /// The chunks, in order.
    pub chunks: Vec<OutputChunk>,
    /// What the writer reports.
    pub report: WriteReport,
}

impl ChunkedOutput {
    /// Chunks of volume `volume_number` of `site`, started at `volume_time`,
    /// with nothing reported.
    pub fn new(
        site: String,
        volume_number: u16,
        volume_time: DateTime<Utc>,
        chunks: Vec<OutputChunk>,
    ) -> Self {
        Self {
            site,
            volume_number,
            volume_time,
            chunks,
            report: WriteReport::default(),
        }
    }

    /// The same chunks with `report`.
    pub fn with_report(mut self, report: WriteReport) -> Self {
        self.report = report;
        self
    }

    /// The object key of `chunk` in the chunks bucket:
    /// `SITE/VOLUME/YYYYMMDD-HHMMSS-NNN-K`, for example
    /// `KIWA/307/20260917-003629-001-S`.
    pub fn chunk_key(&self, chunk: &OutputChunk) -> String {
        format!(
            "{}/{}/{}-{:03}-{}",
            self.site,
            self.volume_number,
            self.volume_time.format("%Y%m%d-%H%M%S"),
            chunk.number,
            chunk.kind.letter()
        )
    }
}

/// Error from a [`VolumeWriter`] or [`PollingPublisher`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BackendError {
    /// This build has no writer for the format.
    #[error("the {} writer is not available in this build", .0.label())]
    WriterUnavailable(OutputFormat),
    /// This build has no polling-directory publisher.
    #[error("the GR2Analyst polling-directory publisher is not available in this build")]
    PublisherUnavailable,
    /// The format's writer in this build cannot write real-time chunks.
    #[error("the {} writer in this build cannot write real-time chunks", .0.label())]
    ChunksUnavailable(OutputFormat),
    /// The format cannot represent something in the volume (for Level II:
    /// a moment with no Message 31 block, more gates than a radial can hold,
    /// and so on).
    #[error("{} cannot represent this volume: {reason}", .format.label())]
    Unrepresentable {
        /// The format being written.
        format: OutputFormat,
        /// What cannot be represented.
        reason: String,
    },
    /// Writing the output failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Any other writer failure.
    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl BackendError {
    /// True for the "not available in this build" errors.
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::WriterUnavailable(_) | Self::PublisherUnavailable | Self::ChunksUnavailable(_)
        )
    }
}

/// Encodes a volume in one radar format.
pub trait VolumeWriter: Send + Sync {
    /// The format this writer produces.
    fn format(&self) -> OutputFormat;

    /// Whether this writer is a real encoder. Stubs return `false`, so
    /// `convert` can refuse before it decodes the input.
    fn is_available(&self) -> bool {
        true
    }

    /// Encode `input` into `out` and report what the output leaves out or
    /// changes. The front end has already created the output file; on
    /// error it removes the partial file.
    fn write(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
        out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError>;

    /// Whether [`Self::write_chunks`] works. `false` unless a writer
    /// overrides it, so `convert --chunks` can refuse before it decodes the
    /// input.
    fn supports_chunks(&self) -> bool {
        false
    }

    /// Encode `input` as NEXRAD real-time chunks (Level II: the start chunk
    /// holds the volume header and the metadata record, every later chunk
    /// one bzip2 LDM record of radials). The default refuses with
    /// [`BackendError::ChunksUnavailable`].
    fn write_chunks(
        &self,
        input: &WriteInput<'_>,
        options: &WriteOptions,
    ) -> Result<ChunkedOutput, BackendError> {
        let _ = (input, options);
        Err(BackendError::ChunksUnavailable(self.format()))
    }
}

/// Where and how `publish` places a volume in a GR2Analyst polling
/// directory.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PublishRequest {
    /// Root of the polling directory (the directory that holds one
    /// subdirectory per site, `config.cfg` and `grlevel2.cfg`).
    pub root: PathBuf,
    /// Site directory and file-name prefix to use instead of the volume's
    /// `instrument_name`.
    pub site: Option<String>,
    /// How many volumes to keep listed in the site's `dir.list`; older files
    /// are removed.
    pub keep: usize,
    /// Level II writer options.
    pub options: WriteOptions,
    /// Add the site to the root `config.cfg` and `grlevel2.cfg` when missing.
    pub update_site_config: bool,
}

impl PublishRequest {
    /// A request for `root` with the defaults: site from the volume, keep 30
    /// volumes, bzip2 records, site configuration updated.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            site: None,
            keep: 30,
            options: WriteOptions::default(),
            update_site_config: true,
        }
    }
}

/// What the publisher did for one volume.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Published {
    /// The site directory name.
    pub site: String,
    /// The Level II file written.
    pub path: PathBuf,
    /// The site's `dir.list`.
    pub dir_list: PathBuf,
    /// Files removed to honour [`PublishRequest::keep`].
    pub removed: Vec<PathBuf>,
    /// What the Level II writer reports.
    pub report: WriteReport,
}

impl Published {
    /// A record of one published file, with nothing reported.
    pub fn new(site: String, path: PathBuf, dir_list: PathBuf, removed: Vec<PathBuf>) -> Self {
        Self {
            site,
            path,
            dir_list,
            removed,
            report: WriteReport::default(),
        }
    }

    /// The same record with `report`.
    pub fn with_report(mut self, report: WriteReport) -> Self {
        self.report = report;
        self
    }
}

/// Places Level II volumes in a GR2Analyst polling directory: one
/// subdirectory per site holding the volumes and a `dir.list` of
/// `<size> <file name>` lines, oldest first.
pub trait PollingPublisher: Send + Sync {
    /// Whether this publisher is real. The stub returns `false`.
    fn is_available(&self) -> bool {
        true
    }

    /// Write `input` as a Level II volume into the polling directory and
    /// update its listing.
    fn publish(
        &self,
        input: &WriteInput<'_>,
        request: &PublishRequest,
    ) -> Result<Published, BackendError>;
}

/// Placeholder for a writer this build does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnavailableWriter(pub OutputFormat);

impl VolumeWriter for UnavailableWriter {
    fn format(&self) -> OutputFormat {
        self.0
    }

    fn is_available(&self) -> bool {
        false
    }

    fn write(
        &self,
        _input: &WriteInput<'_>,
        _options: &WriteOptions,
        _out: &mut dyn Write,
    ) -> Result<WriteReport, BackendError> {
        Err(BackendError::WriterUnavailable(self.0))
    }

    fn write_chunks(
        &self,
        _input: &WriteInput<'_>,
        _options: &WriteOptions,
    ) -> Result<ChunkedOutput, BackendError> {
        Err(BackendError::WriterUnavailable(self.0))
    }
}

/// Placeholder for the polling-directory publisher.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnavailablePublisher;

impl PollingPublisher for UnavailablePublisher {
    fn is_available(&self) -> bool {
        false
    }

    fn publish(
        &self,
        _input: &WriteInput<'_>,
        _request: &PublishRequest,
    ) -> Result<Published, BackendError> {
        Err(BackendError::PublisherUnavailable)
    }
}

/// The writers and the publisher a run of the command uses.
pub struct Backends {
    writers: Vec<Box<dyn VolumeWriter>>,
    publisher: Box<dyn PollingPublisher>,
}

impl Backends {
    /// The backends compiled into this build: every writer of
    /// [`crate::writers`] and the polling-directory publisher.
    pub fn builtin() -> Self {
        use crate::writers::{
            CfRadial1Writer, Fm301Writer, Level2Writer, OdimWriter, PollingDirectoryPublisher,
        };
        Self::stubs()
            .with_writer(Box::new(Level2Writer))
            .with_writer(Box::new(CfRadial1Writer))
            .with_writer(Box::new(OdimWriter))
            .with_writer(Box::new(Fm301Writer))
            .with_publisher(Box::new(PollingDirectoryPublisher))
    }

    /// A stub for every [`OutputFormat`] and the stub publisher.
    pub fn stubs() -> Self {
        Self {
            writers: OutputFormat::ALL
                .into_iter()
                .map(|format| Box::new(UnavailableWriter(format)) as Box<dyn VolumeWriter>)
                .collect(),
            publisher: Box::new(UnavailablePublisher),
        }
    }

    /// Register `writer`, replacing the writer of the same format.
    pub fn with_writer(mut self, writer: Box<dyn VolumeWriter>) -> Self {
        let format = writer.format();
        self.writers.retain(|existing| existing.format() != format);
        self.writers.push(writer);
        self
    }

    /// Register the polling-directory publisher.
    pub fn with_publisher(mut self, publisher: Box<dyn PollingPublisher>) -> Self {
        self.publisher = publisher;
        self
    }

    /// The writer for `format`, or [`BackendError::WriterUnavailable`] when
    /// this build has only a stub for it.
    pub fn writer(&self, format: OutputFormat) -> Result<&dyn VolumeWriter, BackendError> {
        self.writers
            .iter()
            .find(|writer| writer.format() == format && writer.is_available())
            .map(|writer| writer.as_ref())
            .ok_or(BackendError::WriterUnavailable(format))
    }

    /// The publisher, or [`BackendError::PublisherUnavailable`] when this
    /// build has only the stub.
    pub fn publisher(&self) -> Result<&dyn PollingPublisher, BackendError> {
        if self.publisher.is_available() {
            Ok(self.publisher.as_ref())
        } else {
            Err(BackendError::PublisherUnavailable)
        }
    }

    /// Formats with a real writer in this build.
    pub fn available_formats(&self) -> Vec<OutputFormat> {
        OutputFormat::ALL
            .into_iter()
            .filter(|format| self.writer(*format).is_ok())
            .collect()
    }
}

impl Default for Backends {
    fn default() -> Self {
        Self::builtin()
    }
}

impl fmt::Debug for Backends {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backends")
            .field("writers", &self.available_formats())
            .field("publisher", &self.publisher.is_available())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_build_links_every_writer_and_the_publisher() {
        let backends = Backends::builtin();
        assert_eq!(backends.available_formats(), OutputFormat::ALL);
        assert!(backends.publisher().is_ok());
        assert!(
            backends
                .writer(OutputFormat::Level2)
                .is_ok_and(|writer| writer.supports_chunks())
        );
    }

    #[test]
    fn stubs_report_every_writer_and_the_publisher_unavailable() {
        let backends = Backends::stubs();
        assert!(backends.available_formats().is_empty());
        for format in OutputFormat::ALL {
            let err = match backends.writer(format) {
                Ok(_) => panic!("{format} writer should be a stub"),
                Err(err) => err,
            };
            assert!(err.is_unavailable());
            assert_eq!(
                err.to_string(),
                format!(
                    "the {} writer is not available in this build",
                    format.label()
                )
            );
        }
        let err = match backends.publisher() {
            Ok(_) => panic!("publisher should be a stub"),
            Err(err) => err,
        };
        assert!(matches!(err, BackendError::PublisherUnavailable));
    }

    /// A writer that records the volume's site name, to check that
    /// registration replaces the stub.
    struct SiteNameWriter;

    impl VolumeWriter for SiteNameWriter {
        fn format(&self) -> OutputFormat {
            OutputFormat::CfRadial1
        }

        fn write(
            &self,
            input: &WriteInput<'_>,
            _options: &WriteOptions,
            out: &mut dyn Write,
        ) -> Result<WriteReport, BackendError> {
            write!(out, "{}", input.volume.attrs.instrument_name)?;
            Ok(WriteReport::default())
        }
    }

    #[test]
    fn chunk_keys_follow_the_chunks_bucket() {
        let time = DateTime::from_timestamp(1_789_605_389, 0).unwrap_or_default();
        let output = ChunkedOutput::new(
            "KIWA".to_owned(),
            307,
            time,
            vec![
                OutputChunk::new(ChunkKind::Start, 1, Vec::new()),
                OutputChunk::new(ChunkKind::End, 57, Vec::new()),
            ],
        );
        let keys: Vec<String> = output
            .chunks
            .iter()
            .map(|chunk| output.chunk_key(chunk))
            .collect();
        assert_eq!(
            keys,
            [
                "KIWA/307/20260917-003629-001-S",
                "KIWA/307/20260917-003629-057-E"
            ]
        );
        // Writers that do not override `write_chunks` refuse, as do stubs.
        assert!(!SiteNameWriter.supports_chunks());
        assert!(!UnavailableWriter(OutputFormat::Level2).supports_chunks());
    }

    #[test]
    fn a_registered_writer_replaces_its_stub() {
        let backends = Backends::stubs().with_writer(Box::new(SiteNameWriter));
        assert_eq!(backends.available_formats(), [OutputFormat::CfRadial1]);
        assert!(backends.writer(OutputFormat::CfRadial1).is_ok());
        assert!(backends.writer(OutputFormat::Level2).is_err());
    }
}
