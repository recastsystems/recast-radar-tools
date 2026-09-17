//! Level II volumes together with their NEXRAD metadata messages.
//!
//! [`decode_volume_with_metadata`] returns the same [`RadarVolume`] as
//! [`crate::decode_volume_from_bytes`], plus a [`NexradMetadata`] holding the
//! typed metadata messages ([`crate::messages`]) and per-sweep message 31
//! constant blocks, which the shared radar model has no place for.
//!
//! Where each field comes from:
//!
//! - The metadata messages (2, 3, 5 or 7, 8, 13, 15, 18 and 32) are read from
//!   the Archive II metadata record only ([`messages::metadata_record`]: the
//!   first LDM record, or the first 134 frames of raw-record files), never
//!   from the data records. For each field the first message of its type
//!   that decodes is kept. Files from before the metadata record existed
//!   (ARCHIVE2 headers, 1991-2003) have radials in those frames and at most
//!   a message 2.
//! - [`NexradMetadata::per_sweep_elevation_data`] is filled while the volume
//!   is decoded, from the first message 31 radial of each cut, so the radial
//!   data is decompressed once. Only the metadata record is decompressed a
//!   second time.

use recast_radar_core::RadarVolume;

use crate::messages::adaptation::RdaAdaptationData;
use crate::messages::bypass_map::ClutterFilterBypassMap;
use crate::messages::clutter_censor::ClutterCensorZones;
use crate::messages::clutter_filter_map::ClutterFilterMap;
use crate::messages::msg31_blocks::{
    DigitalRadarDataGeneric, ElevationDataBlock, RadialDataBlock, VolumeDataBlock,
};
use crate::messages::performance::PerformanceMaintenance;
use crate::messages::prf::RdaPrfData;
use crate::messages::rda_status::{RdaBuild, RdaStatus};
use crate::messages::vcp::VolumeCoveragePattern;
use crate::messages::{self, MessageBody, MessageWalker};
use crate::{RadialObserver, Result};

/// A decoded Level II volume and its NEXRAD metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct NexradVolume {
    /// The volume, identical to what [`crate::decode_volume_from_bytes`]
    /// returns for the same bytes.
    pub volume: RadarVolume,
    /// Metadata messages and per-sweep message 31 constant blocks.
    pub metadata: NexradMetadata,
}

/// NEXRAD metadata of one Level II volume. Every field is `None` when the
/// file has no such message (or none that decoded); see the module
/// documentation for where each comes from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NexradMetadata {
    /// Message 2, RDA Status Data (Table IV).
    pub rda_status: Option<RdaStatus>,
    /// Message 3, Performance/Maintenance Data (Table V).
    pub performance: Option<Box<PerformanceMaintenance>>,
    /// Message 5 (or 7), Volume Coverage Pattern (Table XI).
    pub vcp: Option<VolumeCoveragePattern>,
    /// Message 18, RDA Adaptation Data (Table XV).
    pub adaptation: Option<Box<RdaAdaptationData>>,
    /// Message 15, Clutter Filter Map (Table XIV).
    pub clutter_filter_map: Option<ClutterFilterMap>,
    /// Message 13, Clutter Filter Bypass Map (Table IX).
    pub bypass_map: Option<ClutterFilterBypassMap>,
    /// Message 8, Clutter Censor Zones (Table XII). The RPG sends it to the
    /// RDA, so no Archive II file in the corpus has one.
    pub clutter_censor_zones: Option<ClutterCensorZones>,
    /// Message 32, RDA PRF Data (Table XVIII), sent from Build 23.
    pub prf: Option<RdaPrfData>,
    /// Message 31 constant blocks of each cut's first radial, in the order of
    /// [`RadarVolume::cuts`]: one entry per cut, except cuts opened by a
    /// message 1 radial or by a radial whose blocks do not decode (listed in
    /// [`Self::errors`]). `None` when no radial of the volume is a message 31
    /// (Message 1 volumes).
    pub per_sweep_elevation_data: Option<Vec<SweepElevationData>>,
    /// RDA build from the message 2 above ([`RdaStatus::rda_build`]); `None`
    /// for the legacy RDA layout, which has no build field.
    pub build: Option<RdaBuild>,
    /// Problems met while reading the metadata, as display text: messages in
    /// the metadata record that could not be framed or decoded (for example
    /// the stale orphan segments of 2008 metadata records, or the zero-filled
    /// Message 15 of KLIX 2005), and cut-opening radials whose constant
    /// blocks did not decode. None of them fails the decode.
    pub errors: Vec<String>,
}

