//! Minimal read-only HDF5 parser — just enough for ODIM_H5 polar volumes.
//!
//! The workspace has no HDF5 dependency (the C library is a heavy, awkward
//! build input on Windows CI), and ODIM files exercise a small, stable
//! corner of the format: BALTRAD/rave, HL-HDF, and h5py (libver "earliest",
//! its default) all write version-0 superblocks, version-1 object headers,
//! old-style groups (symbol table + v1 B-tree + local heap), and contiguous
//! or chunked+deflate dataset layouts. This module implements exactly that
//! subset, byte-for-byte against the HDF5 File Format Specification
//! (The HDF Group, "HDF5 File Format Specification Version 3.0";
//! <https://support.hdfgroup.org/documentation/hdf5/latest/_f_m_t3.html>):
//!
//! - Superblock v0/v1 (v2/v3 — the 1.10+ "latest" layout — is detected and
//!   rejected with a clear error).
//! - Version 1 object headers, including continuation blocks.
//! - Version 2 object headers ("OHDR", with "OCHK" continuation blocks and
//!   Jenkins lookup3 checksum verification). AEMET/Spain writes ODIM H5rad
//!   2.4 files (IRIS 8.13/10.3 export, live in ORD since 2026-06-23) as a
//!   mixed dialect: superblock v0 and old-style groups, but v2 headers on
//!   the leaf metadata groups (`datasetN/{how,what,where}`,
//!   `datasetN/dataM/{how,what}`). Their attributes stay compact (message
//!   0x000C version 1) and their link-info fractal-heap addresses are
//!   undefined, so v2 B-trees, fractal heaps, and dense attribute storage
//!   remain out of scope below.
//! - Messages: dataspace (0x0001), datatype (0x0003), data layout (0x0008,
//!   v3 compact/contiguous/chunked), filter pipeline (0x000B, deflate id 1
//!   and shuffle id 2), attribute (0x000C, versions 1-3), header
//!   continuation (0x0010), symbol table (0x0011), and attribute info
//!   (0x0015) to detect dense attribute storage.
//! - Datatypes: fixed-point, IEEE float (f32/f64), fixed-length strings, and
//!   variable-length strings (global heap collections).
//! - Chunk index: v1 B-trees; raw chunks pass through the inverse filter
//!   pipeline (deflate, then unshuffle) and edge chunks are clipped.
//!
//! Everything else (fractal heaps, dense attributes, v2 B-trees, shared
//! messages, fill values beyond zero, named datatypes, ...) is out of scope
//! and produces an explicit error rather than silent misreads. In particular
//! [`H5File::open`] reads every attribute of every reachable object once and
//! fails when one has a datatype outside the list above or when an object
//! keeps its attributes in dense storage, so [`H5File::attr`] and
//! [`H5File::attrs`] never skip an attribute silently.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::{OdimError, Result};

const SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', b'\r', b'\n', 0x1a, b'\n'];
/// Version 2 object header signature (HDF5 spec section IV.A.2).
const OHDR_SIGNATURE: &[u8; 4] = b"OHDR";
/// Version 2 object header continuation block signature.
const OCHK_SIGNATURE: &[u8; 4] = b"OCHK";
const UNDEFINED_ADDR: u64 = u64::MAX;
/// Defense against corrupt files: deepest group nesting we will walk.
const MAX_GROUP_DEPTH: usize = 16;
/// Defense against corrupt B-trees: most nodes visited per tree walk.
const MAX_BTREE_NODES: usize = 1 << 16;
/// Defense against corrupt/self-referencing v2 header continuations: most
/// header blocks (chunk 0 + OCHK continuations) per object header.
const MAX_HEADER_BLOCKS: usize = 1 << 10;
const MAX_OBJECT_MESSAGES: usize = 4096;
const MAX_OBJECT_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_GROUP_ENTRIES: usize = 1 << 20;
const MAX_DATA_CHUNKS: usize = 1 << 18;
const MAX_DATASPACE_RANK: usize = 32;
const MAX_DATASPACE_DIM: usize = 100 * 1024 * 1024;
const MAX_HDF5_DATASET_BYTES: usize = 256 * 1024 * 1024;
const MAX_HDF5_ATTRIBUTE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HDF5_FILTERS: usize = 32;
const MAX_HDF5_FILTER_VALUES: usize = 1024;
/// Most named objects (groups and datasets) indexed per file. Real ODIM_H5
/// files hold 18-283 objects.
const MAX_OBJECTS: usize = 1 << 14;

/// `true` when the buffer starts with the HDF5 superblock signature.
pub fn looks_like_hdf5_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= SIGNATURE.len() && bytes[..SIGNATURE.len()] == SIGNATURE
}

/// A decoded scalar or 1-D attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum H5Attr {
    Str(String),
    F64(f64),
    I64(i64),
    F64Array(Vec<f64>),
    I64Array(Vec<i64>),
}

impl H5Attr {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    /// Numeric view: integers widen to f64 (ODIM writers disagree about
    /// whether e.g. `nodata` is a long or a double).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(value) => Some(*value),
            Self::I64(value) => Some(*value as f64),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(*value),
            Self::F64(value) => (value.fract() == 0.0).then_some(*value as i64),
            _ => None,
        }
    }
}

