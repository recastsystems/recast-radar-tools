//! Chunk timing model for the NEXRAD real-time Level II chunk feed
//! (`unidata-nexrad-level2-chunks`).
//!
//! # What the feed looks like
//!
//! A volume is published as keys `SITE/VOLID/YYYYMMDD-HHMMSS-CCC-T`. The
//! timestamp in the key is the volume start (the first radial's time, cut to
//! whole seconds), `CCC` counts chunks from 1, and `T` is `S`, `I` or `E`.
//! Chunk 1 (Start) holds the volume header and the metadata messages,
//! including Message 5, the cut sequence the radar is running. Every later
//! chunk is one LDM record of [`RADIALS_PER_CHUNK`] Message 31 radials, and a
//! chunk never holds radials from two cuts. So a 720-radial (0.5 deg) cut
//! fills 6 chunks and a 360-radial cut fills 3. TDWR cuts carry 357-358
//! radials and still take 3. The End chunk closes the last cut that was
//! collected. With AVSET that can come before the plan's last cut. The only
//! clock the listing offers is S3 `LastModified`, which is whole seconds.
//!
//! Some radars also publish status-only chunks between cuts: one LDM record
//! holding a single Message 2 (RDA status) and no radials, about 130 bytes
//! (KMUX, 2026-09-17: one to three per volume). Each shifts the ids of the
//! chunks after it by one. The model therefore counts *plan chunk numbers*:
//! the Start chunk is 1 and radial chunks are numbered as the plan lays them
//! out. [`VolumeObservation::plan_chunk_number`] maps a listed id to its plan
//! number, recognizing status-only chunks by size
//! ([`MIN_RADIAL_CHUNK_BYTES`]).
//!
//! # Model
//!
//! For radial chunk `k` in cut `i`, where the cut has `N` radials, commanded
//! rotation time `R_i = 360 / azimuth rate`, and the chunk is part `p`
//! (from 0):
//!
//! ```text
//! E_k   = R_0 + ... + R_(i-1) + R_i * min((p + 1) * 120, N) / N
//! LM_k  = V + publish_offset + rotation_scale * E_k + inter_cut_gap * i
//! LM_1  = V + start_chunk_offset
//! V'    = V + rotation_scale * (R_0 + ... + R_last) + inter_cut_gap * last
//!           + inter_volume_gap
//! ```
//!
//! Here `V` is the volume key time and `V'` is the next volume's key time.
//! A sweep lasts `rotation_scale * R_i` and starts
//! `rotation_scale * (R_0 + ... + R_(i-1)) + inter_cut_gap * i` after `V`.
//!
//! The rotation times come from a [`ScanPlan`]. Two sources:
//!
//! - [`ScanPlan::new`] with the cut table of the Message 5 in the volume's
//!   own Start chunk (elevation, azimuth rate and super-resolution flag per
//!   cut). That table already includes SAILS, MESO-SAILS, MRLE and base-tilt
//!   insertions and the azimuth rates the radar actually runs. This crate
//!   does not decode Level II messages: decode the Start chunk with
//!   `recast_radar_io_nexrad::NexradMetadata::from_metadata_record` and map
//!   each `VcpCut` to [`ScanCut::new`] (elevation, azimuth rate, and
//!   [`ScanCut::radials_for_azimuth_spacing`] of its half-degree azimuth
//!   bit). `tests/timing.rs` checks that this gives the same plans as the
//!   capture tables of `tools/capture_chunk_listings.py`, a Python decoder
//!   checked against MetPy's `Level2File`, which the other timing tests use.
//! - [`ScanPlan::from_build24`], an approximate plan from the Build 24
//!   Appendix C table in [`super::vcp_catalog`] when only the VCP number is
//!   known. It lacks inserted cuts and staggered-PRT batch cuts: its cut
//!   layout matched the executed one in 6 of 21 NEXRAD volumes of the fitted
//!   captures and 12 of 24 of the held-out ones (see `tests/timing.rs`).
//!
//! # Learned statistics and projection
//!
//! [`TimingParameters::WSR88D`] was measured on one volume, KIWA volume 307
//! (the TD.1 live capture), plus its rollover to 308. Real sites differ. In
//! the committed captures, WSR-88D rotations take 0.93 to 0.99 of the
//! commanded time depending on the site, and TDWR publishes about 25 s after
//! collection instead of about 3 s. The inter-cut gap is never learned (see
//! [`TimingStatistics`]); it stays at the 1.1 s measured on KIWA 307.
//! [`TimingStatistics`] fits `rotation_scale` and `publish_offset` to each
//! completed volume's `LastModified` times. It uses a Theil-Sen estimator, so
//! late-published chunks (up to 15 s late in the captures) do not bias it. It
//! also learns the Start chunk offset, the gap between volumes (from
//! consecutive volumes), and the highest elevation collected (the AVSET
//! cutoff). [`ScanTimingModel::project`] then projects the rest of an
//! in-progress volume: every chunk's expected `LastModified`, when each cut
//! completes, the End chunk, and the next volume. It anchors on the lower
//! quartile of the latest observed residuals.
//!
//! # Known limits
//!
//! Found on captures held out from building the model (`tests/timing.rs`
//! runs every claim on them and pins each deviation):
//!
//! - The End chunk is predicted from the previous volume's top elevation.
//!   When AVSET changes the top between volumes (KBUF 54 -> 55), the
//!   prediction misses by a cut until chunks past it appear.
//! - TDWR Start chunks trail the key time by a varying 27-34 s (TLAS), so the
//!   next Start chunk can be several seconds off even when the key time is
//!   right.
//! - TDWR VCP 80 (TDEN) reports azimuth rates for some cuts that differ from
//!   the rotation it executes, which displaces the chunks after those cuts.

use std::collections::VecDeque;
use std::ops::RangeInclusive;

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use super::vcp_catalog::{VcpDefinition, Waveform};
use crate::{RealtimeChunkObject, RealtimeChunkType};

/// Message 31 radials in one real-time chunk.
pub const RADIALS_PER_CHUNK: u16 = 120;

/// Chunk id of the Start chunk (volume header and metadata, no radials).
pub const START_CHUNK_ID: u16 = 1;

/// Largest chunk id the three-digit key field can carry.
pub const MAX_CHUNK_ID: u16 = 999;

/// Intermediate and End chunks smaller than this hold no radials: they are
/// status-only chunks (one bzip2 record of a single Message 2, 127-130 bytes
/// at KMUX). The smallest radial chunk in the committed captures is 7404
/// bytes (TLAS, a TDWR); 120 compressed Message 31 radials do not fit in
/// 1 KiB.
pub const MIN_RADIAL_CHUNK_BYTES: u64 = 1024;

