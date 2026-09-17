//! Messages without a Product Description Block: the General Status Message
//! (message code 2, ICD 2620001 section 3.2.1.1 and Figure 3-17) and
//! plain-text messages (WMO heading `NOUS..`, e.g. the Free Text Message).
//!
//! [`crate::decode_message`] returns them as [`crate::Level3Message`] variants;
//! [`crate::decode_product`] keeps reporting them as errors.
//!
//! Status halfwords are bit fields. The ICD numbers bits with bit 15 as the
//! least significant, so ICD bit `b` is the value `1 << (15 - b)`; the flag
//! constants below are those values. Field meanings follow 2620001AD (RPG
//! Build 24.0); bits that only 2620001T (Build 13.0) defines are noted.

use crate::Level3Error;
use crate::header::{MESSAGE_HEADER_BYTES, MessageHeader, OperationalMode, TextHeader};
use crate::read::{be_u16, expect_i16, slice};

/// Block length (halfword 11) of the General Status Message before RPG Build
/// 14.0: halfwords 12-52 (2620001T Figure 3-17).
pub const GSM_SHORT_BLOCK_BYTES: u16 = 82;
/// Block length (halfword 11) of the General Status Message from RPG Build
/// 14.0: halfwords 12-100, adding elevations 21-25, VCP supplemental data and
/// the supplemental cut map (2620001AD Figure 3-17).
pub const GSM_BLOCK_BYTES: u16 = 178;

/// First halfword of the general status block (the block divider).
const FIRST_HALFWORD: usize = 10;

/// Declares a status halfword type with named bit flags.
macro_rules! status_bits {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$flag_meta:meta])* $flag:ident = $bit:literal, $label:literal; )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub u16);

        impl $name {
            $( $(#[$flag_meta])* pub const $flag: Self = Self(1 << $bit); )*

            const LABELS: &'static [(u16, &'static str)] = &[$((1 << $bit, $label)),*];

            /// The raw halfword.
            pub const fn bits(self) -> u16 {
                self.0
            }

            /// True when every bit set in `flags` is set.
            pub const fn contains(self, flags: Self) -> bool {
                self.0 & flags.0 == flags.0
            }

            /// True when no bit is set.
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// ICD names of the named flags that are set, least significant
            /// bit first. Set bits the ICD calls spare are not listed.
            pub fn names(self) -> Vec<&'static str> {
                Self::LABELS
                    .iter()
                    .filter(|(bit, _)| self.0 & bit != 0)
                    .map(|(_, label)| *label)
                    .collect()
            }
        }
    };
}

status_bits! {
    /// RDA operability status (halfword 13). No flag set (apart from the
    /// spare bits): indeterminate, the RPG determines the status.
    RdaOperability {
        /// ICD bit 15: automatic calibration disabled (2620001T; spare in 2620001AD).
        AUTOMATIC_CALIBRATION_DISABLED = 0, "Automatic Calibration Disabled";
        /// ICD bit 14: online.
        ONLINE = 1, "Online";
        /// ICD bit 13: maintenance action required.
        MAINTENANCE_REQUIRED = 2, "Maintenance Action Required";
        /// ICD bit 12: maintenance action mandatory.
        MAINTENANCE_MANDATORY = 3, "Maintenance Action Mandatory";
        /// ICD bit 11: commanded shutdown.
        COMMANDED_SHUTDOWN = 4, "Commanded Shutdown";
        /// ICD bit 10: inoperable.
        INOPERABLE = 5, "Inoperable";
        /// ICD bit 8: wideband disconnect.
        WIDEBAND_DISCONNECT = 7, "Wideband Disconnect";
    }
}

status_bits! {
    /// RDA status (halfword 36). No flag set: indeterminate.
    RdaStatus {
        /// ICD bit 14: startup.
        STARTUP = 1, "Startup";
        /// ICD bit 13: standby.
        STANDBY = 2, "Standby";
        /// ICD bit 12: restart.
        RESTART = 3, "Restart";
        /// ICD bit 11: operate.
        OPERATE = 4, "Operate";
        /// ICD bit 9: off-line operate (2620001T; spare in 2620001AD).
        OFFLINE_OPERATE = 6, "Off-line Operate";
    }
}

