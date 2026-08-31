//! Index.dat reader.
//!
//! Implemented from the MIRAX container specification, §5. The three points
//! that a reader most easily gets wrong, and that this module is careful about:
//!
//! * the header fields are at **fixed** offsets — the slide-ID field is 32
//!   bytes whatever the ID's own length;
//! * a record list is a **chain** of `{count, next}` pages, and a `count` of 0
//!   does not end it — only `next == 0` does;
//! * hierarchical records are 16 bytes, non-hierarchical records are **20**,
//!   because the latter carry an explicit `(x, y)`.

use std::path::Path;

use crate::error::{OpenSlideError, Result};

/// Byte offsets and widths of the index header (§5.1).
const VERSION_LEN: i64 = 5;
const SLIDE_ID_LEN: i64 = 32;
const HIER_ROOT_OFFSET: i64 = VERSION_LEN + SLIDE_ID_LEN; // 37
const NONHIER_ROOT_OFFSET: i64 = HIER_ROOT_OFFSET + 4; // 41

const HIER_RECORD_LEN: usize = 16;
const NONHIER_RECORD_LEN: usize = 20;

/// A hierarchical image entry from the index (§5.6).
#[derive(Debug, Clone)]
pub struct HierEntry {
    /// Packed tile position, `y * IMAGENUMBER_X + x`.
    pub image_index: i32,
    pub offset: i32,
    pub length: i32,
    pub fileno: i32,
}

/// A non-hierarchical record (associated image, position buffer, ...) (§5.6).
#[derive(Debug, Clone)]
pub struct NonhierRecord {
    pub x: i32,
    pub y: i32,
    pub offset: i32,
    pub size: i32,
    pub fileno: i32,
}

/// Index version, from the 5-byte header (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexVersion {
    /// `01.01` — no non-hierarchical root.
    V0101,
    /// `01.02`.
    V0102,
}

/// Parsed Index.dat file handle.
pub struct IndexFile {
    reader: crate::util::OpenSlideFile,
    version: IndexVersion,
    hier_root: i32,
    nonhier_root: i32,
}

impl IndexFile {
    /// Open and validate an Index.dat file.
    pub fn open(path: &Path, expected_slide_id: &str) -> Result<Self> {
        let mut reader = crate::util::_openslide_fopen(path)?;

        let mut version_buf = [0u8; VERSION_LEN as usize];
        crate::util::_openslide_fread_exact(&mut reader, &mut version_buf)
            .map_err(|e| OpenSlideError::Format(format!("Cannot read index version: {}", e)))?;
        let version = match &version_buf {
            b"01.01" => IndexVersion::V0101,
            b"01.02" => IndexVersion::V0102,
            other => {
                return Err(OpenSlideError::Format(format!(
                    "Index.dat has unexpected version '{}', expected '01.01' or '01.02'",
                    String::from_utf8_lossy(other)
                )))
            }
        };

        // The slide-ID field is a fixed 32 bytes, right-aligned and left-padded.
        // Deriving its length from the expected ID would shift every later
        // offset on a slide whose SLIDE_ID is not exactly 32 characters.
        let mut id_buf = [0u8; SLIDE_ID_LEN as usize];
        crate::util::_openslide_fread_exact(&mut reader, &mut id_buf).map_err(|e| {
            OpenSlideError::Format(format!("Cannot read slide ID from index: {}", e))
        })?;
        let found_id = std::str::from_utf8(&id_buf)
            .map_err(|_| OpenSlideError::Format("Index slide ID is not valid UTF-8".into()))?
            .trim();
        // 01.01 predates the cross-check; only 01.02 is required to match.
        if version == IndexVersion::V0102 && found_id != expected_slide_id {
            return Err(OpenSlideError::Format(format!(
                "Index.dat slide ID '{}' doesn't match expected '{}'",
                found_id, expected_slide_id
            )));
        }

        let mut file = Self {
            reader,
            version,
            hier_root: 0,
            nonhier_root: 0,
        };
        file.seek_index(HIER_ROOT_OFFSET)?;
        file.hier_root = file.read_i32()?;
        if version == IndexVersion::V0102 {
            file.seek_index(NONHIER_ROOT_OFFSET)?;
            file.nonhier_root = file.read_i32()?;
        }
        Ok(file)
    }