/// Chunk pairs closer than this (in commanded rotation seconds) are left out
/// of the slope fit: whole-second `LastModified` noise dominates them.
const MIN_SLOPE_PAIR_SECONDS: f64 = 30.0;
/// Theil-Sen is quadratic; volumes have about 100 chunks, so this cap only
/// guards against malformed input.
const MAX_FIT_POINTS: usize = 400;
/// A fitted rotation scale outside this range means the plan does not
/// describe the volume (wrong VCP, missing insertions).
const PLAUSIBLE_ROTATION_SCALE: RangeInclusive<f64> = 0.8..=1.25;
/// Residuals used to anchor a projection on the latest observations.
const ANCHOR_RESIDUALS: usize = 9;
/// Elevation slack when comparing a plan's cuts with a learned AVSET top.
const ELEVATION_TOLERANCE_DEG: f32 = 0.05;
/// Consecutive volumes further apart than this do not teach a rollover gap.
const MAX_ROLLOVER_SECONDS: f64 = 3600.0;

/// Errors from building a timing model or learning from a volume.
#[derive(Clone, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum TimingError {
    /// The plan has no cuts.
    #[error("scan plan has no cuts")]
    EmptyPlan,
    /// A cut's azimuth rate is not positive and finite.
    #[error("cut {cut_index} has azimuth rate {rate} deg/s; a positive finite rate is required")]
    InvalidAzimuthRate {
        /// Index of the cut.
        cut_index: usize,
        /// Its azimuth rate, degrees per second.
        rate: f32,
    },
    /// A cut has no radials.
    #[error("cut {cut_index} has no radials")]
    NoRadials {
        /// Index of the cut.
        cut_index: usize,
    },
    /// The plan needs more chunks than a volume's chunk ids allow.
    #[error("scan plan needs {chunks} chunks but chunk ids stop at {MAX_CHUNK_ID}")]
    TooManyChunks {
        /// Chunks the plan needs.
        chunks: u32,
    },
    /// The volume has no End chunk yet, so it cannot be learned from.
    #[error("volume {volume_id} has no End chunk yet")]
    IncompleteVolume {
        /// The volume id.
        volume_id: u16,
    },
    /// The volume's End chunk does not close a cut of the plan (another VCP ran).
    #[error(
        "volume {volume_id} ends at chunk {end_chunk_id}, which does not close a cut of the scan plan"
    )]
    PlanMismatch {
        /// The volume id.
        volume_id: u16,
        /// Key id of its End chunk.
        end_chunk_id: u16,
    },
    /// Fewer than 3 radial chunks carry timestamps.
    #[error("volume {volume_id} has {chunks} timestamped radial chunks; at least 3 are needed")]
    TooFewChunks {
        /// The volume id.
        volume_id: u16,
        /// Timestamped radial chunks.
        chunks: usize,
    },
    /// The fitted rotation scale is outside 0.8 to 1.25.
    #[error("volume {volume_id} fits rotation scale {rotation_scale:.3}, outside 0.8..=1.25")]
    ImplausibleFit {
        /// The volume id.
        volume_id: u16,
        /// The fitted rotation scale.
        rotation_scale: f64,
    },
}

/// Parses one S3 `ListObjectsV2` response from the chunk bucket into chunk
/// objects. Keys that are not chunk keys are skipped. The XML types are the
/// ones the live client uses.
pub fn parse_chunk_listing(xml: &str) -> crate::Result<Vec<RealtimeChunkObject>> {
    let listing: crate::S3ListingXml = quick_xml::de::from_str(xml)?;
    Ok(listing
        .contents
        .into_iter()
        .map(crate::S3Object::from)
        .filter_map(crate::parse_realtime_chunk_object)
        .collect())
}

/// One cut of a volume, meaning one Level II elevation number in execution
/// order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanCut {
    /// Elevation angle in degrees as reported. Message 5 codes angles below
    /// the horizon as values just under 360.
    pub elevation_deg: f32,
    /// Commanded azimuth rate in degrees per second.
    pub azimuth_rate_deg_per_second: f32,
    /// Radials in one rotation: 720 at 0.5 deg azimuth spacing, 360 at 1 deg.
    pub radials: u16,
}

impl ScanCut {
    /// A cut at `elevation_deg`, rotating at `azimuth_rate_deg_per_second`, with `radials` radials.
    pub const fn new(elevation_deg: f32, azimuth_rate_deg_per_second: f32, radials: u16) -> Self {
        Self {
            elevation_deg,
            azimuth_rate_deg_per_second,
            radials,
        }
    }

    /// Radials per rotation for a cut. `half_degree_azimuth` is bit 0 of the
    /// Message 5 super-resolution control field.
    pub const fn radials_for_azimuth_spacing(half_degree_azimuth: bool) -> u16 {
        if half_degree_azimuth { 720 } else { 360 }
    }

    /// Elevation in (-180, 180] degrees.
    pub fn signed_elevation_deg(&self) -> f32 {
        if self.elevation_deg > 180.0 {
            self.elevation_deg - 360.0
        } else {
            self.elevation_deg
        }
    }

    /// Commanded rotation time, `360 / azimuth rate`, in seconds.
    pub fn commanded_rotation_seconds(&self) -> Option<f64> {
        let rate = f64::from(self.azimuth_rate_deg_per_second);
        (rate.is_finite() && rate > 0.0).then(|| 360.0 / rate)
    }

    /// Chunks this cut fills.
    pub const fn chunk_count(&self) -> u16 {
        self.radials.div_ceil(RADIALS_PER_CHUNK)
    }
}

/// Where a radial chunk sits in a plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkPosition {
    /// Index into [`ScanPlan::cuts`]. Level II elevation number is this + 1.
    pub cut_index: usize,
    /// 0-based chunk number within the cut.
    pub part: u16,
    /// Chunks the cut fills.
    pub parts: u16,
}

impl ChunkPosition {
    /// True for the chunk holding the cut's last radials.
    pub const fn closes_cut(&self) -> bool {
        self.part + 1 == self.parts
    }
}

/// The cut sequence of one volume.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanPlan {
    /// The VCP number, when known.
    pub vcp: Option<u16>,
    /// The cuts, in execution order.
    pub cuts: Vec<ScanCut>,
}

impl ScanPlan {
    /// A plan from its cuts.
    pub fn new(vcp: Option<u16>, cuts: Vec<ScanCut>) -> Self {
        Self { vcp, cuts }
    }

    /// Approximate plan from a Build 24 Appendix C definition, with one cut
    /// per physical row.
    ///
    /// Differences from the radar's own Message 5:
    /// - SZCD rows use the default-PRF azimuth rate. The RPG rescales it for
    ///   the PRF selected on site (Appendix C notes), so live rates in the
    ///   captures ranged from 14.46 to 20.38 deg/s.
    /// - VCP 12's 1.3 deg CD/W row carries the ICD's 25.994 deg/s. Its period
    ///   (14.40 s) and live Message 5 (KRGX) both give 24.994.
    /// - SAILS, MESO-SAILS, MRLE and base-tilt cuts are not inserted.
    /// - AVSET may end the volume early.
    ///
    /// Radial counts follow [`Self::build24_radials`].
    pub fn from_build24(definition: &VcpDefinition) -> Self {
        Self {
            vcp: Some(definition.vcp.number()),
            cuts: definition
                .rows
                .iter()
                .map(|row| {
                    ScanCut::new(
                        row.elevation_deg,
                        row.azimuth_rate_deg_per_second,
                        Self::build24_radials(row.waveform),
                    )
                })
                .collect(),
        }
    }