status_bits! {
    /// RDA alarms of the controlling channel (halfword 37). No flag set: no
    /// alarms.
    RdaAlarms {
        /// ICD bit 15: indeterminate, the RPG cannot determine the alarms.
        INDETERMINATE = 0, "Indeterminate";
        /// ICD bit 14: tower/utilities.
        TOWER_UTILITIES = 1, "Tower/Utilities";
        /// ICD bit 13: pedestal.
        PEDESTAL = 2, "Pedestal";
        /// ICD bit 12: transmitter.
        TRANSMITTER = 3, "Transmitter";
        /// ICD bit 11: receiver (2620001T: receiver/signal processor).
        RECEIVER = 4, "Receiver";
        /// ICD bit 10: RDA control.
        RDA_CONTROL = 5, "RDA Control";
        /// ICD bit 9: RDA communications.
        RDA_COMMUNICATIONS = 6, "RDA Communications";
        /// ICD bit 8: signal processor (2620001AD; spare in 2620001T).
        SIGNAL_PROCESSOR = 7, "Signal Processor";
    }
}

status_bits! {
    /// Data transmission enabled (halfword 38).
    DataTransmission {
        /// ICD bit 14: none.
        NONE = 1, "None";
        /// ICD bit 13: reflectivity.
        REFLECTIVITY = 2, "Reflectivity";
        /// ICD bit 12: velocity.
        VELOCITY = 3, "Velocity";
        /// ICD bit 11: spectrum width.
        SPECTRUM_WIDTH = 4, "Spectrum Width";
        /// ICD bit 10: dual polarization data expected.
        DUAL_POL = 5, "Dual Pol Data Expected";
    }
}

status_bits! {
    /// RPG operability status (halfword 39).
    RpgOperability {
        /// ICD bit 15: loadshed.
        LOADSHED = 0, "Loadshed";
        /// ICD bit 14: on-line.
        ONLINE = 1, "On-line";
        /// ICD bit 13: maintenance action required.
        MAINTENANCE_REQUIRED = 2, "Maintenance Action Required";
        /// ICD bit 12: maintenance action mandatory.
        MAINTENANCE_MANDATORY = 3, "Maintenance Action Mandatory";
        /// ICD bit 11: commanded shutdown.
        COMMANDED_SHUTDOWN = 4, "Commanded Shutdown";
    }
}

status_bits! {
    /// RPG alarms (halfword 40).
    RpgAlarms {
        /// ICD bit 15: no alarms.
        NO_ALARMS = 0, "No Alarms";
        /// ICD bit 14: node connectivity.
        NODE_CONNECTIVITY = 1, "Node Connectivity";
        /// ICD bit 13: wideband failure (2620001AD; spare in 2620001T).
        WIDEBAND_FAILURE = 2, "Wideband Failure";
        /// ICD bit 12: RPG control task failure.
        CONTROL_TASK_FAILURE = 3, "RPG Control Task Failure";
        /// ICD bit 11: data base failure.
        DATA_BASE_FAILURE = 4, "Data Base Failure";
        /// ICD bit 9: RPG input buffer loadshed (wideband).
        INPUT_BUFFER_LOADSHED = 6, "RPG Input Buffer Loadshed (Wideband)";
        /// ICD bit 7: product storage loadshed.
        PRODUCT_STORAGE_LOADSHED = 8, "Product Storage Loadshed";
        /// ICD bit 4: backup communications (2620001AD; spare in 2620001T).
        BACKUP_COMMS = 11, "Backup Comms";
        /// ICD bit 3: RPG/RPG intercomputer link failure.
        INTERCOMPUTER_LINK_FAILURE = 12, "RPG/RPG Intercomputer Link Failure";
        /// ICD bit 2: redundant channel error.
        REDUNDANT_CHANNEL_ERROR = 13, "Redundant Channel Error";
        /// ICD bit 1: task failure.
        TASK_FAILURE = 14, "Task Failure";
        /// ICD bit 0: media failure.
        MEDIA_FAILURE = 15, "Media Failure";
    }
}

status_bits! {
    /// RPG status (halfword 41).
    RpgStatus {
        /// ICD bit 15: restart.
        RESTART = 0, "Restart";
        /// ICD bit 14: operate.
        OPERATE = 1, "Operate";
        /// ICD bit 13: standby.
        STANDBY = 2, "Standby";
        /// ICD bit 11: test mode (2620001T; spare in 2620001AD).
        TEST_MODE = 4, "Test Mode";
    }
}

