use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{LittleEndian, ReadBytesExt};

use crate::error::{OpenSlideError, Result};

const INDEX_VERSION: &str = "01.02";

/// A hierarchical image entry from the index.
#[derive(Debug, Clone)]
pub struct HierEntry {
    pub image_index: i32,
    pub offset: i32,
    pub length: i32,
    pub fileno: i32,
}

/// A non-hierarchical record (associated image, position buffer, etc.)
#[derive(Debug, Clone)]
pub struct NonhierRecord {
    pub offset: i32,
    pub size: i32,
    pub fileno: i32,
}

/// Parsed Index.dat file handle.
pub struct IndexFile {
    reader: BufReader<std::fs::File>,
    hier_root: i64,
    nonhier_root: i64,
}

impl IndexFile {
    /// Open and validate an Index.dat file.
    pub fn open(path: &Path, expected_slide_id: &str) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| {
            OpenSlideError::Io(std::io::Error::new(
                e.kind(),
                format!("Cannot open index file {}: {}", path.display(), e),
            ))
        })?;
        let mut reader = BufReader::new(file);

        // Read and verify version
        let mut version_buf = [0u8; 5];
        reader
            .read_exact(&mut version_buf)
            .map_err(|e| OpenSlideError::Format(format!("Cannot read index version: {}", e)))?;
        let version = std::str::from_utf8(&version_buf)
            .map_err(|_| OpenSlideError::Format("Index version is not valid UTF-8".into()))?;
        if version != INDEX_VERSION {
            return Err(OpenSlideError::Format(format!(
                "Index.dat has unexpected version '{}', expected '{}'",
                version, INDEX_VERSION
            )));
        }

        // Read and verify slide ID
        let id_len = expected_slide_id.len();
        let mut id_buf = vec![0u8; id_len];
        reader.read_exact(&mut id_buf).map_err(|e| {
            OpenSlideError::Format(format!("Cannot read slide ID from index: {}", e))
        })?;
        let found_id = std::str::from_utf8(&id_buf)
            .map_err(|_| OpenSlideError::Format("Index slide ID is not valid UTF-8".into()))?;
        if found_id != expected_slide_id {
            return Err(OpenSlideError::Format(format!(
                "Index.dat slide ID '{}' doesn't match expected '{}'",
                found_id, expected_slide_id
            )));
        }

        // Root positions are right after version + slide_id
        let hier_root = (INDEX_VERSION.len() + id_len) as i64;
        let nonhier_root = hier_root + 4;

        Ok(Self {
            reader,
            hier_root,
            nonhier_root,
        })
    }

    fn read_i32(&mut self) -> Result<i32> {
        self.reader
            .read_i32::<LittleEndian>()
            .map_err(|e| OpenSlideError::Format(format!("Cannot read i32 from index: {}", e)))
    }

    fn seek(&mut self, pos: i64) -> Result<()> {
        self.reader.seek(SeekFrom::Start(pos as u64)).map_err(|e| {
            OpenSlideError::Format(format!("Cannot seek in index to {}: {}", pos, e))
        })?;
        Ok(())
    }

    /// Read a non-hierarchical record by record number.
    pub fn read_nonhier_record(&mut self, recordno: i32) -> Result<NonhierRecord> {
        if recordno < 0 {
            return Err(OpenSlideError::InvalidArgument(
                "Negative record number".into(),
            ));
        }

        self.seek(self.nonhier_root)?;
        let table_base = self.read_i32()?;

        // seek to record pointer
        self.seek(table_base as i64 + 4 * recordno as i64)?;
        let list_head = self.read_i32()?;

        // seek to list head
        self.seek(list_head as i64)?;

        // read initial value (0 means data follows, 0x302e3130 means empty)
        let initial = self.read_i32()?;
        if initial == 0x302e3130 {
            // Magic constant = empty section
            return Err(OpenSlideError::Format("Nonhier record is empty".into()));
        }
        if initial != 0 {
            return Err(OpenSlideError::Format(format!(
                "Expected 0 at beginning of data page, got {}",
                initial
            )));
        }

        // read pointer to data page
        let data_page = self.read_i32()?;
        self.seek(data_page as i64)?;

        // read page size (should be 1)
        let page_size = self.read_i32()?;
        if page_size < 1 {
            return Err(OpenSlideError::Format(
                "Expected at least one data item in nonhier record".into(),
            ));
        }

        // read next pointer (sometimes nonzero) and 2 zeros
        let _next_ptr = self.read_i32()?;
        let zero1 = self.read_i32()?;
        let zero2 = self.read_i32()?;
        if zero1 != 0 || zero2 != 0 {
            return Err(OpenSlideError::Format(
                "Expected zero values in nonhier record prologue".into(),
            ));
        }

        // read actual data
        let offset = self.read_i32()?;
        let size = self.read_i32()?;
        let fileno = self.read_i32()?;

        if offset < 0 || size < 0 || fileno < 0 {
            return Err(OpenSlideError::Format(
                "Negative value in nonhier record".into(),
            ));
        }

        Ok(NonhierRecord {
            offset,
            size,
            fileno,
        })
    }

    /// Read all hierarchical entries for all zoom levels.
    ///
    /// Returns a Vec (one per zoom level) of Vec<HierEntry>.
    pub fn read_hier_data_pages(
        &mut self,
        zoom_levels: i32,
        images_across: i32,
        images_down: i32,
    ) -> Result<Vec<Vec<HierEntry>>> {
        self.seek(self.hier_root)?;
        let root_ptr = self.read_i32()?;
        if root_ptr < 0 {
            return Err(OpenSlideError::Format(
                "Can't read initial hier pointer".into(),
            ));
        }

        let mut all_entries = Vec::with_capacity(zoom_levels as usize);
        let mut seek_location = root_ptr as i64;

        for zoom_level in 0..zoom_levels {
            self.seek(seek_location)?;
            let level_ptr = self.read_i32()?;
            if level_ptr < 0 {
                return Err(OpenSlideError::Format(format!(
                    "Can't read zoom level {} pointer",
                    zoom_level
                )));
            }

            self.seek(level_ptr as i64)?;

            // read initial 0
            let initial = self.read_i32()?;
            if initial != 0 {
                return Err(OpenSlideError::Format(format!(
                    "Expected 0 at beginning of data page for level {}",
                    zoom_level
                )));
            }

            // read pointer to first data page
            let first_page = self.read_i32()?;
            if first_page < 0 {
                return Err(OpenSlideError::Format(
                    "Can't read initial data page pointer".into(),
                ));
            }

            self.seek(first_page as i64)?;

            let mut entries = Vec::new();

            // Read linked list of data pages
            loop {
                let page_len = self.read_i32()?;
                if page_len < 0 {
                    return Err(OpenSlideError::Format("Can't read page length".into()));
                }

                let next_ptr = self.read_i32()?;
                if next_ptr < 0 {
                    return Err(OpenSlideError::Format("Can't read next pointer".into()));
                }

                for _ in 0..page_len {
                    let image_index = self.read_i32()?;
                    let offset = self.read_i32()?;
                    let length = self.read_i32()?;
                    let fileno = self.read_i32()?;

                    if image_index < 0 || offset < 0 || length < 0 || fileno < 0 {
                        return Err(OpenSlideError::Format(
                            "Negative value in hier entry".into(),
                        ));
                    }

                    let y = image_index / images_across;
                    if y >= images_down {
                        return Err(OpenSlideError::Format(format!(
                            "y ({}) outside of bounds for zoom level ({})",
                            y, zoom_level
                        )));
                    }

                    entries.push(HierEntry {
                        image_index,
                        offset,
                        length,
                        fileno,
                    });
                }

                if next_ptr == 0 {
                    break;
                }
                self.seek(next_ptr as i64)?;
            }

            all_entries.push(entries);
            seek_location += 4; // advance to next zoom level
        }

        Ok(all_entries)
    }

    /// Read tile entries from a single hier record at a given offset in the
    /// pointer table. The offset is the sequential index across all HIER layers
    /// (e.g. offset 0 = first HIER_0 level, offset 10 = first HIER_1 level, etc.)
    ///
    /// Returns Ok(entries) if the record contains tile data, or Err if it doesn't
    /// match the expected tile data page structure.
    pub fn read_hier_record_at_offset(&mut self, record_offset: i32) -> Result<Vec<HierEntry>> {
        self.seek(self.hier_root)?;
        let root_ptr = self.read_i32()?;
        if root_ptr < 0 {
            return Err(OpenSlideError::Format(
                "Can't read initial hier pointer".into(),
            ));
        }

        let seek_location = root_ptr as i64 + record_offset as i64 * 4;
        self.seek(seek_location)?;
        let level_ptr = self.read_i32()?;
        if level_ptr < 0 {
            return Err(OpenSlideError::Format(format!(
                "Can't read hier record pointer at offset {}",
                record_offset
            )));
        }

        self.seek(level_ptr as i64)?;

        let initial = self.read_i32()?;
        if initial != 0 {
            return Err(OpenSlideError::Format(format!(
                "Expected 0 at beginning of data page at offset {}, got {}",
                record_offset, initial
            )));
        }

        let first_page = self.read_i32()?;
        if first_page < 0 {
            return Err(OpenSlideError::Format(
                "Can't read initial data page pointer".into(),
            ));
        }

        self.seek(first_page as i64)?;

        let mut entries = Vec::new();
        loop {
            let page_len = self.read_i32()?;
            if page_len < 0 {
                return Err(OpenSlideError::Format("Can't read page length".into()));
            }

            let next_ptr = self.read_i32()?;

            for _ in 0..page_len {
                let image_index = self.read_i32()?;
                let offset = self.read_i32()?;
                let length = self.read_i32()?;
                let fileno = self.read_i32()?;

                entries.push(HierEntry {
                    image_index,
                    offset,
                    length,
                    fileno,
                });
            }

            if next_ptr <= 0 {
                break;
            }
            self.seek(next_ptr as i64)?;
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;
    use std::io::Write;

    /// Build a minimal synthetic Index.dat for testing.
    fn build_test_index(slide_id: &str) -> Vec<u8> {
        let mut buf = Vec::new();

        // version
        buf.write_all(b"01.02").unwrap();
        // slide ID
        buf.write_all(slide_id.as_bytes()).unwrap();

        let hier_root_pos = buf.len();

        // hier root pointer (will point to zoom level table)
        // nonhier root pointer (will point to nonhier table)
        // We'll fill these in after we know the positions.
        buf.write_i32::<LittleEndian>(0).unwrap(); // hier_root -> placeholder
        buf.write_i32::<LittleEndian>(0).unwrap(); // nonhier_root -> placeholder

        // === Build a simple hier level with 1 entry ===

        // Zoom level pointer table (pointed to by hier_root ptr)
        let zoom_table_pos = buf.len();
        buf.write_i32::<LittleEndian>(0).unwrap(); // pointer for level 0 -> placeholder

        // Level 0 list head
        let list_head_pos = buf.len();
        buf.write_i32::<LittleEndian>(0).unwrap(); // initial 0
        buf.write_i32::<LittleEndian>(0).unwrap(); // pointer to first data page -> placeholder

        // Data page for level 0
        let data_page_pos = buf.len();
        buf.write_i32::<LittleEndian>(1).unwrap(); // page_len = 1 entry
        buf.write_i32::<LittleEndian>(0).unwrap(); // next_ptr = 0 (no more pages)

        // One entry: image_index=0, offset=1000, length=2000, fileno=0
        buf.write_i32::<LittleEndian>(0).unwrap(); // image_index
        buf.write_i32::<LittleEndian>(1000).unwrap(); // offset
        buf.write_i32::<LittleEndian>(2000).unwrap(); // length
        buf.write_i32::<LittleEndian>(0).unwrap(); // fileno

        // === Build a simple nonhier section ===
        // nonhier table
        let nonhier_table_pos = buf.len();
        buf.write_i32::<LittleEndian>(0).unwrap(); // record 0 pointer -> placeholder

        // record 0 list head
        let nonhier_list_head_pos = buf.len();
        buf.write_i32::<LittleEndian>(0).unwrap(); // initial 0
        buf.write_i32::<LittleEndian>(0).unwrap(); // pointer to data page -> placeholder

        // nonhier data page
        let nonhier_data_page_pos = buf.len();
        buf.write_i32::<LittleEndian>(1).unwrap(); // page_size = 1
        buf.write_i32::<LittleEndian>(0).unwrap(); // next (sometimes nonzero)
        buf.write_i32::<LittleEndian>(0).unwrap(); // zero
        buf.write_i32::<LittleEndian>(0).unwrap(); // zero
        buf.write_i32::<LittleEndian>(5000).unwrap(); // offset
        buf.write_i32::<LittleEndian>(3000).unwrap(); // size
        buf.write_i32::<LittleEndian>(0).unwrap(); // fileno

        // Now patch all the pointers

        // hier_root -> zoom_table_pos
        let hier_ptr_pos = hier_root_pos;
        buf[hier_ptr_pos..hier_ptr_pos + 4].copy_from_slice(&(zoom_table_pos as i32).to_le_bytes());

        // zoom_table[0] -> list_head_pos
        buf[zoom_table_pos..zoom_table_pos + 4]
            .copy_from_slice(&(list_head_pos as i32).to_le_bytes());

        // list_head: pointer to data_page_pos
        let list_head_ptr_offset = list_head_pos + 4;
        buf[list_head_ptr_offset..list_head_ptr_offset + 4]
            .copy_from_slice(&(data_page_pos as i32).to_le_bytes());

        // nonhier_root -> nonhier_table_pos
        let nonhier_ptr_pos = hier_root_pos + 4;
        buf[nonhier_ptr_pos..nonhier_ptr_pos + 4]
            .copy_from_slice(&(nonhier_table_pos as i32).to_le_bytes());

        // nonhier_table[0] -> nonhier_list_head_pos
        buf[nonhier_table_pos..nonhier_table_pos + 4]
            .copy_from_slice(&(nonhier_list_head_pos as i32).to_le_bytes());

        // nonhier list head: pointer to nonhier_data_page_pos
        let nonhier_list_head_ptr = nonhier_list_head_pos + 4;
        buf[nonhier_list_head_ptr..nonhier_list_head_ptr + 4]
            .copy_from_slice(&(nonhier_data_page_pos as i32).to_le_bytes());

        buf
    }

    #[test]
    fn test_index_open_and_read_hier() {
        let slide_id = "test-slide-123";
        let data = build_test_index(slide_id);

        let dir = std::env::temp_dir().join("openslide_test_index");
        let _ = std::fs::create_dir_all(&dir);
        let index_path = dir.join("Index.dat");
        std::fs::write(&index_path, &data).unwrap();

        let mut idx = IndexFile::open(&index_path, slide_id).unwrap();

        // Read 1 zoom level, 1 image across, 1 image down
        let entries = idx.read_hier_data_pages(1, 1, 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].len(), 1);
        assert_eq!(entries[0][0].image_index, 0);
        assert_eq!(entries[0][0].offset, 1000);
        assert_eq!(entries[0][0].length, 2000);
        assert_eq!(entries[0][0].fileno, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_index_read_nonhier() {
        let slide_id = "test-slide-123";
        let data = build_test_index(slide_id);

        let dir = std::env::temp_dir().join("openslide_test_index_nh");
        let _ = std::fs::create_dir_all(&dir);
        let index_path = dir.join("Index.dat");
        std::fs::write(&index_path, &data).unwrap();

        let mut idx = IndexFile::open(&index_path, slide_id).unwrap();
        let record = idx.read_nonhier_record(0).unwrap();
        assert_eq!(record.offset, 5000);
        assert_eq!(record.size, 3000);
        assert_eq!(record.fileno, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_index_wrong_version() {
        let mut data = Vec::new();
        data.extend_from_slice(b"02.00");
        data.extend_from_slice(b"id");

        let dir = std::env::temp_dir().join("openslide_test_index_badver");
        let _ = std::fs::create_dir_all(&dir);
        let index_path = dir.join("Index.dat");
        std::fs::write(&index_path, &data).unwrap();

        let result = IndexFile::open(&index_path, "id");
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_index_wrong_slide_id() {
        let mut data = Vec::new();
        data.extend_from_slice(b"01.02");
        data.extend_from_slice(b"wrong-id");

        let dir = std::env::temp_dir().join("openslide_test_index_badid");
        let _ = std::fs::create_dir_all(&dir);
        let index_path = dir.join("Index.dat");
        std::fs::write(&index_path, &data).unwrap();

        let result = IndexFile::open(&index_path, "expected-id");
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