    /// Radials per rotation for an Appendix C waveform. Split-cut rotations
    /// (CS, CD/W, SZCS, SZCD) are 0.5 deg super resolution (720). Batch and
    /// CD/WO rotations are 1 deg (360). This matches the Message 5
    /// super-resolution flag and the decoded radial counts in every captured
    /// VCP 12, 34, 35, 212 and 215 volume. No VCP 112 volume was on air at
    /// capture time, so its MPDA rows follow the same rule unverified.
    pub const fn build24_radials(waveform: Waveform) -> u16 {
        match waveform {
            Waveform::ContiguousSurveillance
            | Waveform::ContiguousDopplerWithRangeAmbiguity
            | Waveform::Sz2ContiguousSurveillance
            | Waveform::Sz2ContiguousDoppler => 720,
            Waveform::Batch | Waveform::ContiguousDopplerWithoutRangeAmbiguity => 360,
        }
    }

    /// Plan chunk number of the End chunk if every cut is collected.
    pub fn full_end_chunk_id(&self) -> u32 {
        u32::from(START_CHUNK_ID)
            + self
                .cuts
                .iter()
                .map(|cut| u32::from(cut.chunk_count()))
                .sum::<u32>()
    }

    /// Cut and part of the radial chunk with plan chunk number `chunk_id`
    /// (see the module documentation). `None` for the Start chunk and for
    /// numbers past the plan.
    pub fn chunk_position(&self, chunk_id: u16) -> Option<ChunkPosition> {
        let mut first = START_CHUNK_ID.checked_add(1)?;
        for (cut_index, cut) in self.cuts.iter().enumerate() {
            let parts = cut.chunk_count();
            if chunk_id >= first && chunk_id - first < parts {
                return Some(ChunkPosition {
                    cut_index,
                    part: chunk_id - first,
                    parts,
                });
            }
            first = first.checked_add(parts)?;
        }
        None
    }

    /// Plan chunk numbers a cut fills.
    pub fn cut_chunk_ids(&self, cut_index: usize) -> Option<RangeInclusive<u16>> {
        let mut first = START_CHUNK_ID.checked_add(1)?;
        for (index, cut) in self.cuts.iter().enumerate() {
            let parts = cut.chunk_count();
            if parts == 0 {
                return None;
            }
            let last = first.checked_add(parts - 1)?;
            if index == cut_index {
                return Some(first..=last);
            }
            first = last.checked_add(1)?;
        }
        None
    }

    /// Index of the last cut an AVSET-truncated volume collects: trailing
    /// cuts above `top_elevation_deg` are dropped.
    pub fn last_cut_at_or_below(&self, top_elevation_deg: f32) -> Option<usize> {
        self.cuts.iter().rposition(|cut| {
            cut.signed_elevation_deg() <= top_elevation_deg + ELEVATION_TOLERANCE_DEG
        })
    }
}

/// The parameters of the timing equations in the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingParameters {
    /// Actual rotation time divided by the commanded `360 / azimuth rate`.
    pub rotation_scale: f64,
    /// Seconds from the end of one cut's rotation to the start of the next.
    pub inter_cut_gap_seconds: f64,
    /// A radial chunk's `LastModified` minus (volume key time + modeled time
    /// of its last radial). Covers the sub-second part of the volume start
    /// plus the publishing delay.
    pub publish_offset_seconds: f64,
    /// Start chunk `LastModified` minus the volume key time.
    pub start_chunk_offset_seconds: f64,
    /// Next volume key time minus the modeled end of this volume's last cut
    /// (antenna retrace and volume setup).
    pub inter_volume_gap_seconds: f64,
}

impl TimingParameters {
    /// WSR-88D defaults, measured on the TD.1 live capture of KIWA volume
    /// 307 (2026-09-17 00:36Z, VCP 215, 15 cuts) and its successor 308:
    /// - Rotation scale: 0.978, total decoded rotation over total commanded
    ///   rotation.
    /// - Inter-cut gap: 1.1 s, median decoded gap between cuts.
    /// - Publish offset: 2.9 s, median `LastModified` residual.
    /// - Start chunk offset: 2 s.
    /// - Inter-volume gap: 9.0 s, from 307 to 308.
    ///
    /// Other sites differ. [`TimingStatistics`] learns per-site values.
    pub const WSR88D: Self = Self {
        rotation_scale: 0.978,
        inter_cut_gap_seconds: 1.1,
        publish_offset_seconds: 2.9,
        start_chunk_offset_seconds: 2.0,
        inter_volume_gap_seconds: 9.0,
    };
}

impl Default for TimingParameters {
    fn default() -> Self {
        Self::WSR88D
    }
}

/// Modeled timing of one cut, in seconds after the volume key time. Chunk
/// numbers are plan chunk numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutTiming {
    /// Index into [`ScanPlan::cuts`].
    pub cut_index: usize,
    /// Elevation angle, degrees.
    pub elevation_deg: f32,
    /// Plan chunk number of the cut's first chunk.
    pub first_chunk_id: u16,
    /// Plan chunk number of the cut's last chunk.
    pub last_chunk_id: u16,
    /// When the cut's rotation starts.
    pub start_seconds: f64,
    /// When the cut's rotation ends.
    pub end_seconds: f64,
    /// `rotation_scale * 360 / azimuth rate`.
    pub sweep_seconds: f64,
    /// Expected `LastModified` of the cut's last chunk.
    pub complete_last_modified_seconds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CutSpan {
    first_chunk_id: u16,
    chunk_count: u16,
    radials: u16,
    commanded_rotation_seconds: f64,
    commanded_seconds_before: f64,
}

/// A [`ScanPlan`] with [`TimingParameters`], ready to answer timing questions.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanTimingModel {
    plan: ScanPlan,
    parameters: TimingParameters,
    spans: Vec<CutSpan>,
}