/// Message 31 constant blocks of the first radial of one cut.
///
/// The ELV block is constant within a cut. VOL and RAD blocks are sent with
/// every radial and can change within a cut (noise levels, and the Nyquist
/// velocity of Doppler sectors), so these are the values at the cut's start.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepElevationData {
    /// Index of the cut in [`RadarVolume::cuts`].
    pub cut_index: usize,
    /// Data Header Block elevation number (1-based cut number in the VCP).
    pub elevation_number: u8,
    /// Data Header Block elevation angle of the first radial, degrees.
    pub elevation_angle_deg: f32,
    /// Elevation Data Constant block (Table XVII-F): atmospheric attenuation
    /// and calibration constant.
    pub elevation: Option<ElevationDataBlock>,
    /// Volume Data Constant block (Table XVII-E).
    pub volume: Option<VolumeDataBlock>,
    /// Radial Data Constant block (Table XVII-H).
    pub radial: Option<RadialDataBlock>,
    /// The VOL block's ZDR bias estimate in dB, converted with the same
    /// radial's ZDR moment block (Table XVII-E notes 20 and 33; see
    /// [`VolumeDataBlock::zdr_bias_estimate_db`]). `None` without a VOL
    /// block, in the 44-byte VOL layout (before Build 20), or when the RPG
    /// reports it as not available.
    pub zdr_bias_estimate_db: Option<f32>,
}

/// Decode a Level II volume and its NEXRAD metadata.
///
/// Accepts the same inputs as [`crate::decode_volume_from_bytes`] and returns
/// the same volume, with the same errors. Problems in the metadata alone do
/// not fail the call; they are listed in [`NexradMetadata::errors`].
pub fn decode_volume_with_metadata(bytes: &[u8]) -> Result<NexradVolume> {
    let mut sweeps = SweepCollector::default();
    let volume = crate::decode_volume_observed(bytes, &mut sweeps)?;
    let mut metadata = NexradMetadata::from_metadata_record(bytes);
    metadata.per_sweep_elevation_data = sweeps.saw_message_31.then_some(sweeps.sweeps);
    metadata.errors.extend(sweeps.errors);
    Ok(NexradVolume { volume, metadata })
}

impl NexradMetadata {
    /// The metadata messages of a Level II file or real-time start chunk,
    /// read from its metadata record, without decoding the volume.
    /// [`Self::per_sweep_elevation_data`] stays `None`.
    pub fn from_metadata_record(bytes: &[u8]) -> Self {
        let mut metadata = Self::default();
        let record = match messages::metadata_record(bytes) {
            Ok(record) => record,
            Err(error) => {
                metadata.errors.push(format!("metadata record: {error}"));
                return metadata;
            }
        };
        for item in MessageWalker::new(&record) {
            let body = match item {
                Ok((_, body)) => body,
                Err(error) => {
                    metadata.errors.push(format!("metadata record: {error}"));
                    continue;
                }
            };
            match body {
                MessageBody::RdaStatus(status) => set_first(&mut metadata.rda_status, status),
                MessageBody::Performance(data) => set_first(&mut metadata.performance, data),
                MessageBody::Vcp(vcp) => set_first(&mut metadata.vcp, vcp),
                MessageBody::Adaptation(data) => set_first(&mut metadata.adaptation, data),
                MessageBody::ClutterFilterMap(map) => {
                    set_first(&mut metadata.clutter_filter_map, map);
                }
                MessageBody::BypassMap(map) => set_first(&mut metadata.bypass_map, map),
                MessageBody::ClutterCensorZones(zones) => {
                    set_first(&mut metadata.clutter_censor_zones, zones);
                }
                MessageBody::Prf(prf) => set_first(&mut metadata.prf, prf),
                _ => {}
            }
        }
        metadata.build = metadata.rda_status.as_ref().and_then(RdaStatus::rda_build);
        metadata
    }
}

fn set_first<T>(slot: &mut Option<T>, value: T) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

/// Collects the constant blocks of each cut's first message 31 radial while
/// the volume decoder runs.
///
/// The decoder creates a cut only for the radial that opens it, and always
/// appends it, so a message 31 that leaves the volume with more cuts than the
/// previous one did is the first radial of the last cut. Later radials may go
/// to earlier cuts (for example out-of-order real-time chunks), but never
/// open one.
#[derive(Default)]
struct SweepCollector {
    /// Cuts in the volume after the previous message 31.
    cuts_seen: usize,
    sweeps: Vec<SweepElevationData>,
    errors: Vec<String>,
    saw_message_31: bool,
}

impl RadialObserver for SweepCollector {
    fn message_31(&mut self, body: &[u8], volume: &RadarVolume) {
        self.saw_message_31 = true;
        if volume.cuts.len() == self.cuts_seen {
            return;
        }
        self.cuts_seen = volume.cuts.len();
        let cut_index = volume.cuts.len() - 1;
        // A cut with more radials was opened by message 1 radials (which the
        // observer does not see) and this radial went to another cut.
        if volume.cuts[cut_index].radials.len() != 1 {
            return;
        }
        match DigitalRadarDataGeneric::decode(body) {
            Ok(radial) => self.sweeps.push(SweepElevationData {
                cut_index,
                elevation_number: radial.header.elevation_number,
                elevation_angle_deg: radial.header.elevation_angle_deg,
                elevation: radial.elevation,
                volume: radial.volume,
                radial: radial.radial,
                zdr_bias_estimate_db: radial.zdr_bias_estimate_db(),
            }),
            Err(error) => self
                .errors
                .push(format!("cut {cut_index} first radial: {error}")),
        }
    }
}
