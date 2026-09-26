//! The Hydrologic Rainfall Analysis Project (HRAP) grid and the national
//! radar grids of the same family: the polar stereographic grids of the
//! Hourly Digital Precipitation Array (product 81, packet 17, the "1/40 LFM"
//! grid of ICD 2620001 Table III), of its precipitation rate arrays (packet
//! 18, the "1/4 LFM" grid) and of the Radar Coded Message (product 74, the
//! "1/16 LFM" grid of 2620001P Appendix B).
//!
//! The projection is the NWS HRAP definition: a polar stereographic projection
//! of a sphere of radius 6371.2 km, true at 60N, with the grid's vertical axis
//! along 105W; the mesh length is 4.7625 km at 60N and the North Pole is at
//! grid coordinates (401, 1601). Grid coordinate `x` grows east of 105W and `y`
//! north. A 1/4 LFM box is 10 HRAP units and a 1/16 LFM box 2.5.
//!
//! **National grids** (not stated in the ICDs; derived from real products):
//! the boxes of all three grids have a corner at the North Pole, so box
//! edges lie at HRAP `x = 401 + k * size` and `y = 1601 + k * size` (integers
//! for HRAP boxes, `1 (mod 10)` for 1/4 LFM boxes). NWS's own description of
//! its national RCM reflectivity mosaic (Kitzmiller, Samplatsky and Keller
//! 2002, NOAA/TDL: 1/16 LFM boxes, 460 x 360, lower-left corner 119.036W
//! 23.097N, aligned with the 1/4 LFM and HRAP grids) agrees: its
//! corners fall on these edges, the lower-left at HRAP (1, 1). A product's local array
//! is a block of national boxes placed so that the box holding the radar is
//! at a fixed row and column ([`LocalGrid::national`]):
//!
//! | Array | Boxes | Size (HRAP) | Radar's box (row, column from 0) |
//! |---|---|---|---|
//! | DPA accumulation (packet 17) | 131 x 131 | 1 | (65, 65) |
//! | DPA rate array (packet 18) | 13 x 13 | 10 | (6, 6) |
//! | Radar Coded Message local grid | 25 x 25 (100 x 100 fine) | 10 (2.5) | (12, 12), box `MM` |
//!
//! Rows run from north to south and columns from west to east. The evidence,
//! from `tools/level3_dpa_golden.py` and `tools/level3_lfm_golden.py`
//! (`testdata/level3/golden-dpa.json`, `golden-lfm.json`):
//!
//! - DPA accumulation: the "outside coverage" level (255) of real products
//!   from 47 sites against the boxes beyond 230 km: among west and north
//!   offsets in quarter boxes this placement puts the fewest boxes on the
//!   wrong side of the coverage circle at 46 sites, and at KPDT it is 2
//!   boxes (of about 6800 outside) behind a quarter-box shift north.
//! - Radar Coded Message: six volumes at radars from 71W to 107W (KTLX
//!   2013 and 2022, KBOX, KLWX, KMLB and KGGW 2022). Of the 72 storm
//!   centroids and tornado vortex signatures the messages name, placed on
//!   this grid from the STI and TVS products of the same volume, 68 lie in
//!   the named fine box and 4 within 0.32 km of it; of the 1600 offsets of
//!   the grid in quarter HRAP units over one period, only this one keeps
//!   every feature of the six volumes within 0.5 km of its box. The
//!   intensity groups agree best with the Digital Hybrid Scan Reflectivity
//!   of the same volume under this alignment at five radars, and at KBOX
//!   3 boxes of 394 behind a quarter-unit shift west. Appendix B puts the
//!   radar in box `NM` (row 13); all six messages put it in `MM` (row 12), and
//!   letters the fine boxes down the columns.
//! - DPA rate arrays: their "ND" level (7) against the boxes wholly beyond
//!   230 km: 55 mismatched boxes of 4732 in 28 real arrays from 26 sites
//!   under this placement, 268 under the earlier placement from the corner
//!   of the 131 x 131 array.

/// Earth radius of the HRAP sphere, metres.
pub const EARTH_RADIUS_M: f64 = 6_371_200.0;
/// HRAP mesh length at 60N, metres.
pub const MESH_M: f64 = 4_762.5;
/// Latitude of true scale, degrees north.
pub const TRUE_LATITUDE_DEG: f64 = 60.0;
/// Longitude of the grid's vertical axis, degrees east.
pub const VERTICAL_LONGITUDE_DEG: f64 = -105.0;
/// HRAP grid coordinates of the North Pole.
pub const POLE: (f64, f64) = (401.0, 1601.0);

/// `R (1 + sin 60)`: the projection's scale constant, metres.
fn scale_m() -> f64 {
    EARTH_RADIUS_M * (1.0 + TRUE_LATITUDE_DEG.to_radians().sin())
}

/// Polar stereographic coordinates in metres (`x` east along the grid, `y`
/// north, origin at the pole) of a latitude and longitude in degrees.
pub fn project(latitude_deg: f64, longitude_deg: f64) -> (f64, f64) {
    let lat = latitude_deg.to_radians();
    let dlon = (longitude_deg - VERTICAL_LONGITUDE_DEG).to_radians();
    let rho = scale_m() * (std::f64::consts::FRAC_PI_4 - lat / 2.0).tan();
    (rho * dlon.sin(), -rho * dlon.cos())
}

