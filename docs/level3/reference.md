# NEXRAD / TDWR Level III reference

Decoder reference for `recast-radar-io-level3`, distilled from the WSR-88D Radar
Operations Center interface control documents and checked against the real
products in `testdata/level3/manifest.toml`. Section, table and figure numbers
refer to ICD 2620001AD unless another document is named.

Where this file says **observed**, the statement comes from the corpus files, not
from an ICD.

## 1. Sources

| Document | Title | Revision / build | Date | SHA-256 of the PDF used |
|---|---|---|---|---|
| 2620001AD | ICD for the RPG to Class 1 User | AD, RPG Build 24.0 (latest released) | 19 Aug 2025 | `89d64ae5bee6524eb7cedade86d0977532889e305d82cc13a06a0bf74313a8a9` |
| 2620001P | ICD for the RPG to Class 1 User | P, RPG Build 12.0 (legacy product codes) | 24 May 2010 | `fe5629485eb3a25063bd0c6c87cc022caa8f57ef2b3dfb8a43d86e50f5f49527` |
| 2620001H | ICD for the RPG to Class 1 User | H, Open Build 6.0 (products 43-47, 87) | 29 Jul 2004 | `20b6087ad57730cd547fcfb68cb589976657ad2ef62eb7e03a49812f36e8311b` |
| 2620003AE | ICD for Product Specification | AE, Build 24.0 (mnemonics, per-product sections) | 19 Aug 2025 | `9a90b8c728d6bd7ce8b0f10969ed128ee4e1f2a6b21fa0d7619522bbbecda9c9` |
| 2620063E | ICD for SPG to AWIPS Class 1 User | E, SPG Build 12.0 (TDWR products) | 14 Jun 2022 | `5f901097f2e57624e2cda34110808b3c6444c034b31c9f1b626c34023eee29d1` |
| 2620070C | ICD for the TDWR SPG Product Specification | C, SPG Build 11.0 | 07 Jul 2021 | `9223dd5f5fe17b5c7ff2905054bf2758390b825dcc6fa4d44e5a98367fe626bf` |

All of them are published at <https://www.roc.noaa.gov/interface-control-documents.php>
(`https://www.roc.noaa.gov/public-documents/icds/<number><rev>.pdf`). Product
mnemonics come from the 2620003AE section titles and the product-size tables in
Appendix C of 2620001AD and 2620001P. Products 181/183/185/187 (16-level TDWR
products) are not in any ICD revision obtained; they are described from MetPy
1.7.1 (`metpy.io.nexrad.Level3File.prod_spec_map`) and the corpus.

## 2. Conventions

- All binary fields are big-endian. `INT*2`/`INT*4` are two's-complement
  signed; offsets, lengths and counts are treated as unsigned.
- A **halfword** is 16 bits. ICD halfword `n` of a message starts at byte
  `2 * (n - 1)` of the Message Header Block (halfword 1 = message code).
- **Dates** are modified Julian days with 1 = 1 January 1970 (Figure 3-3):
  `date = 1970-01-01 + (days - 1)`. Times are seconds after midnight UTC
  (halfword pairs) or minutes after midnight (product-dependent fields).
- **Offsets** in the Product Description Block (halfwords 55-60) are counted in
  halfwords from the start of the Message Header Block (section 3.3.1.1).
- **Coordinates** (section 3.3.3): symbology-block packets use 1/4 km units with
  the radar at the origin, I to the east and J to the north; graphic
  alphanumeric packets use screen pixels (section 3.3.1.3); packet 21 uses
  1/8 km (Figure 3-15).
- Angles are scaled integers in 0.1 degree.

## 3. Framing

A Level III file is one product message, optionally wrapped. The wrapping is
not defined by 2620001; it is **observed** in the corpus:

1. **NOAAPort envelope** (optional): `\x01\r\r\n`, a 3-5 digit sequence number
   and `" \r\r\n"` or `"\r\r\n"`; the file then ends with `\r\r\n\x03` (ETX).
2. **WMO abbreviated heading**: `T1T2A1A2ii CCCC YYGGgg\r\r\n`, e.g.
   `SDUS54 KOUN 202016`. `NOUS` headings are pure text (free text message,
   no binary message follows).
3. **AWIPS identifier** line: product category (3) + site (3), e.g.
   `N0RTLX\r\r\n`.
4. **zlib stream** (optional, NOAAPort distribution): one or more concatenated
   zlib frames (each starts `0x78`, e.g. `78 DA`), frames of 4000 bytes
   uncompressed. The decompressed data starts with a NOAAPort communications
   control block (first byte `0x40`, second byte = length in halfwords, 24
   bytes observed), then repeats the WMO heading and AWIPS line, then the
   message.
5. **Message** (section 3.3.1): Message Header Block, Product Description
   Block, then the blocks below. Message length (halfwords 5-6) counts the
   message only.

The General Status Message (message code 2, Figure 3-17) uses the same
Message Header Block followed by a general status block (`-1`, block length
82 or 178 bytes, mode of operation, RDA status, VCP, elevation angles x10,
...), and has no Product Description Block.

### 3.1 Message Header Block (Figure 3-3)

| Halfword | Byte | Type | Field |
|---|---|---|---|
| 1 | 0 | INT*2 | Message code (Table II; product code for products, negative for annotations) |
| 2 | 2 | INT*2 | Date of message (modified Julian) |
| 3-4 | 4 | INT*4 | Time of message, seconds after midnight UTC |
| 5-6 | 8 | INT*4 | Length of message in bytes including this header |
| 7 | 12 | INT*2 | Source ID |
| 8 | 14 | INT*2 | Destination ID |
| 9 | 16 | INT*2 | Number of blocks (header + product description + data blocks) |

**Observed:** archived 1990s products (NCEI) carry date 0 and time 0 in this
block; decoders must accept them.

### 3.2 Product Description Block (Figure 3-6 sheets 2, 6 and 7)

| Halfword | Byte | Type | Field |
|---|---|---|---|
| 10 | 18 | INT*2 | Block divider, -1 |
| 11-12 | 20 | INT*4 | Radar latitude, 0.001 degree |
| 13-14 | 24 | INT*4 | Radar longitude, 0.001 degree |
| 15 | 28 | INT*2 | Radar height, feet MSL |
| 16 | 30 | INT*2 | Product code (Table III) |
| 17 | 32 | INT*2 | Operational mode: 0 maintenance, 1 clear air, 2 precipitation |
| 18 | 34 | INT*2 | Volume coverage pattern |
| 19 | 36 | INT*2 | Sequence number (-13 for alert-generated products) |
| 20 | 38 | INT*2 | Volume scan number (1-80) |
| 21 | 40 | INT*2 | Volume scan date (for SAILS products: elevation start date, Note 5) |
| 22-23 | 42 | INT*4 | Volume scan start time, seconds |
| 24 | 46 | INT*2 | Generation date (134/135: end of volume date, Note 4) |
| 25-26 | 48 | INT*4 | Generation time, seconds |
| 27 | 52 | INT*2 | Product dependent P1 (Table V) |
| 28 | 54 | INT*2 | Product dependent P2 (Table V) |
| 29 | 56 | INT*2 | Elevation number (0 for volume products) |
| 30 | 58 | INT*2 | Product dependent P3 (Table V) |
| 31-46 | 60 | INT*2 x 16 | Data level thresholds 1-16, or product dependent (section 5) |
| 47-53 | 92 | INT*2 x 7 | Product dependent P4-P10 (Table V) |
| 54 | 106 | INT*1 + INT*1 | Version (high byte, Note 2), spot blank (low byte) |
| 55-56 | 108 | INT*4 | Offset to symbology block (halfwords) |
| 57-58 | 112 | INT*4 | Offset to graphic alphanumeric block (halfwords) |
| 59-60 | 116 | INT*4 | Offset to tabular alphanumeric block (halfwords) |

