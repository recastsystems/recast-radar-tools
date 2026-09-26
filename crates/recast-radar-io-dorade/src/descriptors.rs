//! Every DORADE descriptor field as a model attribute
//! (`docs/design/fm301-model.md` section 2).
//!
//! The sweepfile decoder reads the blocks it needs for coordinates and
//! fields; this module reads every field of each descriptor block and names
//! it `dorade_<block>_<field>` after the member of the DORADE structure
//! (R. Oye and M. Case, "DORADE Data Format", NCAR/ATD 1995, revised by
//! W.-C. Lee; lrose-core `DoradeData.hh`), with the unit the format
//! document gives as a suffix (`_km`, `_deg`, `_db`, `_us`, `_cm`, ...).
//! Members for which the documents give no unit (`scan_mode_pram0`,
//! `data_red_parm0`, the coplane baselines, `pc_xmtr_bandwidth`) or no
//! clear one (`field_of_view`: "mra") have no suffix. Values are kept
//! verbatim, missing-value sentinels (-999, -9999, -32768) included.
//!
//! | Block | Where |
//! |---|---|
//! | SSWB, VOLD, RADD (1995 layout and the 300-byte extension), CFAC, CSFD, SWIB, COMM, SEDS | the sweep's attributes (`Sweep::other`) |
//! | PARM (104-byte layout and the 216-byte extension) | the field's attributes (`FieldAttrs::other`) |
//! | CELV | the sweep variable `dorade_celv_distance` |
//! | RYIB, ASIB | per-ray variables (see `dorade.rs`) |
//!
//! A block shorter than a field leaves that field out.
//!
//! Not carried: the NULL end marker and the RKTB rotation angle table that
//! follows it in untrimmed sweepfiles (the three NOXP 2009-05 fixtures).
//! RKTB is the writer's index of the rays: an angle-to-ray lookup table and,
//! per ray, the rotation angle, the file offset and the byte size of its
//! RYIB group. The decoder reads the rays themselves, so like the Level II
//! block pointers and sizes it is structural and not read. XSTF, FRIB,
//! FRAD and the other optional blocks are skipped: no real sample holds
//! one.
//!
//! SEDS, the Solo II edit summary that follows RKTB in edited sweepfiles
//! (the NOAA P-3 tail radar sweeps of Hurricane Michael), is kept as text:
//! the sweep attribute `dorade_seds_text`, and the volume's FM301
//! `history`, as LROSE Radx reads it.

use recast_radar_core::model::{ArrayBuf, AttrValue, Scalar};

use crate::dorade::Endian;

/// Attributes collected from one block.
pub(crate) type Attrs = Vec<(Box<str>, AttrValue)>;

/// Reads typed fields of one block into attributes named
/// `dorade_<block>_<field>`.
struct Reader<'a> {
    endian: Endian,
    block: &'a [u8],
    prefix: &'static str,
    out: Attrs,
}

impl<'a> Reader<'a> {
    fn new(endian: Endian, block: &'a [u8], prefix: &'static str) -> Self {
        Self {
            endian,
            block,
            prefix,
            out: Vec::new(),
        }
    }

    fn fits(&self, offset: usize, len: usize) -> bool {
        offset
            .checked_add(len)
            .is_some_and(|end| end <= self.block.len())
    }

    fn push(&mut self, name: &str, value: AttrValue) {
        self.out
            .push((format!("{}{name}", self.prefix).into_boxed_str(), value));
    }

    fn i16(&mut self, name: &str, offset: usize) {
        if self.fits(offset, 2) {
            let value = self.endian.i16(self.block, offset);
            self.push(name, AttrValue::Scalar(Scalar::I16(value)));
        }
    }

    fn i32(&mut self, name: &str, offset: usize) {
        if self.fits(offset, 4) {
            let value = self.endian.i32(self.block, offset);
            self.push(name, AttrValue::Scalar(Scalar::I32(value)));
        }
    }

    fn f32(&mut self, name: &str, offset: usize) {
        if self.fits(offset, 4) {
            let value = self.endian.f32(self.block, offset);
            self.push(name, AttrValue::Scalar(Scalar::F32(value)));
        }
    }

    fn f64(&mut self, name: &str, offset: usize) {
        if self.fits(offset, 8) {
            let value = self.endian.f64(self.block, offset);
            self.push(name, AttrValue::Scalar(Scalar::F64(value)));
        }
    }

    fn text(&mut self, name: &str, offset: usize, len: usize) {
        if self.fits(offset, len) {
            let value = text(&self.block[offset..offset + len]);
            self.push(name, AttrValue::Text(value.into_boxed_str()));
        }
    }