impl ScanTimingModel {
    /// A model of `plan` with `parameters`; fails on a plan with no cuts, a cut without radials or a non-positive azimuth rate, or more chunks than chunk ids allow.
    pub fn new(plan: ScanPlan, parameters: TimingParameters) -> Result<Self, TimingError> {
        if plan.cuts.is_empty() {
            return Err(TimingError::EmptyPlan);
        }
        let total_chunks = plan.full_end_chunk_id();
        if total_chunks > u32::from(MAX_CHUNK_ID) {
            return Err(TimingError::TooManyChunks {
                chunks: total_chunks,
            });
        }
        let mut spans = Vec::with_capacity(plan.cuts.len());
        let mut first_chunk_id = START_CHUNK_ID + 1;
        let mut commanded_seconds_before = 0.0;
        for (cut_index, cut) in plan.cuts.iter().enumerate() {
            if cut.radials == 0 {
                return Err(TimingError::NoRadials { cut_index });
            }
            let rotation =
                cut.commanded_rotation_seconds()
                    .ok_or(TimingError::InvalidAzimuthRate {
                        cut_index,
                        rate: cut.azimuth_rate_deg_per_second,
                    })?;
            spans.push(CutSpan {
                first_chunk_id,
                chunk_count: cut.chunk_count(),
                radials: cut.radials,
                commanded_rotation_seconds: rotation,
                commanded_seconds_before,
            });
            // Bounded by the TooManyChunks check above.
            first_chunk_id += cut.chunk_count();
            commanded_seconds_before += rotation;
        }
        Ok(Self {
            plan,
            parameters,
            spans,
        })
    }

    /// The scan plan.
    pub fn plan(&self) -> &ScanPlan {
        &self.plan
    }

    /// The timing parameters.
    pub fn parameters(&self) -> TimingParameters {
        self.parameters
    }

    /// The same plan under other parameters.
    pub fn with_parameters(&self, parameters: TimingParameters) -> Self {
        Self {
            parameters,
            ..self.clone()
        }
    }

    /// Plan chunk number of the End chunk if every cut is collected.
    pub fn full_end_chunk_id(&self) -> u16 {
        self.spans.last().map_or(START_CHUNK_ID, |span| {
            span.first_chunk_id + span.chunk_count - 1
        })
    }

    /// Cut and part of the radial chunk with plan chunk number `chunk_id`.
    pub fn chunk_position(&self, chunk_id: u16) -> Option<ChunkPosition> {
        self.plan.chunk_position(chunk_id)
    }

    /// Commanded seconds from the volume start to the chunk's last radial,
    /// and the chunk's cut index.
    fn commanded_terms(&self, chunk_id: u16) -> Option<(f64, usize)> {
        let position = self.plan.chunk_position(chunk_id)?;
        let span = self.spans.get(position.cut_index)?;
        let radials_through = (u32::from(position.part) + 1) * u32::from(RADIALS_PER_CHUNK);
        let fraction =
            f64::from(radials_through.min(u32::from(span.radials))) / f64::from(span.radials);
        Some((
            span.commanded_seconds_before + span.commanded_rotation_seconds * fraction,
            position.cut_index,
        ))
    }

    /// Modeled seconds from the volume key time to the last radial of the
    /// radial chunk with plan chunk number `chunk_id`.
    pub fn chunk_radials_end_seconds(&self, chunk_id: u16) -> Option<f64> {
        let (commanded, cut_index) = self.commanded_terms(chunk_id)?;
        Some(self.elapsed(commanded, cut_index))
    }

    /// Expected `LastModified` of the chunk with plan chunk number
    /// `chunk_id`, in seconds after the volume key time.
    pub fn chunk_last_modified_seconds(&self, chunk_id: u16) -> Option<f64> {
        if chunk_id == START_CHUNK_ID {
            return Some(self.parameters.start_chunk_offset_seconds);
        }
        self.chunk_radials_end_seconds(chunk_id)
            .map(|seconds| seconds + self.parameters.publish_offset_seconds)
    }

    /// Modeled timing of one cut.
    pub fn cut_timing(&self, cut_index: usize) -> Option<CutTiming> {
        let span = self.spans.get(cut_index)?;
        let cut = self.plan.cuts.get(cut_index)?;
        let start_seconds = self.elapsed(span.commanded_seconds_before, cut_index);
        let sweep_seconds = self.parameters.rotation_scale * span.commanded_rotation_seconds;
        let last_chunk_id = span.first_chunk_id + span.chunk_count - 1;
        Some(CutTiming {
            cut_index,
            elevation_deg: cut.elevation_deg,
            first_chunk_id: span.first_chunk_id,
            last_chunk_id,
            start_seconds,
            end_seconds: start_seconds + sweep_seconds,
            sweep_seconds,
            complete_last_modified_seconds: start_seconds
                + sweep_seconds
                + self.parameters.publish_offset_seconds,
        })
    }

    /// Modeled timing of every cut in plan order.
    pub fn cut_timings(&self) -> Vec<CutTiming> {
        (0..self.spans.len())
            .filter_map(|cut_index| self.cut_timing(cut_index))
            .collect()
    }

    /// Modeled seconds from this volume's key time to the next volume's key
    /// time, if the volume ends with plan chunk number `end_chunk_id`.
    pub fn next_volume_seconds(&self, end_chunk_id: u16) -> Option<f64> {
        let position = self.plan.chunk_position(end_chunk_id)?;
        let span = self.spans.get(position.cut_index)?;
        Some(
            self.elapsed(
                span.commanded_seconds_before + span.commanded_rotation_seconds,
                position.cut_index,
            ) + self.parameters.inter_volume_gap_seconds,
        )
    }

    fn elapsed(&self, commanded_seconds: f64, cut_index: usize) -> f64 {
        self.parameters.rotation_scale * commanded_seconds
            + self.parameters.inter_cut_gap_seconds * cut_index as f64
    }