The block is 102 bytes; the first byte after it is at offset 120.

### 3.3 Compression (Figure 3-6 sheet 7 Note 3, Appendix D)

For products that define it, halfword 51 is the compression method (0 none,
1 bzip2) and halfwords 52-53 the size in bytes of the uncompressed data that
follows the Product Description Block. When compressed, everything after byte
120 is one bzip2 stream; the decompressed bytes replace it and the block offsets
refer to the decompressed message. Products under 1000 bytes are not compressed.
Halfwords 51-53 have other meanings for products that do not compress
(e.g. calibration constant for 16-21 and 37), so the check is per product;
**observed**: every compressed corpus file starts `BZh` at byte 120.

## 4. Blocks

### 4.1 Product Symbology Block (section 3.3.1.2, Figure 3-6 sheets 3 and 8)

| Bytes | Type | Field |
|---|---|---|
| 0 | INT*2 | Divider, -1 |
| 2 | INT*2 | Block ID, 1 |
| 4 | INT*4 | Block length in bytes, including divider and block ID |
| 8 | INT*2 | Number of layers (1-18) |
| 10 | per layer | Divider -1 (INT*2), layer length (INT*4, excludes divider and length), display packets |

### 4.2 Graphic Alphanumeric Block (section 3.3.1.3, Figure 3-6 sheets 4 and 9)

| Bytes | Type | Field |
|---|---|---|
| 0 | INT*2 | Divider, -1 |
| 2 | INT*2 | Block ID, 2 |
| 4 | INT*4 | Block length |
| 8 | INT*2 | Number of pages (1-48) |
| 10 | per page | Page number (INT*2), page length in bytes (INT*2), packets (8 text and 10 unlinked vectors, screen coordinates) |

Produced for products 31, 37, 38, 97, 58, 59, 61, 141, 143 (section 3.3.1.3).
**Observed** in the corpus for 36, 37, 38, 58, 59, 60 (legacy), 61 and 141,
including the TDWR SPG versions of 37, 58, 59, 61 and 141.

### 4.3 Tabular Alphanumeric Block (section 3.3.1.4, Figure 3-6 sheets 5 and 10)

| Bytes | Type | Field |
|---|---|---|
| 0 | INT*2 | Divider, -1 |
| 2 | INT*2 | Block ID, 3 |
| 4 | INT*4 | Block length |
| 8 | 18 bytes | Second Message Header Block; message code = alphanumeric code (48->100, 58->101, 59->102, 61->104, 78->107, 79->108, 80->109, 132->110, 133->111, 141/143/172 unchanged) |
| 26 | 102 bytes | Second Product Description Block |
| 128 | pages | Divider -1, number of pages (INT*2), then per page: lines of `INT*2 count` + `count` ASCII bytes (MSB set = special symbol), ending with `-1` |

Maximum 17 lines per page, 80 characters per line. **Observed** second-header
message codes: 48->100, 58->101, 59->102, 60->103 (legacy), 61->104, 78->107,
79->108, 80->109, 141->141, 172->172, and 0 for product 171 (KTLX 2013).

### 4.4 Stand-alone tabular alphanumeric products (section 3.3.2, Figure 3-16)

Products 62 (SS), 75 (FTM), 77 (PTM) and 82 (SPD) carry no symbology block;
the "offset to symbology" points at the page block (divider -1, number of
pages, pages as in 4.3). **Observed** variants:

- Alphanumeric message codes 100-111 distributed on their own (1999 corpus
  file with code 102) use the same stand-alone layout.
- Product 62: the "offset to graphic" points at cell trend data (packets 22
  then 21, running to the end of the message) and is one halfword too large: the
  packet code 22 is at `2 * (offset - 1)`.
- Product 82 version 0 (1995 `SUP`) is a symbology-block product (layers with
  packets 18 and 1), not a page block. Distinguish by the bytes at the offset:
  divider -1, block ID 1, plausible length, layer count 1-18 and a layer divider
  -1 at +10 mean a symbology block.
- Product 74 (Radar Coded Message): the offset points at ASCII text starting
  `1234 ROBUU` with sections `/NEXRAA`, `/NEXRBB`, `/NEXRCC` (legacy 2620001
  Appendix B, no longer in the ICD).
- `NOUS` free text messages (FTM) may be plain text with no binary message.

## 5. Data level encodings (Figure 3-6 sheet 6 Note 1)

Key used in the product table (section 8). `N` is the data level (byte or run
color), `hw(n)` the unsigned halfword.

- **T16** (all products not listed below): halfword `31 + N` describes level
  `N` (0-15). If bit 15 (MSB) is set, the low byte is a code: 0 BLANK, 1 TH,
  2 ND, 3 RF, 4 BI, 5 GC, 6 IC, 7 GR, 8 WS, 9 DS, 10 RA, 11 HR, 12 BD, 13 HA,
  14 UK, 15 LH, 16 GH. Otherwise the low byte is a value; high-byte bit 14
  (ICD "bit 1") divides it by 100, bit 13 ("bit 2") by 20, bit 12 ("bit 3") by
  10; bit 11 means ">", bit 10 "<", bit 9 "+", bit 8 "-" (negate). Example: `0x8401`
  = code TH with "<" set.
- **L256-dBZ** (32, 94, 153, 193, 195; TDWR 180, 186 per 2620063E): level 0
  below threshold, 1 missing (193: 2 edit/remove, 254 chaff); `hw31/10` minimum
  dBZ, `hw32/10` increment, `hw33` number of levels; level `N >= 2` =
  `hw31/10 + (N - 2) * hw32/10`.
- **L256-vel** (93, 99, 154; TDWR 182): level 0 below threshold, 1 range
  folded; same scale in m/s. (2620063E lists TDWR 184 with 256 levels but its
  sheet 6 note describes only 180, 182 and 186.)
- **L256-sw** (155): 0 below threshold, 1 range folded, levels 129-152 =
  `hw31/10 + (N - 129) * hw32/10` m/s.
- **DPA** (81, packet 17): 0 no accumulation, 255 outside coverage; level
  `1..254` = `hw31/10 + (N - 1) * hw32/1000` dBA.