status_bits! {
    /// RPG narrowband status (halfword 42).
    RpgNarrowband {
        /// ICD bit 15: commanded disconnect.
        COMMANDED_DISCONNECT = 0, "Commanded Disconnect";
        /// ICD bit 14: narrowband loadshed.
        LOADSHED = 1, "Narrowband Loadshed";
    }
}

status_bits! {
    /// Product availability (halfword 44).
    ProductAvailability {
        /// ICD bit 15: product availability.
        AVAILABLE = 0, "Product Availability";
        /// ICD bit 14: degraded availability.
        DEGRADED = 1, "Degraded Availability";
        /// ICD bit 13: not available.
        NOT_AVAILABLE = 2, "Not Available";
    }
}

status_bits! {
    /// Clutter mitigation decision status (halfword 46): enabled flag and the
    /// elevation segments with clutter mitigation decision enabled.
    ClutterMitigation {
        /// ICD bit 15: clutter mitigation decision enabled.
        ENABLED = 0, "Enabled";
        /// ICD bit 14: elevation segment 1.
        SEGMENT_1 = 1, "Segment 1";
        /// ICD bit 13: elevation segment 2.
        SEGMENT_2 = 2, "Segment 2";
        /// ICD bit 12: elevation segment 3.
        SEGMENT_3 = 3, "Segment 3";
        /// ICD bit 11: elevation segment 4.
        SEGMENT_4 = 4, "Segment 4";
        /// ICD bit 10: elevation segment 5.
        SEGMENT_5 = 5, "Segment 5";
    }
}

status_bits! {
    /// VCP supplemental data (halfword 58, RPG Build 14.0 and later).
    VcpSupplemental {
        /// ICD bit 15: AVSET enabled.
        AVSET = 0, "AVSET";
        /// ICD bit 14: SAILS enabled VCP in use; supplemental cuts are SAILS cuts.
        SAILS = 1, "SAILS";
        /// ICD bit 13: site-specific VCP in use.
        SITE_SPECIFIC_VCP = 2, "Site-Specific VCP";
        /// ICD bit 12: radial by radial noise (RxRN) enabled.
        RXR_NOISE = 3, "RxRN";
        /// ICD bit 11: coherency based thresholding (CBT) enabled.
        CBT = 4, "CBT";
        /// ICD bit 10: VCP sequence in use.
        VCP_SEQUENCE = 5, "VCP Sequence";
        /// ICD bit 9: SPRT VCP in use.
        SPRT = 6, "SPRT";
        /// ICD bit 8: MRLE enabled VCP in use; supplemental cuts are MRLE cuts.
        MRLE = 7, "MRLE";
        /// ICD bit 7: base tilt enabled VCP in use.
        BASE_TILT = 8, "Base Tilt";
        /// ICD bit 6: MPDA VCP in use.
        MPDA = 9, "MPDA";
        /// ICD bit 5: low resolution VMI (clear: high resolution).
        LOW_RESOLUTION_VMI = 10, "Low Resolution VMI";
    }
}

/// Supplemental cut map (halfwords 59-60, RPG Build 14.0 and later): which
/// elevation cuts of the VCP are supplemental (SAILS or MRLE, see
/// [`VcpSupplemental`]) and how many supplemental and added MPDA cuts there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SupplementalCuts {
    /// Raw halfwords 59 and 60.
    pub halfwords: [u16; 2],
}

impl SupplementalCuts {
    /// True when elevation cut `cut` (1-25) is a supplemental cut: halfword 59
    /// ICD bits 15-0 are cuts 1-16, halfword 60 ICD bits 15-7 are cuts 17-25.
    pub fn is_supplemental(&self, cut: usize) -> bool {
        match cut {
            1..=16 => self.halfwords[0] & (1 << (cut - 1)) != 0,
            17..=25 => self.halfwords[1] & (1 << (cut - 17)) != 0,
            _ => false,
        }
    }

    /// Number of supplemental elevations in the VCP (halfword 60 ICD bits 6-3).
    pub fn supplemental_count(&self) -> u16 {
        (self.halfwords[1] >> 9) & 0xF
    }

    /// Number of added MPDA elevations in the VCP (halfword 60 ICD bits 2-0).
    pub fn mpda_count(&self) -> u16 {
        (self.halfwords[1] >> 13) & 0x7
    }
}