    /// Projects the rest of `volume` from the chunks observed so far.
    ///
    /// Choosing the End chunk (as a plan chunk number):
    /// - an observed End chunk wins;
    /// - otherwise `expected_end_chunk_id` (a plan chunk number, for example
    ///   [`TimingStatistics::expected_end_chunk_id`]), if it closes a cut;
    /// - otherwise the plan's last chunk.
    ///
    /// It is extended if chunks past it were already observed.
    ///
    /// Model times are shifted by the lower quartile of the residuals of the
    /// last 9 observed radial chunks. Late publishing only adds delay, so the
    /// lower quartile tracks the timing and ignores backlogs. An unobserved
    /// chunk is never expected before the newest observed `LastModified`.
    ///
    /// Observed chunks keep their ids. An unobserved chunk's id is its plan
    /// number plus the status-only chunks observed so far: status-only chunks
    /// still to come cannot be foreseen.
    pub fn project(
        &self,
        volume: &VolumeObservation,
        expected_end_chunk_id: Option<u16>,
    ) -> ScanTimingProjection {
        let full_end = self.full_end_chunk_id();
        let status_only_chunks = volume.status_only_chunks();
        let status_offset = u16::try_from(status_only_chunks).unwrap_or(u16::MAX);
        let plan_chunks = volume.plan_chunks();
        let observed_end = volume
            .end_chunk_id()
            .and_then(|id| volume.closing_plan_chunk_number(id))
            .filter(|number| self.closing_chunk(*number).is_some());
        let latest_observed_number = plan_chunks.iter().map(|(number, _)| *number).max();
        let mut end = observed_end
            .or_else(|| expected_end_chunk_id.filter(|id| self.closing_chunk(*id).is_some()))
            .unwrap_or(full_end);
        if observed_end.is_none()
            && let Some(latest) = latest_observed_number
            && latest > end
        {
            end = self.closing_chunk_at_or_after(latest).unwrap_or(full_end);
        }

        let residuals: Vec<f64> = plan_chunks
            .iter()
            .filter(|(number, _)| *number > START_CHUNK_ID)
            .filter_map(|(number, chunk)| {
                let modeled = self.chunk_last_modified_seconds(*number)?;
                Some(seconds_between(chunk.last_modified, volume.volume_time) - modeled)
            })
            .collect();
        let anchor = &residuals[residuals.len().saturating_sub(ANCHOR_RESIDUALS)..];
        let anchor_correction_seconds = lower_quartile(anchor).unwrap_or(0.0);
        let latest_observed = volume.chunks.iter().map(|chunk| chunk.last_modified).max();
        let observed_by_number = |number: u16| {
            plan_chunks
                .iter()
                .find(|(listed, _)| *listed == number)
                .map(|(_, chunk)| *chunk)
        };
        let actual_id = |number: u16| {
            observed_by_number(number)
                .map_or(number.saturating_add(status_offset), |chunk| chunk.chunk_id)
        };

        let mut chunks: Vec<ProjectedChunk> = (START_CHUNK_ID..=end)
            .filter_map(|number| {
                let modeled = self.chunk_last_modified_seconds(number)?;
                let correction = if number == START_CHUNK_ID {
                    0.0
                } else {
                    anchor_correction_seconds
                };
                let mut expected = offset_time(volume.volume_time, modeled + correction);
                let observed = observed_by_number(number).map(|chunk| chunk.last_modified);
                if observed.is_none()
                    && let Some(latest) = latest_observed
                {
                    expected = expected.max(latest);
                }
                Some(ProjectedChunk {
                    chunk_id: actual_id(number),
                    plan_chunk_number: Some(number),
                    cut_index: self.plan.chunk_position(number).map(|p| p.cut_index),
                    expected_last_modified: expected,
                    observed_last_modified: observed,
                })
            })
            .collect();
        for chunk in volume.chunks.iter().filter(|chunk| chunk.is_status_only()) {
            if chunks
                .iter()
                .all(|listed| listed.chunk_id != chunk.chunk_id)
            {
                chunks.push(ProjectedChunk {
                    chunk_id: chunk.chunk_id,
                    plan_chunk_number: None,
                    cut_index: None,
                    expected_last_modified: chunk.last_modified,
                    observed_last_modified: Some(chunk.last_modified),
                });
            }
        }
        chunks.sort_by_key(|chunk| chunk.chunk_id);
        let expected_end_id = match (observed_end, volume.end_chunk_id()) {
            (Some(_), Some(id)) => id,
            _ => actual_id(end),
        };
        chunks.retain(|chunk| chunk.chunk_id <= expected_end_id);

        let last_cut = self
            .plan
            .chunk_position(end)
            .map_or(self.spans.len().saturating_sub(1), |p| p.cut_index);
        let cuts = (0..=last_cut)
            .filter_map(|cut_index| {
                let timing = self.cut_timing(cut_index)?;
                let closing = chunks
                    .iter()
                    .find(|chunk| chunk.plan_chunk_number == Some(timing.last_chunk_id))?;
                Some(ProjectedCut {
                    cut_index,
                    elevation_deg: timing.elevation_deg,
                    first_chunk_id: actual_id(timing.first_chunk_id),
                    last_chunk_id: closing.chunk_id,
                    sweep_seconds: timing.sweep_seconds,
                    expected_complete: closing.expected_last_modified,
                    observed_complete: closing.observed_last_modified,
                })
            })
            .collect();

        let expected_volume_end = chunks
            .last()
            .map_or(volume.volume_time, |chunk| chunk.expected_last_modified);
        let next_volume_seconds = self
            .next_volume_seconds(end)
            .map_or(0.0, |seconds| seconds + anchor_correction_seconds);
        let expected_next_volume_time = offset_time(volume.volume_time, next_volume_seconds);
        ScanTimingProjection {
            site: volume.site.clone(),
            volume_id: volume.volume_id,
            volume_time: volume.volume_time,
            vcp: self.plan.vcp,
            parameters: self.parameters,
            anchor_correction_seconds,
            expected_end_chunk_id: expected_end_id,
            expected_end_plan_chunk_number: end,
            status_only_chunks,
            complete: observed_end.is_some() && volume.is_complete(),
            chunks,
            cuts,
            expected_volume_end,
            expected_next_volume_time,
            expected_next_start_chunk: offset_time(
                expected_next_volume_time,
                self.parameters.start_chunk_offset_seconds,
            ),
        }
    }

    fn closing_chunk(&self, chunk_id: u16) -> Option<ChunkPosition> {
        self.plan
            .chunk_position(chunk_id)
            .filter(ChunkPosition::closes_cut)
    }

    fn closing_chunk_at_or_after(&self, chunk_id: u16) -> Option<u16> {
        let position = self.plan.chunk_position(chunk_id)?;
        self.spans
            .get(position.cut_index)
            .map(|span| span.first_chunk_id + span.chunk_count - 1)
    }
}

/// One listed chunk: id, type, `LastModified` and size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkTimestamp {
    /// Chunk number within the volume.
    pub chunk_id: u16,
    /// Chunk type.
    pub chunk_type: RealtimeChunkType,
    /// When the chunk was published (`LastModified`).
    pub last_modified: DateTime<Utc>,
    /// Object size in bytes, from the listing.
    pub size: u64,
}

impl ChunkTimestamp {
    /// An Intermediate or End chunk smaller than [`MIN_RADIAL_CHUNK_BYTES`]:
    /// a status-only chunk that holds no radials.
    pub fn is_status_only(&self) -> bool {
        self.chunk_type != RealtimeChunkType::Start && self.size < MIN_RADIAL_CHUNK_BYTES
    }
}

/// The timestamped chunks of one volume, as a listing showed them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeObservation {
    /// The site.
    pub site: String,
    /// The volume id.
    pub volume_id: u16,
    /// Volume start from the key (whole seconds).
    pub volume_time: DateTime<Utc>,
    /// Sorted by chunk id, one entry per id.
    pub chunks: Vec<ChunkTimestamp>,
}