- **L256-DSP** (138): 0 no accumulation; `hw31` minimum (0), `hw32` increment
  in 0.01 inch, `hw33` levels; level 1 is the first non-zero accumulation.
  **MetPy 1.7.1 maps level 1 to missing and level `N >= 2` to
  `(N - 2) * hw32/100`**, which disagrees with the ICD text; the golden
  `physical` values for product 138 follow MetPy.
- **HRVIL** (134): 0 below threshold, 1 flagged, 255 reserved. `hw31` linear
  scale, `hw32` linear offset, `hw34` log scale, `hw35` log offset are 16-bit
  floats (sign 1 bit, exponent 5 bits, fraction 10 bits: `E = 0` ->
  `(-1)^S * 2 * F/1024`, else `(-1)^S * 2^(E-16) * (1 + F/1024)`); `hw33` log
  start. `N < hw33`: VIL = `(N - offset_lin) / scale_lin`; otherwise
  VIL = `exp((N - offset_log) / scale_log)` kg/m2.
- **HREET** (135): 0 below threshold, 1 bad; `hw31` data mask (0x7F), `hw32`
  scale, `hw33` offset, `hw34` topped mask (0x80); kft =
  `(N & mask) / scale - offset`, topped = `N & topped_mask != 0`.
- **GEN** (159, 161, 163, 167, 168, 170, 172-176, 189-192): `hw31-32` scale
  and `hw33-34` offset as IEEE-754 `REAL*4`; `hw36` maximum data value,
  `hw37` leading flags, `hw38` trailing flags; value = `(N - offset) / scale`
  for `leading <= N <= max - trailing`; with 2 leading flags, 0 = below
  threshold and 1 = range folded; 170/172-175 have 1 leading flag (0 no data).
  176 (DPR) values are 16-bit (maximum 65535, no flags).
- **CAT-HC** (165, 177): class = `N / 10`: 0 ND, 10 BI, 20 GC, 30 IC, 40 DS,
  50 WS, 60 RA, 70 HR, 80 BD, 90 GR, 100 HA, 110 LH, 120 GH (165 version 1),
  140 UK, 150 RF.
- **CAT-RRC** (197): 0 NP, 10 UF, 20 CZ, 30 TZ, 40 SA, 50 KL, 60 KH, 70 Z1,
  80 Z6, 90 Z8, 100 SI.
- **EDR** (156/157, legacy, per MetPy): `hw31/1000` scale, `hw32/1000` offset,
  `hw33` levels, `hw34` leading flags; value = scale * N + offset.
- **n/a**: graphic, alphanumeric and contour products.

Product versions (sheet 7 Note 2): 32 v2, 58/59/61/141 v1, 67 v1, 78-80 v1,
81 v2, 82 v1, 134 v1, 138 v2, 149 v1, 165 v1 (adds LH/GH), 172 v2
(**observed** v3 in 2026 files).

## 6. Display data packets

Every packet starts with a 16-bit packet code. "len" is an unsigned 16-bit byte
count of what follows the length field unless stated. Coordinates are INT*2.

### 6.1 Image packets

**16 Digital Radial Data Array** (Figure 3-11c)

| Bytes | Field |
|---|---|
| 0 | code 16 |
| 2 | index of first range bin |
| 4 | number of range bins (up to 1840) |
| 6, 8 | I, J center of sweep (1/4 km) |
| 10 | range scale factor x0.001 (cosine of elevation; 1.0 for volume products) |
| 12 | number of radials (up to 720) |
| 14 | per radial: bytes in radial (INT*2), start angle x10, delta angle x10, then one level byte per bin, padded to a halfword (bytes may be bins + 1) |

**0xAF1F Radial Data Packet, 16 levels** (Figure 3-10): same 14-byte header
(scale factor = pixels per range bin x0.001); per radial: number of RLE
halfwords, start angle x10, delta angle x10, then `2 * n` bytes of runs, each
byte `run << 4 | level` (4-bit run 0-15, 4-bit level). **Observed:** in every
corpus file the runs of each radial sum to exactly the number of range bins,
and packet 16 radials with an odd bin count carry one pad byte.

**0xBA07 / 0xBA0F Raster Data Packet** (Figure 3-11)

| Bytes | Field |
|---|---|
| 0 | code 0xBA07 or 0xBA0F |
| 2, 4 | op flags 0x8000, 0x00C0 |
| 6, 8 | I, J start (1/4 km) |
| 10, 12 | X scale integer, X scale fraction (reserved) |
| 14, 16 | Y scale integer, Y scale fraction (reserved) |
| 18 | number of rows (up to 464) |
| 20 | packing descriptor, 2 |
| 22 | per row: bytes in row (INT*2), bytes `run << 4 \| level` |

**17 Digital Precipitation Data Array** (Figure 3-11a): code, two spare
halfwords, number of LFM boxes per row (131), number of rows (131); per row:
bytes in row, then pairs of bytes (8-bit run, 8-bit level).

**18 Precipitation Rate Data Array** (Figure 3-11b): code, two spares, boxes
per row (13), rows (13); per row: bytes in row, bytes `run << 4 | level`.

**33 Digital Raster Data Array** (Figure 3-11d): code, I start, J start (pixels),
I scale, J scale, number of cells per row, number of rows; per row: bytes in
row, one level byte per cell.