    fn f32_array(&mut self, name: &str, offset: usize, count: usize) {
        if self.fits(offset, count * 4) {
            let values = (0..count)
                .map(|index| self.endian.f32(self.block, offset + index * 4))
                .collect();
            self.push(name, AttrValue::Array(ArrayBuf::F32(values)));
        }
    }

    fn i16_array(&mut self, name: &str, offset: usize, count: usize) {
        if self.fits(offset, count * 2) {
            let values = (0..count)
                .map(|index| self.endian.i16(self.block, offset + index * 2))
                .collect();
            self.push(name, AttrValue::Array(ArrayBuf::I16(values)));
        }
    }

    fn i32_array(&mut self, name: &str, offset: usize, count: usize) {
        if self.fits(offset, count * 4) {
            let values = (0..count)
                .map(|index| self.endian.i32(self.block, offset + index * 4))
                .collect();
            self.push(name, AttrValue::Array(ArrayBuf::I32(values)));
        }
    }

    fn finish(self) -> Attrs {
        self.out
    }
}

/// Text with NUL padding and surrounding blanks removed.
pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

/// SSWB (super sweep identification block). The 196-byte block written by
/// most producers packs `d_start_time` right after `radar_name`; the
/// 200-byte block of Radx aligns it to 8 bytes.
pub(crate) fn sswb(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_sswb_");
    r.i32("last_used", 8);
    r.i32("start_time", 12);
    r.i32("stop_time", 16);
    r.i32("sizeof_file", 20);
    r.i32("compression_flag", 24);
    r.i32("volume_time_stamp", 28);
    r.i32("num_params", 32);
    r.text("radar_name", 36, 8);
    let base = if block.len() >= 200 { 48 } else { 44 };
    r.f64("d_start_time", base);
    r.f64("d_stop_time", base + 8);
    r.i32("version_num", base + 16);
    r.i32("num_key_tables", base + 20);
    r.i32("status", base + 24);
    r.i32_array("place_holder", base + 28, 7);
    // Eight key tables of (offset, size, type).
    r.i32_array("key_table", base + 56, 24);
    r.finish()
}

/// VOLD (volume descriptor).
pub(crate) fn vold(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_vold_");
    r.i16("format_version", 8);
    r.i16("volume_num", 10);
    r.i32("maximum_bytes", 12);
    r.text("proj_name", 16, 20);
    r.i16("year", 36);
    r.i16("month", 38);
    r.i16("day", 40);
    r.i16("data_set_hour", 42);
    r.i16("data_set_minute", 44);
    r.i16("data_set_second", 46);
    r.text("flight_num", 48, 8);
    r.text("gen_facility", 56, 8);
    r.i16("gen_year", 64);
    r.i16("gen_month", 66);
    r.i16("gen_day", 68);
    r.i16("number_sensor_des", 70);
    r.finish()
}

/// RADD (radar descriptor): the 144-byte 1995 layout and the 300-byte
/// extension.
pub(crate) fn radd(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_radd_");
    r.text("radar_name", 8, 8);
    r.f32("radar_const_db", 16);
    r.f32("peak_power_kw", 20);
    r.f32("noise_power_dbm", 24);
    r.f32("receiver_gain_db", 28);
    r.f32("antenna_gain_db", 32);
    r.f32("system_gain_db", 36);
    r.f32("horz_beam_width_deg", 40);
    r.f32("vert_beam_width_deg", 44);
    r.i16("radar_type", 48);
    r.i16("scan_mode", 50);
    r.f32("req_rotat_vel_deg_per_s", 52);
    r.f32("scan_mode_pram0", 56);
    r.f32("scan_mode_pram1", 60);
    r.i16("num_parameter_des", 64);
    r.i16("total_num_des", 66);
    r.i16("data_compress", 68);
    r.i16("data_reduction", 70);
    r.f32("data_red_parm0", 72);
    r.f32("data_red_parm1", 76);
    r.f32("radar_longitude_deg", 80);
    r.f32("radar_latitude_deg", 84);
    r.f32("radar_altitude_km", 88);
    r.f32("eff_unamb_vel_mps", 92);
    r.f32("eff_unamb_range_km", 96);
    r.i16("num_freq_trans", 100);
    r.i16("num_ipps_trans", 102);
    r.f32_array("freq_ghz", 104, 5);
    r.f32_array("interpulse_per_ms", 124, 5);
    r.i32("extension_num", 144);
    r.text("config_name", 148, 8);
    r.i32("config_num", 156);
    r.f32("aperture_size_cm", 160);
    r.f32("field_of_view", 164);
    r.f32("aperture_eff_percent", 168);
    r.f32_array("aux_freq_ghz", 172, 11);
    r.f32_array("aux_ipp_ms", 216, 11);
    r.f32("pulse_width_us", 260);
    r.f32("primary_cop_baseln", 264);
    r.f32("secondary_cop_baseln", 268);
    r.f32("pc_xmtr_bandwidth", 272);
    r.i32("pc_waveform_type", 276);
    r.text("site_name", 280, 20);
    r.finish()
}