impl VolumeObservation {
    /// Groups chunk objects into volumes, ordered by volume time. Chunks
    /// without `LastModified` are skipped. If a chunk id repeats, the first
    /// copy is kept.
    pub fn from_chunks(chunks: impl IntoIterator<Item = RealtimeChunkObject>) -> Vec<Self> {
        let mut volumes: Vec<Self> = Vec::new();
        for chunk in chunks {
            let Some(last_modified) = chunk.object.last_modified else {
                continue;
            };
            let timestamp = ChunkTimestamp {
                chunk_id: chunk.chunk_id,
                chunk_type: chunk.chunk_type,
                last_modified,
                size: chunk.object.size,
            };
            match volumes.iter_mut().find(|volume| {
                volume.site == chunk.site
                    && volume.volume_id == chunk.volume_id
                    && volume.volume_time == chunk.volume_time
            }) {
                Some(volume) => volume.chunks.push(timestamp),
                None => volumes.push(Self {
                    site: chunk.site,
                    volume_id: chunk.volume_id,
                    volume_time: chunk.volume_time,
                    chunks: vec![timestamp],
                }),
            }
        }
        for volume in &mut volumes {
            volume.chunks.sort_by_key(|chunk| chunk.chunk_id);
            volume.chunks.dedup_by_key(|chunk| chunk.chunk_id);
        }
        volumes.sort_by(|left, right| {
            left.volume_time
                .cmp(&right.volume_time)
                .then_with(|| left.site.cmp(&right.site))
        });
        volumes
    }

    /// The chunk with id `chunk_id`, if listed.
    pub fn chunk(&self, chunk_id: u16) -> Option<&ChunkTimestamp> {
        self.chunks
            .binary_search_by_key(&chunk_id, |chunk| chunk.chunk_id)
            .ok()
            .and_then(|index| self.chunks.get(index))
    }

    /// Status-only chunks listed (see [`ChunkTimestamp::is_status_only`]).
    pub fn status_only_chunks(&self) -> usize {
        self.chunks
            .iter()
            .filter(|chunk| chunk.is_status_only())
            .count()
    }

    /// Plan chunk number of listed chunk `chunk_id`: the id minus the
    /// status-only chunks listed before it. `None` for a status-only chunk
    /// or an id that is not listed.
    pub fn plan_chunk_number(&self, chunk_id: u16) -> Option<u16> {
        let chunk = self.chunk(chunk_id)?;
        if chunk.is_status_only() {
            return None;
        }
        let before = self
            .chunks
            .iter()
            .take_while(|listed| listed.chunk_id < chunk_id)
            .filter(|listed| listed.is_status_only())
            .count();
        chunk_id.checked_sub(u16::try_from(before).ok()?)
    }

    /// The listed chunk with plan chunk number `number`.
    pub fn chunk_by_plan_number(&self, number: u16) -> Option<&ChunkTimestamp> {
        self.plan_chunks()
            .into_iter()
            .find(|(listed, _)| *listed == number)
            .map(|(_, chunk)| chunk)
    }

    /// Every listed chunk that is not status-only, with its plan chunk
    /// number, in id order.
    fn plan_chunks(&self) -> Vec<(u16, &ChunkTimestamp)> {
        let mut status_only = 0u16;
        let mut numbered = Vec::with_capacity(self.chunks.len());
        for chunk in &self.chunks {
            if chunk.is_status_only() {
                status_only = status_only.saturating_add(1);
            } else if let Some(number) = chunk.chunk_id.checked_sub(status_only) {
                numbered.push((number, chunk));
            }
        }
        numbered
    }

    /// Plan chunk number of the last radial chunk listed at or before
    /// `chunk_id`: the chunk that closes the volume's last cut when
    /// `chunk_id` is the End chunk.
    pub fn closing_plan_chunk_number(&self, chunk_id: u16) -> Option<u16> {
        self.plan_chunks()
            .into_iter()
            .filter(|(number, chunk)| *number > START_CHUNK_ID && chunk.chunk_id <= chunk_id)
            .map(|(number, _)| number)
            .max()
    }

    /// Id of the End chunk, if listed.
    pub fn end_chunk_id(&self) -> Option<u16> {
        self.chunks
            .iter()
            .find(|chunk| chunk.chunk_type == RealtimeChunkType::End)
            .map(|chunk| chunk.chunk_id)
    }

    /// Start, End, and every chunk id between them are present.
    pub fn is_complete(&self) -> bool {
        let Some(end) = self.end_chunk_id() else {
            return false;
        };
        self.chunks.len() == usize::from(end)
            && self
                .chunks
                .iter()
                .zip(START_CHUNK_ID..)
                .all(|(chunk, id)| chunk.chunk_id == id)
            && self
                .chunks
                .first()
                .is_some_and(|chunk| chunk.chunk_type == RealtimeChunkType::Start)
    }

    /// The chunks a listing taken at `at` would have shown, meaning those
    /// with `LastModified <= at`.
    pub fn observed_by(&self, at: DateTime<Utc>) -> Self {
        Self {
            chunks: self
                .chunks
                .iter()
                .copied()
                .filter(|chunk| chunk.last_modified <= at)
                .collect(),
            ..self.clone()
        }
    }
}

/// One chunk in a [`ScanTimingProjection`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedChunk {
    /// Key chunk id: observed, or expected (plan number plus the status-only
    /// chunks observed so far).
    pub chunk_id: u16,
    /// `None` for an observed status-only chunk.
    pub plan_chunk_number: Option<u16>,
    /// `None` for the Start chunk and status-only chunks.
    pub cut_index: Option<usize>,
    /// Expected publication time.
    pub expected_last_modified: DateTime<Utc>,
    /// Observed publication time, once listed.
    pub observed_last_modified: Option<DateTime<Utc>>,
}

/// One cut in a [`ScanTimingProjection`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedCut {
    /// Index into the plan's cuts.
    pub cut_index: usize,
    /// Elevation angle, degrees.
    pub elevation_deg: f32,
    /// Key chunk ids, as in [`ProjectedChunk::chunk_id`].
    pub first_chunk_id: u16,
    /// Key id of the cut's last chunk.
    pub last_chunk_id: u16,
    /// Modeled rotation time of the cut, seconds.
    pub sweep_seconds: f64,
    /// Expected `LastModified` of the cut's last chunk.
    pub expected_complete: DateTime<Utc>,
    /// Observed publication of the cut's last chunk, once listed.
    pub observed_complete: Option<DateTime<Utc>>,
}

/// The rest of a volume projected from what has been observed.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanTimingProjection {
    /// The site.
    pub site: String,
    /// The volume id.
    pub volume_id: u16,
    /// Volume start time from the key.
    pub volume_time: DateTime<Utc>,
    /// The VCP number, when known.
    pub vcp: Option<u16>,
    /// The timing parameters the projection used.
    pub parameters: TimingParameters,
    /// Seconds added to the model from the latest observations.
    pub anchor_correction_seconds: f64,
    /// Key id of the observed End chunk, else of the expected one.
    pub expected_end_chunk_id: u16,
    /// Plan chunk number of the chunk that closes the last cut.
    pub expected_end_plan_chunk_number: u16,
    /// Status-only chunks observed so far.
    pub status_only_chunks: usize,
    /// Every chunk through the End chunk has been observed.
    pub complete: bool,
    /// Chunks `1..=expected_end_chunk_id`, status-only ones included.
    pub chunks: Vec<ProjectedChunk>,
    /// Cuts through the one holding the End chunk.
    pub cuts: Vec<ProjectedCut>,
    /// Expected `LastModified` of the End chunk.
    pub expected_volume_end: DateTime<Utc>,
    /// Expected key time of the next volume.
    pub expected_next_volume_time: DateTime<Utc>,
    /// Expected `LastModified` of the next volume's Start chunk.
    pub expected_next_start_chunk: DateTime<Utc>,
}