/// General Status Message (message code 2, ICD 2620001 Figure 3-17): state of
/// the RDA and RPG, the scan strategy and equipment status.
///
/// The general status block follows the Message Header Block. It is
/// [`GSM_SHORT_BLOCK_BYTES`] long (halfwords 10-52) before RPG Build 14.0 and
/// [`GSM_BLOCK_BYTES`] long (halfwords 10-100) from Build 14.0; the Build 14.0
/// fields are `None` in the short form.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneralStatusMessage {
    /// NOAAPort/WMO/AWIPS transmission header, when the file has one.
    pub text_header: Option<TextHeader>,
    /// Message Header Block (message code 2).
    pub message_header: MessageHeader,
    /// Length of the block in bytes after this field (halfword 11).
    pub block_length: u16,
    /// Mode of operation (halfword 12).
    pub mode: OperationalMode,
    /// RDA operability status (halfword 13).
    pub rda_operability: RdaOperability,
    /// RDA volume coverage pattern (halfword 14).
    pub vcp: u16,
    /// Number of elevation cuts (halfword 15), at most 20 before Build 14.0 and
    /// 25 from it.
    pub elevation_cuts: u16,
    /// Elevation angle slots in degrees (0.1 degree scaled integers): cuts
    /// 1-20 (halfwords 16-35), then cuts 21-25 (halfwords 53-57) in the long
    /// form. Slots after the last cut are 0.
    pub elevations_deg: Vec<f64>,
    /// RDA status (halfword 36).
    pub rda_status: RdaStatus,
    /// RDA alarms (halfword 37).
    pub rda_alarms: RdaAlarms,
    /// Data transmission enabled (halfword 38).
    pub data_transmission: DataTransmission,
    /// RPG operability status (halfword 39).
    pub rpg_operability: RpgOperability,
    /// RPG alarms (halfword 40).
    pub rpg_alarms: RpgAlarms,
    /// RPG status (halfword 41).
    pub rpg_status: RpgStatus,
    /// RPG narrowband status (halfword 42).
    pub rpg_narrowband: RpgNarrowband,
    /// Horizontal channel reflectivity calibration correction in dB, the
    /// difference from adaptation data (halfword 43, dB/4).
    pub horizontal_calibration_db: f64,
    /// Product availability (halfword 44).
    pub product_availability: ProductAvailability,
    /// Super resolution elevation cuts (halfword 45): ICD bit 15 (the least
    /// significant) is elevation cut 1. See
    /// [`super_resolution`](Self::super_resolution).
    pub super_resolution_cuts: u16,
    /// Clutter mitigation decision status (halfword 46).
    pub clutter_mitigation: ClutterMitigation,
    /// Vertical channel reflectivity calibration correction in dB (halfword 47, dB/4).
    pub vertical_calibration_db: f64,
    /// RDA build number (halfword 48), scaled by 10 (190 is Build 19.0); 0 for
    /// legacy RDA systems. See [`rda_build_version`](Self::rda_build_version).
    pub rda_build: u16,
    /// RDA channel number (halfword 49): 0 NWS single thread, 1 RDA 1, 2 RDA 2
    /// (NWS or FAA redundant).
    pub rda_channel: u16,
    /// Reserved halfwords 50 and 51 (dial-up users only).
    pub reserved: [u16; 2],
    /// RPG build version (halfword 52), scaled by 10. See
    /// [`rpg_build_version`](Self::rpg_build_version).
    pub rpg_build: u16,
    /// VCP supplemental data (halfword 58); `None` before Build 14.0.
    pub vcp_supplemental: Option<VcpSupplemental>,
    /// Supplemental cut map (halfwords 59-60); `None` before Build 14.0.
    pub supplemental_cuts: Option<SupplementalCuts>,
    /// Raw halfwords of the general status block as unsigned values, from the
    /// block divider (halfword 10) to the end of the block:
    /// `halfwords[n - 10]` is ICD halfword `n`.
    pub halfwords: Vec<u16>,
}