    /// `01.01` indexes carry no non-hierarchical layers at all.
    pub fn has_nonhier(&self) -> bool {
        self.version == IndexVersion::V0102
    }

    fn read_i32(&mut self) -> Result<i32> {
        let mut buf = [0u8; 4];
        crate::util::_openslide_fread_exact(&mut self.reader, &mut buf)
            .map_err(|e| OpenSlideError::Format(format!("Cannot read i32 from index: {}", e)))?;
        Ok(i32::from_le_bytes(buf))
    }

    fn seek_index(&mut self, pos: i64) -> Result<()> {
        if pos < 0 {
            return Err(OpenSlideError::Format(format!(
                "Negative index offset {}",
                pos
            )));
        }
        crate::util::_openslide_fseek(&mut self.reader, pos, crate::util::OpenSlideSeekWhence::Set)
            .map_err(|e| OpenSlideError::Format(format!("Cannot seek in index to {}: {}", pos, e)))
    }

    /// Read one root-table slot. `root` is the table's file offset, `entry` the
    /// slot index within it. A slot of 0 means "no records for this address".
    fn root_slot(&mut self, root: i32, entry: i32) -> Result<i32> {
        if entry < 0 {
            return Err(OpenSlideError::InvalidArgument(
                "Negative index table entry".into(),
            ));
        }
        self.seek_index(root as i64 + 4 * entry as i64)?;
        self.read_i32()
    }

    /// Walk a page chain, collecting every record.
    ///
    /// Each page is `{i32 count, i32 next}` followed by `count` records of
    /// `record_len` bytes. The first page of a chain is commonly a stub with
    /// `count == 0` whose `next` points at the real page, but that is a
    /// convention and not a rule: a non-zero count on the first page is read
    /// like any other.
    fn walk_pages(&mut self, first_page: i32, record_len: usize) -> Result<Vec<[i32; 5]>> {
        let mut out = Vec::new();
        let mut page = first_page;
        let mut seen = 0usize;
        while page != 0 {
            if page < 0 {
                return Err(OpenSlideError::Format("Negative index page pointer".into()));
            }
            // A malformed file can make the chain loop; bound the walk.
            seen += 1;
            if seen > 1 << 24 {
                return Err(OpenSlideError::Format("Index page chain too long".into()));
            }

            self.seek_index(page as i64)?;
            let count = self.read_i32()?;
            let next = self.read_i32()?;
            if count < 0 {
                return Err(OpenSlideError::Format("Negative index page count".into()));
            }
            for _ in 0..count {
                let mut rec = [0i32; 5];
                for slot in rec.iter_mut().take(record_len / 4) {
                    *slot = self.read_i32()?;
                }
                out.push(rec);
            }
            page = next;
        }
        Ok(out)
    }

    /// All hierarchical records at a root-table entry (§5.3 gives the entry).
    pub fn hier_records(&mut self, entry: i32) -> Result<Vec<HierEntry>> {
        let head = self.root_slot(self.hier_root, entry)?;
        if head == 0 {
            return Ok(Vec::new());
        }
        let raw = self.walk_pages(head, HIER_RECORD_LEN)?;
        let mut out = Vec::with_capacity(raw.len());
        for r in raw {
            let entry = HierEntry {
                image_index: r[0],
                offset: r[1],
                length: r[2],
                fileno: r[3],
            };
            // A negative image_index is the format's "no position" sentinel;
            // such a record is skipped rather than treated as corruption.
            if entry.image_index < 0 {
                continue;
            }
            if entry.offset < 0 || entry.length < 0 || entry.fileno < 0 {
                return Err(OpenSlideError::Format(format!(
                    "Corrupt hierarchical record: offset={}, length={}, fileno={}",
                    entry.offset, entry.length, entry.fileno
                )));
            }
            out.push(entry);
        }
        Ok(out)
    }