**28 / 29 Generic Data Packet** (Figure 3-15c, Appendix E): code, reserved
halfword (0), INT*4 length, then that many bytes of XDR (RFC 1832) data. 28
starts with the Product Description structure (Figure E-1), 29 with the
External Data Description (Figure E-1b). **Observed** (and implemented by
MetPy's `Level3XDRParser`): XDR strings are a u32 length plus bytes padded to 4;
product description fields are name, description, code, type, generation time,
radar name, latitude, longitude, height (floats), volume time, elevation time,
elevation angle, volume number, operational mode, VCP, elevation number,
compression, size (all 4-byte XDR ints even where Figure E-1 says INT*2),
then a parameter list and a component list, each a count followed by an extra
4-byte pointer word before the first element and between elements. Radial
component (type 1, Figure E-3): description, bin size and range to first bin
(floats), parameters, radial count, then per radial azimuth, elevation, width
(floats), number of bins (int, not REAL*4), attributes string, int array.
Text component (type 4): parameters, string.

### 6.2 Text and vector packets

| Code | Figure | Layout after code |
|---|---|---|
| 1 Write Text (no value) | 3-8b sh 1, 4 | len, I, J, ASCII characters |
| 2 Write Special Symbols | 3-8b sh 3, 5 | len, I, J, symbol characters: `!` past storm cell, `"` current cell, `#` forecast cell, `$` past MDA, `%` forecast MDA (I, J at symbol center) |
| 8 Write Text (uniform value) | 3-8b sh 2 | len, color level (0-15), I, J, ASCII characters |
| 6 Linked Vector (no value) | 3-7 sh 1 | len, I start, J start, then (I, J) end points |
| 9 Linked Vector (uniform value) | 3-7 sh 2-3 | len, color level, I start, J start, (I, J) end points |
| 7 Unlinked Vector (no value) | 3-8 sh 1, 3 | len, then (I begin, J begin, I end, J end) per vector |
| 10 Unlinked Vector (uniform value) | 3-8 sh 2, 4 | len, color level, then 4 halfwords per vector |
| 0x0802 Set Color Level | 3-8a sh 1-2 | color value indicator 0x0002, contour level; 6 bytes total |
| 0x0E03 Linked Contour Vectors | 3-8a sh 1-2 | initial point indicator 0x8000, I start, J start, len (4 x vectors), (I, J) points |
| 0x3501 Unlinked Contour Vectors | 3-8a sh 1, 3 | len (8 x vectors), then (I begin, J begin, I end, J end) per vector |

### 6.3 Symbol packets

All have `code, len` then repeated fixed-size records (Figure 3-14 sheets 1-4,
3-12, 3-13).

| Code | Record | Bytes per record |
|---|---|---|
| 3 Mesocyclone / 11 3D Correlated Shear | I, J, radius (1/4 km; radius 0 = none) | 6 |
| 4 Wind Barb | color level (RMS, 1-5), X, Y, direction (deg, points into wind), speed (kt) | 10 |
| 5 Vector Arrow | I, J, direction (deg), arrow length (pixels), arrow head length (pixels) | 10 |
| 12 TVS / 26 ETVS | I, J | 4 |
| 13 Hail Positive (filled) / 14 Hail Probable | I, J | 4 |
| 15 Storm ID | I, J, two ASCII characters | 6 |
| 19 HDA Hail | I, J, probability of hail (%), probability of severe hail (%), max hail size (in); -999 = beyond range | 10 |
| 20 Point Feature | I, J, feature type, attribute (radius 1/4 km for types 1-4, 9-11) | 8 |
| 25 STI Circle | I, J, radius | 6 |
| 23 SCIT Past / 24 SCIT Forecast | nested packets (2, 6, 25) filling len | variable |

Point feature types (packet 20): 1 mesocyclone extrapolated, 3 mesocyclone
(persistent, new or increasing), 5 TVS extrapolated, 6 ETVS extrapolated, 7 TVS,
8 ETVS, 9 MDA strength rank >= 5 with base <= 1 km ARL or on the lowest
elevation, 10 MDA rank >= 5 with elevated base, 11 MDA rank < 5.

### 6.4 Cell trend packets (product 62)

**21 Cell Trend Data** (Figure 3-15): code, len, cell ID (2 ASCII), I, J
(1/8 km); then until len is consumed: trend code (INT*2: 1 cell top, 2 cell base,
3 max reflectivity height, 4 POH, 5 POSH, 6 cell VIL, 7 max reflectivity,
8 centroid height), number of volumes (byte), latest volume pointer (byte),
volume values (INT*2 each, circular list). Heights are in hundreds of feet;
values above 700 had 1000 added to flag top/base on the highest/lowest
elevation; -999 = unknown.

**22 Cell Trend Volume Scan Times** (Figure 3-15a): code, len, number of
volumes (byte), latest pointer (byte), times in minutes after midnight.

## 7. Products and packets in the corpus

Packet codes present in `testdata/level3/manifest.toml` (file counts): 1 (15),
2 (9), 3 (1), 4 (5), 6 (9), 8 (31), 10 (30), 11 (1), 12 (3), 13 (1), 14 (1),
15 (13), 16 (93), 17 (4), 18 (4), 19 (3), 20 (4), 21 (2), 22 (2), 23 (8),
24 (8), 25 (1), 28 (5), 0x0802 (7), 0x0E03 (7), 0xAF1F (43), 0xBA07 (26).

Packet codes from spec section 4.5 with **no real sample found**: 5, 7, 9, 26,
29, 0xBA0F, 0x3501 (also 33). Searched: the 67 AWS `unidata-nexrad-level3`
files selected below (one per product ID family), all MetPy test data, every
product in the NCEI archive tarballs KAMA 1994-05-09, KFWS 1995-05-17,
KTLX 1999-05-04 and KTLX 2008-05-10, and 1718 non-empty TVS products (NTV,
2021-2022, TLX/OKC/INX/FWS/LIX) for ETVS packet 26. These packets belong to
products that are not distributed publicly (e.g. 49, 84, 87, 88, 143) or to
detections not present in the samples.

Excluded from the corpus: the 1990s `IRM` product (message code 83, "Spare" in
every Table III revision) uses packet codes 30, 31 and 32, which no ICD revision
obtained defines.

The AWS bucket (`https://unidata-nexrad-level3.s3.amazonaws.com/`, keys
`SSS_PPP_YYYY_MM_DD_HH_MM_SS`, three-letter site without the K/P/T prefix) was
surveyed on 2026-09-16 across all 206 site prefixes. It holds 146 product IDs:
DAA DHR DOD DPA DPR DSD DSP DTA DU3 DU6 DVL EET HHC N0B N0C N0F N0G N0H N0K N0M
N0Q N0R N0S N0U N0V N0X N0Z N1B N1C N1F N1G N1H N1K N1M N1P N1Q N1S N1U N1X N2B
N2C N2F N2H N2K N2M N2Q N2S N2U N2X N3B N3C N3F N3H N3K N3M N3P N3Q N3S N3U N3X
NAB NAC NAF NAG NAH NAK NAM NAQ NAU NAX NBB NBC NBF NBH NBK NBM NBQ NBU NBX NC1
NC2 NC3 NC4 NC5 NCR NCZ NET NHI NHL NLA NLL NMD NML NRR NSS NST NSW NTP NTV NVL
NVW NXB NXC NXF NXG NXH NXK NXM NXQ NXU NXX NYB NYC NYF NYG NYH NYK NYM NYQ NYU
NYX NZB NZC NZF NZG NZH NZK NZM NZQ NZU NZX OHA PTA RCM RSL SPD TR0 TR1 TR2 TV0
TV1 TV2 TZ0 TZ1 TZ2 TZL. At KTLX these IDs have no keys after 2022: DOD DSD N0F
N0Q N0R N0U N0V N0Z N1F N1Q N1S N1U N2F N2Q N2S N3F N3P N3Q N3S NAF NAQ NAU NBF
NBQ NCZ NET NHI NHL NLA NML NSS NSW NTV PTA RCM RSL SPD. From the bucket the
corpus takes one file per product family (elevation letter 0) from KTLX and TOKC
at 2026-06-22 08:06Z (mesocyclone detections present); hourly products DPA, DSP,
N1P and NTP from 2026-06-29 17:36Z (no accumulation); DU6 from 12:06Z; IDs that
stop in 2022 from 2022-05-03 00:52Z (KTLX, TOKC, KGJX NYQ, KSHV NZQ) or the last
available time (N0R/N0V/N0Z 2022-09-08); plus PTA (2020), NC1 (PAKC 2021), TR0
(TJFK 2021), NLL (KRAX 2022) and RSL (TOKC 2022). MetPy's `staticdata/nids`
files (2011-2022) are all included, and legacy products come from the NCEI
tarballs (KFWS 1995-05-17, KTLX 1999-05-04 message 102).

## 8. Product table

"Packets" and "AWIPS IDs" list what the corpus contains (— = no corpus file).
"Source" is the Table III that defines the code (legacy codes from the newest
revision that still lists them). Product-dependent halfwords follow Table V of
the same document; "xN" means the stored integer is the value times N.

| Code | Mnemonic | Name | Table III format | Packets (corpus) | AWIPS IDs (corpus) | Data levels | Product-dependent halfwords (Table V) | Source |
|---:|---|---|---|---|---|---|---|---|
| 16 | R | Base Reflectivity 0.54 nm x 1 deg, 124 nm, 8 levels | Radial Image | — | — | T16 | hw30 elevation x10; hw47 max reflectivity dBZ; hw51-52 calibration constant (Real*4, dB) | 2620001P T.III |
| 17 | R | Base Reflectivity 1.1 nm x 1 deg, 248 nm, 8 levels | Radial Image | — | — | T16 | as 16 | 2620001P T.III |
| 18 | R | Base Reflectivity 2.2 nm x 1 deg, 248 nm, 8 levels | Radial Image | — | — | T16 | as 16 | 2620001P T.III |
| 19 | R | Base Reflectivity 0.54 nm x 1 deg, 124 nm, 16 levels | Radial Image | 0xaf1f | N0R | T16 | as 16; hw50 delta time/supplemental scan (later builds) | 2620001P T.III |
| 20 | R | Base Reflectivity 1.1 nm x 1 deg, 248 nm, 16 levels | Radial Image | 0xaf1f | N0Z | T16 | as 19 | 2620001P T.III |
| 21 | R | Base Reflectivity 2.2 nm x 2 deg, 248 nm, 16 levels | Radial Image | — | — | T16 | as 16 | 2620001P T.III |
| 22 | V | Base Velocity 0.13 nm x 1 deg, 32 nm, 8 levels | Radial Image | — | — | T16 | hw30 elevation x10; hw47 max negative velocity kt; hw48 max positive velocity kt | 2620001P T.III |
| 23 | V | Base Velocity 0.27 nm x 1 deg, 62 nm, 8 levels | Radial Image | — | — | T16 | as 22 | 2620001P T.III |
| 24 | V | Base Velocity 0.54 nm x 1 deg, 124 nm, 8 levels | Radial Image | — | — | T16 | as 22 | 2620001P T.III |
| 25 | V | Base Velocity 0.13 nm x 1 deg, 32 nm, 16 levels | Radial Image | 0xaf1f | NOW | T16 | as 22 | 2620001P T.III |
| 26 | V | Base Velocity 0.27 nm x 1 deg, 62 nm, 16 levels | Radial Image | — | — | T16 | as 22 | 2620001P T.III |
| 27 | V | Base Velocity 0.54 nm x 1 deg, 124 nm, 16 levels | Radial Image | 0xaf1f | N0V | T16 | as 22; hw50 delta time/supplemental scan (later builds) | 2620001P T.III |
| 28 | SW | Base Spectrum Width 0.13 nm x 1 deg, 32 nm, 8 levels | Radial Image | 0xaf1f | NSP | T16 | hw30 elevation x10; hw47 max spectrum width kt | 2620001P T.III |
| 29 | SW | Base Spectrum Width 0.27 nm x 1 deg, 62 nm, 8 levels | Radial Image | — | — | T16 | as 28 | 2620001P T.III |
| 30 | SW | Base Spectrum Width 0.54 nm x 1 deg, 124 nm, 8 levels | Radial Image | 0xaf1f | NSW | T16 | hw30 elevation x10; hw47 max spectrum width kt; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE) | 2620001AD T.III |
| 31 | USP | User Selectable Storm Total Precipitation | Radial Image / Geographic Alpha | — | — | T16 | hw27 end hour; hw28 time span h; hw30 null product flag; hw47 max rainfall in x10; hw48-49 begin date/min; hw50-51 end date/min; hw52 bias x100; hw53 G-R pairs x100 | 2620001AD T.III |
| 32 | DHR | Digital Hybrid Scan Reflectivity | Radial Image | 1, 16 | DHR | L256-dBZ | hw47 max reflectivity dBZ; hw48 date of hybrid scan; hw49 avg time min; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 33 | HSR | Hybrid Scan Reflectivity | Radial Image | — | — | T16 | hw47 max reflectivity dBZ; hw48-49 date/time | 2620001P T.III |
| 34 | — | Clutter Filter Control | Radial Image | 0xaf1f | NC1, NC2, NC3, NC4, NC5 | T16 | hw27 channel/segment bit map; hw28 CMD generated bypass map flag; hw48-49 bypass map date/min; hw50-51 notchwidth map date/min | 2620001P T.III |
| 35 | CR | Composite Reflectivity 0.54 nm, 124 nm, 8 levels | Raster Image / Non-geographic Alpha | — | — | T16 | hw47 max reflectivity dBZ; hw51-52 calibration constant (Real*4, dB) | 2620001P T.III |
| 36 | CR | Composite Reflectivity 2.2 nm, 248 nm, 8 levels | Raster Image / Non-geographic Alpha | 8, 10, 0xba07 | NCO | T16 | as 35 | 2620001P T.III |
| 37 | CR | Composite Reflectivity 0.54 nm, 124 nm, 16 levels | Raster Image / Non-geographic Alpha | 8, 10, 0xba07 | NCR | T16 | hw30 AVSET termination elevation x10 (else 0); hw47 max reflectivity dBZ; hw51-52 calibration constant (Real*4, dB) | 2620001AD T.III |
| 38 | CR | Composite Reflectivity 2.2 nm, 248 nm, 16 levels | Raster Image / Non-geographic Alpha | 8, 10, 0xba07 | NCZ | T16 | as 37 | 2620001AD T.III |
| 41 | ET | Echo Tops | Raster Image | 0xba07 | NET | T16 | hw30 AVSET termination elevation x10; hw47 max echo top kft | 2620001AD T.III |
| 43 | — | Severe Weather Analysis (Reflectivity) | Radial Image | — | — | T16 | hw27-28 window azimuth/range; hw30 elevation | 2620001H T.III |
| 44 | — | Severe Weather Analysis (Velocity) | Radial Image | — | — | T16 | as 43 | 2620001H T.III |
| 45 | — | Severe Weather Analysis (Spectrum Width) | Radial Image | — | — | T16 | as 43 | 2620001H T.III |
| 46 | — | Severe Weather Analysis (Shear) | Radial Image | — | — | T16 | as 43 | 2620001H T.III |
| 47 | — | Severe Weather Probability | Geographic Alphanumeric | 8 | NWP | n/a | hw47 max SWP percent; hw48 max SWP box size nm x10 | 2620001H T.III |
| 48 | VWP | VAD Wind Profile | Non-geographic Alphanumeric | 4, 8, 10 | NVW | T16 (5 levels: RMS) | hw47 max speed kt; hw48 direction of max speed; hw49 altitude of max speed ft/10 | 2620001AD T.III |
| 50 | RCS | Cross Section (Reflectivity) | Raster Image | — | — | T16 | hw47-50 azimuth/range of points 1 and 2 (x10); hw51-52 calibration constant (Real*4, dB) | 2620001AD T.III |
| 51 | VCS | Cross Section (Velocity) | Raster Image | — | — | T16 | hw47-50 azimuth/range of points 1 and 2 (x10) | 2620001AD T.III |
| 55 | SRR | Storm Relative Mean Radial Velocity (Region) | Radial Image | — | — | T16 | hw27-28 window azimuth/range x10; hw30 elevation; hw47-48 max neg/pos velocity kt; hw49 motion source; hw50 height; hw51-52 storm speed/direction x10 | 2620001P T.III |
| 56 | SRM | Storm Relative Mean Radial Velocity (Map) | Radial Image | 0xaf1f | N0S, N1S, N2S, N3S | T16 | hw30 elevation x10; hw47-48 max neg/pos velocity kt; hw49 motion source flag; hw51 avg storm speed kt x10; hw52 avg storm direction x10 | 2620001AD T.III |
| 57 | VIL | Vertically Integrated Liquid | Raster Image | 0xba07 | NVL | T16 | hw30 AVSET elevation; hw47 max VIL kg/m2 | 2620001AD T.III |
| 58 | STI | Storm Tracking Information | Geographic and Non-geographic Alpha | 2, 6, 8, 10, 15, 23, 24, 25 | NST | n/a | hw47 total number of storms | 2620001AD T.III |
| 59 | HI | Hail Index | Geographic and Non-geographic Alpha | 8, 10, 13, 14, 15, 19 | NHI | n/a | none | 2620001AD T.III |
| 60 | M | Mesocyclone | Geographic and Non-geographic Alpha | 3, 8, 10, 11, 15 | NME | n/a | none | 2620001P T.III |
| 61 | TVS | Tornado Vortex Signature | Geographic and Non-geographic Alphanumeric | 8, 10, 12, 15 | NTV | n/a | hw47 number of TVS (negative: exceeded max); hw48 number of ETVS | 2620001AD T.III |
| 62 | SS | Storm Structure | Alphanumeric (stand-alone) | 21, 22 | NSS | n/a | none | 2620001AD T.III |
| 63 | LRA | Layer Composite Reflectivity Layer 1 Average | Raster Image | — | — | T16 | hw47 max reflectivity; hw48-49 layer bottom/top; hw51-52 calibration constant (Real*4, dB) | 2620001P T.III |
| 64 | LRA | Layer Composite Reflectivity Layer 2 Average | Raster Image | — | — | T16 | as 63 | 2620001P T.III |
| 65 | LRM | Layer Composite Reflectivity Layer 1 Maximum | Raster Image | 0xba07 | NLL | T16 | hw47 max reflectivity; hw48-49 layer bottom/top kft; hw51-52 calibration constant (Real*4, dB) | 2620001P T.III |
| 66 | LRM | Layer Composite Reflectivity Layer 2 Maximum | Raster Image | 0xba07 | NML | T16 | hw30 AVSET elevation; hw47 max reflectivity; hw48-49 layer bottom/top kft; hw51-52 calibration constant (Real*4, dB) | 2620001AD T.III |
| 67 | APR | Layer Composite Reflectivity - AP Removed | Raster Image | 0xba07 | NLA | T16 | as 66 | 2620001AD T.III |
| 73 | UAM | User Alert Message | Alphanumeric | — | — | n/a | none | 2620001P T.III |
| 74 | RCM | Radar Coded Message | Alphanumeric | none | RCM | n/a | none (ASCII after PDB, "1234 ROBUU") | 2620001P T.III |
| 75 | FTM | Free Text Message | Alphanumeric (stand-alone) | none | FTM | n/a | hw47 RPG ID number | 2620001AD T.III |
| 77 | PTM | PUP Text Message | Alphanumeric (stand-alone) | — | — | n/a | none | 2620001AD T.III |
| 78 | OHP | Surface Rainfall Accumulation (1 hr) | Radial Image | 0xaf1f, 0xba07 | N1P | T16 | hw47 max rainfall in x10; hw48 bias x100; hw49 G-R pairs x100; hw50-51 end date/min | 2620001AD T.III |
| 79 | THP | Surface Rainfall Accumulation (3 hr) | Radial Image | 0xaf1f | N3P | T16 | as 78 | 2620001AD T.III |
| 80 | STP | Storm Total Rainfall Accumulation | Radial Image | 0xaf1f, 0xba07 | NTP | T16 | hw47 max rainfall in x10; hw48-49 begin date/min; hw50-51 end date/min; hw52 bias x100; hw53 G-R pairs x100 | 2620001AD T.III |
| 81 | DPA | Hourly Digital Precipitation Array | Raster Image / Alphanumeric | 1, 17, 18 | DPA | DPA | hw47 max rainfall dBA x1000; hw48 bias x100; hw49 G-R pairs x100; hw50-51 end date/min | 2620001AD T.III |
| 82 | SPD | Supplemental Precipitation Data | Alphanumeric (stand-alone) | 1, 18 | SPD, SUP | n/a | none | 2620001AD T.III |
| 84 | VAD | Velocity Azimuth Display | Non-geographic Alphanumeric | — | — | T16 (8 levels) | hw30 wind altitude kft; hw47 wind speed kt; hw48 wind direction; hw49 elevation x10; hw50 slant range nm x10; hw51 RMS error kt | 2620001AD T.III |
| 85 | RCS | Cross Section Reflectivity (8 levels) | Raster Image | — | — | T16 | as 50 | 2620001P T.III |
| 86 | VCS | Cross Section Velocity (8 levels) | Raster Image | — | — | T16 | as 51 | 2620001AD T.III |
| 87 | CS | Combined Shear | Raster Image | — | — | T16 | see 2620001H Table V | 2620001H T.III |
| 89 | LRA | Layer Composite Reflectivity Layer 3 Average | Raster Image | — | — | T16 | as 63 | 2620001P T.III |
| 90 | LRM | Layer Composite Reflectivity Layer 3 Maximum | Raster Image | 0xba07 | NHL | T16 | as 66 | 2620001AD T.III |
| 93 | DBV | ITWS Digital Base Velocity | Radial Image | — | — | L256-vel | hw30 elevation x10; hw47-48 max neg/pos velocity kt; hw50 velocity precision code (1 or 2) | 2620001AD T.III |
| 94 | DR | Base Reflectivity Data Array | Radial Image | 16 | N0Q, N1Q, N2Q, N3Q, NAQ, NBQ, NYQ, NZQ | L256-dBZ | hw30 elevation x10; hw47 max reflectivity dBZ; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 95 | CRE | Composite Reflectivity Edited for AP 0.54 nm, 8 levels | Raster Image | — | — | T16 | as 35 | 2620001P T.III |
| 96 | CRE | Composite Reflectivity Edited for AP 2.2 nm, 8 levels | Raster Image | — | — | T16 | as 35 | 2620001P T.III |
| 97 | CRE | Composite Reflectivity Edited for AP 0.54 nm, 16 levels | Raster Image / Non-geographic Alpha | — | — | T16 | as 37 | 2620001AD T.III |
| 98 | CRE | Composite Reflectivity Edited for AP 2.2 nm, 16 levels | Raster Image | — | — | T16 | as 35 | 2620001P T.III |
| 99 | DV | Base Velocity Data Array | Radial Image | 16 | N0U, N1U, N2U, N3U, NAU, NBU | L256-vel | hw30 elevation x10; hw47-48 max neg/pos velocity kt; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 100 | — | Site Adaptable Parameters for VAD Wind Profile (product 48) | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 101 | — | Storm Track Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 102 | — | Hail Index Alphanumeric Block | Alphanumeric block | none | 102 | n/a | none | 2620001AD T.III |
| 103 | — | Mesocyclone Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001P T.III |
| 104 | — | TVS Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 105 | — | Site Adaptable Parameters for Combined Shear | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 107 | — | Surface Rainfall (1 hr) Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 108 | — | Surface Rainfall (3 hr) Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 109 | — | Storm Total Rainfall Accumulation Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 110 | — | Clutter Likelihood Reflectivity Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 111 | — | Clutter Likelihood Doppler Alphanumeric Block | Alphanumeric block | — | — | n/a | none | 2620001AD T.III |
| 113 | PRC | Power Removed Control | Radial Image | 0xaf1f | N0F, NAF, NBF, NXF, NYF | T16 (13 levels) | hw27 RPG cut number; hw28 CMD generated flag; hw30 elevation x10; hw47 clutter map time min; hw48 clutter map date; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 132 | CLR | Clutter Likelihood Reflectivity | Radial Image | — | — | T16 (11 levels) | hw30 elevation x10; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE) | 2620001AD T.III |
| 133 | CLD | Clutter Likelihood Doppler | Radial Image | — | — | T16 (12 levels) | hw30 elevation x10 | 2620001P T.III |
| 134 | DVL | High Resolution VIL | Radial Image | 16 | DVL | HRVIL | hw30 AVSET elevation; hw47 max digital VIL; hw48 number of artifact-edited radials; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 135 | EET | Enhanced Echo Tops | Radial Image | 16 | EET | HREET | hw30 AVSET elevation; hw47 max echo top kft; hw48 edited radials; hw49 reflectivity threshold dBZ; hw50 spurious points removed; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 136 | SO | SuperOb | Latitude, Longitude (packet 27) | — | — | n/a | see 2620001P Table V | 2620001P T.III |
| 137 | ULR | User Selectable Layer Composite Reflectivity | Radial Image | — | — | T16 | hw27-28 requested layer bottom/top kft; hw47 max reflectivity; hw48-49 actual layer bottom/top | 2620001AD T.III |
| 138 | DSP | Digital Storm Total Precipitation | Radial Image | 1, 16 | DSP | L256-DSP | hw27-28 begin date/min; hw30 bias x100; hw47 max rainfall in x100; hw48-49 end date/min; hw50 G-R pairs x100; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 139 | MRU | Mesocyclone Rapid Update | Geographic and Non-geographic Alpha | — | — | n/a | hw30 elevation | 2620001P T.III |
| 140 | GFM | Gust Front MIGFA | Generic Data Format | — | — | n/a | hw49 detection count | 2620001AD T.III |
| 141 | MD | Mesocyclone Detection | Geographic and Non-geographic Alpha | 2, 6, 8, 10, 20, 23, 24 | NMD | n/a | hw27 min reflectivity threshold dBZ; hw28 overlap display filter; hw30 min display filter strength rank | 2620001AD T.III |
| 143 | TRU | Tornado Vortex Signature Rapid Update | Geographic and Non-geographic Alphanumeric | — | — | n/a | hw30 elevation x10; hw47 number of TVS; hw48 number of ETVS; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE) | 2620001AD T.III |
| 144 | OSW | One-hour Snow Water Equivalent | Radial Image | — | — | T16 | hw27 missing period min; hw30 use RCA flag; hw47 max in x1000; hw48-51 start/end date and min; hw52-53 azimuth/range of max | 2620001AD T.III |
| 145 | OSD | One-hour Snow Depth | Radial Image | — | — | T16 | as 144 (hw47 max in x100) | 2620001AD T.III |
| 146 | SSW | Storm Total Snow Water Equivalent | Radial Image | — | — | T16 | as 145 | 2620001AD T.III |
| 147 | SSD | Storm Total Snow Depth | Radial Image | — | — | T16 | as 144 (hw47 max in x10) | 2620001AD T.III |
| 149 | DMD | Digital Mesocyclone Detection | Generic Data Format | — | — | n/a | hw27 min reflectivity threshold; hw30 elevation x10; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 150 | USW | User Selectable Snow Water Equivalent | Radial Image | — | — | T16 | hw27 end hour; hw28 span h; hw30 high-scale/RCA flags; hw47 max; hw48-51 start/end date and hour; hw52-53 azimuth/range of max | 2620001AD T.III |
| 151 | USD | User Selectable Snow Depth | Radial Image | — | — | T16 | as 150 | 2620001AD T.III |
| 152 | ASP | Archive III Status Product | Generic Data Format | 28 | RSL | n/a | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 153 | SDR | Super Resolution Reflectivity Data Array | Radial Image | 16 | H0Z, N0B | L256-dBZ | hw30 elevation x10; hw47 max reflectivity dBZ; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 154 | SDV | Super Resolution Velocity Data Array | Radial Image | 16 | H0V, N0G | L256-vel | hw30 elevation x10; hw47-48 max neg/pos velocity kt; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 155 | SDW | Super Resolution Spectrum Width Data Array | Radial Image | 16 | H0W | L256-sw | hw30 elevation x10; hw47 max spectrum width kt; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 156 | — | Eddy Dissipation Rate | Digital Radial Data Array | — | — | EDR | see 2620001P Table V | 2620001P T.III |
| 157 | — | Eddy Dissipation Rate Confidence | Digital Radial Data Array | — | — | EDR | see 2620001P Table V | 2620001P T.III |
| 158 | — | Differential Reflectivity (16 levels) | Radial Image | — | — | T16 | hw30 elevation; hw47-48 min/max ZDR x10 | 2620001P T.III |
| 159 | DZD | Digital Differential Reflectivity | Radial Image | 16 | N0X, N1X, N2X, N3X, NAX, NBX | GEN | hw30 elevation x10; hw47-48 min/max ZDR dB x10; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 160 | — | Correlation Coefficient (16 levels) | Radial Image | — | — | T16 | hw30 elevation; hw47-48 min/max CC | 2620001P T.III |
| 161 | DCC | Digital Correlation Coefficient | Radial Image | 16 | N0C, N1C, N2C, N3C, NAC, NBC | GEN | hw30 elevation x10; hw47-48 min/max CC x300; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 162 | — | Specific Differential Phase (16 levels) | Radial Image | — | — | T16 | hw30 elevation; hw47-48 min/max KDP | 2620001P T.III |
| 163 | DKD | Digital Specific Differential Phase | Radial Image | 16 | N0K, N1K, N2K, N3K, NAK, NBK | GEN | hw30 elevation x10; hw47-48 min/max KDP x20; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 164 | — | Hydrometeor Classification (16 levels) | Radial Image | — | — | T16 | hw30 elevation | 2620001P T.III |
| 165 | DHC | Digital Hydrometeor Classification | Radial Image | 16 | N0H, N1H, N2H, N3H, NAH, NBH | CAT-HC | hw30 elevation x10; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 166 | ML | Melting Layer | Linked Contour Vectors / Set Color Level | 0x0802, 0x0e03 | N0M, N1M, N2M, N3M, NAM, NBM | n/a | hw30 elevation x10; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE) | 2620001AD T.III |
| 167 | SDC | Super Res Digital Correlation Coefficient | Radial Image | 16 | H0C | GEN | hw30 elevation x10; hw47-48 min/max CC x300; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 168 | SDP | Super Res Digital Phi | Radial Image | — | — | GEN | hw30 elevation x10; hw47-48 min/max PhiDP deg; hw50 delta time (bits 5-15, s) / supplemental scan (bits 0-4: 0 none, 1 SAILS, 2 MRLE); hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 169 | OHA | One Hour Accumulation | Radial Image | 0xaf1f | OHA | T16 | hw30 null product flag (low byte); hw47 max accum in x10; hw48-49 end date/min; hw50 bias x100; hw51 G-R pairs x100 | 2620001AD T.III |
| 170 | DAA | Digital Accumulation Array | Radial Image | 16 | DAA | GEN | hw27 threshold min time in hour; hw28 total time in hour; hw30 null product flag; hw47 max accum in x10; hw48-49 end date/min; hw50 bias x100; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 171 | STA | Storm Total Accumulation | Radial Image | 1, 0xaf1f | PTA | T16 | hw27-28 start date/min; hw30 null product flag; hw47 max accum in x10; hw48-49 end date/min; hw50 bias x100; hw51 G-R pairs x100 | 2620001AD T.III |
| 172 | DSA | Digital Storm Total Accumulation | Radial Image | 1, 16 | DTA | GEN | hw27-28 start date/min; hw30 null product flag; hw47 max accum in x10; hw48-49 end date/min; hw50 bias x100; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 173 | DUA | Digital User-Selectable Accumulation | Radial Image | 16 | DU3, DU6 | GEN | hw27 end time min; hw28 span min; hw30 missing period flag (high byte) / null product flag (low byte); hw47 max accum in x10; hw48 end date; hw49 start time min; hw50 bias x100; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 174 | DOD | Digital One-Hour Difference Accumulation | Radial Image | 16 | DOD | GEN | hw47 max difference in x10; hw48-49 end date/min; hw50 min difference in x10; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 175 | DSD | Digital Storm Total Difference Accumulation | Radial Image | 16 | DSD | GEN | hw27-28 start date/min; hw30 null product flag; hw47 max difference x10; hw48-49 end date/min; hw50 min difference x10; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 176 | DPR | Digital Instantaneous Precipitation Rate | Generic Radial Product Format | 28 | DPR | GEN (u16 levels) | hw27-28 hybrid rate scan date/min; hw30 precip detected flag (high byte) / bias applied flag (low byte); hw47 max rate in/h x1000; hw48 percent bins filled x100; hw49 highest elevation x10; hw50 bias x100; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 177 | HHC | Hybrid Hydrometeor Classification | Radial Image | 16 | HHC | CAT-HC | hw47 mode filter size; hw48 percent bins filled x100; hw49 highest elevation x10; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 178 | IHL | Icing Hazard Levels | Generic Radial Product Format | — | — | n/a | hw30 AVSET elevation; hw47 max icing top kft; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 179 | HHL | Hail Hazard Layers | Generic Radial Product Format | — | — | n/a | hw30 AVSET elevation; hw47 max hail top kft; hw48 HSDA status; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 180 | DR | TDWR Base Reflectivity 0.08 nm x 1 deg, 48 nm | Radial Image | 16 | TZ0, TZ1, TZ2 | L256-dBZ | hw30 elevation x10; hw47 max reflectivity dBZ; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620063E T.III |
| 181 | — | TDWR Base Reflectivity (16 levels) | Radial Image | 0xaf1f | TR0, TR1, TR2 | T16 | hw30 elevation x10; hw47 max reflectivity dBZ | MetPy only |
| 182 | DV | TDWR Base Velocity 0.08 nm x 1 deg, 48 nm | Radial Image | 16 | TV0, TV1, TV2 | L256-vel | hw30 elevation x10; hw47-48 max neg/pos velocity; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620063E T.III |
| 183 | — | TDWR Base Velocity (16 levels) | Radial Image | — | — | T16 | hw30 elevation x10; hw47-48 max neg/pos velocity | MetPy only |
| 184 | SW | TDWR Base Spectrum Width 0.08 nm x 1 deg, 48 nm | Radial Image | — | — | 256 levels (encoding not given) | hw30 elevation x10; hw47 max spectrum width; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620063E T.III |
| 185 | — | TDWR Base Spectrum Width (16 levels) | Radial Image | — | — | T16 | hw30 elevation x10; hw47 max spectrum width | MetPy only |
| 186 | DR | TDWR Long Range Base Reflectivity 0.16 nm x 1 deg, 225 nm | Radial Image | 16 | TZL | L256-dBZ | hw30 elevation x10; hw47 max reflectivity dBZ; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620063E T.III |
| 187 | — | TDWR Long Range Base Reflectivity (16 levels) | Radial Image | — | — | T16 | hw30 elevation x10; hw47 max reflectivity dBZ | MetPy only |
| 189 | RQ | Quasi-Vertical Profile Reflectivity | Raster Image / Non-Geographic | — | — | GEN | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 190 | CCQ | Quasi-Vertical Profile Correlation Coefficient | Raster Image / Non-Geographic | — | — | GEN | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 191 | ZDQ | Quasi-Vertical Profile Differential Reflectivity | Raster Image / Non-Geographic | — | — | GEN | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 192 | KDQ | Quasi-Vertical Profile Specific Differential Phase | Raster Image / Non-Geographic | — | — | GEN | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 193 | SRQ | Super Resolution Digital Reflectivity Data-Quality-Edited | Radial Image | — | — | L256-dBZ | hw30 elevation x10; hw47 max reflectivity; hw48 edited radials; hw49 AVSET status; hw50 chaff detection status; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 195 | DRQ | Digital Reflectivity, DQA-Edited Data Array | Radial Image | — | — | L256-dBZ | hw30 elevation x10; hw47 max reflectivity; hw48 edited radials; hw49 AVSET status; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 196 | MBA | Microburst AMDA | Generic Data Format | — | — | n/a | hw27 half degree scan count; hw49 detection count | 2620001AD T.III |
| 197 | RRC | Rain Rate Classification | Radial Image | 16 | NRR | CAT-RRC | hw47 mode filter size; hw48 percent bins filled x100; hw49 highest elevation x10; hw50 dry snow multiplier x10; hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
| 202 | SCL | Shift Change Checklist | Generic Data Format | — | — | n/a | hw51 compression (0 none, 1 bzip2); hw52-53 uncompressed size (bytes, after PDB) | 2620001AD T.III |