impl GeneralStatusMessage {
    /// Parses the general status block after the Message Header Block of `message`.
    pub(crate) fn parse(
        text_header: Option<TextHeader>,
        message_header: MessageHeader,
        message: &[u8],
    ) -> Result<Self, Level3Error> {
        let start = MESSAGE_HEADER_BYTES;
        expect_i16(message, start, -1, "general status block divider")?;
        let block_length = be_u16(message, start + 2, "general status block length")?;
        if block_length < GSM_SHORT_BLOCK_BYTES {
            return Err(Level3Error::InvalidMessage {
                code: message_header.code,
                reason: format!(
                    "general status block length {block_length} is shorter than \
                     {GSM_SHORT_BLOCK_BYTES} bytes"
                ),
            });
        }
        let block = slice(
            message,
            start,
            4 + usize::from(block_length),
            "general status block",
        )?;
        let halfwords: Vec<u16> = block
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        // The block holds at least halfwords 10-52 (checked above); the long
        // form holds halfwords 10-100.
        let hw = |n: usize| halfwords[n - FIRST_HALFWORD];
        let long = block_length >= GSM_BLOCK_BYTES;
        let angle = |n: usize| f64::from(hw(n).cast_signed()) / 10.0;
        let quarter_db = |n: usize| f64::from(hw(n).cast_signed()) / 4.0;

        let mut elevations_deg: Vec<f64> = (16..=35).map(angle).collect();
        if long {
            elevations_deg.extend((53..=57).map(angle));
        }
        Ok(Self {
            text_header,
            message_header,
            block_length,
            mode: OperationalMode::from_code(hw(12)),
            rda_operability: RdaOperability(hw(13)),
            vcp: hw(14),
            elevation_cuts: hw(15),
            elevations_deg,
            rda_status: RdaStatus(hw(36)),
            rda_alarms: RdaAlarms(hw(37)),
            data_transmission: DataTransmission(hw(38)),
            rpg_operability: RpgOperability(hw(39)),
            rpg_alarms: RpgAlarms(hw(40)),
            rpg_status: RpgStatus(hw(41)),
            rpg_narrowband: RpgNarrowband(hw(42)),
            horizontal_calibration_db: quarter_db(43),
            product_availability: ProductAvailability(hw(44)),
            super_resolution_cuts: hw(45),
            clutter_mitigation: ClutterMitigation(hw(46)),
            vertical_calibration_db: quarter_db(47),
            rda_build: hw(48),
            rda_channel: hw(49),
            reserved: [hw(50), hw(51)],
            rpg_build: hw(52),
            vcp_supplemental: long.then(|| VcpSupplemental(hw(58))),
            supplemental_cuts: long.then(|| SupplementalCuts {
                halfwords: [hw(59), hw(60)],
            }),
            halfwords,
        })
    }

    /// Elevation angles of the VCP's cuts: the first
    /// [`elevation_cuts`](Self::elevation_cuts) slots of
    /// [`elevations_deg`](Self::elevations_deg).
    pub fn cut_elevations_deg(&self) -> &[f64] {
        let n = usize::from(self.elevation_cuts).min(self.elevations_deg.len());
        &self.elevations_deg[..n]
    }

    /// True when elevation cut `cut` (1-16) has super resolution enabled.
    pub fn super_resolution(&self, cut: usize) -> bool {
        (1..=16).contains(&cut) && self.super_resolution_cuts & (1 << (cut - 1)) != 0
    }

    /// RDA build as a version number, e.g. 19.0 (2620001AD Figure 3-17 Note 2).
    pub fn rda_build_version(&self) -> f64 {
        f64::from(self.rda_build) / 10.0
    }

    /// RPG build as a version number, e.g. 19.0.
    pub fn rpg_build_version(&self) -> f64 {
        f64::from(self.rpg_build) / 10.0
    }
}

/// A plain-text message: a file whose WMO heading starts with `NOUS` (e.g. the
/// Free Text Message, AWIPS category FTM), carrying text instead of a binary
/// Level III message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMessage {
    /// WMO/AWIPS transmission header (always present: the heading identifies
    /// the message as text).
    pub text_header: TextHeader,
    /// The text after the heading and AWIPS identifier lines, one `char` per
    /// byte (ISO 8859-1), without the transmission trailer (see
    /// [`crate::decode_message`]). Line endings are kept as in the file.
    pub text: String,
}

impl TextMessage {
    pub(crate) fn new(text_header: TextHeader, text: &[u8]) -> Self {
        Self {
            text_header,
            text: text.iter().copied().map(char::from).collect(),
        }
    }
}