/// Raw dataset elements, converted from the on-disk datatype.
#[derive(Clone, Debug, PartialEq)]
pub enum H5Data {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl H5Data {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(values) => values.len(),
            Self::U16(values) => values.len(),
            Self::F32(values) => values.len(),
            Self::F64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A dataset: dimension sizes (row-major) plus the element array.
#[derive(Clone, Debug)]
pub struct H5Dataset {
    pub dims: Vec<usize>,
    pub data: H5Data,
}

/// Read-only HDF5 file view over a byte slice.
pub struct H5File<'a> {
    bytes: &'a [u8],
    offset_size: usize,
    length_size: usize,
    /// Absolute path ("/a/b") → object header address for every object
    /// reachable from the root group.
    objects: BTreeMap<String, u64>,
}

impl<'a> H5File<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        if !looks_like_hdf5_bytes(bytes) {
            return Err(invalid(0, "missing HDF5 superblock signature"));
        }
        let version = *bytes.get(8).ok_or_else(|| truncated(8, 1, bytes.len()))?;
        if version > 1 {
            // Real-world note: netCDF-4 files (modern CfRadial 1.x and all
            // CfRadial 2, written by Radx/netCDF) carry this superblock —
            // every public CfRadial sample checked in 2026 does. Point
            // those users at the conversion that actually works.
            return Err(invalid(
                8,
                format!(
                    "HDF5 superblock version {version} (1.10+ 'latest' layout) is unsupported. \
                     If this is a netCDF-4 CfRadial file, convert it to classic netCDF \
                     (`nccopy -k classic` or RadxConvert) and open the .nc; ODIM_H5 writers \
                     should use default/earliest library settings"
                ),
            ));
        }
        let offset_size = read_u8(bytes, 13)? as usize;
        let length_size = read_u8(bytes, 14)? as usize;
        if !(4..=8).contains(&offset_size) || !(4..=8).contains(&length_size) {
            return Err(invalid(13, "unsupported HDF5 offset/length sizes"));
        }
        // v0: fixed fields end at 24; v1 inserts 4 bytes (indexed-storage k).
        let addr_block = if version == 0 { 24 } else { 28 };
        // base, free-space, EOF, driver-info addresses; then the root group
        // symbol table entry, whose object header address is field 2.
        let root_entry = addr_block + 4 * offset_size;
        let root_header = read_offset(bytes, root_entry + offset_size, offset_size)?;
        let mut file = Self {
            bytes,
            offset_size,
            length_size,
            objects: BTreeMap::new(),
        };
        let header = file.parse_object_header(root_header)?;
        file.objects.insert("/".to_owned(), root_header);
        let mut visited_groups = BTreeSet::from([root_header]);
        file.walk_group("", &header, &mut visited_groups, 0)?;
        file.validate_attributes()?;
        Ok(file)
    }

    /// Every attribute of every reachable object decodes, and no object keeps
    /// its attributes in dense storage (fractal heap plus v2 B-tree), so the
    /// attribute readers see all of them.
    fn validate_attributes(&self) -> Result<()> {
        for (path, address) in &self.objects {
            let header = self.parse_object_header(*address)?;
            for message in &header.messages {
                match message.kind {
                    0x000C => {
                        self.parse_attribute(&message.body, None)
                            .map_err(|err| match err {
                                OdimError::LimitExceeded(_) => err,
                                err => invalid(
                                    *address as usize,
                                    format!(
                                        "an attribute of HDF5 object '{path}' cannot be read: {err}"
                                    ),
                                ),
                            })?;
                    }
                    0x0015 if self.dense_attribute_heap(&message.body)? => {
                        return Err(invalid(
                            *address as usize,
                            format!(
                                "HDF5 object '{path}' stores its attributes densely \
                                 (fractal heap and v2 B-tree), which is unsupported"
                            ),
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// `true` when an attribute info message (0x0015; HDF5 spec IV.A.2.v)
    /// points at a fractal heap, i.e. the object uses dense attribute
    /// storage.
    fn dense_attribute_heap(&self, body: &[u8]) -> Result<bool> {
        let version = read_u8(body, 0)?;
        if version != 0 {
            return Err(invalid(
                0,
                format!("attribute info message version {version} unsupported"),
            ));
        }
        let flags = read_u8(body, 1)?;
        // Bit 0: a 2-byte maximum creation index precedes the addresses.
        let heap_at = if flags & 0x01 != 0 { 4 } else { 2 };
        Ok(read_offset(body, heap_at, self.offset_size)? != UNDEFINED_ADDR)
    }

    /// Names of the direct children of `path` (groups and datasets).
    pub fn child_names(&self, path: &str) -> Vec<String> {
        let prefix = if path == "/" {
            "/".to_owned()
        } else {
            format!("{}/", path.trim_end_matches('/'))
        };
        self.objects
            .keys()
            .filter_map(|key| {
                let rest = key.strip_prefix(&prefix)?;
                (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_owned())
            })
            .collect()
    }

    pub fn has_object(&self, path: &str) -> bool {
        self.objects.contains_key(path)
    }

    /// Read one attribute of the object at `path`.
    pub fn attr(&self, path: &str, name: &str) -> Option<H5Attr> {
        let header = self.parse_object_header(*self.objects.get(path)?).ok()?;
        for message in &header.messages {
            if message.kind != 0x000C {
                continue;
            }
            if let Ok(Some((_, attr))) = self.parse_attribute(&message.body, Some(name)) {
                return Some(attr);
            }
        }
        None
    }

    /// Every attribute of the object at `path`, in header order. Empty when
    /// the object does not exist. [`Self::open`] checked that each one
    /// decodes and that none is in dense storage.
    pub fn attrs(&self, path: &str) -> Vec<(String, H5Attr)> {
        let Some(header) = self
            .objects
            .get(path)
            .and_then(|address| self.parse_object_header(*address).ok())
        else {
            return Vec::new();
        };
        header
            .messages
            .iter()
            .filter(|message| message.kind == 0x000C)
            .filter_map(|message| self.parse_attribute(&message.body, None).ok().flatten())
            .collect()
    }

    /// Read the full dataset at `path`.
    pub fn dataset(&self, path: &str) -> Result<H5Dataset> {
        let address = *self
            .objects
            .get(path)
            .ok_or_else(|| invalid(0, format!("HDF5 object '{path}' not found")))?;
        let header = self.parse_object_header(address)?;
        let mut dims: Option<Vec<usize>> = None;
        let mut dtype: Option<Datatype> = None;
        let mut layout: Option<Layout> = None;
        let mut filters: Vec<Filter> = Vec::new();
        for message in &header.messages {
            match message.kind {
                0x0001 => dims = Some(self.parse_dataspace(&message.body)?),
                0x0003 => dtype = Some(self.parse_datatype(&message.body)?),
                0x0008 => layout = Some(self.parse_layout(&message.body)?),
                0x000B => filters = self.parse_filter_pipeline(&message.body)?,
                _ => {}
            }
        }
        let dims = dims.ok_or_else(|| invalid(0, format!("dataset '{path}' has no dataspace")))?;
        let dtype = dtype.ok_or_else(|| invalid(0, format!("dataset '{path}' has no datatype")))?;
        let layout = layout.ok_or_else(|| invalid(0, format!("dataset '{path}' has no layout")))?;
        let element_count = checked_product(&dims, "HDF5 dataset element count")?;
        let byte_len = checked_allocation_bytes(
            element_count,
            dtype.size,
            MAX_HDF5_DATASET_BYTES,
            "HDF5 dataset",
        )?;
        checked_allocation_bytes(
            element_count,
            dtype.decoded_element_bytes(),
            MAX_HDF5_DATASET_BYTES,
            "decoded HDF5 dataset",
        )?;
        let raw = match layout {
            Layout::Compact(data) => data,
            Layout::Contiguous { address, size } => {
                if address == UNDEFINED_ADDR {
                    vec![0u8; byte_len] // never written: fill value (zero)
                } else {
                    self.slice(address, (size as usize).min(byte_len))?.to_vec()
                }
            }
            Layout::Chunked {
                btree_address,
                chunk_dims,
            } => self.read_chunked(btree_address, &chunk_dims, &dims, dtype.size, &filters)?,
        };
        if raw.len() < byte_len {
            return Err(invalid(
                0,
                format!(
                    "dataset '{path}' raw stream too short: {} < {byte_len}",
                    raw.len()
                ),
            ));
        }
        let data = dtype.convert(&raw[..byte_len])?;
        Ok(H5Dataset { dims, data })
    }

    // ----- object graph -------------------------------------------------

    fn walk_group(
        &mut self,
        prefix: &str,
        header: &ObjectHeader,
        visited_groups: &mut BTreeSet<u64>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_GROUP_DEPTH {
            return Err(limit(format!(
                "HDF5 group nesting is deeper than {MAX_GROUP_DEPTH} levels (limit)"
            )));
        }
        for message in &header.messages {
            if message.kind != 0x0011 {
                continue;
            }
            // Symbol table message: v1 B-tree of SNOD leaves + local heap.
            let btree = read_offset(&message.body, 0, self.offset_size)?;
            let heap = read_offset(&message.body, self.offset_size, self.offset_size)?;
            let heap_data = self.local_heap_data(heap)?;
            let mut entries = Vec::new();
            let mut visited_nodes = BTreeSet::new();
            self.collect_group_entries(btree, &mut entries, &mut visited_nodes)?;
            for (name_offset, child_address) in entries {
                let name = heap_string(self.bytes, heap_data, name_offset)?;
                let path = format!("{prefix}/{name}");
                if self.objects.contains_key(&path) {
                    continue; // hard-link cycle guard
                }
                if self.objects.len() >= MAX_OBJECTS {
                    return Err(limit(format!(
                        "HDF5 file indexes more than {MAX_OBJECTS} objects (limit)"
                    )));
                }
                let child = self.parse_object_header(child_address)?;
                self.objects.insert(path.clone(), child_address);
                if visited_groups.insert(child_address) {
                    self.walk_group(&path, &child, visited_groups, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn collect_group_entries(
        &self,
        node_address: u64,
        out: &mut Vec<(u64, u64)>,
        visited: &mut BTreeSet<u64>,
    ) -> Result<()> {
        if !visited.insert(node_address) {
            return Err(invalid(
                address_to_usize(node_address)?,
                "cycle in HDF5 group B-tree",
            ));
        }
        if visited.len() > MAX_BTREE_NODES {
            return Err(limit(format!(
                "HDF5 group B-tree has more than {MAX_BTREE_NODES} nodes (limit)"
            )));
        }
        let node = self.slice(node_address, 8 + 2 * self.offset_size)?;
        if &node[..4] != b"TREE" {
            return Err(invalid(node_address as usize, "expected TREE signature"));
        }
        let level = node[5];
        let entries = u16::from_le_bytes([node[6], node[7]]) as usize;
        // keys/children alternate after the two sibling addresses.
        let mut cursor = address_to_usize(node_address)?
            .checked_add(8 + 2 * self.offset_size)
            .ok_or_else(|| invalid(0, "HDF5 group B-tree cursor overflow"))?;
        for _ in 0..entries {
            cursor += self.length_size; // key (heap offset) — unused here
            let child = read_offset(self.bytes, cursor, self.offset_size)?;
            cursor += self.offset_size;
            if level == 0 {
                self.read_snod(child, out)?;
            } else {
                self.collect_group_entries(child, out, visited)?;
            }
        }
        Ok(())
    }

    fn read_snod(&self, address: u64, out: &mut Vec<(u64, u64)>) -> Result<()> {
        let head = self.slice(address, 8)?;
        if &head[..4] != b"SNOD" {
            return Err(invalid(address as usize, "expected SNOD signature"));
        }
        let count = u16::from_le_bytes([head[6], head[7]]) as usize;
        let entry_size = 2 * self.offset_size + 8 + 16;
        let mut cursor = address_to_usize(address)?
            .checked_add(8)
            .ok_or_else(|| invalid(0, "HDF5 symbol-table address overflow"))?;
        for _ in 0..count {
            let name_offset = read_offset(self.bytes, cursor, self.length_size)?;
            let header = read_offset(self.bytes, cursor + self.offset_size, self.offset_size)?;
            if out.len() >= MAX_GROUP_ENTRIES {
                return Err(limit(format!(
                    "HDF5 group has more than {MAX_GROUP_ENTRIES} entries (limit)"
                )));
            }
            out.push((name_offset, header));
            cursor = cursor
                .checked_add(entry_size)
                .ok_or_else(|| invalid(cursor, "HDF5 symbol-table cursor overflow"))?;
        }
        Ok(())
    }

    fn local_heap_data(&self, address: u64) -> Result<u64> {
        let head = self.slice(address, 8 + 2 * self.length_size + self.offset_size)?;
        if &head[..4] != b"HEAP" {
            return Err(invalid(address as usize, "expected HEAP signature"));
        }
        read_offset(head, 8 + 2 * self.length_size, self.offset_size)
    }

    fn parse_object_header(&self, address: u64) -> Result<ObjectHeader> {
        // Version 2 headers announce themselves with a signature; version 1
        // headers have none and start with the version byte.
        if self
            .slice(address, OHDR_SIGNATURE.len())
            .is_ok_and(|sig| sig == OHDR_SIGNATURE)
        {
            return self.parse_object_header_v2(address);
        }
        let head = self.slice(address, 16)?;
        if head[0] != 1 {
            return Err(invalid(
                address as usize,
                format!("object header version {} is unsupported", head[0]),
            ));
        }
        let total_messages = u16::from_le_bytes([head[2], head[3]]) as usize;
        if total_messages > MAX_OBJECT_MESSAGES {
            return Err(limit(format!(
                "HDF5 object header declares {total_messages} messages (limit {MAX_OBJECT_MESSAGES})"
            )));
        }
        let block_size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
        if block_size > MAX_OBJECT_MESSAGE_BYTES {
            return Err(limit(format!(
                "HDF5 object-header message block is {block_size} bytes (limit {MAX_OBJECT_MESSAGE_BYTES})"
            )));
        }
        let mut messages = Vec::with_capacity(total_messages);
        // (start, length) message blocks; the first follows 4 pad bytes.
        let first_block = address_to_usize(address)?
            .checked_add(16)
            .ok_or_else(|| invalid(0, "HDF5 object-header address overflow"))?;
        let mut blocks = vec![(first_block, block_size)];
        let mut scheduled_blocks = BTreeSet::from([first_block]);
        let mut block_index = 0;
        let mut message_bytes = 0usize;
        while block_index < blocks.len() && messages.len() < total_messages {
            let (start, len) = blocks[block_index];
            block_index += 1;
            let mut cursor = start;
            let end = start
                .checked_add(len)
                .ok_or_else(|| invalid(start, "HDF5 object-header block overflow"))?;
            self.bytes
                .get(start..end)
                .ok_or_else(|| truncated(start, len, self.bytes.len()))?;
            while cursor
                .checked_add(8)
                .is_some_and(|header_end| header_end <= end)
                && messages.len() < total_messages
            {
                let header = self.slice(cursor as u64, 8)?;
                let kind = u16::from_le_bytes([header[0], header[1]]);
                let size = u16::from_le_bytes([header[2], header[3]]) as usize;
                let body_start = cursor
                    .checked_add(8)
                    .ok_or_else(|| invalid(cursor, "HDF5 message address overflow"))?;
                let body_end = body_start
                    .checked_add(size)
                    .ok_or_else(|| invalid(body_start, "HDF5 message size overflow"))?;
                if body_end > end {
                    return Err(truncated(cursor, 8 + size, end.saturating_sub(cursor)));
                }
                let body = self.slice(body_start as u64, size)?.to_vec();
                if kind == 0x0010 {
                    // Continuation: offset + length of the next block.
                    let offset = read_offset(&body, 0, self.offset_size)?;
                    let length = read_offset(&body, self.offset_size, self.length_size)?;
                    let offset = address_to_usize(offset)?;
                    let length = usize::try_from(length)
                        .map_err(|_| invalid(cursor, "HDF5 continuation length overflows usize"))?;
                    if length > MAX_OBJECT_MESSAGE_BYTES {
                        return Err(limit(format!(
                            "HDF5 continuation block is {length} bytes (limit {MAX_OBJECT_MESSAGE_BYTES})"
                        )));
                    }
                    if blocks.len() >= MAX_HEADER_BLOCKS {
                        return Err(limit(format!(
                            "HDF5 object header has more than {MAX_HEADER_BLOCKS} continuation blocks (limit)"
                        )));
                    }
                    if !scheduled_blocks.insert(offset) {
                        return Err(invalid(offset, "cycle in HDF5 object-header continuations"));
                    }
                    blocks.push((offset, length));
                } else {
                    message_bytes = message_bytes.checked_add(size).ok_or_else(|| {
                        invalid(cursor, "HDF5 object-header message size overflow")
                    })?;
                    if message_bytes > MAX_OBJECT_MESSAGE_BYTES {
                        return Err(limit(format!(
                            "HDF5 object header holds more than {MAX_OBJECT_MESSAGE_BYTES} bytes of messages (limit)"
                        )));
                    }
                    messages.push(Message { kind, body });
                }
                cursor = body_end;
            }
        }
        Ok(ObjectHeader { messages })
    }

    /// Version 2 object header ("OHDR"), HDF5 spec section IV.A.2.
    ///
    /// Wire layout (all little-endian):
    /// `OHDR` (4) | version=2 (1) | flags (1) |
    /// [access/mod/change/birth times, 4×u32, when flags bit 5] |
    /// [max-compact/min-dense attribute counts, 2×u16, when flags bit 4] |
    /// size-of-chunk-0 (1/2/4/8 bytes per flags bits 0-1) | messages |
    /// checksum (u32, Jenkins lookup3 over the chunk from the signature on).
    ///
    /// Messages: type (u8 — v1 uses u16), size (u16), flags (u8),
    /// [creation order (u16) when header flags bit 2], body — with NO
    /// inter-message 8-byte alignment (v1 pads). A trailing gap smaller
    /// than one message header may precede the checksum. Continuation
    /// messages (0x0010) point at "OCHK" blocks: signature (4) | messages |
    /// checksum (u32), whose stored length INCLUDES signature and checksum.
    fn parse_object_header_v2(&self, address: u64) -> Result<ObjectHeader> {
        let head = self.slice(address, 6)?;
        let version = head[4];
        if version != 2 {
            return Err(invalid(
                address as usize,
                format!("OHDR object header version {version} unsupported (need 2)"),
            ));
        }
        let flags = head[5];
        let address = address_to_usize(address)?;
        let mut cursor = address
            .checked_add(6)
            .ok_or_else(|| invalid(address, "HDF5 v2 header address overflow"))?;
        if flags & 0x20 != 0 {
            cursor = cursor
                .checked_add(16)
                .ok_or_else(|| invalid(cursor, "HDF5 v2 timestamp fields overflow"))?;
        }
        if flags & 0x10 != 0 {
            cursor = cursor
                .checked_add(4)
                .ok_or_else(|| invalid(cursor, "HDF5 v2 attribute fields overflow"))?;
        }
        let size_width = 1usize << (flags & 0x03);
        let chunk0_size = usize::try_from(read_uint(self.bytes, cursor, size_width)?)
            .map_err(|_| invalid(cursor, "HDF5 v2 chunk size overflows usize"))?;
        if chunk0_size > MAX_OBJECT_MESSAGE_BYTES {
            return Err(limit(format!(
                "HDF5 v2 header message block is {chunk0_size} bytes (limit {MAX_OBJECT_MESSAGE_BYTES})"
            )));
        }
        cursor = cursor
            .checked_add(size_width)
            .ok_or_else(|| invalid(cursor, "HDF5 v2 header cursor overflow"))?;
        // Creation-order tracking widens every message header by 2 bytes.
        let message_header = if flags & 0x04 != 0 { 6 } else { 4 };
        let mut messages = Vec::new();
        // (message region start, message region length, chunk start for the
        // checksum). Chunk 0's checksummed span begins at the signature.
        let mut blocks = vec![(cursor, chunk0_size, address)];
        let mut scheduled_blocks = BTreeSet::from([address]);
        let mut block_index = 0;
        let mut message_bytes = 0usize;
        while block_index < blocks.len() {
            if blocks.len() > MAX_HEADER_BLOCKS {
                return Err(limit(format!(
                    "HDF5 v2 header has more than {MAX_HEADER_BLOCKS} continuation blocks (limit)"
                )));
            }
            let (start, len, chunk_start) = blocks[block_index];
            block_index += 1;
            let end = start
                .checked_add(len)
                .ok_or_else(|| invalid(start, "HDF5 v2 message block overflow"))?;
            if chunk_start > end {
                return Err(invalid(chunk_start, "invalid HDF5 v2 checksum span"));
            }
            let stored = u32::from_le_bytes(array_at(self.bytes, end)?);
            let computed = jenkins_lookup3(self.slice(chunk_start as u64, end - chunk_start)?);
            if stored != computed {
                return Err(invalid(
                    chunk_start,
                    format!(
                        "HDF5 v2 object header checksum mismatch (stored {stored:#010x}, computed {computed:#010x})"
                    ),
                ));
            }
            let mut cursor = start;
            // Stop on the trailing gap: any leftover space smaller than one
            // message header is padding before the checksum.
            while cursor
                .checked_add(message_header)
                .is_some_and(|header_end| header_end <= end)
            {
                let header = self.slice(cursor as u64, message_header)?;
                let kind = u16::from(header[0]);
                let size = u16::from_le_bytes([header[1], header[2]]) as usize;
                // header[3] = message flags; header[4..6] = creation order.
                let body_start = cursor
                    .checked_add(message_header)
                    .ok_or_else(|| invalid(cursor, "HDF5 v2 message address overflow"))?;
                let body_end = body_start
                    .checked_add(size)
                    .ok_or_else(|| invalid(body_start, "HDF5 v2 message size overflow"))?;
                if body_end > end {
                    return Err(truncated(cursor, message_header + size, end - cursor));
                }
                let body = self.slice(body_start as u64, size)?.to_vec();
                if kind == 0x0010 {
                    let offset = address_to_usize(read_offset(&body, 0, self.offset_size)?)?;
                    let length =
                        usize::try_from(read_uint(&body, self.offset_size, self.length_size)?)
                            .map_err(|_| invalid(cursor, "HDF5 v2 continuation length overflow"))?;
                    if length < 8 {
                        return Err(invalid(cursor, "HDF5 v2 continuation block too short"));
                    }
                    if length > MAX_OBJECT_MESSAGE_BYTES {
                        return Err(limit(format!(
                            "HDF5 v2 continuation block is {length} bytes (limit {MAX_OBJECT_MESSAGE_BYTES})"
                        )));
                    }
                    if self.slice(offset as u64, 4)? != OCHK_SIGNATURE {
                        return Err(invalid(offset, "expected OCHK signature"));
                    }
                    if !scheduled_blocks.insert(offset) {
                        return Err(invalid(offset, "cycle in HDF5 v2 header continuations"));
                    }
                    let message_start = offset
                        .checked_add(4)
                        .ok_or_else(|| invalid(offset, "HDF5 v2 continuation address overflow"))?;
                    // Message region excludes the signature and checksum.
                    blocks.push((message_start, length - 8, offset));
                } else {
                    if messages.len() >= MAX_OBJECT_MESSAGES {
                        return Err(limit(format!(
                            "HDF5 v2 object header has more than {MAX_OBJECT_MESSAGES} messages (limit)"
                        )));
                    }
                    message_bytes = message_bytes
                        .checked_add(size)
                        .ok_or_else(|| invalid(cursor, "HDF5 v2 message byte count overflow"))?;
                    if message_bytes > MAX_OBJECT_MESSAGE_BYTES {
                        return Err(limit(format!(
                            "HDF5 v2 object header holds more than {MAX_OBJECT_MESSAGE_BYTES} bytes of messages (limit)"
                        )));
                    }
                    messages.push(Message { kind, body });
                }
                cursor = body_end;
            }
        }
        Ok(ObjectHeader { messages })
    }

    // ----- messages -----------------------------------------------------

    fn parse_dataspace(&self, body: &[u8]) -> Result<Vec<usize>> {
        let version = *body.first().ok_or_else(|| truncated(0, 1, 0))?;
        let rank = *body.get(1).ok_or_else(|| truncated(1, 1, body.len()))? as usize;
        if rank > MAX_DATASPACE_RANK {
            return Err(limit(format!(
                "HDF5 dataspace rank is {rank} (limit {MAX_DATASPACE_RANK})"
            )));
        }
        let dims_start: usize = match version {
            1 => 8, // version, rank, flags, reserved[5]
            2 => 4, // version, rank, flags, type
            other => {
                return Err(invalid(0, format!("dataspace version {other} unsupported")));
            }
        };
        let mut dims = Vec::with_capacity(rank);
        for index in 0..rank {
            let at = index
                .checked_mul(self.length_size)
                .and_then(|value| dims_start.checked_add(value))
                .ok_or_else(|| invalid(dims_start, "HDF5 dataspace cursor overflow"))?;
            let dim = usize::try_from(read_offset(body, at, self.length_size)?)
                .map_err(|_| invalid(at, "HDF5 dimension overflows usize"))?;
            if dim > MAX_DATASPACE_DIM {
                return Err(limit(format!(
                    "HDF5 dataspace dimension is {dim} (limit {MAX_DATASPACE_DIM})"
                )));
            }
            dims.push(dim);
        }
        Ok(dims)
    }

    fn parse_datatype(&self, body: &[u8]) -> Result<Datatype> {
        if body.len() < 8 {
            return Err(truncated(0, 8, body.len()));
        }
        let class = body[0] & 0x0F;
        let bits = u32::from_le_bytes([body[1], body[2], body[3], 0]);
        let size = u32::from_le_bytes([body[4], body[5], body[6], body[7]]) as usize;
        let big_endian = bits & 1 != 0;
        match class {
            0 if (1..=8).contains(&size) => Ok(Datatype {
                class: DtClass::Int {
                    signed: bits & (1 << 3) != 0,
                },
                size,
                big_endian,
            }),
            1 if matches!(size, 4 | 8) => Ok(Datatype {
                class: DtClass::Float,
                size,
                big_endian,
            }),
            3 if size <= MAX_HDF5_ATTRIBUTE_BYTES => Ok(Datatype {
                class: DtClass::FixedString,
                size,
                big_endian: false,
            }),
            9 if bits & 0x0F == 1 && size <= MAX_HDF5_ATTRIBUTE_BYTES => Ok(Datatype {
                class: DtClass::VlenString,
                size,
                big_endian: false,
            }),
            other => Err(invalid(
                0,
                format!("HDF5 datatype class {other} unsupported"),
            )),
        }
    }

    fn parse_layout(&self, body: &[u8]) -> Result<Layout> {
        let version = *body.first().ok_or_else(|| truncated(0, 1, 0))?;
        if version != 3 {
            return Err(invalid(
                0,
                format!("data layout message version {version} unsupported (need v3)"),
            ));
        }
        let class = *body.get(1).ok_or_else(|| truncated(1, 1, body.len()))?;
        match class {
            0 => {
                let size = usize::from(read_le_u16(body, 2)?);
                if size > MAX_HDF5_DATASET_BYTES {
                    return Err(limit(format!(
                        "HDF5 compact dataset is {size} bytes (limit {MAX_HDF5_DATASET_BYTES})"
                    )));
                }
                let end = 4usize
                    .checked_add(size)
                    .ok_or_else(|| invalid(4, "HDF5 compact layout size overflow"))?;
                let data = body
                    .get(4..end)
                    .ok_or_else(|| truncated(4, size, body.len()))?;
                Ok(Layout::Compact(data.to_vec()))
            }
            1 => Ok(Layout::Contiguous {
                address: read_offset(body, 2, self.offset_size)?,
                size: read_offset(body, 2 + self.offset_size, self.length_size)?,
            }),
            2 => {
                let dimensionality =
                    *body.get(2).ok_or_else(|| truncated(2, 1, body.len()))? as usize;
                if dimensionality == 0 || dimensionality > MAX_DATASPACE_RANK + 1 {
                    return Err(invalid(2, "invalid HDF5 chunk dimensionality"));
                }
                let btree_address = read_offset(body, 3, self.offset_size)?;
                let mut chunk_dims = Vec::with_capacity(dimensionality);
                for index in 0..dimensionality {
                    let at = index
                        .checked_mul(4)
                        .and_then(|value| 3usize.checked_add(self.offset_size)?.checked_add(value))
                        .ok_or_else(|| invalid(3, "HDF5 chunk-dimension cursor overflow"))?;
                    let dim = u32::from_le_bytes(array_at(body, at)?) as usize;
                    if dim == 0 {
                        return Err(invalid(at, "invalid HDF5 chunk dimension"));
                    }
                    if dim > MAX_DATASPACE_DIM {
                        return Err(limit(format!(
                            "HDF5 chunk dimension is {dim} (limit {MAX_DATASPACE_DIM})"
                        )));
                    }
                    chunk_dims.push(dim);
                }
                // The trailing entry is the element size; drop it.
                chunk_dims.pop();
                Ok(Layout::Chunked {
                    btree_address,
                    chunk_dims,
                })
            }
            other => Err(invalid(0, format!("data layout class {other} unsupported"))),
        }
    }

    fn parse_filter_pipeline(&self, body: &[u8]) -> Result<Vec<Filter>> {
        let version = *body.first().ok_or_else(|| truncated(0, 1, 0))?;
        let count = *body.get(1).ok_or_else(|| truncated(1, 1, body.len()))? as usize;
        if count > MAX_HDF5_FILTERS {
            return Err(limit(format!(
                "HDF5 filter pipeline has {count} filters (limit {MAX_HDF5_FILTERS})"
            )));
        }
        let mut filters = Vec::with_capacity(count);
        let mut cursor = match version {
            1 => 8,
            2 => 2,
            other => {
                return Err(invalid(
                    0,
                    format!("filter pipeline version {other} unsupported"),
                ));
            }
        };
        for _ in 0..count {
            let id = read_le_u16(body, cursor)?;
            let has_name = version == 1 || id >= 256;
            let name_len = if has_name {
                usize::from(read_le_u16(
                    body,
                    cursor
                        .checked_add(2)
                        .ok_or_else(|| invalid(cursor, "HDF5 filter cursor overflow"))?,
                )?)
            } else {
                0
            };
            let after_id = cursor
                .checked_add(if has_name { 4 } else { 2 })
                .ok_or_else(|| invalid(cursor, "HDF5 filter cursor overflow"))?;
            let value_count = usize::from(read_le_u16(
                body,
                after_id
                    .checked_add(2)
                    .ok_or_else(|| invalid(after_id, "HDF5 filter cursor overflow"))?,
            )?);
            if value_count > MAX_HDF5_FILTER_VALUES {
                return Err(limit(format!(
                    "HDF5 filter has {value_count} client values (limit {MAX_HDF5_FILTER_VALUES})"
                )));
            }
            let mut at = after_id
                .checked_add(4)
                .ok_or_else(|| invalid(after_id, "HDF5 filter cursor overflow"))?;
            if name_len > 0 {
                let padded_name = if version == 1 {
                    name_len
                        .checked_add(7)
                        .map(|value| value / 8 * 8)
                        .ok_or_else(|| invalid(at, "HDF5 filter name length overflow"))?
                } else {
                    name_len
                };
                checked_range(body, at, padded_name)?;
                at = at
                    .checked_add(padded_name)
                    .ok_or_else(|| invalid(at, "HDF5 filter name cursor overflow"))?;
            }
            let mut client_values = Vec::with_capacity(value_count);
            for index in 0..value_count {
                let value_at = index
                    .checked_mul(4)
                    .and_then(|value| at.checked_add(value))
                    .ok_or_else(|| invalid(at, "HDF5 filter value cursor overflow"))?;
                client_values.push(u32::from_le_bytes(array_at(body, value_at)?));
            }
            at = value_count
                .checked_mul(4)
                .and_then(|value| at.checked_add(value))
                .ok_or_else(|| invalid(at, "HDF5 filter value length overflow"))?;
            if version == 1 && value_count % 2 == 1 {
                checked_range(body, at, 4)?;
                at = at
                    .checked_add(4)
                    .ok_or_else(|| invalid(at, "HDF5 filter padding overflow"))?;
            }
            filters.push(Filter { id, client_values });
            cursor = at;
        }
        Ok(filters)
    }

    /// Parse one attribute message body; returns the value when the
    /// attribute's name matches.
    /// The attribute in `body`, with its name; `None` when `wanted` names
    /// another attribute.
    fn parse_attribute(
        &self,
        body: &[u8],
        wanted: Option<&str>,
    ) -> Result<Option<(String, H5Attr)>> {
        let version = *body.first().ok_or_else(|| truncated(0, 1, 0))?;
        if !(1..=3).contains(&version) {
            return Err(invalid(
                0,
                format!("attribute version {version} unsupported"),
            ));
        }
        let header_len = if version == 3 { 9 } else { 8 };
        if body.len() < header_len {
            return Err(truncated(0, header_len, body.len()));
        }
        let flags = body[1];
        if version >= 2 && flags & 0x03 != 0 {
            return Err(invalid(
                0,
                "shared attribute datatype/dataspace unsupported",
            ));
        }
        let name_size = usize::from(read_le_u16(body, 2)?);
        let dt_size = usize::from(read_le_u16(body, 4)?);
        let ds_size = usize::from(read_le_u16(body, 6)?);
        let mut cursor = header_len;
        let pad = |len: usize| -> Result<usize> {
            if version == 1 {
                len.checked_add(7)
                    .map(|value| value / 8 * 8)
                    .ok_or_else(|| invalid(0, "HDF5 attribute padding overflow"))
            } else {
                Ok(len)
            }
        };
        let name_bytes = checked_range(body, cursor, name_size)?;
        let name = name_bytes
            .split(|byte| *byte == 0)
            .next()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        cursor = cursor
            .checked_add(pad(name_size)?)
            .ok_or_else(|| invalid(cursor, "HDF5 attribute name cursor overflow"))?;
        if wanted.is_some_and(|wanted| name != wanted) {
            return Ok(None);
        }
        let name = name.into_owned();
        let dtype = self.parse_datatype(checked_range(body, cursor, dt_size)?)?;
        cursor = cursor
            .checked_add(pad(dt_size)?)
            .ok_or_else(|| invalid(cursor, "HDF5 attribute datatype cursor overflow"))?;
        let dims = self.parse_dataspace(checked_range(body, cursor, ds_size)?)?;
        cursor = cursor
            .checked_add(pad(ds_size)?)
            .ok_or_else(|| invalid(cursor, "HDF5 attribute dataspace cursor overflow"))?;
        let count = checked_product(&dims, "HDF5 attribute element count")?.max(1);
        checked_allocation_bytes(
            count,
            dtype.size,
            MAX_HDF5_ATTRIBUTE_BYTES,
            "HDF5 attribute",
        )?;
        let data = body
            .get(cursor..)
            .ok_or_else(|| truncated(cursor, 0, body.len()))?;
        self.attr_value(&dtype, count, data)
            .map(|value| Some((name, value)))
    }

    fn attr_value(&self, dtype: &Datatype, count: usize, data: &[u8]) -> Result<H5Attr> {
        if matches!(dtype.class, DtClass::Int { .. } | DtClass::Float) {
            // Reserve only for values the message actually holds.
            let needed = count.saturating_mul(dtype.size);
            if data.len() < needed {
                return Err(truncated(0, needed, data.len()));
            }
        }
        match dtype.class {
            DtClass::FixedString => {
                let bytes = data.get(..dtype.size.min(data.len())).unwrap_or_default();
                let text = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
                Ok(H5Attr::Str(String::from_utf8_lossy(text).into_owned()))
            }
            DtClass::VlenString => {
                // Element: u32 byte length + global heap reference
                // (collection address + u32 object index).
                if data.len() < 4 + self.offset_size + 4 {
                    return Err(truncated(0, 4 + self.offset_size + 4, data.len()));
                }
                let collection = read_offset(data, 4, self.offset_size)?;
                let index = u32::from_le_bytes(array_at(data, 4 + self.offset_size)?);
                let object = self.global_heap_object(collection, index)?;
                let text = object.split(|byte| *byte == 0).next().unwrap_or_default();
                Ok(H5Attr::Str(String::from_utf8_lossy(text).into_owned()))
            }
            DtClass::Int { signed } => {
                let mut values = Vec::with_capacity(count);
                for index in 0..count {
                    let raw = data
                        .get(index * dtype.size..(index + 1) * dtype.size)
                        .ok_or_else(|| truncated(index * dtype.size, dtype.size, data.len()))?;
                    values.push(read_int(raw, signed, dtype.big_endian));
                }
                Ok(if count == 1 {
                    H5Attr::I64(values[0])
                } else {
                    H5Attr::I64Array(values)
                })
            }
            DtClass::Float => {
                let mut values = Vec::with_capacity(count);
                for index in 0..count {
                    let raw = data
                        .get(index * dtype.size..(index + 1) * dtype.size)
                        .ok_or_else(|| truncated(index * dtype.size, dtype.size, data.len()))?;
                    values.push(read_float(raw, dtype.big_endian)?);
                }
                Ok(if count == 1 {
                    H5Attr::F64(values[0])
                } else {
                    H5Attr::F64Array(values)
                })
            }
        }
    }

    fn global_heap_object(&self, collection: u64, index: u32) -> Result<Vec<u8>> {
        let head = self.slice(collection, 8 + self.length_size)?;
        if &head[..4] != b"GCOL" {
            return Err(invalid(collection as usize, "expected GCOL signature"));
        }
        let overflow = || invalid(0, "HDF5 global heap offset overflow");
        let total =
            usize::try_from(read_offset(head, 8, self.length_size)?).map_err(|_| overflow())?;
        let start = address_to_usize(collection)?;
        let object_header = 8 + self.length_size;
        // `slice` above proved the collection header lies inside the file.
        let mut cursor = start + object_header;
        let end = start
            .checked_add(total)
            .ok_or_else(overflow)?
            .min(self.bytes.len());
        while cursor
            .checked_add(object_header)
            .is_some_and(|header_end| header_end <= end)
        {
            let object_index = u16::from_le_bytes([self.bytes[cursor], self.bytes[cursor + 1]]);
            let size = usize::try_from(read_offset(self.bytes, cursor + 8, self.length_size)?)
                .map_err(|_| overflow())?;
            if object_index == 0 {
                break; // free space marker terminates the collection
            }
            let data_start = cursor + object_header;
            if u32::from(object_index) == index {
                return Ok(self.slice(data_start as u64, size)?.to_vec());
            }
            cursor = size
                .div_ceil(8)
                .checked_mul(8)
                .and_then(|padded| data_start.checked_add(padded))
                .ok_or_else(overflow)?;
        }
        Err(invalid(
            collection as usize,
            format!("global heap object {index} not found"),
        ))
    }

    // ----- chunked data -------------------------------------------------

    fn read_chunked(
        &self,
        btree_address: u64,
        chunk_dims: &[usize],
        dims: &[usize],
        element_size: usize,
        filters: &[Filter],
    ) -> Result<Vec<u8>> {
        if chunk_dims.len() != dims.len() {
            return Err(invalid(
                0,
                "HDF5 chunk dimensionality does not match dataset rank",
            ));
        }
        let elements = checked_product(dims, "HDF5 chunked dataset element count")?;
        let total = checked_allocation_bytes(
            elements,
            element_size,
            MAX_HDF5_DATASET_BYTES,
            "HDF5 chunked dataset",
        )?;
        let mut out = vec![0u8; total];
        if btree_address == UNDEFINED_ADDR {
            return Ok(out); // dataset never written
        }
        let mut chunks = Vec::new();
        let mut visited_nodes = BTreeSet::new();
        self.collect_chunks(
            btree_address,
            chunk_dims.len() + 1,
            &mut chunks,
            &mut visited_nodes,
        )?;
        let chunk_elements = checked_product(chunk_dims, "HDF5 chunk element count")?;
        let chunk_bytes = checked_allocation_bytes(
            chunk_elements,
            element_size,
            MAX_HDF5_DATASET_BYTES,
            "HDF5 chunk",
        )?;
        for chunk in chunks {
            if chunk.stored_size > MAX_HDF5_DATASET_BYTES {
                return Err(limit(format!(
                    "HDF5 stored chunk is {} bytes (limit {MAX_HDF5_DATASET_BYTES})",
                    chunk.stored_size
                )));
            }
            let stored = self.slice(chunk.address, chunk.stored_size)?;
            let raw = apply_inverse_filters(
                stored,
                filters,
                chunk.filter_mask,
                element_size,
                chunk_bytes,
            )?;
            if raw.len() < chunk_bytes {
                return Err(invalid(
                    chunk.address as usize,
                    "decoded chunk shorter than chunk dimensions",
                ));
            }
            copy_chunk(
                &mut out,
                &raw,
                dims,
                chunk_dims,
                &chunk.offsets,
                element_size,
            );
        }
        Ok(out)
    }

    fn collect_chunks(
        &self,
        node_address: u64,
        key_dims: usize,
        out: &mut Vec<ChunkRef>,
        visited: &mut BTreeSet<u64>,
    ) -> Result<()> {
        if !visited.insert(node_address) {
            return Err(invalid(
                address_to_usize(node_address)?,
                "cycle in HDF5 chunk B-tree",
            ));
        }
        if visited.len() > MAX_BTREE_NODES {
            return Err(invalid(0, "HDF5 chunk B-tree too large"));
        }
        let node = self.slice(node_address, 8 + 2 * self.offset_size)?;
        if &node[..4] != b"TREE" {
            return Err(invalid(node_address as usize, "expected TREE signature"));
        }
        if node[4] != 1 {
            return Err(invalid(node_address as usize, "expected chunk B-tree node"));
        }
        let level = node[5];
        let entries = u16::from_le_bytes([node[6], node[7]]) as usize;
        let key_size = 8 + 8 * key_dims;
        let mut cursor = address_to_usize(node_address)?
            .checked_add(8 + 2 * self.offset_size)
            .ok_or_else(|| invalid(0, "HDF5 chunk B-tree cursor overflow"))?;
        for _ in 0..entries {
            let key = self.slice(cursor as u64, key_size)?;
            let stored_size = u32::from_le_bytes(array_at(key, 0)?) as usize;
            let filter_mask = u32::from_le_bytes(array_at(key, 4)?);
            let mut offsets = Vec::with_capacity(key_dims.saturating_sub(1));
            for dim in 0..key_dims.saturating_sub(1) {
                let at = 8 + dim * 8;
                let offset = u64::from_le_bytes(array_at(key, at)?);
                offsets.push(usize::try_from(offset).map_err(|_| {
                    invalid(
                        address_to_usize(node_address).unwrap_or(0),
                        "HDF5 chunk offset overflows usize",
                    )
                })?);
            }
            cursor = cursor
                .checked_add(key_size)
                .ok_or_else(|| invalid(cursor, "HDF5 chunk B-tree key overflow"))?;
            let child = read_offset(self.bytes, cursor, self.offset_size)?;
            cursor = cursor
                .checked_add(self.offset_size)
                .ok_or_else(|| invalid(cursor, "HDF5 chunk B-tree child overflow"))?;
            if level == 0 {
                if out.len() >= MAX_DATA_CHUNKS {
                    return Err(limit(format!(
                        "HDF5 dataset has more than {MAX_DATA_CHUNKS} chunks (limit)"
                    )));
                }
                out.push(ChunkRef {
                    address: child,
                    stored_size,
                    filter_mask,
                    offsets,
                });
            } else {
                self.collect_chunks(child, key_dims, out, visited)?;
            }
        }
        Ok(())
    }

    fn slice(&self, address: u64, len: usize) -> Result<&'a [u8]> {
        let start = address_to_usize(address)?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| invalid(start, "HDF5 byte range overflow"))?;
        self.bytes
            .get(start..end)
            .ok_or_else(|| truncated(start, len, self.bytes.len()))
    }
}

struct ObjectHeader {
    messages: Vec<Message>,
}

struct Message {
    kind: u16,
    body: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum DtClass {
    Int { signed: bool },
    Float,
    FixedString,
    VlenString,
}

#[derive(Clone, Copy, Debug)]
struct Datatype {
    class: DtClass,
    size: usize,
    big_endian: bool,
}

impl Datatype {
    /// Bytes per element of the [`H5Data`] storage [`Self::convert`] produces.
    fn decoded_element_bytes(&self) -> usize {
        match self.class {
            DtClass::Int { signed: false } if self.size <= 2 => self.size,
            DtClass::Int { .. } => 8,
            DtClass::Float => self.size,
            DtClass::FixedString | DtClass::VlenString => self.size,
        }
    }

    /// Convert a raw element buffer into the closest [`H5Data`] storage.
    fn convert(&self, raw: &[u8]) -> Result<H5Data> {
        match self.class {
            DtClass::Int { signed: false } if self.size == 1 => Ok(H5Data::U8(raw.to_vec())),
            DtClass::Int { signed: false } if self.size == 2 => Ok(H5Data::U16(
                raw.chunks_exact(2)
                    .map(|pair| {
                        if self.big_endian {
                            u16::from_be_bytes([pair[0], pair[1]])
                        } else {
                            u16::from_le_bytes([pair[0], pair[1]])
                        }
                    })
                    .collect(),
            )),
            DtClass::Int { signed } => Ok(H5Data::F64(
                raw.chunks_exact(self.size)
                    .map(|chunk| read_int(chunk, signed, self.big_endian) as f64)
                    .collect(),
            )),
            DtClass::Float if self.size == 4 => Ok(H5Data::F32(
                raw.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|quad| {
                        let bits = if self.big_endian {
                            u32::from_be_bytes(*quad)
                        } else {
                            u32::from_le_bytes(*quad)
                        };
                        f32::from_bits(bits)
                    })
                    .collect(),
            )),
            DtClass::Float if self.size == 8 => Ok(H5Data::F64(
                raw.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|oct| {
                        let bits = if self.big_endian {
                            u64::from_be_bytes(*oct)
                        } else {
                            u64::from_le_bytes(*oct)
                        };
                        f64::from_bits(bits)
                    })
                    .collect(),
            )),
            _ => Err(invalid(0, "unsupported dataset element type")),
        }
    }
}

enum Layout {
    Compact(Vec<u8>),
    Contiguous {
        address: u64,
        size: u64,
    },
    Chunked {
        btree_address: u64,
        chunk_dims: Vec<usize>,
    },
}

struct Filter {
    id: u16,
    client_values: Vec<u32>,
}

struct ChunkRef {
    address: u64,
    stored_size: usize,
    filter_mask: u32,
    offsets: Vec<usize>,
}

/// Run the inverse filter pipeline over one stored chunk. Filters apply in
/// reverse pipeline order on read: deflate (id 1) inflates, shuffle (id 2)
/// de-interleaves byte planes. `filter_mask` bit N set = filter N skipped.
fn apply_inverse_filters(
    stored: &[u8],
    filters: &[Filter],
    filter_mask: u32,
    element_size: usize,
    max_output: usize,
) -> Result<Vec<u8>> {
    if stored.len() > MAX_HDF5_DATASET_BYTES {
        return Err(limit(format!(
            "HDF5 stored filter input is {} bytes (limit {MAX_HDF5_DATASET_BYTES})",
            stored.len()
        )));
    }
    let mut data = stored.to_vec();
    for (index, filter) in filters.iter().enumerate().rev() {
        if filter_mask & (1 << index) != 0 {
            continue;
        }
        match filter.id {
            1 => {
                // gzip/deflate (zlib stream per the HDF5 deflate filter).
                let mut decoder = ZlibDecoder::new(&data[..]);
                let mut inflated = Vec::new();
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    let remaining = max_output.saturating_sub(inflated.len());
                    if remaining == 0 {
                        let mut probe = [0u8; 1];
                        let count = decoder
                            .read(&mut probe)
                            .map_err(|err| invalid(0, format!("HDF5 deflate chunk: {err}")))?;
                        if count != 0 {
                            return Err(limit(format!(
                                "HDF5 deflate chunk expands beyond its {max_output}-byte chunk size (limit)"
                            )));
                        }
                        break;
                    }
                    let read_len = remaining.min(chunk.len());
                    let count = decoder
                        .read(&mut chunk[..read_len])
                        .map_err(|err| invalid(0, format!("HDF5 deflate chunk: {err}")))?;
                    if count == 0 {
                        break;
                    }
                    inflated.try_reserve(count).map_err(|err| {
                        invalid(0, format!("cannot reserve HDF5 deflate output: {err}"))
                    })?;
                    inflated.extend_from_slice(&chunk[..count]);
                }
                data = inflated;
            }
            2 => {
                let size = filter
                    .client_values
                    .first()
                    .copied()
                    .map(|v| v as usize)
                    .unwrap_or(element_size)
                    .max(1);
                data = unshuffle(&data, size);
            }
            other => {
                return Err(invalid(0, format!("HDF5 filter id {other} unsupported")));
            }
        }
        if data.len() > max_output {
            return Err(limit(format!(
                "HDF5 filter output exceeds its {max_output}-byte chunk size (limit)"
            )));
        }
    }
    if data.len() > max_output {
        return Err(limit(format!(
            "HDF5 filter output exceeds its {max_output}-byte chunk size (limit)"
        )));
    }
    Ok(data)
}

/// Inverse of the HDF5 shuffle filter: byte plane k holds byte k of every
/// element; re-interleave.
fn unshuffle(data: &[u8], element_size: usize) -> Vec<u8> {
    if element_size <= 1 || !data.len().is_multiple_of(element_size) {
        return data.to_vec();
    }
    let count = data.len() / element_size;
    let mut out = vec![0u8; data.len()];
    for plane in 0..element_size {
        for element in 0..count {
            out[element * element_size + plane] = data[plane * count + element];
        }
    }
    out
}

/// Copy one decoded chunk into the dataset buffer, clipping edge chunks.
fn copy_chunk(
    out: &mut [u8],
    chunk: &[u8],
    dims: &[usize],
    chunk_dims: &[usize],
    offsets: &[usize],
    element_size: usize,
) {
    // Treat the dataset as (outer, row) where row = innermost dimension —
    // sufficient for the 1-D/2-D arrays polar volumes use; higher ranks
    // copy via the same row loop with composite outer indices.
    let rank = dims.len();
    if rank == 0 || chunk_dims.len() != rank || offsets.len() < rank {
        return;
    }
    let row_len = dims[rank - 1];
    let chunk_row_len = chunk_dims[rank - 1];
    let row_offset = offsets[rank - 1];
    let copy_cols = chunk_row_len.min(row_len.saturating_sub(row_offset));
    if copy_cols == 0 {
        return;
    }
    // Number of rows in the chunk = product of all but the last chunk dim.
    let chunk_rows: usize = chunk_dims[..rank - 1].iter().product::<usize>().max(1);
    for chunk_row in 0..chunk_rows {
        // Decompose the chunk row into per-dimension indices.
        let mut remaining = chunk_row;
        let mut out_index = 0usize;
        let mut in_bounds = true;
        for dim in 0..rank - 1 {
            let stride: usize = chunk_dims[dim + 1..rank - 1]
                .iter()
                .product::<usize>()
                .max(1);
            let local = remaining / stride;
            remaining %= stride;
            let Some(global) = offsets[dim].checked_add(local) else {
                in_bounds = false;
                break;
            };
            if global >= dims[dim] {
                in_bounds = false;
                break;
            }
            let out_stride: usize = dims[dim + 1..].iter().product();
            out_index += global * out_stride;
        }
        if !in_bounds {
            continue;
        }
        out_index += row_offset;
        let src = chunk_row * chunk_row_len * element_size;
        let dst = out_index * element_size;
        let len = copy_cols * element_size;
        if src + len <= chunk.len() && dst + len <= out.len() {
            out[dst..dst + len].copy_from_slice(&chunk[src..src + len]);
        }
    }
}

fn heap_string(bytes: &[u8], heap_data: u64, name_offset: u64) -> Result<String> {
    let start = heap_data
        .checked_add(name_offset)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid(0, "HDF5 local heap name offset overflow"))?;
    let tail = bytes
        .get(start..)
        .ok_or_else(|| truncated(start, 1, bytes.len()))?;
    let name = tail.split(|byte| *byte == 0).next().unwrap_or_default();
    Ok(String::from_utf8_lossy(name).into_owned())
}

fn read_u8(bytes: &[u8], at: usize) -> Result<u8> {
    bytes
        .get(at)
        .copied()
        .ok_or_else(|| truncated(at, 1, bytes.len()))
}

fn checked_range(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    let end = at
        .checked_add(len)
        .ok_or_else(|| invalid(at, "HDF5 byte range overflow"))?;
    bytes
        .get(at..end)
        .ok_or_else(|| truncated(at, len, bytes.len()))
}

fn read_le_u16(bytes: &[u8], at: usize) -> Result<u16> {
    let raw = checked_range(bytes, at, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn address_to_usize(address: u64) -> Result<usize> {
    usize::try_from(address).map_err(|_| invalid(0, "HDF5 address overflows usize"))
}

fn checked_product(values: &[usize], context: &'static str) -> Result<usize> {
    values.iter().try_fold(1usize, |product, value| {
        product
            .checked_mul(*value)
            .ok_or_else(|| invalid(0, format!("{context} overflow")))
    })
}

fn checked_allocation_bytes(
    count: usize,
    element_size: usize,
    limit: usize,
    context: &'static str,
) -> Result<usize> {
    let bytes = count
        .checked_mul(element_size)
        .ok_or_else(|| invalid(0, format!("{context} byte-size overflow")))?;
    if bytes > limit {
        return Err(OdimError::LimitExceeded(format!(
            "{context} requires {bytes} bytes (limit {limit})"
        )));
    }
    Ok(bytes)
}

/// Little-endian unsigned integer of `size` bytes (HDF5 metadata is always
/// little-endian).
fn read_offset(bytes: &[u8], at: usize, size: usize) -> Result<u64> {
    let raw = checked_range(bytes, at, size)?;
    let mut value = 0u64;
    for (index, byte) in raw.iter().enumerate() {
        value |= u64::from(*byte) << (8 * index);
    }
    // Map a size-4 undefined address (all ones) to the canonical sentinel.
    if size < 8 && value == (1u64 << (8 * size)) - 1 {
        return Ok(UNDEFINED_ADDR);
    }
    Ok(value)
}

/// Little-endian unsigned integer of `size` bytes WITHOUT the undefined-
/// address sentinel mapping of [`read_offset`] — for sizes and lengths,
/// where an all-ones value is a value, not "undefined".
fn read_uint(bytes: &[u8], at: usize, size: usize) -> Result<u64> {
    let raw = checked_range(bytes, at, size)?;
    let mut value = 0u64;
    for (index, byte) in raw.iter().enumerate() {
        value |= u64::from(*byte) << (8 * index);
    }
    Ok(value)
}

/// Bob Jenkins' lookup3 `hashlittle` over little-endian words — the
/// H5_checksum_lookup3 metadata checksum used by v2 object headers and
/// their continuation blocks (and other 1.8+ structures).
fn jenkins_lookup3(data: &[u8]) -> u32 {
    let init = 0xdead_beef_u32.wrapping_add(data.len() as u32);
    let (mut a, mut b, mut c) = (init, init, init);
    let word = |block: &[u8; 12], at: usize| {
        u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]])
    };
    let mut rest = data;
    // A final block of exactly 12 bytes goes through the tail path below.
    while rest.len() > 12
        && let Some((block, tail)) = rest.split_first_chunk::<12>()
    {
        a = a.wrapping_add(word(block, 0));
        b = b.wrapping_add(word(block, 4));
        c = c.wrapping_add(word(block, 8));
        // mix(a, b, c)
        a = a.wrapping_sub(c) ^ c.rotate_left(4);
        c = c.wrapping_add(b);
        b = b.wrapping_sub(a) ^ a.rotate_left(6);
        a = a.wrapping_add(c);
        c = c.wrapping_sub(b) ^ b.rotate_left(8);
        b = b.wrapping_add(a);
        a = a.wrapping_sub(c) ^ c.rotate_left(16);
        c = c.wrapping_add(b);
        b = b.wrapping_sub(a) ^ a.rotate_left(19);
        a = a.wrapping_add(c);
        c = c.wrapping_sub(b) ^ b.rotate_left(4);
        b = b.wrapping_add(a);
        rest = tail;
    }
    if rest.is_empty() {
        // hashlittle: a zero-length tail skips the final mix entirely.
        return c;
    }
    // The 1..=12 byte tail reads as three zero-padded words (the C switch
    // adds only the bytes present, which is the same thing).
    let mut tail = [0u8; 12];
    for (slot, byte) in tail.iter_mut().zip(rest) {
        *slot = *byte;
    }
    a = a.wrapping_add(word(&tail, 0));
    b = b.wrapping_add(word(&tail, 4));
    c = c.wrapping_add(word(&tail, 8));
    // final(a, b, c)
    c = (c ^ b).wrapping_sub(b.rotate_left(14));
    a = (a ^ c).wrapping_sub(c.rotate_left(11));
    b = (b ^ a).wrapping_sub(a.rotate_left(25));
    c = (c ^ b).wrapping_sub(b.rotate_left(16));
    a = (a ^ c).wrapping_sub(c.rotate_left(4));
    b = (b ^ a).wrapping_sub(a.rotate_left(14));
    c = (c ^ b).wrapping_sub(b.rotate_left(24));
    c
}

fn read_int(raw: &[u8], signed: bool, big_endian: bool) -> i64 {
    let mut value = 0u64;
    if big_endian {
        for byte in raw {
            value = (value << 8) | u64::from(*byte);
        }
    } else {
        for (index, byte) in raw.iter().enumerate() {
            value |= u64::from(*byte) << (8 * index);
        }
    }
    if signed && !raw.is_empty() && raw.len() < 8 {
        let sign_bit = 1u64 << (8 * raw.len() - 1);
        if value & sign_bit != 0 {
            value |= !((1u64 << (8 * raw.len())) - 1);
        }
    }
    value as i64
}

fn read_float(raw: &[u8], big_endian: bool) -> Result<f64> {
    if let Ok(word) = <[u8; 4]>::try_from(raw) {
        let bits = if big_endian {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        };
        return Ok(f64::from(f32::from_bits(bits)));
    }
    if let Ok(word) = <[u8; 8]>::try_from(raw) {
        let bits = if big_endian {
            u64::from_be_bytes(word)
        } else {
            u64::from_le_bytes(word)
        };
        return Ok(f64::from_bits(bits));
    }
    Err(invalid(0, format!("float width {} unsupported", raw.len())))
}

/// The `N` bytes of `bytes` starting at `at`.
fn array_at<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N]> {
    bytes
        .get(at..)
        .and_then(|tail| tail.first_chunk::<N>())
        .copied()
        .ok_or_else(|| truncated(at, N, bytes.len()))
}

fn limit(reason: String) -> OdimError {
    OdimError::LimitExceeded(reason)
}

fn invalid(offset: usize, reason: impl Into<String>) -> OdimError {
    OdimError::InvalidMessage {
        offset,
        reason: reason.into(),
    }
}

fn truncated(offset: usize, needed: usize, available: usize) -> OdimError {
    OdimError::Truncated {
        what: "HDF5 structure",
        offset,
        needed,
        available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(bytes: &[u8]) -> H5File<'_> {
        H5File {
            bytes,
            offset_size: 8,
            length_size: 8,
            objects: BTreeMap::new(),
        }
    }

    // Real HDF5 inputs: corpus entry `odim-bejab-20190606-0000-pvol`
    // (superblock v0, 8-byte offsets and lengths, version-1 object headers,
    // v1 group and chunk B-trees). Byte offsets: tools/golden_io_formats.py,
    // section `odim`, key `bejab_hdf5` (h5py `h5o.get_info` object-header
    // addresses and chunk info, plus a version-1 header / B-tree reader
    // written from the HDF5 file format specification).
    const BEJAB: &str = "odim-bejab-20190606-0000-pvol";

    fn corpus(id: &str) -> Vec<u8> {
        recast_radar_testdata::bytes(id).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn magic_sniffer_matches_signature_only() {
        // HDF5 format specification: the superblock signature is the 8 bytes
        // 89 48 44 46 0d 0a 1a 0a (golden signatures.bejab_first8).
        let bejab = corpus(BEJAB);
        assert_eq!(bejab[..8], [0x89, 0x48, 0x44, 0x46, 0x0d, 0x0a, 0x1a, 0x0a]);
        assert!(looks_like_hdf5_bytes(&bejab));
        assert!(looks_like_hdf5_bytes(&bejab[..8]));
        assert!(!looks_like_hdf5_bytes(&bejab[..7]));
        assert!(looks_like_hdf5_bytes(&corpus(
            "cfrad1-xsapr-sgp-20110520-ppi-netcdf4"
        )));
        assert!(!looks_like_hdf5_bytes(&corpus(
            "cfrad1-xsapr-sgp-20110520-ppi-classic"
        )));
        assert!(!looks_like_hdf5_bytes(&corpus(
            "l2-ktlx-20240315-000217-trim"
        )));
    }

    /// `open` reads every attribute once: a real attribute whose datatype is
    /// turned into an unsupported class (6, compound), or an attribute
    /// message turned into an attribute info message that points at a
    /// fractal heap (dense storage), fails to open instead of being skipped
    /// by `attr` and `attrs`.
    #[test]
    fn open_rejects_unreadable_and_dense_attributes() {
        let original = corpus(BEJAB);
        let file = H5File::open(&original).expect("real file opens");
        assert!(!file.attrs("/what").is_empty());
        let header = file.parse_object_header(file.objects["/what"]).unwrap();
        let body = header
            .messages
            .iter()
            .find(|message| message.kind == 0x000C)
            .expect("an attribute of /what")
            .body
            .clone();
        assert_eq!(body[0], 1, "version-1 attribute message");
        assert!(body.len() >= 18);
        let at = original
            .windows(body.len())
            .position(|window| window == body.as_slice())
            .expect("the message body is in the file");
        // Version 1: an 8-byte header, then the name padded to 8 bytes.
        let name_size = usize::from(u16::from_le_bytes([body[2], body[3]]));
        let datatype_at = at + 8 + name_size.div_ceil(8) * 8;

        let mut unreadable = original.clone();
        unreadable[datatype_at] = (unreadable[datatype_at] & 0xF0) | 6;
        let Err(err) = H5File::open(&unreadable) else {
            panic!("an unsupported attribute datatype must fail");
        };
        let text = err.to_string();
        assert!(
            text.contains("'/what'") && text.contains("class 6"),
            "{text}"
        );

        // A version-1 message header is type (u16), size (u16), flags and
        // three reserved bytes; the body follows.
        let mut dense = original.clone();
        dense[at - 8..at - 6].copy_from_slice(&0x0015u16.to_le_bytes());
        dense[at] = 0;
        dense[at + 1] = 0;
        dense[at + 2..at + 10].copy_from_slice(&ROOT_HEADER.to_le_bytes());
        dense[at + 10..at + 18].copy_from_slice(&UNDEFINED_ADDR.to_le_bytes());
        let Err(err) = H5File::open(&dense) else {
            panic!("dense attribute storage must fail");
        };
        assert!(err.to_string().contains("densely"), "{err}");
    }

    /// Root group object header: address 96 (h5py), 3 messages in 2 chunks;
    /// the first block starts at 112 and holds the continuation message whose
    /// body (offset, length) sits at 120 and points at 800.
    const ROOT_HEADER: u64 = 96;
    const ROOT_FIRST_BLOCK: u64 = 112;
    const ROOT_CONTINUATION_BODY: usize = 120;

    #[test]
    fn v1_object_header_rejects_continuation_cycle() {
        let mut bytes = corpus(BEJAB);
        assert_eq!((bytes[8], bytes[13], bytes[14]), (0, 8, 8));
        {
            let file = parser(&bytes);
            let header = file
                .parse_object_header(ROOT_HEADER)
                .expect("real root header parses");
            // nmesgs 3 including the continuation message.
            assert_eq!(header.messages.len(), 2);
        }
        let target = &bytes[ROOT_CONTINUATION_BODY..ROOT_CONTINUATION_BODY + 8];
        assert_eq!(u64::from_le_bytes(target.try_into().unwrap()), 800);
        // Point the continuation back at the header's own first block.
        bytes[ROOT_CONTINUATION_BODY..ROOT_CONTINUATION_BODY + 8]
            .copy_from_slice(&ROOT_FIRST_BLOCK.to_le_bytes());

        let file = parser(&bytes);
        let Err(err) = file.parse_object_header(ROOT_HEADER) else {
            panic!("continuation cycle must fail");
        };
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    /// Root symbol-table B-tree node at 136 (TREE, type 0, level 0, 3
    /// entries), first child pointer at 168 (an SNOD). Chunk B-tree of
    /// `dataset1/data1/data` at 3440 (TREE, type 1, level 0, 1 entry, key
    /// dimensionality 3), first child pointer at 3496 = h5py chunk byte offset
    /// 6112, stored size 103544.
    const GROUP_BTREE: u64 = 136;
    const GROUP_CHILD0: usize = 168;
    const CHUNK_BTREE: u64 = 3440;
    const CHUNK_CHILD0: usize = 3496;

    #[test]
    fn btree_walks_reject_self_references() {
        let original = corpus(BEJAB);
        {
            let file = parser(&original);
            let mut entries = Vec::new();
            file.collect_group_entries(GROUP_BTREE, &mut entries, &mut BTreeSet::new())
                .expect("real group B-tree walks");
            assert_eq!(entries.len(), 14, "h5py: 14 root children");
            let mut refs = Vec::new();
            file.collect_chunks(CHUNK_BTREE, 3, &mut refs, &mut BTreeSet::new())
                .expect("real chunk B-tree walks");
            assert_eq!(refs.len(), 1);
            assert_eq!((refs[0].address, refs[0].stored_size), (6112, 103_544));
        }

        // Group node: raise it to level 1 and point its first child at itself.
        let mut group = original.clone();
        assert_eq!(
            group[GROUP_BTREE as usize..GROUP_BTREE as usize + 6],
            *b"TREE\0\0"
        );
        group[GROUP_BTREE as usize + 5] = 1;
        group[GROUP_CHILD0..GROUP_CHILD0 + 8].copy_from_slice(&GROUP_BTREE.to_le_bytes());
        let file = parser(&group);
        let err = file
            .collect_group_entries(GROUP_BTREE, &mut Vec::new(), &mut BTreeSet::new())
            .expect_err("group B-tree cycle must fail");
        assert!(err.to_string().contains("cycle"), "{err}");

        // Chunk node: the same edit on the dataset's chunk B-tree.
        let mut chunks = original;
        assert_eq!(
            chunks[CHUNK_BTREE as usize..CHUNK_BTREE as usize + 6],
            *b"TREE\x01\0"
        );
        chunks[CHUNK_BTREE as usize + 5] = 1;
        chunks[CHUNK_CHILD0..CHUNK_CHILD0 + 8].copy_from_slice(&CHUNK_BTREE.to_le_bytes());
        let file = parser(&chunks);
        let err = file
            .collect_chunks(CHUNK_BTREE, 3, &mut Vec::new(), &mut BTreeSet::new())
            .expect_err("chunk B-tree cycle must fail");
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn unshuffle_reinterleaves_byte_planes() {
        // Two u16 elements 0x0201, 0x0403 shuffled = planes [01 03][02 04].
        let shuffled = [0x01, 0x03, 0x02, 0x04];
        assert_eq!(unshuffle(&shuffled, 2), vec![0x01, 0x02, 0x03, 0x04]);
        // Non-multiple lengths pass through untouched.
        assert_eq!(unshuffle(&[1, 2, 3], 2), vec![1, 2, 3]);
    }

    #[test]
    fn read_int_sign_extends_little_and_big_endian() {
        assert_eq!(read_int(&[0xFF], true, false), -1);
        assert_eq!(read_int(&[0xFF], false, false), 255);
        assert_eq!(read_int(&[0xFE, 0xFF], true, false), -2);
        assert_eq!(read_int(&[0xFF, 0xFE], true, true), -2);
        assert_eq!(read_int(&[0x2A, 0, 0, 0, 0, 0, 0, 0], false, false), 42);
    }

    #[test]
    fn undefined_addresses_normalize_across_offset_sizes() {
        assert_eq!(
            read_offset(&[0xFF, 0xFF, 0xFF, 0xFF], 0, 4).unwrap(),
            UNDEFINED_ADDR
        );
        assert_eq!(read_offset(&[0x10, 0, 0, 0], 0, 4).unwrap(), 0x10);
    }

    /// Unlike `read_offset`, `read_uint` must NOT map all-ones to the
    /// undefined sentinel — 0xFF is a legal 1-byte chunk size.
    #[test]
    fn read_uint_keeps_all_ones_values() {
        assert_eq!(read_uint(&[0xFF], 0, 1).unwrap(), 0xFF);
        assert_eq!(read_uint(&[0xFF, 0xFF], 0, 2).unwrap(), 0xFFFF);
        assert_eq!(read_uint(&[0x83, 0x01], 0, 2).unwrap(), 0x0183);
    }

    #[test]
    fn truncated_messages_return_errors_instead_of_indexing() {
        let file = parser(&[]);
        assert!(file.parse_attribute(&[1], Some("name")).is_err());
        let Err(_) = file.parse_layout(&[3, 0]) else {
            panic!("truncated compact layout must fail");
        };
        let Err(_) = file.parse_filter_pipeline(&[1, 1]) else {
            panic!("truncated filter pipeline must fail");
        };
    }

    /// Jenkins lookup3 (hashlittle) known-answer vectors. The 30-byte
    /// phrase with init 0 is the published lookup3 self-test value; the
    /// shorter vectors pin every tail-length branch class (empty, <4,
    /// exactly 12 = one full block, 13 = block + 1-byte tail) and were
    /// cross-checked against real HDF5 v2 header checksums (AEMET espdg
    /// PVOL fixture) with an independent Python implementation.
    #[test]
    fn jenkins_lookup3_matches_reference_vectors() {
        assert_eq!(jenkins_lookup3(b""), 0xdead_beef);
        assert_eq!(
            jenkins_lookup3(b"Four score and seven years ago"),
            0x1777_0551
        );
        assert_eq!(jenkins_lookup3(b"abc"), 0x0e39_7631);
        assert_eq!(jenkins_lookup3(b"0123456789ab"), 0x1065_e50a);
        assert_eq!(jenkins_lookup3(b"0123456789abc"), 0x7351_ce56);
    }
}
