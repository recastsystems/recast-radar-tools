//! Product table: code, mnemonic, name and kind for every product code in
//! `docs/level3/reference.md` section 8 (ICD 2620001AD Table III, legacy codes
//! from 2620001P/2620001H, TDWR codes from 2620063E; 181/183/185/187 from MetPy).
//!
//! Legacy codes that Table III of 2620001G/H lists as spare (39, 40, 42, 49,
//! 52, 53, 68-72, 83, 88, 106) come from the 1990s Table III that NCDC
//! reproduces in its Level III data documentation (DSI-7000, 11 April 2005,
//! "copied from the NWS Interface Control Document for RPG/Associated PUP
//! #2620001"): 39 and 40 Composite Reflectivity Contour, 42 Echo Tops
//! Contour, 49 Combined Moment, 52 Cross Section (Spectrum Width), 53 Weak
//! Echo Region, 68-72 Layer Composite Turbulence (layers 1-3, average and
//! maximum; there 67 is the layer 1 average, a code 2620001AD gives to the
//! AP-removed layer composite reflectivity), 83 Radar Coded Message
//! (Unedited) (`IRM`, the pre-edit message sent to the RPG operator), 88
//! Combined Shear Contour and 106 Site Adaptable Parameters for Combined
//! Shear Contour. The corpus products of 39, 42 and 53 (NCEI archive,
//! 1994-2001) agree with those names: 39 contours the composite reflectivity
//! (thresholds `20+` to `70+` dBZ, the composite reflectivity attribute
//! table on its graphic pages; KGRR 2001-10-11) and 42 the echo tops
//! (thresholds `25+` to `70+`; at KIND 1994-09-10 16:42 its contours run
//! along the matching level boundaries of the Echo Tops product 41 of the
//! same volume, not along those of VIL or composite reflectivity).
//! Mnemonics LTA and LTM (68-72) are 2620003AE's.

use ProductKind::{Generic, Graphic, Radial, Raster, Tabular, Text};

/// What a product's symbology carries (from its Table III format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProductKind {
    /// Radial image (packets 16 or 0xAF1F).
    Radial,
    /// Raster image (packets 0xBA07/0xBA0F, 17, 18).
    Raster,
    /// Generic data format (packets 28 or 29).
    Generic,
    /// Geographic or non-geographic alphanumeric: symbols, vectors, contours and text.
    Graphic,
    /// Tabular alphanumeric pages: stand-alone tabular products and alphanumeric blocks.
    Tabular,
    /// Free-form text messages (73 UAM, 74 RCM, 75 FTM, 77 PTM).
    Text,
}

/// Static description of one product code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductInfo {
    /// Product code (Product Description Block halfword 16).
    pub code: i16,
    /// ICD mnemonic, e.g. `DR`; empty when the ICD gives none.
    pub mnemonic: &'static str,
    /// Product name.
    pub name: &'static str,
    /// Kind of product.
    pub kind: ProductKind,
}

/// Looks up a product code.
pub fn product_info(code: i16) -> Option<&'static ProductInfo> {
    PRODUCTS
        .binary_search_by_key(&code, |p| p.code)
        .ok()
        .and_then(|i| PRODUCTS.get(i))
}

/// All known products, sorted by code.
pub fn products() -> &'static [ProductInfo] {
    &PRODUCTS
}

const fn p(
    code: i16,
    mnemonic: &'static str,
    name: &'static str,
    kind: ProductKind,
) -> ProductInfo {
    ProductInfo {
        code,
        mnemonic,
        name,
        kind,
    }
}