/// CFAC (correction factor descriptor): sixteen corrections, which the
/// decoder applies to the ray coordinates, range and site.
pub(crate) fn cfac(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_cfac_");
    for (index, name) in [
        "azimuth_corr_deg",
        "elevation_corr_deg",
        "range_delay_corr_m",
        "longitude_corr_deg",
        "latitude_corr_deg",
        "pressure_alt_corr_km",
        "radar_alt_corr_km",
        "ew_gndspd_corr_mps",
        "ns_gndspd_corr_mps",
        "vert_vel_corr_mps",
        "heading_corr_deg",
        "roll_corr_deg",
        "pitch_corr_deg",
        "drift_corr_deg",
        "rot_angle_corr_deg",
        "tilt_corr_deg",
    ]
    .into_iter()
    .enumerate()
    {
        r.f32(name, 8 + index * 4);
    }
    r.finish()
}

/// CSFD (cell spacing descriptor, floating point).
pub(crate) fn csfd(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_csfd_");
    r.i32("num_segments", 8);
    r.f32("dist_to_first_m", 12);
    r.f32_array("spacing_m", 16, 8);
    r.i16_array("num_cells", 48, 8);
    r.finish()
}

/// CELV (cell range vector) header; the distances become a variable.
pub(crate) fn celv(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_celv_");
    r.i32("number_cells", 8);
    r.finish()
}

/// SWIB (sweep information block).
pub(crate) fn swib(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_swib_");
    r.text("radar_name", 8, 8);
    r.i32("sweep_num", 16);
    r.i32("num_rays", 20);
    r.f32("start_angle_deg", 24);
    r.f32("stop_angle_deg", 28);
    r.f32("fixed_angle_deg", 32);
    r.i32("filter_flag", 36);
    r.finish()
}

/// COMM (comment block). The first COMM of a sweepfile is
/// `dorade_comm_comment`, the `index`-th (0-based) after it
/// `dorade_comm_comment_<index>`, so no comment replaces another.
pub(crate) fn comm(endian: Endian, block: &[u8], index: usize) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_comm_");
    let len = block.len().saturating_sub(8);
    let name = if index == 0 {
        "comment".to_owned()
    } else {
        format!("comment_{index}")
    };
    r.text(&name, 8, len);
    r.finish()
}

/// SEDS (Solo II edit summary: the editor commands run on the sweepfile,
/// as text). The first SEDS of a sweepfile is `dorade_seds_text`, the
/// `index`-th (0-based) after it `dorade_seds_text_<index>`. The text is
/// kept as stored, lines and trailing newlines included; only the NUL
/// padding at the end of the block is removed.
pub(crate) fn seds(block: &[u8], index: usize) -> Attrs {
    let body = block.get(8..).unwrap_or_default();
    let text = String::from_utf8_lossy(body);
    let name = if index == 0 {
        "dorade_seds_text".to_owned()
    } else {
        format!("dorade_seds_text_{index}")
    };
    vec![(
        name.into_boxed_str(),
        AttrValue::Text(text.trim_end_matches(char::from(0)).into()),
    )]
}

/// PARM (parameter descriptor) fields the field itself does not already
/// carry as its name, description, units and coding: the 104-byte layout
/// and the 216-byte extension.
pub(crate) fn parm(endian: Endian, block: &[u8]) -> Attrs {
    let mut r = Reader::new(endian, block, "dorade_parm_");
    r.i16("interpulse_time", 64);
    r.i16("xmitted_freq", 66);
    r.f32("recvr_bandwidth_mhz", 68);
    r.i16("pulse_width_m", 72);
    r.i16("polarization", 74);
    r.i16("num_samples", 76);
    r.i16("binary_format", 78);
    r.text("threshold_field", 80, 8);
    r.f32("threshold_value", 88);
    r.f32("parameter_scale", 92);
    r.f32("parameter_bias", 96);
    r.i32("bad_data", 100);
    r.i32("extension_num", 104);
    r.text("config_name", 108, 8);
    r.i32("config_num", 116);
    r.i32("offset_to_data", 120);
    r.f32("mks_conversion", 124);
    r.i32("num_qnames", 128);
    r.text("qdata_names", 132, 32);
    r.i32("num_criteria", 164);
    r.text("criteria_names", 168, 32);
    r.i32("number_cells", 200);
    r.f32("meters_to_first_cell", 204);
    r.f32("meters_between_cells", 208);
    r.f32("eff_unamb_vel_mps", 212);
    r.finish()
}