/// Latitude and longitude in degrees of polar stereographic coordinates in
/// metres (the inverse of [`project`]).
pub fn unproject(x_m: f64, y_m: f64) -> (f64, f64) {
    let rho = x_m.hypot(y_m);
    let lat = std::f64::consts::FRAC_PI_2 - 2.0 * (rho / scale_m()).atan();
    let lon = VERTICAL_LONGITUDE_DEG + x_m.atan2(-y_m).to_degrees();
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    (lat.to_degrees(), lon)
}

/// HRAP grid coordinates of a latitude and longitude.
pub fn to_grid(latitude_deg: f64, longitude_deg: f64) -> (f64, f64) {
    let (x, y) = project(latitude_deg, longitude_deg);
    (x / MESH_M + POLE.0, y / MESH_M + POLE.1)
}

/// Polar stereographic metres of HRAP grid coordinates.
pub fn grid_to_metres(x: f64, y: f64) -> (f64, f64) {
    ((x - POLE.0) * MESH_M, (y - POLE.1) * MESH_M)
}

/// HRAP grid coordinate of the pole's `x`, where every national grid has a
/// box corner.
const POLE_X: f64 = POLE.0;
/// HRAP grid coordinate of the pole's `y`.
const POLE_Y: f64 = POLE.1;

/// Size of a 1/4 LFM box in HRAP units.
pub const QUARTER_LFM: f64 = 10.0;
/// Size of a 1/16 LFM box in HRAP units.
pub const SIXTEENTH_LFM: f64 = 2.5;

/// A local array of national grid boxes: boxes of `box_size` HRAP units
/// whose north-west corner is at HRAP (`west`, `north`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalGrid {
    /// HRAP `x` of the west edge.
    pub west: f64,
    /// HRAP `y` of the north edge.
    pub north: f64,
    /// Box size in HRAP units (1 for the 131 x 131 DPA, 10 for the rate
    /// arrays, 2.5 for the Radar Coded Message's fine grid).
    pub box_size: f64,
}

impl LocalGrid {
    /// The local array of national boxes of `box_size` HRAP units in which the
    /// box holding the radar at `latitude_deg`, `longitude_deg` is at
    /// (`radar_row`, `radar_column`) (see the module documentation).
    pub fn national(
        latitude_deg: f64,
        longitude_deg: f64,
        box_size: f64,
        radar_row: u32,
        radar_column: u32,
    ) -> Self {
        let (hx, hy) = to_grid(latitude_deg, longitude_deg);
        let radar_west = POLE_X + box_size * ((hx - POLE_X) / box_size).floor();
        let radar_north = POLE_Y + box_size * (((hy - POLE_Y) / box_size).floor() + 1.0);
        Self {
            west: radar_west - f64::from(radar_column) * box_size,
            north: radar_north + f64::from(radar_row) * box_size,
            box_size,
        }
    }

    /// The 131 x 131 HRAP array of a DPA product (packet 17) for a radar at
    /// `latitude_deg`, `longitude_deg`: west edge `floor(hx) - 65`, north
    /// edge `floor(hy) + 66`.
    pub fn dpa(latitude_deg: f64, longitude_deg: f64) -> Self {
        Self::national(latitude_deg, longitude_deg, 1.0, 65, 65)
    }

    /// The 13 x 13 1/4 LFM precipitation rate array (packet 18 of products
    /// 81 and 82) for a radar at `latitude_deg`, `longitude_deg`.
    pub fn rate_array(latitude_deg: f64, longitude_deg: f64) -> Self {
        Self::national(latitude_deg, longitude_deg, QUARTER_LFM, 6, 6)
    }

    /// The 100 x 100 fine (1/16 LFM) grid of a Radar Coded Message (product
    /// 74) for a radar at `latitude_deg`, `longitude_deg`: the 25 x 25 1/4
    /// LFM local grid with the radar's box at row 12, column 12, each box
    /// split into 4 x 4 fine boxes.
    pub fn radar_coded_message(latitude_deg: f64, longitude_deg: f64) -> Self {
        Self {
            box_size: SIXTEENTH_LFM,
            ..Self::national(latitude_deg, longitude_deg, QUARTER_LFM, 12, 12)
        }
    }

    /// HRAP grid coordinates of the centre of box (`row`, `column`).
    pub fn box_centre(&self, row: usize, column: usize) -> (f64, f64) {
        (
            self.west + (column as f64 + 0.5) * self.box_size,
            self.north - (row as f64 + 0.5) * self.box_size,
        )
    }

    /// Latitude and longitude in degrees of the centre of box (`row`,
    /// `column`).
    pub fn box_centre_lat_lon(&self, row: usize, column: usize) -> (f64, f64) {
        let (x, y) = self.box_centre(row, column);
        let (xm, ym) = grid_to_metres(x, y);
        unproject(xm, ym)
    }
}