#[rustfmt::skip]
static PRODUCTS: [ProductInfo; 156] = [
    p(16, "R", "Base Reflectivity 0.54 nm x 1 deg, 124 nm, 8 levels", Radial),
    p(17, "R", "Base Reflectivity 1.1 nm x 1 deg, 248 nm, 8 levels", Radial),
    p(18, "R", "Base Reflectivity 2.2 nm x 1 deg, 248 nm, 8 levels", Radial),
    p(19, "R", "Base Reflectivity 0.54 nm x 1 deg, 124 nm, 16 levels", Radial),
    p(20, "R", "Base Reflectivity 1.1 nm x 1 deg, 248 nm, 16 levels", Radial),
    p(21, "R", "Base Reflectivity 2.2 nm x 2 deg, 248 nm, 16 levels", Radial),
    p(22, "V", "Base Velocity 0.13 nm x 1 deg, 32 nm, 8 levels", Radial),
    p(23, "V", "Base Velocity 0.27 nm x 1 deg, 62 nm, 8 levels", Radial),
    p(24, "V", "Base Velocity 0.54 nm x 1 deg, 124 nm, 8 levels", Radial),
    p(25, "V", "Base Velocity 0.13 nm x 1 deg, 32 nm, 16 levels", Radial),
    p(26, "V", "Base Velocity 0.27 nm x 1 deg, 62 nm, 16 levels", Radial),
    p(27, "V", "Base Velocity 0.54 nm x 1 deg, 124 nm, 16 levels", Radial),
    p(28, "SW", "Base Spectrum Width 0.13 nm x 1 deg, 32 nm, 8 levels", Radial),
    p(29, "SW", "Base Spectrum Width 0.27 nm x 1 deg, 62 nm, 8 levels", Radial),
    p(30, "SW", "Base Spectrum Width 0.54 nm x 1 deg, 124 nm, 8 levels", Radial),
    p(31, "USP", "User Selectable Storm Total Precipitation", Radial),
    p(32, "DHR", "Digital Hybrid Scan Reflectivity", Radial),
    p(33, "HSR", "Hybrid Scan Reflectivity", Radial),
    p(34, "", "Clutter Filter Control", Radial),
    p(35, "CR", "Composite Reflectivity 0.54 nm, 124 nm, 8 levels", Raster),
    p(36, "CR", "Composite Reflectivity 2.2 nm, 248 nm, 8 levels", Raster),
    p(37, "CR", "Composite Reflectivity 0.54 nm, 124 nm, 16 levels", Raster),
    p(38, "CR", "Composite Reflectivity 2.2 nm, 248 nm, 16 levels", Raster),
    p(39, "", "Composite Reflectivity Contour", Graphic),
    p(40, "", "Composite Reflectivity Contour", Graphic),
    p(41, "ET", "Echo Tops", Raster),
    p(42, "", "Echo Tops Contour", Graphic),
    p(43, "", "Severe Weather Analysis (Reflectivity)", Radial),
    p(44, "", "Severe Weather Analysis (Velocity)", Radial),
    p(45, "", "Severe Weather Analysis (Spectrum Width)", Radial),
    p(46, "", "Severe Weather Analysis (Shear)", Radial),
    p(47, "", "Severe Weather Probability", Graphic),
    p(48, "VWP", "VAD Wind Profile", Graphic),
    p(49, "", "Combined Moment", Raster),
    p(50, "RCS", "Cross Section (Reflectivity)", Raster),
    p(51, "VCS", "Cross Section (Velocity)", Raster),
    p(52, "", "Cross Section (Spectrum Width)", Raster),
    p(53, "", "Weak Echo Region", Raster),
    p(55, "SRR", "Storm Relative Mean Radial Velocity (Region)", Radial),
    p(56, "SRM", "Storm Relative Mean Radial Velocity (Map)", Radial),
    p(57, "VIL", "Vertically Integrated Liquid", Raster),
    p(58, "STI", "Storm Tracking Information", Graphic),
    p(59, "HI", "Hail Index", Graphic),
    p(60, "M", "Mesocyclone", Graphic),
    p(61, "TVS", "Tornado Vortex Signature", Graphic),
    p(62, "SS", "Storm Structure", Tabular),
    p(63, "LRA", "Layer Composite Reflectivity Layer 1 Average", Raster),
    p(64, "LRA", "Layer Composite Reflectivity Layer 2 Average", Raster),
    p(65, "LRM", "Layer Composite Reflectivity Layer 1 Maximum", Raster),
    p(66, "LRM", "Layer Composite Reflectivity Layer 2 Maximum", Raster),
    p(67, "APR", "Layer Composite Reflectivity - AP Removed", Raster),
    p(68, "LTA", "Layer Composite Turbulence Layer 2 Average", Raster),
    p(69, "LTA", "Layer Composite Turbulence Layer 3 Average", Raster),
    p(70, "LTM", "Layer Composite Turbulence Layer 1 Maximum", Raster),
    p(71, "LTM", "Layer Composite Turbulence Layer 2 Maximum", Raster),
    p(72, "LTM", "Layer Composite Turbulence Layer 3 Maximum", Raster),
    p(73, "UAM", "User Alert Message", Text),
    p(74, "RCM", "Radar Coded Message", Text),
    p(75, "FTM", "Free Text Message", Text),
    p(77, "PTM", "PUP Text Message", Text),
    p(78, "OHP", "Surface Rainfall Accumulation (1 hr)", Radial),
    p(79, "THP", "Surface Rainfall Accumulation (3 hr)", Radial),
    p(80, "STP", "Storm Total Rainfall Accumulation", Radial),
    p(81, "DPA", "Hourly Digital Precipitation Array", Raster),
    p(82, "SPD", "Supplemental Precipitation Data", Tabular),
    p(83, "IRM", "Radar Coded Message (Unedited)", Raster),
    p(84, "VAD", "Velocity Azimuth Display", Graphic),
    p(85, "RCS", "Cross Section Reflectivity (8 levels)", Raster),
    p(86, "VCS", "Cross Section Velocity (8 levels)", Raster),
    p(87, "CS", "Combined Shear", Raster),
    p(88, "", "Combined Shear Contour", Graphic),
    p(89, "LRA", "Layer Composite Reflectivity Layer 3 Average", Raster),
    p(90, "LRM", "Layer Composite Reflectivity Layer 3 Maximum", Raster),
    p(93, "DBV", "ITWS Digital Base Velocity", Radial),
    p(94, "DR", "Base Reflectivity Data Array", Radial),
    p(95, "CRE", "Composite Reflectivity Edited for AP 0.54 nm, 8 levels", Raster),
    p(96, "CRE", "Composite Reflectivity Edited for AP 2.2 nm, 8 levels", Raster),
    p(97, "CRE", "Composite Reflectivity Edited for AP 0.54 nm, 16 levels", Raster),
    p(98, "CRE", "Composite Reflectivity Edited for AP 2.2 nm, 16 levels", Raster),
    p(99, "DV", "Base Velocity Data Array", Radial),
    p(100, "", "Site Adaptable Parameters for VAD Wind Profile (product 48)", Tabular),
    p(101, "", "Storm Track Alphanumeric Block", Tabular),
    p(102, "", "Hail Index Alphanumeric Block", Tabular),
    p(103, "", "Mesocyclone Alphanumeric Block", Tabular),
    p(104, "", "TVS Alphanumeric Block", Tabular),
    p(105, "", "Site Adaptable Parameters for Combined Shear", Tabular),
    p(106, "", "Site Adaptable Parameters for Combined Shear Contour", Tabular),
    p(107, "", "Surface Rainfall (1 hr) Alphanumeric Block", Tabular),
    p(108, "", "Surface Rainfall (3 hr) Alphanumeric Block", Tabular),
    p(109, "", "Storm Total Rainfall Accumulation Alphanumeric Block", Tabular),
    p(110, "", "Clutter Likelihood Reflectivity Alphanumeric Block", Tabular),
    p(111, "", "Clutter Likelihood Doppler Alphanumeric Block", Tabular),
    p(113, "PRC", "Power Removed Control", Radial),
    p(132, "CLR", "Clutter Likelihood Reflectivity", Radial),
    p(133, "CLD", "Clutter Likelihood Doppler", Radial),
    p(134, "DVL", "High Resolution VIL", Radial),
    p(135, "EET", "Enhanced Echo Tops", Radial),
    p(136, "SO", "SuperOb", Graphic),
    p(137, "ULR", "User Selectable Layer Composite Reflectivity", Radial),
    p(138, "DSP", "Digital Storm Total Precipitation", Radial),
    p(139, "MRU", "Mesocyclone Rapid Update", Graphic),
    p(140, "GFM", "Gust Front MIGFA", Generic),
    p(141, "MD", "Mesocyclone Detection", Graphic),
    p(143, "TRU", "Tornado Vortex Signature Rapid Update", Graphic),
    p(144, "OSW", "One-hour Snow Water Equivalent", Radial),
    p(145, "OSD", "One-hour Snow Depth", Radial),
    p(146, "SSW", "Storm Total Snow Water Equivalent", Radial),
    p(147, "SSD", "Storm Total Snow Depth", Radial),
    p(149, "DMD", "Digital Mesocyclone Detection", Generic),
    p(150, "USW", "User Selectable Snow Water Equivalent", Radial),
    p(151, "USD", "User Selectable Snow Depth", Radial),
    p(152, "ASP", "Archive III Status Product", Generic),
    p(153, "SDR", "Super Resolution Reflectivity Data Array", Radial),
    p(154, "SDV", "Super Resolution Velocity Data Array", Radial),
    p(155, "SDW", "Super Resolution Spectrum Width Data Array", Radial),
    p(156, "", "Eddy Dissipation Rate", Radial),
    p(157, "", "Eddy Dissipation Rate Confidence", Radial),
    p(158, "", "Differential Reflectivity (16 levels)", Radial),
    p(159, "DZD", "Digital Differential Reflectivity", Radial),
    p(160, "", "Correlation Coefficient (16 levels)", Radial),
    p(161, "DCC", "Digital Correlation Coefficient", Radial),
    p(162, "", "Specific Differential Phase (16 levels)", Radial),
    p(163, "DKD", "Digital Specific Differential Phase", Radial),
    p(164, "", "Hydrometeor Classification (16 levels)", Radial),
    p(165, "DHC", "Digital Hydrometeor Classification", Radial),
    p(166, "ML", "Melting Layer", Graphic),
    p(167, "SDC", "Super Res Digital Correlation Coefficient", Radial),
    p(168, "SDP", "Super Res Digital Phi", Radial),
    p(169, "OHA", "One Hour Accumulation", Radial),
    p(170, "DAA", "Digital Accumulation Array", Radial),
    p(171, "STA", "Storm Total Accumulation", Radial),
    p(172, "DSA", "Digital Storm Total Accumulation", Radial),
    p(173, "DUA", "Digital User-Selectable Accumulation", Radial),
    p(174, "DOD", "Digital One-Hour Difference Accumulation", Radial),
    p(175, "DSD", "Digital Storm Total Difference Accumulation", Radial),
    p(176, "DPR", "Digital Instantaneous Precipitation Rate", Generic),
    p(177, "HHC", "Hybrid Hydrometeor Classification", Radial),
    p(178, "IHL", "Icing Hazard Levels", Generic),
    p(179, "HHL", "Hail Hazard Layers", Generic),
    p(180, "DR", "TDWR Base Reflectivity 0.08 nm x 1 deg, 48 nm", Radial),
    p(181, "", "TDWR Base Reflectivity (16 levels)", Radial),
    p(182, "DV", "TDWR Base Velocity 0.08 nm x 1 deg, 48 nm", Radial),
    p(183, "", "TDWR Base Velocity (16 levels)", Radial),
    p(184, "SW", "TDWR Base Spectrum Width 0.08 nm x 1 deg, 48 nm", Radial),
    p(185, "", "TDWR Base Spectrum Width (16 levels)", Radial),
    p(186, "DR", "TDWR Long Range Base Reflectivity 0.16 nm x 1 deg, 225 nm", Radial),
    p(187, "", "TDWR Long Range Base Reflectivity (16 levels)", Radial),
    p(189, "RQ", "Quasi-Vertical Profile Reflectivity", Raster),
    p(190, "CCQ", "Quasi-Vertical Profile Correlation Coefficient", Raster),
    p(191, "ZDQ", "Quasi-Vertical Profile Differential Reflectivity", Raster),
    p(192, "KDQ", "Quasi-Vertical Profile Specific Differential Phase", Raster),
    p(193, "SRQ", "Super Resolution Digital Reflectivity Data-Quality-Edited", Radial),
    p(195, "DRQ", "Digital Reflectivity, DQA-Edited Data Array", Radial),
    p(196, "MBA", "Microburst AMDA", Generic),
    p(197, "RRC", "Rain Rate Classification", Radial),
    p(202, "SCL", "Shift Change Checklist", Generic),
];