    /// All non-hierarchical records at a root-table entry (§5.4 gives the entry).
    ///
    /// Returns every record of the chain, not just the first: a layer level can
    /// legitimately hold several, distinguished by `(x, y)`.
    pub fn nonhier_records(&mut self, entry: i32) -> Result<Vec<NonhierRecord>> {
        if !self.has_nonhier() {
            return Ok(Vec::new());
        }
        let head = self.root_slot(self.nonhier_root, entry)?;
        if head == 0 {
            return Ok(Vec::new());
        }
        let raw = self.walk_pages(head, NONHIER_RECORD_LEN)?;
        let mut out = Vec::with_capacity(raw.len());
        for r in raw {
            let rec = NonhierRecord {
                x: r[0],
                y: r[1],
                offset: r[2],
                size: r[3],
                fileno: r[4],
            };
            if rec.x < 0 {
                continue; // negative sentinel
            }
            if rec.offset < 0 || rec.size < 0 || rec.fileno < 0 {
                return Err(OpenSlideError::Format(format!(
                    "Corrupt non-hierarchical record: offset={}, size={}, fileno={}",
                    rec.offset, rec.size, rec.fileno
                )));
            }
            out.push(rec);
        }
        Ok(out)
    }

    /// The record at `(x, 0)` of a non-hierarchical layer level, if present.
    ///
    /// Most layers hold a single record at the origin; the position and
    /// intensity-correction records of the stitching layer are distinguished by
    /// `x`.
    pub fn nonhier_record_at(&mut self, entry: i32, x: i32) -> Result<Option<NonhierRecord>> {
        Ok(self
            .nonhier_records(entry)?
            .into_iter()
            .find(|r| r.x == x && r.y == 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::LittleEndian;
    use byteorder::WriteBytesExt;
    use std::io::Write;

    const TEST_ID: &str = "0123456789ABCDEF0123456789ABCDEF";

    struct IndexBuilder {
        buf: Vec<u8>,
    }

    impl IndexBuilder {
        fn new(version: &[u8; 5], slide_id: &str) -> Self {
            let mut buf = Vec::new();
            buf.write_all(version).unwrap();
            let mut id = slide_id.as_bytes().to_vec();
            id.resize(SLIDE_ID_LEN as usize, b' ');
            buf.write_all(&id).unwrap();
            buf.write_i32::<LittleEndian>(0).unwrap(); // hier root
            buf.write_i32::<LittleEndian>(0).unwrap(); // nonhier root
            Self { buf }
        }

        fn here(&self) -> i32 {
            self.buf.len() as i32
        }

        fn put_i32(&mut self, v: i32) {
            self.buf.write_i32::<LittleEndian>(v).unwrap();
        }

        fn patch(&mut self, at: usize, v: i32) {
            self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }

        /// A chain: a stub page with count 0, then a page holding `records`.
        fn page_chain(&mut self, records: &[&[i32]]) -> i32 {
            let stub = self.here();
            self.put_i32(0); // count
            let next_slot = self.buf.len();
            self.put_i32(0); // next, patched below
            let real = self.here();
            self.patch(next_slot, real);
            self.put_i32(records.len() as i32);
            self.put_i32(0); // last page
            for r in records {
                for v in r.iter() {
                    self.put_i32(*v);
                }
            }
            stub
        }

        fn finish(mut self, hier: &[i32], nonhier: &[i32]) -> Vec<u8> {
            let hier_root = self.here();
            for v in hier {
                self.put_i32(*v);
            }
            let nonhier_root = self.here();
            for v in nonhier {
                self.put_i32(*v);
            }
            self.patch(HIER_ROOT_OFFSET as usize, hier_root);
            self.patch(NONHIER_ROOT_OFFSET as usize, nonhier_root);
            self.buf
        }
    }

    fn write_temp(name: &str, data: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("openslide_mirax_index_{}", name));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Index.dat");
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn reads_hier_records_across_a_page_chain() {
        let mut b = IndexBuilder::new(b"01.02", TEST_ID);
        let head = b.page_chain(&[&[7, 1000, 2000, 0], &[8, 3000, 400, 1]]);
        let data = b.finish(&[0, head], &[]);
        let path = write_temp("hier", &data);

        let mut idx = IndexFile::open(&path, TEST_ID).unwrap();
        assert!(idx.hier_records(0).unwrap().is_empty(), "slot 0 is empty");
        let recs = idx.hier_records(1).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].image_index, 7);
        assert_eq!(recs[0].offset, 1000);
        assert_eq!(recs[1].fileno, 1);
    }

    /// The bug this replaces: only the first record of the first page was
    /// reachable, and any record not at (0, 0) was rejected outright.
    #[test]
    fn reads_every_nonhier_record_including_non_origin() {
        let mut b = IndexBuilder::new(b"01.02", TEST_ID);
        let head = b.page_chain(&[&[0, 0, 296, 100, 3], &[1, 0, 396, 200, 3]]);
        let data = b.finish(&[], &[head]);
        let path = write_temp("nonhier", &data);

        let mut idx = IndexFile::open(&path, TEST_ID).unwrap();
        let recs = idx.nonhier_records(0).unwrap();
        assert_eq!(recs.len(), 2, "both records must be reachable");
        assert_eq!(recs[1].x, 1);
        assert_eq!(recs[1].offset, 396);

        let second = idx.nonhier_record_at(0, 1).unwrap().unwrap();
        assert_eq!(second.size, 200);
        assert!(idx.nonhier_record_at(0, 9).unwrap().is_none());
    }

    /// A first page carrying records directly is legal; only `next == 0` ends
    /// the chain.
    #[test]
    fn first_page_may_carry_records() {
        let mut b = IndexBuilder::new(b"01.02", TEST_ID);
        let head = b.here();
        b.put_i32(1); // count on the *first* page
        b.put_i32(0); // no next
        for v in [5, 100, 50, 0] {
            b.put_i32(v);
        }
        let data = b.finish(&[head], &[]);
        let path = write_temp("firstpage", &data);

        let mut idx = IndexFile::open(&path, TEST_ID).unwrap();
        let recs = idx.hier_records(0).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].image_index, 5);
    }

    #[test]
    fn accepts_version_0101_without_a_nonhier_root() {
        let mut b = IndexBuilder::new(b"01.01", TEST_ID);
        let head = b.page_chain(&[&[3, 8, 9, 0]]);
        let data = b.finish(&[head], &[]);
        let path = write_temp("v0101", &data);

        let mut idx = IndexFile::open(&path, "a-different-id").unwrap();
        assert!(!idx.has_nonhier());
        assert_eq!(idx.hier_records(0).unwrap().len(), 1);
        assert!(idx.nonhier_records(0).unwrap().is_empty());
    }

    /// Header offsets are fixed: a short SLIDE_ID must not shift them.
    #[test]
    fn header_offsets_do_not_depend_on_the_slide_id_length() {
        let short = "SHORT-ID";
        let mut b = IndexBuilder::new(b"01.02", short);
        let head = b.page_chain(&[&[11, 12, 13, 0]]);
        let data = b.finish(&[head], &[]);
        let path = write_temp("shortid", &data);

        let mut idx = IndexFile::open(&path, short).unwrap();
        assert_eq!(idx.hier_records(0).unwrap()[0].image_index, 11);
    }

    #[test]
    fn negative_position_sentinel_is_skipped_not_fatal() {
        let mut b = IndexBuilder::new(b"01.02", TEST_ID);
        let head = b.page_chain(&[&[-1, 0, 0, 0], &[4, 10, 20, 0]]);
        let data = b.finish(&[head], &[]);
        let path = write_temp("sentinel", &data);

        let mut idx = IndexFile::open(&path, TEST_ID).unwrap();
        let recs = idx.hier_records(0).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].image_index, 4);
    }

    #[test]
    fn rejects_an_unknown_version() {
        let data = IndexBuilder::new(b"02.00", TEST_ID).finish(&[], &[]);
        let path = write_temp("badver", &data);
        assert!(IndexFile::open(&path, TEST_ID).is_err());
    }

    #[test]
    fn rejects_a_mismatched_slide_id_on_0102() {
        let data = IndexBuilder::new(b"01.02", TEST_ID).finish(&[], &[]);
        let path = write_temp("badid", &data);
        assert!(IndexFile::open(&path, "0000000000000000000000000000FFFF").is_err());
    }
}