impl ScanTimingProjection {
    /// The projected chunk with key id `chunk_id`.
    pub fn chunk(&self, chunk_id: u16) -> Option<&ProjectedChunk> {
        self.chunks.iter().find(|chunk| chunk.chunk_id == chunk_id)
    }

    /// The lowest-numbered chunk not yet observed.
    pub fn next_chunk(&self) -> Option<&ProjectedChunk> {
        self.chunks
            .iter()
            .find(|chunk| chunk.observed_last_modified.is_none())
    }

    /// The cut the next chunk belongs to (the one being collected or
    /// published).
    pub fn current_cut(&self) -> Option<&ProjectedCut> {
        let cut_index = self.next_chunk()?.cut_index?;
        self.cuts.iter().find(|cut| cut.cut_index == cut_index)
    }
}

/// What [`TimingStatistics::observe_volume`] measured on one volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeFit {
    /// The volume id.
    pub volume_id: u16,
    /// Measured rotation scale (actual over commanded rotation time).
    pub rotation_scale: f64,
    /// Measured publish offset, seconds.
    pub publish_offset_seconds: f64,
    /// Measured Start chunk offset, seconds, when the Start chunk was listed.
    pub start_chunk_offset_seconds: Option<f64>,
    /// Present when the previous observed volume at the site came right
    /// before this one (the next volume id, with 999 followed by 1).
    pub inter_volume_gap_seconds: Option<f64>,
    /// Key id of the End chunk.
    pub end_chunk_id: u16,
    /// Plan chunk number of the chunk that closes the last cut.
    pub end_plan_chunk_number: u16,
    /// Status-only chunks in the volume.
    pub status_only_chunks: usize,
    /// Highest elevation collected (the AVSET cutoff when the volume ends early).
    pub top_elevation_deg: f32,
    /// Radial chunks used in the fit.
    pub radial_chunks: usize,
    /// Median absolute residual of the fit, seconds.
    pub median_abs_residual_seconds: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct PreviousVolume {
    site: String,
    volume_id: u16,
    volume_time: DateTime<Utc>,
    radials_end_seconds: f64,
}

/// Timing statistics learned from completed volumes at one site.
///
/// Each parameter is the median of the last `window` volumes that measured
/// it. Until then, [`TimingParameters`] defaults apply. The inter-cut gap is
/// never learned: `LastModified` cannot separate it from the rotation scale,
/// which absorbs it.
#[derive(Clone, Debug, PartialEq)]
pub struct TimingStatistics {
    defaults: TimingParameters,
    window: usize,
    rotation_scale: VecDeque<f64>,
    publish_offset: VecDeque<f64>,
    start_chunk_offset: VecDeque<f64>,
    inter_volume_gap: VecDeque<f64>,
    /// VCP number and highest elevation of the most recent observed volume.
    top_elevation: Option<(Option<u16>, f32)>,
    previous: Option<PreviousVolume>,
    volumes_observed: usize,
}

impl Default for TimingStatistics {
    fn default() -> Self {
        Self::new(TimingParameters::WSR88D)
    }
}

impl TimingStatistics {
    /// Volumes each learned parameter is the median of, by default.
    pub const DEFAULT_WINDOW: usize = 5;

    /// Statistics that start from `defaults`, over [`TimingStatistics::DEFAULT_WINDOW`] volumes.
    pub fn new(defaults: TimingParameters) -> Self {
        Self::with_window(defaults, Self::DEFAULT_WINDOW)
    }

    /// `window` is clamped to at least 1.
    pub fn with_window(defaults: TimingParameters, window: usize) -> Self {
        Self {
            defaults,
            window: window.max(1),
            rotation_scale: VecDeque::new(),
            publish_offset: VecDeque::new(),
            start_chunk_offset: VecDeque::new(),
            inter_volume_gap: VecDeque::new(),
            top_elevation: None,
            previous: None,
            volumes_observed: 0,
        }
    }

    /// Volumes that contributed a fit.
    pub fn volumes_observed(&self) -> usize {
        self.volumes_observed
    }

    /// Highest elevation collected in the most recent observed volume.
    pub fn top_elevation_deg(&self) -> Option<f32> {
        self.top_elevation.map(|(_, top)| top)
    }

    /// Learned parameters, with defaults where nothing was measured yet.
    pub fn parameters(&self) -> TimingParameters {
        TimingParameters {
            rotation_scale: median(&self.rotation_scale).unwrap_or(self.defaults.rotation_scale),
            inter_cut_gap_seconds: self.defaults.inter_cut_gap_seconds,
            publish_offset_seconds: median(&self.publish_offset)
                .unwrap_or(self.defaults.publish_offset_seconds),
            start_chunk_offset_seconds: median(&self.start_chunk_offset)
                .unwrap_or(self.defaults.start_chunk_offset_seconds),
            inter_volume_gap_seconds: median(&self.inter_volume_gap)
                .unwrap_or(self.defaults.inter_volume_gap_seconds),
        }
    }

    /// Expected End chunk, as a plan chunk number, for a volume running
    /// `plan`: the last chunk of the last cut at or below the top elevation
    /// learned from the most recent volume. If nothing was learned, or that
    /// volume ran a different VCP, it is the plan's last chunk.
    pub fn expected_end_chunk_id(&self, plan: &ScanPlan) -> Option<u16> {
        let last_cut = match self.top_elevation {
            Some((vcp, top)) if vcp == plan.vcp => plan.last_cut_at_or_below(top)?,
            _ => plan.cuts.len().checked_sub(1)?,
        };
        plan.cut_chunk_ids(last_cut).map(|ids| *ids.end())
    }

    /// A model for `plan` with the learned parameters.
    pub fn model(&self, plan: ScanPlan) -> Result<ScanTimingModel, TimingError> {
        ScanTimingModel::new(plan, self.parameters())
    }

    /// Projects an in-progress volume with the learned parameters and End
    /// chunk.
    pub fn project(
        &self,
        plan: ScanPlan,
        volume: &VolumeObservation,
    ) -> Result<ScanTimingProjection, TimingError> {
        let expected_end = self.expected_end_chunk_id(&plan);
        Ok(self.model(plan)?.project(volume, expected_end))
    }

    /// Learns from one completed volume and the plan it ran.
    ///
    /// With `g` the default inter-cut gap, `rotation_scale` is the Theil-Sen
    /// slope of `LastModified - V - g * cut_index` against commanded elapsed
    /// rotation time, over the radial chunks (status-only chunks are left
    /// out). Pairs less than 30 commanded seconds apart are left out.
    /// `publish_offset` is the median residual. Volumes whose last radial
    /// chunk does not close a plan cut, or whose fit is implausible, are
    /// rejected without changing the statistics.
    pub fn observe_volume(
        &mut self,
        plan: &ScanPlan,
        volume: &VolumeObservation,
    ) -> Result<VolumeFit, TimingError> {
        let volume_id = volume.volume_id;
        let end_chunk_id = volume
            .end_chunk_id()
            .ok_or(TimingError::IncompleteVolume { volume_id })?;
        let model = ScanTimingModel::new(plan.clone(), self.defaults)?;
        let mismatch = TimingError::PlanMismatch {
            volume_id,
            end_chunk_id,
        };
        let end_plan_chunk_number = volume
            .closing_plan_chunk_number(end_chunk_id)
            .ok_or(mismatch.clone())?;
        let end_position = model.closing_chunk(end_plan_chunk_number).ok_or(mismatch)?;

        let gap = self.defaults.inter_cut_gap_seconds;
        let points: Vec<(f64, f64)> = volume
            .plan_chunks()
            .into_iter()
            .filter(|(number, chunk)| {
                *number > START_CHUNK_ID
                    && *number <= end_plan_chunk_number
                    && chunk.chunk_id <= end_chunk_id
            })
            .filter_map(|(number, chunk)| {
                let (commanded, cut_index) = model.commanded_terms(number)?;
                let observed = seconds_between(chunk.last_modified, volume.volume_time);
                Some((commanded, observed - gap * cut_index as f64))
            })
            .collect();
        if points.len() < 3 {
            return Err(TimingError::TooFewChunks {
                volume_id,
                chunks: points.len(),
            });
        }
        let rotation_scale = theil_sen_slope(&points).ok_or(TimingError::TooFewChunks {
            volume_id,
            chunks: points.len(),
        })?;
        if !PLAUSIBLE_ROTATION_SCALE.contains(&rotation_scale) {
            return Err(TimingError::ImplausibleFit {
                volume_id,
                rotation_scale,
            });
        }
        let mut offsets: Vec<f64> = points
            .iter()
            .map(|(commanded, observed)| observed - rotation_scale * commanded)
            .collect();
        let publish_offset_seconds = median_in_place(&mut offsets).unwrap_or(0.0);
        let mut abs_residuals: Vec<f64> = offsets
            .iter()
            .map(|offset| (offset - publish_offset_seconds).abs())
            .collect();
        let median_abs_residual_seconds = median_in_place(&mut abs_residuals).unwrap_or(0.0);

        let start_chunk_offset_seconds = volume
            .chunk(START_CHUNK_ID)
            .filter(|chunk| chunk.chunk_type == RealtimeChunkType::Start)
            .map(|chunk| seconds_between(chunk.last_modified, volume.volume_time));

        let last_span = model.spans.get(end_position.cut_index).copied();
        let radials_end_seconds = last_span.map_or(0.0, |span| {
            rotation_scale * (span.commanded_seconds_before + span.commanded_rotation_seconds)
                + gap * end_position.cut_index as f64
        });
        let inter_volume_gap_seconds = self.previous.as_ref().and_then(|previous| {
            // Ids run 1..=999 and 999 is followed by 1.
            let consecutive = previous.site == volume.site
                && super::iterator::next_volume_id(previous.volume_id) == volume_id;
            let between = seconds_between(volume.volume_time, previous.volume_time);
            (consecutive && between > 0.0 && between < MAX_ROLLOVER_SECONDS)
                .then_some(between - previous.radials_end_seconds)
        });
        let top_elevation_deg = plan
            .cuts
            .iter()
            .take(end_position.cut_index + 1)
            .map(ScanCut::signed_elevation_deg)
            .fold(f32::NEG_INFINITY, f32::max);

        push_window(&mut self.rotation_scale, rotation_scale, self.window);
        push_window(
            &mut self.publish_offset,
            publish_offset_seconds,
            self.window,
        );
        if let Some(offset) = start_chunk_offset_seconds {
            push_window(&mut self.start_chunk_offset, offset, self.window);
        }
        if let Some(gap) = inter_volume_gap_seconds {
            push_window(&mut self.inter_volume_gap, gap, self.window);
        }
        self.top_elevation = Some((plan.vcp, top_elevation_deg));
        self.previous = Some(PreviousVolume {
            site: volume.site.clone(),
            volume_id,
            volume_time: volume.volume_time,
            radials_end_seconds,
        });
        self.volumes_observed += 1;

        Ok(VolumeFit {
            volume_id,
            rotation_scale,
            publish_offset_seconds,
            start_chunk_offset_seconds,
            inter_volume_gap_seconds,
            end_chunk_id,
            end_plan_chunk_number,
            status_only_chunks: volume.status_only_chunks(),
            top_elevation_deg,
            radial_chunks: points.len(),
            median_abs_residual_seconds,
        })
    }
}

fn push_window(values: &mut VecDeque<f64>, value: f64, window: usize) {
    if !value.is_finite() {
        return;
    }
    values.push_back(value);
    while values.len() > window {
        values.pop_front();
    }
}

fn median(values: &VecDeque<f64>) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    median_in_place(&mut sorted)
}

fn median_in_place(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values.get(middle).copied()
    } else {
        Some((values.get(middle - 1)? + values.get(middle)?) / 2.0)
    }
}

fn lower_quartile(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 4).copied()
}

/// Median of pairwise slopes over pairs at least
/// [`MIN_SLOPE_PAIR_SECONDS`] apart in x. Falls back to all pairs with
/// distinct x when no pair is that far apart.
fn theil_sen_slope(points: &[(f64, f64)]) -> Option<f64> {
    let stride = points.len().div_ceil(MAX_FIT_POINTS).max(1);
    let sample: Vec<(f64, f64)> = points.iter().step_by(stride).copied().collect();
    let slopes = |min_dx: f64| -> Vec<f64> {
        let mut slopes = Vec::new();
        for (index, (x0, y0)) in sample.iter().enumerate() {
            for (x1, y1) in sample.iter().skip(index + 1) {
                let dx = x1 - x0;
                if dx.abs() >= min_dx {
                    slopes.push((y1 - y0) / dx);
                }
            }
        }
        slopes
    };
    let mut wide = slopes(MIN_SLOPE_PAIR_SECONDS);
    if wide.is_empty() {
        wide = slopes(f64::MIN_POSITIVE);
    }
    median_in_place(&mut wide)
}

fn seconds_between(later: DateTime<Utc>, earlier: DateTime<Utc>) -> f64 {
    (later - earlier).num_milliseconds() as f64 / 1000.0
}

/// `base + seconds`, or `base` when the offset is not finite or overflows.
fn offset_time(base: DateTime<Utc>, seconds: f64) -> DateTime<Utc> {
    if !seconds.is_finite() {
        return base;
    }
    Duration::try_milliseconds((seconds * 1000.0).round() as i64)
        .and_then(|offset| base.checked_add_signed(offset))
        .unwrap_or(base)
}
