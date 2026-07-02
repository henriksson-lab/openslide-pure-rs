use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff::OpenslideHash, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const SAKURA_MAGIC: &[u8] = b"SVGigaPixelImage";
const SCAN_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
struct SakuraLevel {
    width: u64,
    height: u64,
    downsample: f64,
    tile_size: u32,
}

struct SakuraSlide {
    levels: Vec<SakuraLevel>,
    properties: HashMap<String, String>,
    sqlite: Option<SqliteDatabase>,
    tile_source: Option<TileSource>,
    tile_index: Mutex<Option<TileBlobIndex>>,
    associated_images: HashMap<String, Vec<u8>>,
}

#[derive(Debug, Clone)]
struct TileSource {
    table_name: String,
    root_page: u32,
    columns: Vec<String>,
    blob_col: usize,
    address: TileAddress,
    level_col: Option<usize>,
    focal_plane: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileAddress {
    Xy { x_col: usize, y_col: usize },
    Linear { index_col: usize },
    SakuraTileId { id_col: usize },
}

#[derive(Debug, Clone, Default)]
struct TileBlobIndex {
    tiles: HashMap<TileKey, Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TileKey {
    level: Option<i64>,
    color: Option<u8>,
    x: i64,
    y: i64,
}

pub fn detect(path: &Path) -> bool {
    sakura_detect(path).unwrap_or(false)
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if !detect(path) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Sakura svslide file".into(),
        ));
    }

    Ok(Box::new(SakuraSlide::open(path)?))
}

impl SakuraSlide {
    fn open(path: &Path) -> Result<Self> {
        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), "sakura".into());

        let sqlite = SqliteDatabase::open(path).ok();
        let mut tile_source = sqlite
            .as_ref()
            .and_then(|db| find_sakura_tile_id_source(db).or_else(|| find_tile_source(db)));
        if let Some(db) = &sqlite {
            add_schema_properties(db, &mut properties);
            add_properties(db, &mut properties);
            add_sqlite_metadata_properties(db, &mut properties);
        }
        let associated_images = sqlite
            .as_ref()
            .map(|db| find_associated_images(db, tile_source.as_ref()))
            .transpose()?
            .unwrap_or_default();

        if let Some(source) = &tile_source {
            properties.insert("sakura.tile-table".into(), source.table_name.clone());
            properties.insert(
                "sakura.tile-blob-column".into(),
                source.columns[source.blob_col].clone(),
            );
            properties.insert(
                "sakura.tile-x-column".into(),
                match source.address {
                    TileAddress::Xy { x_col, .. } => source.columns[x_col].clone(),
                    TileAddress::Linear { index_col } => source.columns[index_col].clone(),
                    TileAddress::SakuraTileId { id_col } => source.columns[id_col].clone(),
                },
            );
            if let TileAddress::Xy { y_col, .. } = source.address {
                properties.insert("sakura.tile-y-column".into(), source.columns[y_col].clone());
            }
            if let Some(level_col) = source.level_col {
                properties.insert(
                    "sakura.tile-level-column".into(),
                    source.columns[level_col].clone(),
                );
            }
        }
        if let (Some(db), Some(source)) = (&sqlite, &tile_source) {
            if let Some(quickhash1) = compute_quickhash1(db, source)? {
                properties.insert(properties::PROPERTY_QUICKHASH1.into(), quickhash1);
            }
        }

        let header = if let Some(db) = &sqlite {
            match read_header(db)? {
                Some(header) => Some(header),
                None => find_header_like_record(path)?,
            }
        } else {
            find_header_like_record(path)?
        };

        let mut levels = if let Some(header) = header {
            if let Some(source) = &mut tile_source {
                if matches!(source.address, TileAddress::SakuraTileId { .. }) {
                    source.focal_plane = Some(chosen_focal_plane(header.focal_planes));
                }
            }
            properties.insert(
                "sakura.Header.tile-size".into(),
                header.tile_size.to_string(),
            );
            properties.insert("sakura.Header.width".into(), header.width.to_string());
            properties.insert("sakura.Header.height".into(), header.height.to_string());
            properties.insert(
                "sakura.Header.focal-planes".into(),
                header.focal_planes.to_string(),
            );
            let upstream_levels = if let (Some(db), Some(source)) = (&sqlite, &tile_source) {
                build_sakura_tile_id_levels(db, source, header)?
            } else {
                Vec::new()
            };
            if upstream_levels.is_empty() {
                vec![SakuraLevel {
                    width: header.width,
                    height: header.height,
                    downsample: 1.0,
                    tile_size: header.tile_size,
                }]
            } else {
                upstream_levels
            }
        } else {
            Vec::new()
        };

        if levels.is_empty() {
            if let (Some(db), Some(source)) = (&sqlite, &tile_source) {
                if let Some(level) = infer_single_level(db, source)? {
                    properties.insert(
                        "sakura.inferred-tile-size".into(),
                        level.tile_size.to_string(),
                    );
                    levels.push(level);
                }
            }
        }
        for (name, data) in &associated_images {
            if let Ok(format) = detect_image_format(data) {
                if let Ok(image) = decode::decode_to_rgba(format, data) {
                    properties.insert(
                        format!("openslide.associated.{name}.width"),
                        image.width.to_string(),
                    );
                    properties.insert(
                        format!("openslide.associated.{name}.height"),
                        image.height.to_string(),
                    );
                }
            }
        }

        Ok(Self {
            levels,
            properties,
            sqlite,
            tile_source,
            tile_index: Mutex::new(None),
            associated_images,
        })
    }

    fn cached_tile_blob(
        &self,
        db: &SqliteDatabase,
        source: &TileSource,
        level: u32,
        level_data: &SakuraLevel,
        channel: u32,
        tile_x: i64,
        tile_y: i64,
    ) -> Result<Option<Vec<u8>>> {
        let mut guard = self
            .tile_index
            .lock()
            .map_err(|_| OpenSlideError::Format("Sakura tile index cache is poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(db.build_tile_index(source, &self.levels)?);
        }
        let Some(index) = guard.as_ref() else {
            return Ok(None);
        };
        let key = TileKey {
            level: match source.address {
                TileAddress::SakuraTileId { .. } => Some(level_data.downsample as i64),
                _ => source.level_col.map(|_| level as i64),
            },
            color: match source.address {
                TileAddress::SakuraTileId { .. } => Some(channel as u8),
                _ => None,
            },
            x: tile_x,
            y: tile_y,
        };
        Ok(index.tiles.get(&key).cloned())
    }
}

fn sakura_detect(path: &Path) -> Result<bool> {
    let db = SqliteDatabase::open(path)?;
    let unique_table_name = get_unique_table_name(&db)?;
    let Some(unique_table) = db
        .tables
        .iter()
        .find(|table| table.name == unique_table_name)
    else {
        return Ok(false);
    };
    let rows = db.read_table_rows(unique_table.root_page)?;
    Ok(has_sakura_magic_bytes(unique_table, &rows))
}

fn get_unique_table_name(db: &SqliteDatabase) -> Result<String> {
    let Some(table) = db
        .tables
        .iter()
        .find(|table| table.name == "DataManagerSQLiteConfigXPO")
    else {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing Sakura unique table config".into(),
        ));
    };
    let Some(table_name_col) = column_index(table, "TableName") else {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing Sakura TableName column".into(),
        ));
    };
    let rows = db.read_table_rows(table.root_page)?;
    if rows.len() != 1 {
        return Err(OpenSlideError::Format(
            "Found != 1 Sakura unique tables".into(),
        ));
    }
    rows[0]
        .text(table_name_col)
        .map(str::to_string)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Missing Sakura unique table name".into()))
}

fn has_sakura_magic_bytes(table: &SqliteTable, rows: &[SqliteRow]) -> bool {
    let Some(id_col) = column_index(table, "id") else {
        return false;
    };
    let Some(data_col) = column_index(table, "data") else {
        return false;
    };
    rows.iter().any(|row| {
        row.text(id_col) == Some("++MagicBytes") && row.bytes(data_col) == Some(SAKURA_MAGIC)
    })
}

fn column_index(table: &SqliteTable, name: &str) -> Option<usize> {
    table
        .columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case(name))
}

impl SlideBackend for SakuraSlide {
    fn vendor(&self) -> &'static str {
        "sakura"
    }

    fn channel_count(&self) -> u32 {
        3
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        ["red", "green", "blue"].get(channel as usize).copied()
    }

    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (level.width, level.height))
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.levels
            .get(level as usize)
            .map(|level| level.downsample)
    }

    fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        if channel >= self.channel_count() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (slide has {} channels)",
                channel,
                self.channel_count()
            )));
        }
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        let db = self.sqlite.as_ref().ok_or_else(|| {
            OpenSlideError::UnsupportedFormat("Sakura SQLite schema could not be read".into())
        })?;
        let source = self.tile_source.as_ref().ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(
                "Sakura SQLite schema does not expose a recognized tile BLOB table".into(),
            )
        })?;

        let lx = x as f64 / level_data.downsample;
        let ly = y as f64 / level_data.downsample;
        let mut output = GrayImage::new(w, h);
        let tile_size = level_data.tile_size as f64;
        let col_start = (lx / tile_size).floor().max(0.0) as i64;
        let col_end = ((lx + w as f64) / tile_size).ceil().max(0.0) as i64;
        let row_start = (ly / tile_size).floor().max(0.0) as i64;
        let row_end = ((ly + h as f64) / tile_size).ceil().max(0.0) as i64;

        for row in row_start..row_end {
            for col in col_start..col_end {
                let Some(blob) =
                    self.cached_tile_blob(db, source, level, level_data, channel, col, row)?
                else {
                    continue;
                };
                if matches!(source.address, TileAddress::SakuraTileId { .. }) {
                    let tile = decode::decode_channel(ImageFormat::Jpeg, &blob, 0)?;
                    blit_gray_channel(
                        &tile,
                        &mut output,
                        col as f64 * tile_size - lx,
                        row as f64 * tile_size - ly,
                    );
                } else {
                    let (rgb, tile_w, tile_h) = decode_tile_blob(&blob)?;
                    blit_rgb_channel(
                        &rgb,
                        tile_w,
                        tile_h,
                        channel,
                        &mut output,
                        col as f64 * tile_size - lx,
                        row as f64 * tile_size - ly,
                    );
                }
            }
        }

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        let mut names = self
            .associated_images
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let data = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{name}'"))
        })?;
        decode::decode_to_rgba(detect_image_format(data)?, data)
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        let Some(level) = self.levels.get(level as usize) else {
            return 0;
        };
        let across = level.width.div_ceil(level.tile_size as u64);
        let down = level.height.div_ceil(level.tile_size as u64);
        across.saturating_mul(down).min(usize::MAX as u64) as usize
    }
}

#[derive(Debug, Clone)]
struct SqliteDatabase {
    path: PathBuf,
    page_size: usize,
    reserved_bytes: usize,
    tables: Vec<SqliteTable>,
}

#[derive(Debug, Clone)]
struct SqliteTable {
    name: String,
    root_page: u32,
    columns: Vec<String>,
}

#[derive(Debug, Clone)]
struct SqliteRow {
    values: Vec<SqliteValue>,
}

#[derive(Debug, Clone)]
enum SqliteValue {
    Null,
    Integer(i64),
    Real(f64),
    Blob(Vec<u8>),
    Text(String),
}

impl SqliteDatabase {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0u8; 100];
        file.read_exact(&mut header)?;
        if &header[..SQLITE_MAGIC.len()] != SQLITE_MAGIC {
            return Err(OpenSlideError::UnsupportedFormat(
                "Not a SQLite file".into(),
            ));
        }
        let page_size = match u16::from_be_bytes([header[16], header[17]]) {
            1 => 65536,
            512..=32768 => u16::from_be_bytes([header[16], header[17]]) as usize,
            other => {
                return Err(OpenSlideError::Format(format!(
                    "Unsupported SQLite page size {other}"
                )))
            }
        };
        let reserved_bytes = header[20] as usize;
        if reserved_bytes >= page_size {
            return Err(OpenSlideError::Format(
                "Invalid SQLite reserved byte count".into(),
            ));
        }
        let mut db = Self {
            path: path.to_path_buf(),
            page_size,
            reserved_bytes,
            tables: Vec::new(),
        };
        db.tables = db.read_schema_tables()?;
        Ok(db)
    }

    fn read_schema_tables(&self) -> Result<Vec<SqliteTable>> {
        let rows = self.read_table_rows(1)?;
        let mut tables = Vec::new();
        for row in rows {
            if row.text(0) != Some("table") {
                continue;
            }
            let Some(name) = row.text(1) else {
                continue;
            };
            if name.starts_with("sqlite_") {
                continue;
            }
            let Some(root_page) = row.integer(3).and_then(|value| u32::try_from(value).ok()) else {
                continue;
            };
            let Some(sql) = row.text(4) else {
                continue;
            };
            let columns = parse_create_table_columns(sql);
            if !columns.is_empty() {
                tables.push(SqliteTable {
                    name: name.to_string(),
                    root_page,
                    columns,
                });
            }
        }
        Ok(tables)
    }

    fn read_table_rows(&self, root_page: u32) -> Result<Vec<SqliteRow>> {
        let mut file = File::open(&self.path)?;
        let mut rows = Vec::new();
        self.read_btree_page(&mut file, root_page, &mut rows, 0)?;
        Ok(rows)
    }

    fn build_tile_index(
        &self,
        source: &TileSource,
        levels: &[SakuraLevel],
    ) -> Result<TileBlobIndex> {
        let mut index = TileBlobIndex::default();
        for row in self.read_table_rows(source.root_page)? {
            let Some(blob) = row.blob(source.blob_col) else {
                continue;
            };
            let level = source
                .level_col
                .and_then(|level_col| row.integer(level_col));
            let Some((x, y)) = tile_coordinates(source, &row, level, levels) else {
                continue;
            };
            let (level, color) = match source.address {
                TileAddress::SakuraTileId { id_col } => {
                    let Some(tile_id) = row.text(id_col) else {
                        continue;
                    };
                    let Some(parsed) = parse_tileid(tile_id)? else {
                        continue;
                    };
                    if Some(parsed.focal_plane) != source.focal_plane {
                        continue;
                    }
                    (Some(parsed.downsample), Some(parsed.color as u8))
                }
                _ => (level, None),
            };
            index
                .tiles
                .insert(TileKey { level, color, x, y }, blob.to_vec());
        }
        Ok(index)
    }

    fn read_btree_page(
        &self,
        file: &mut File,
        page_no: u32,
        rows: &mut Vec<SqliteRow>,
        depth: u32,
    ) -> Result<()> {
        if page_no == 0 || depth > 64 {
            return Err(OpenSlideError::Format(
                "Invalid or excessively deep SQLite btree".into(),
            ));
        }
        let page = self.read_page(file, page_no)?;
        let header_offset = if page_no == 1 { 100 } else { 0 };
        if page.len() < header_offset + 8 {
            return Err(OpenSlideError::Format("Short SQLite btree page".into()));
        }
        let page_type = page[header_offset];
        let cell_count = read_be_u16(&page[header_offset + 3..header_offset + 5]) as usize;
        let cell_ptrs_start = header_offset
            + if matches!(page_type, 0x05 | 0x02) {
                12
            } else {
                8
            };
        if page.len() < cell_ptrs_start + cell_count * 2 {
            return Err(OpenSlideError::Format(
                "SQLite btree cell pointer array exceeds page".into(),
            ));
        }

        match page_type {
            0x0d => {
                for i in 0..cell_count {
                    let ptr =
                        read_be_u16(&page[cell_ptrs_start + i * 2..cell_ptrs_start + i * 2 + 2])
                            as usize;
                    rows.push(self.parse_leaf_table_cell(file, &page, ptr)?);
                }
            }
            0x05 => {
                for i in 0..cell_count {
                    let ptr =
                        read_be_u16(&page[cell_ptrs_start + i * 2..cell_ptrs_start + i * 2 + 2])
                            as usize;
                    if ptr + 4 > page.len() {
                        return Err(OpenSlideError::Format(
                            "SQLite interior table cell exceeds page".into(),
                        ));
                    }
                    let child = read_be_u32(&page[ptr..ptr + 4]);
                    self.read_btree_page(file, child, rows, depth + 1)?;
                }
                let rightmost = read_be_u32(&page[header_offset + 8..header_offset + 12]);
                self.read_btree_page(file, rightmost, rows, depth + 1)?;
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Sakura SQLite table uses unsupported btree page type 0x{other:02x}"
                )))
            }
        }
        Ok(())
    }

    fn read_page(&self, file: &mut File, page_no: u32) -> Result<Vec<u8>> {
        let offset = (page_no as u64 - 1) * self.page_size as u64;
        let mut page = vec![0; self.page_size];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut page)?;
        Ok(page)
    }

    fn parse_leaf_table_cell(
        &self,
        file: &mut File,
        page: &[u8],
        offset: usize,
    ) -> Result<SqliteRow> {
        let (payload_len, n1) = read_varint(&page[offset..])?;
        let (_rowid, n2) = read_varint(&page[offset + n1..])?;
        let start = offset + n1 + n2;
        let payload = self.read_cell_payload(file, page, start, payload_len as usize)?;
        parse_record(&payload)
    }

    fn read_cell_payload(
        &self,
        file: &mut File,
        page: &[u8],
        start: usize,
        payload_len: usize,
    ) -> Result<Vec<u8>> {
        if payload_len == 0 {
            return Ok(Vec::new());
        }
        let local_len = self.local_payload_len(payload_len);
        let local_end = start
            .checked_add(local_len)
            .ok_or_else(|| OpenSlideError::Format("SQLite local payload overflow".into()))?;
        if local_end > page.len() {
            return Err(OpenSlideError::Format(
                "SQLite local payload exceeds page".into(),
            ));
        }

        let mut payload = Vec::with_capacity(payload_len);
        payload.extend_from_slice(&page[start..local_end]);
        if payload.len() == payload_len {
            return Ok(payload);
        }

        if local_end + 4 > page.len() {
            return Err(OpenSlideError::Format(
                "SQLite overflow page pointer exceeds page".into(),
            ));
        }
        let mut overflow_page = read_be_u32(&page[local_end..local_end + 4]);
        let usable = self.usable_size();
        while overflow_page != 0 && payload.len() < payload_len {
            let overflow = self.read_page(file, overflow_page)?;
            if overflow.len() < 4 {
                return Err(OpenSlideError::Format("Short SQLite overflow page".into()));
            }
            overflow_page = read_be_u32(&overflow[..4]);
            let remaining = payload_len - payload.len();
            let take = remaining.min(usable.saturating_sub(4));
            if 4 + take > overflow.len() {
                return Err(OpenSlideError::Format(
                    "SQLite overflow payload exceeds page".into(),
                ));
            }
            payload.extend_from_slice(&overflow[4..4 + take]);
        }
        if payload.len() != payload_len {
            return Err(OpenSlideError::Format(
                "SQLite overflow chain ended before payload was complete".into(),
            ));
        }
        Ok(payload)
    }

    fn local_payload_len(&self, payload_len: usize) -> usize {
        let usable = self.usable_size();
        let max_local = usable.saturating_sub(35);
        if payload_len <= max_local {
            return payload_len;
        }
        let min_local = ((usable.saturating_sub(12)) * 32 / 255).saturating_sub(23);
        let mut local = min_local + ((payload_len - min_local) % usable.saturating_sub(4));
        if local > max_local {
            local = min_local;
        }
        local
    }

    fn usable_size(&self) -> usize {
        self.page_size - self.reserved_bytes
    }
}

impl SqliteRow {
    fn integer(&self, index: usize) -> Option<i64> {
        match self.values.get(index) {
            Some(SqliteValue::Integer(value)) => Some(*value),
            Some(SqliteValue::Real(value)) => {
                if value.is_finite()
                    && value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value <= i64::MAX as f64
                {
                    Some(*value as i64)
                } else {
                    None
                }
            }
            Some(SqliteValue::Text(value)) => value.trim().parse().ok(),
            _ => None,
        }
    }

    fn float(&self, index: usize) -> Option<f64> {
        match self.values.get(index) {
            Some(SqliteValue::Real(value)) if value.is_finite() => Some(*value),
            Some(SqliteValue::Integer(value)) => Some(*value as f64),
            Some(SqliteValue::Text(value)) => value.trim().parse().ok(),
            _ => None,
        }
    }

    fn text(&self, index: usize) -> Option<&str> {
        match self.values.get(index) {
            Some(SqliteValue::Text(value)) => Some(value),
            _ => None,
        }
    }

    fn blob(&self, index: usize) -> Option<&[u8]> {
        match self.values.get(index) {
            Some(SqliteValue::Blob(value)) => Some(value),
            _ => None,
        }
    }

    fn bytes(&self, index: usize) -> Option<&[u8]> {
        match self.values.get(index) {
            Some(SqliteValue::Blob(value)) => Some(value),
            Some(SqliteValue::Text(value)) => Some(value.as_bytes()),
            _ => None,
        }
    }

    fn value_as_property(&self, index: usize) -> Option<String> {
        match self.values.get(index) {
            Some(SqliteValue::Integer(value)) => Some(value.to_string()),
            Some(SqliteValue::Real(value)) if value.is_finite() => Some(format_float(*value)),
            Some(SqliteValue::Text(value)) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            Some(SqliteValue::Blob(value)) if value.len() <= 128 => std::str::from_utf8(value)
                .ok()
                .map(str::trim)
                .and_then(|s| (!s.is_empty() && !s.contains('\0')).then(|| s.to_string())),
            _ => None,
        }
    }
}

fn find_tile_source(db: &SqliteDatabase) -> Option<TileSource> {
    db.tables
        .iter()
        .filter_map(|table| Some((tile_source_score(table), tile_source_from_table(table)?)))
        .max_by_key(|(score, _)| *score)
        .map(|(_, source)| source)
}

fn find_sakura_tile_id_source(db: &SqliteDatabase) -> Option<TileSource> {
    let unique_table_name = get_unique_table_name(db).ok()?;
    let table = db
        .tables
        .iter()
        .find(|table| table.name == unique_table_name)?;
    Some(TileSource {
        table_name: table.name.clone(),
        root_page: table.root_page,
        columns: table.columns.clone(),
        blob_col: column_index(table, "data")?,
        address: TileAddress::SakuraTileId {
            id_col: column_index(table, "id")?,
        },
        level_col: None,
        focal_plane: None,
    })
}

fn tile_source_from_table(table: &SqliteTable) -> Option<TileSource> {
    let address = if let (Some(x_col), Some(y_col)) = (
        find_column(
            &table.columns,
            &[
                "tileposx",
                "tilepositionx",
                "tilex",
                "tilecol",
                "tilecolumn",
                "tilecolumnindex",
                "columnindex",
                "posx",
                "xpos",
                "xposition",
                "x",
                "col",
                "column",
            ],
        ),
        find_column(
            &table.columns,
            &[
                "tileposy",
                "tilepositiony",
                "tiley",
                "tilerow",
                "tilerowindex",
                "rowindex",
                "posy",
                "ypos",
                "yposition",
                "y",
                "row",
            ],
        ),
    ) {
        TileAddress::Xy { x_col, y_col }
    } else {
        TileAddress::Linear {
            index_col: find_column(
                &table.columns,
                &[
                    "tileindex",
                    "tileidx",
                    "tileid",
                    "tileposition",
                    "tilepos",
                    "tileno",
                    "tilenumber",
                    "position",
                    "index",
                ],
            )?,
        }
    };
    let excluded = address_columns(&address);
    let blob_col = find_blob_column(&table.columns, &excluded)?;
    let level_col = find_column(
        &table.columns,
        &[
            "level",
            "levelno",
            "levelindex",
            "pyramidlevel",
            "pyramid",
            "resolution",
            "resolutionindex",
            "zoom",
            "zoomlevel",
            "z",
        ],
    );
    Some(TileSource {
        table_name: table.name.clone(),
        root_page: table.root_page,
        columns: table.columns.clone(),
        blob_col,
        address,
        level_col,
        focal_plane: None,
    })
}

fn tile_source_score(table: &SqliteTable) -> i32 {
    let mut score = table.columns.len().min(10) as i32;
    let table_name = normalize_identifier(&table.name);
    if table_name.contains("tile") {
        score += 20;
    }
    if table_name.contains("pyramid") || table_name.contains("level") {
        score += 10;
    }
    if table_name.contains("image") {
        score += 5;
    }
    score
}

fn address_columns(address: &TileAddress) -> Vec<usize> {
    match *address {
        TileAddress::Xy { x_col, y_col } => vec![x_col, y_col],
        TileAddress::Linear { index_col } => vec![index_col],
        TileAddress::SakuraTileId { id_col } => vec![id_col],
    }
}

fn find_blob_column(columns: &[String], excluded: &[usize]) -> Option<usize> {
    let priorities = [
        "tiledata",
        "imagedata",
        "jpeg",
        "jpg",
        "png",
        "blob",
        "image",
        "data",
        "tile",
    ];
    priorities.iter().find_map(|needle| {
        columns.iter().enumerate().find_map(|(index, column)| {
            if excluded.contains(&index) {
                return None;
            }
            let normalized = normalize_identifier(column);
            (normalized == *needle || normalized.contains(needle)).then_some(index)
        })
    })
}

fn tile_coordinates(
    source: &TileSource,
    row: &SqliteRow,
    level: Option<i64>,
    levels: &[SakuraLevel],
) -> Option<(i64, i64)> {
    match source.address {
        TileAddress::Xy { x_col, y_col } => Some((row.integer(x_col)?, row.integer(y_col)?)),
        TileAddress::SakuraTileId { id_col } => {
            let parsed = parse_tileid(row.text(id_col)?).ok()??;
            if Some(parsed.focal_plane) != source.focal_plane {
                return None;
            }
            let tile_span = parsed
                .downsample
                .checked_mul(levels.first()?.tile_size as i64)?;
            if tile_span <= 0 || parsed.x % tile_span != 0 || parsed.y % tile_span != 0 {
                return None;
            }
            Some((parsed.x / tile_span, parsed.y / tile_span))
        }
        TileAddress::Linear { index_col } => {
            let index = row.integer(index_col)?;
            if index < 0 {
                return None;
            }
            let level_index = level.unwrap_or(0);
            if level_index < 0 {
                return None;
            }
            let level = levels
                .get(level_index as usize)
                .or_else(|| levels.first())?;
            let tiles_across = level.width.div_ceil(level.tile_size as u64).max(1) as i64;
            Some((index % tiles_across, index / tiles_across))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SakuraTileId {
    x: i64,
    y: i64,
    downsample: i64,
    color: i64,
    focal_plane: i64,
}

fn make_tileid(x: i64, y: i64, downsample: i64, color: i64, focal_plane: i64) -> String {
    format!("T;{x}|{y};{downsample};{color};{focal_plane}")
}

fn parse_tileid(tileid: &str) -> Result<Option<SakuraTileId>> {
    if !tileid.starts_with("T;") || tileid.ends_with('#') {
        return Ok(None);
    }
    let fields = tileid[2..]
        .split([';', '|'])
        .map(str::parse::<i64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| OpenSlideError::Format(format!("Bad field value in tile ID {tileid}")))?;
    if fields.len() != 5 {
        return Err(OpenSlideError::Format(format!(
            "Bad field count in tile ID {tileid}"
        )));
    }
    let parsed = SakuraTileId {
        x: fields[0],
        y: fields[1],
        downsample: fields[2],
        color: fields[3],
        focal_plane: fields[4],
    };
    if parsed.x < 0
        || parsed.y < 0
        || parsed.downsample < 1
        || !(0..=2).contains(&parsed.color)
        || parsed.focal_plane < 0
    {
        return Err(OpenSlideError::Format(format!(
            "Bad field value in tile ID {tileid}"
        )));
    }
    if tileid
        != make_tileid(
            parsed.x,
            parsed.y,
            parsed.downsample,
            parsed.color,
            parsed.focal_plane,
        )
    {
        return Err(OpenSlideError::Format(format!(
            "Couldn't round-trip tile ID {tileid}"
        )));
    }
    Ok(Some(parsed))
}

fn chosen_focal_plane(focal_planes: u32) -> i64 {
    if focal_planes == 0 {
        return 0;
    }
    i64::from((focal_planes / 2) + (focal_planes % 2) - 1)
}

fn build_sakura_tile_id_levels(
    db: &SqliteDatabase,
    source: &TileSource,
    header: SakuraHeader,
) -> Result<Vec<SakuraLevel>> {
    if !matches!(source.address, TileAddress::SakuraTileId { .. }) {
        return Ok(Vec::new());
    }
    let rows = db.read_table_rows(source.root_page)?;
    let mut downsamples = BTreeSet::new();
    for row in rows {
        let TileAddress::SakuraTileId { id_col } = source.address else {
            return Ok(Vec::new());
        };
        let Some(tileid) = row.text(id_col) else {
            continue;
        };
        let Some(parsed) = parse_tileid(tileid)? else {
            continue;
        };
        if parsed.focal_plane != 0 {
            continue;
        }
        if parsed.downsample & (parsed.downsample - 1) != 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid downsample {}",
                parsed.downsample
            )));
        }
        downsamples.insert(parsed.downsample);
    }
    Ok(downsamples
        .into_iter()
        .map(|downsample| SakuraLevel {
            width: header.width / downsample as u64,
            height: header.height / downsample as u64,
            downsample: downsample as f64,
            tile_size: header.tile_size,
        })
        .collect())
}

fn compute_quickhash1(db: &SqliteDatabase, source: &TileSource) -> Result<Option<String>> {
    if !matches!(source.address, TileAddress::SakuraTileId { .. }) {
        return Ok(None);
    }
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    if !hash_columns(
        &mut quickhash1,
        db,
        "SVSlideDataXPO",
        &["SlideId", "Date", "Creator", "Description", "Keywords"],
    )? {
        return Ok(None);
    }
    if !hash_columns(
        &mut quickhash1,
        db,
        "SVHRScanDataXPO",
        &["ScanId", "Date", "Name", "Description"],
    )? {
        return Ok(None);
    }
    if !hash_unique_table_values(&mut quickhash1, db, "Header", true)? {
        return Ok(None);
    }
    if !hash_sakura_tiles(&mut quickhash1, db, source)? {
        return Ok(None);
    }
    Ok(quickhash1.openslide_hash_get_string())
}

fn hash_columns(
    quickhash1: &mut OpenslideHash,
    db: &SqliteDatabase,
    table_name: &str,
    columns: &[&str],
) -> Result<bool> {
    let Some(table) = db.tables.iter().find(|table| table.name == table_name) else {
        return Ok(false);
    };
    let Some(oid_col) = column_index(table, "OID") else {
        return Ok(false);
    };
    let Some(column_indexes) = columns
        .iter()
        .map(|column| column_index(table, column))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(false);
    };
    let mut rows = db.read_table_rows(table.root_page)?;
    rows.sort_by_key(|row| row.integer(oid_col).unwrap_or(i64::MAX));
    for row in rows {
        for col in &column_indexes {
            if let Some(bytes) = row.bytes(*col) {
                quickhash1.openslide_hash_data(bytes);
            }
            quickhash1.openslide_hash_data(&[0]);
        }
    }
    Ok(true)
}

fn hash_unique_table_values(
    quickhash1: &mut OpenslideHash,
    db: &SqliteDatabase,
    id: &str,
    nul_terminate: bool,
) -> Result<bool> {
    let unique_table_name = get_unique_table_name(db)?;
    let Some(table) = db
        .tables
        .iter()
        .find(|table| table.name == unique_table_name)
    else {
        return Ok(false);
    };
    let Some(id_col) = column_index(table, "id") else {
        return Ok(false);
    };
    let Some(data_col) = column_index(table, "data") else {
        return Ok(false);
    };
    let mut found = false;
    for row in db.read_table_rows(table.root_page)? {
        if row.text(id_col) == Some(id) {
            found = true;
            if let Some(bytes) = row.bytes(data_col) {
                quickhash1.openslide_hash_data(bytes);
            }
            if nul_terminate {
                quickhash1.openslide_hash_data(&[0]);
            }
        }
    }
    Ok(found)
}

fn hash_sakura_tiles(
    quickhash1: &mut OpenslideHash,
    db: &SqliteDatabase,
    source: &TileSource,
) -> Result<bool> {
    let TileAddress::SakuraTileId { id_col } = source.address else {
        return Ok(false);
    };
    let rows = db.read_table_rows(source.root_page)?;
    let mut tileids = Vec::new();
    let mut max_downsample = 0;
    for row in &rows {
        let Some(tileid) = row.text(id_col) else {
            continue;
        };
        let Some(parsed) = parse_tileid(tileid)? else {
            continue;
        };
        if parsed.downsample > max_downsample {
            tileids.clear();
            max_downsample = parsed.downsample;
        }
        if parsed.downsample == max_downsample {
            tileids.push(tileid.to_string());
        }
    }
    let data_col = source.blob_col;
    tileids.sort();
    for tileid in tileids {
        for row in &rows {
            if row.text(id_col) == Some(tileid.as_str()) {
                if let Some(bytes) = row.bytes(data_col) {
                    quickhash1.openslide_hash_data(bytes);
                }
                break;
            }
        }
    }
    Ok(true)
}

fn infer_single_level(db: &SqliteDatabase, source: &TileSource) -> Result<Option<SakuraLevel>> {
    let mut max_x = None::<i64>;
    let mut max_y = None::<i64>;
    let mut tile_size = None::<u32>;
    let mut row_count = 0u64;

    for row in db.read_table_rows(source.root_page)?.into_iter().take(256) {
        row_count += 1;
        if let TileAddress::Xy { x_col, y_col } = source.address {
            if let Some(x) = row.integer(x_col) {
                max_x = Some(max_x.map_or(x, |current| current.max(x)));
            }
            if let Some(y) = row.integer(y_col) {
                max_y = Some(max_y.map_or(y, |current| current.max(y)));
            }
        }
        if tile_size.is_none() {
            if let Some(blob) = row.blob(source.blob_col) {
                if let Ok((_, width, height)) = decode_tile_blob(blob) {
                    if width == height && width > 0 {
                        tile_size = Some(width);
                    } else {
                        tile_size = Some(width.max(height));
                    }
                }
            }
        }
    }

    let Some(tile_size) = tile_size else {
        return Ok(None);
    };
    let (max_x, max_y) = match source.address {
        TileAddress::Xy { .. } => {
            let (Some(max_x), Some(max_y)) = (max_x, max_y) else {
                return Ok(None);
            };
            if max_x < 0 || max_y < 0 {
                return Ok(None);
            }
            (max_x as u64, max_y as u64)
        }
        TileAddress::Linear { .. } => {
            if row_count == 0 {
                return Ok(None);
            }
            let tiles_across = integer_sqrt_ceil(row_count).max(1);
            let tiles_down = row_count.div_ceil(tiles_across);
            (tiles_across - 1, tiles_down - 1)
        }
        TileAddress::SakuraTileId { .. } => return Ok(None),
    };
    Ok(Some(SakuraLevel {
        width: (max_x + 1) * tile_size as u64,
        height: (max_y + 1) * tile_size as u64,
        downsample: 1.0,
        tile_size,
    }))
}

fn find_associated_images(
    db: &SqliteDatabase,
    tile_source: Option<&TileSource>,
) -> Result<HashMap<String, Vec<u8>>> {
    let mut images = HashMap::new();
    for table in &db.tables {
        if tile_source.is_some_and(|source| source.table_name == table.name) {
            continue;
        }
        let blob_cols = associated_blob_columns(&table.columns);
        if blob_cols.is_empty() {
            continue;
        }
        let name_col = find_column(
            &table.columns,
            &[
                "name",
                "type",
                "imagetype",
                "imagekind",
                "kind",
                "role",
                "description",
                "filename",
                "filepath",
                "path",
                "purpose",
                "category",
                "subtype",
                "imagelabel",
                "imageclass",
                "imagerole",
                "imageusage",
                "usagetype",
            ],
        );
        let table_name = associated_name_from_text(&table.name);
        for row in db.read_table_rows(table.root_page)?.into_iter().take(256) {
            let row_name = name_col
                .and_then(|col| row.text(col))
                .and_then(associated_name_from_text);
            for &blob_col in &blob_cols {
                let Some(blob) = associated_image_bytes(&row, blob_col) else {
                    continue;
                };
                if detect_image_format(&blob).is_err() {
                    continue;
                }
                let Some(name) = row_name
                    .clone()
                    .or_else(|| associated_name_from_text(&table.columns[blob_col]))
                    .or_else(|| table_name.clone())
                else {
                    continue;
                };
                images.entry(name).or_insert(blob);
            }
        }
    }
    Ok(images)
}

fn associated_image_bytes(row: &SqliteRow, column: usize) -> Option<Vec<u8>> {
    if let Some(blob) = row.blob(column) {
        return Some(blob.to_vec());
    }
    let text = row.text(column)?;
    decode_base64_image_text(text)
}

fn associated_blob_columns(columns: &[String]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            let normalized = normalize_identifier(column);
            let is_coordinate = matches!(
                normalized.as_str(),
                "x" | "y"
                    | "tilex"
                    | "tiley"
                    | "tilecol"
                    | "tilerow"
                    | "tilecolumn"
                    | "row"
                    | "col"
                    | "column"
                    | "index"
                    | "tileindex"
                    | "tileid"
                    | "tileno"
                    | "level"
                    | "levelno"
                    | "levelindex"
                    | "pyramidlevel"
                    | "resolution"
                    | "zoom"
                    | "zoomlevel"
                    | "offset"
                    | "byteoffset"
                    | "length"
                    | "bytelen"
                    | "bytesize"
                    | "imagesize"
                    | "compressedsize"
            );
            if is_coordinate {
                return None;
            }
            let looks_like_blob = [
                "thumbnail",
                "thumb",
                "label",
                "macro",
                "overview",
                "preview",
                "imagedata",
                "image",
                "jpeg",
                "jpg",
                "png",
                "bmp",
                "blob",
                "picture",
                "payload",
                "data",
                "content",
                "bytes",
                "binary",
                "media",
                "mediaobject",
                "mediadata",
                "binarydata",
                "compressedimage",
                "encodedimage",
                "base64image",
                "imagebase64",
            ]
            .iter()
            .any(|needle| normalized.contains(needle));
            looks_like_blob.then_some(index)
        })
        .collect()
}

fn associated_name_from_text(value: &str) -> Option<String> {
    let normalized = normalize_identifier(value);
    if normalized.contains("label")
        || normalized.contains("barcode")
        || normalized.contains("slideid")
        || normalized.contains("slideidentifier")
    {
        Some("label".into())
    } else if normalized.contains("macro")
        || normalized.contains("localization")
        || normalized.contains("localisation")
        || normalized.contains("reference")
        || normalized.contains("mapimage")
        || normalized.contains("referencemap")
        || normalized.contains("localiser")
        || normalized.contains("navigation")
        || normalized.contains("navigator")
        || normalized.contains("navimage")
    {
        Some("macro".into())
    } else if normalized.contains("thumbnail")
        || normalized.contains("thumb")
        || normalized == "thumbimage"
    {
        Some("thumbnail".into())
    } else if normalized.contains("overview")
        || normalized.contains("overviewimage")
        || normalized.contains("slideoverview")
        || normalized.contains("wholeimagesmall")
        || normalized.contains("smallimage")
    {
        Some("overview".into())
    } else if normalized.contains("slidepreview")
        || normalized.contains("preview")
        || normalized.contains("viewimage")
    {
        Some("preview".into())
    } else {
        None
    }
}

fn decode_base64_image_text(value: &str) -> Option<Vec<u8>> {
    let trimmed = strip_image_data_uri(value);
    let decoded = decode_base64(trimmed).ok()?;
    detect_image_format(&decoded).is_ok().then_some(decoded)
}

fn strip_image_data_uri(value: &str) -> &str {
    let trimmed = value.trim();
    if let Some((prefix, payload)) = trimmed.split_once(',') {
        let prefix = prefix.to_ascii_lowercase();
        if prefix.starts_with("data:") && prefix.contains(";base64") {
            return payload.trim();
        }
    }
    trimmed
}

fn decode_base64(data: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut chunk = [0u8; 4];
    let mut padding = [false; 4];
    let mut chunk_len = 0usize;
    let mut seen_padding = false;

    for b in data.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let is_padding = b == b'=';
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                seen_padding = true;
                0
            }
            _ => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Invalid Sakura base64 byte {b}"
                )))
            }
        };
        if seen_padding && b != b'=' {
            return Err(OpenSlideError::UnsupportedFormat(
                "Invalid Sakura base64 padding".into(),
            ));
        }
        chunk[chunk_len] = value;
        padding[chunk_len] = is_padding;
        chunk_len += 1;
        if chunk_len == 4 {
            out.push((chunk[0] << 2) | (chunk[1] >> 4));
            if !padding[2] {
                out.push((chunk[1] << 4) | (chunk[2] >> 2));
            }
            if !padding[3] {
                out.push((chunk[2] << 6) | chunk[3]);
            }
            chunk_len = 0;
            padding = [false; 4];
        }
    }
    if chunk_len != 0 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Truncated Sakura base64 data".into(),
        ));
    }
    Ok(out)
}

fn add_schema_properties(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    properties.insert("sakura.sqlite.page-size".into(), db.page_size.to_string());
    properties.insert(
        "sakura.sqlite.reserved-bytes".into(),
        db.reserved_bytes.to_string(),
    );
    properties.insert(
        "sakura.sqlite.table-count".into(),
        db.tables.len().to_string(),
    );
    for (i, table) in db.tables.iter().enumerate() {
        properties.insert(format!("sakura.sqlite.table[{i}].name"), table.name.clone());
        properties.insert(
            format!("sakura.sqlite.table[{i}].root-page"),
            table.root_page.to_string(),
        );
        properties.insert(
            format!("sakura.sqlite.table[{i}].columns"),
            table.columns.join(","),
        );
    }
}

fn add_properties(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    let Some(slide_table) = db
        .tables
        .iter()
        .find(|table| table.name == "SVSlideDataXPO")
    else {
        add_version_property(db, properties);
        return;
    };
    let Some(scan_table) = db
        .tables
        .iter()
        .find(|table| table.name == "SVHRScanDataXPO")
    else {
        add_version_property(db, properties);
        return;
    };
    let Ok(slide_rows) = db.read_table_rows(slide_table.root_page) else {
        add_version_property(db, properties);
        return;
    };
    let Ok(scan_rows) = db.read_table_rows(scan_table.root_page) else {
        add_version_property(db, properties);
        return;
    };
    let Some(slide_oid_col) = column_index(slide_table, "OID") else {
        add_version_property(db, properties);
        return;
    };
    let Some(scan_parent_col) = column_index(scan_table, "ParentSlide") else {
        add_version_property(db, properties);
        return;
    };

    let joined = scan_rows.iter().find_map(|scan_row| {
        let parent = scan_row.integer(scan_parent_col)?;
        let slide_row = slide_rows
            .iter()
            .find(|slide_row| slide_row.integer(slide_oid_col) == Some(parent))?;
        Some((slide_row, scan_row))
    });

    if let Some((slide_row, scan_row)) = joined {
        for column in [
            "SlideId",
            "Date",
            "Description",
            "Creator",
            "DiagnosisCode",
            "Keywords",
        ] {
            add_text_property(properties, slide_table, slide_row, column);
        }
        for column in ["ScanId", "FocussingMethod"] {
            add_text_property(properties, scan_table, scan_row, column);
        }
        for column in ["ResolutionMmPerPix", "NominalLensMagnification"] {
            add_float_property(properties, scan_table, scan_row, column);
        }
        if let Some(mmpp) =
            column_index(scan_table, "ResolutionMmPerPix").and_then(|col| scan_row.float(col))
        {
            properties.insert(
                crate::properties::PROPERTY_MPP_X.into(),
                format_float(mmpp * 1000.0),
            );
            properties.insert(
                crate::properties::PROPERTY_MPP_Y.into(),
                format_float(mmpp * 1000.0),
            );
        }
        if let Some(value) = properties
            .get("sakura.NominalLensMagnification")
            .filter(|value| !value.is_empty())
            .cloned()
        {
            properties.insert(crate::properties::PROPERTY_OBJECTIVE_POWER.into(), value);
        }
    }

    add_version_property(db, properties);
}

fn add_text_property(
    properties: &mut HashMap<String, String>,
    table: &SqliteTable,
    row: &SqliteRow,
    column: &str,
) {
    let Some(value) = column_index(table, column)
        .and_then(|col| row.value_as_property(col))
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    properties.insert(format!("sakura.{column}"), value);
}

fn add_float_property(
    properties: &mut HashMap<String, String>,
    table: &SqliteTable,
    row: &SqliteRow,
    column: &str,
) {
    let Some(value) = column_index(table, column).and_then(|col| row.float(col)) else {
        return;
    };
    properties.insert(format!("sakura.{column}"), format_float(value));
}

fn add_version_property(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    let Ok(unique_table_name) = get_unique_table_name(db) else {
        return;
    };
    let Some(table) = db
        .tables
        .iter()
        .find(|table| table.name == unique_table_name)
    else {
        return;
    };
    let Some(id_col) = column_index(table, "id") else {
        return;
    };
    let Some(data_col) = column_index(table, "data") else {
        return;
    };
    let Ok(rows) = db.read_table_rows(table.root_page) else {
        return;
    };
    for row in rows {
        if row.text(id_col) == Some("++VersionBytes") {
            if let Some(value) = row.value_as_property(data_col) {
                properties.insert("sakura.VersionBytes".into(), value);
            }
            return;
        }
    }
}

fn add_sqlite_metadata_properties(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    for table in metadata_tables(&db.tables).into_iter().take(8) {
        let Some(rows) = db.read_table_rows(table.root_page).ok() else {
            continue;
        };
        let name_col = find_column(
            &table.columns,
            &["name", "key", "property", "attribute", "field", "tag"],
        );
        let value_col = find_column(
            &table.columns,
            &["value", "val", "text", "data", "content", "string"],
        );
        if let (Some(name_col), Some(value_col)) = (name_col, value_col) {
            for row in rows.into_iter().take(128) {
                let Some(name) = row.text(name_col).map(normalize_property_component) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let Some(value) = row.value_as_property(value_col) else {
                    continue;
                };
                properties
                    .entry(format!("sakura.metadata.{name}"))
                    .or_insert(value);
            }
        } else {
            for (row_index, row) in rows.into_iter().take(16).enumerate() {
                for (col_index, column) in table.columns.iter().enumerate() {
                    let Some(value) = row.value_as_property(col_index) else {
                        continue;
                    };
                    let column = normalize_property_component(column);
                    if column.is_empty() {
                        continue;
                    }
                    properties
                        .entry(format!(
                            "sakura.metadata.{}.{}.{}",
                            normalize_property_component(&table.name),
                            row_index,
                            column
                        ))
                        .or_insert(value);
                }
            }
        }
    }
}

fn metadata_tables(tables: &[SqliteTable]) -> Vec<&SqliteTable> {
    let mut tables = tables
        .iter()
        .filter(|table| {
            let name = normalize_identifier(&table.name);
            !name.contains("tile")
                && !name.contains("image")
                && (name.contains("meta")
                    || name.contains("property")
                    || name.contains("setting")
                    || name.contains("slideinfo")
                    || name.contains("scaninfo")
                    || name.contains("slide")
                    || name.contains("scan")
                    || name.contains("specimen")
                    || name.contains("case")
                    || name.contains("patient")
                    || find_column(
                        &table.columns,
                        &["name", "key", "property", "attribute", "field", "tag"],
                    )
                    .is_some())
        })
        .collect::<Vec<_>>();
    tables.sort_by_key(|table| normalize_identifier(&table.name));
    tables
}

fn normalize_property_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn integer_sqrt_ceil(value: u64) -> u64 {
    if value <= 1 {
        return value;
    }
    let floor = (value as f64).sqrt() as u64;
    if floor.saturating_mul(floor) == value {
        floor
    } else {
        floor + 1
    }
}

fn find_column(columns: &[String], needles: &[&str]) -> Option<usize> {
    columns.iter().position(|column| {
        let normalized = normalize_identifier(column);
        needles.iter().any(|needle| {
            normalized == *needle
                || if needle.len() == 1 {
                    false
                } else {
                    normalized.contains(needle)
                }
        })
    })
}

fn parse_create_table_columns(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };
    let mut depth = 0usize;
    let mut current = String::new();
    let mut parts = Vec::new();
    for ch in sql[open + 1..].chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' if depth == 0 => {
                if !current.trim().is_empty() {
                    parts.push(current);
                }
                break;
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }

    parts
        .into_iter()
        .filter_map(|part| first_identifier(part.trim()))
        .filter(|name| {
            !matches!(
                normalize_identifier(name).as_str(),
                "constraint" | "primary" | "foreign" | "unique" | "check"
            )
        })
        .collect()
}

fn first_identifier(value: &str) -> Option<String> {
    let trimmed = value.trim_start();
    let first = trimmed.chars().next()?;
    if matches!(first, '"' | '\'' | '`' | '[') {
        let close = if first == '[' { ']' } else { first };
        let end = trimmed[1..].find(close)?;
        Some(trimmed[1..1 + end].to_string())
    } else {
        Some(
            trimmed
                .split_whitespace()
                .next()?
                .trim_matches(',')
                .to_string(),
        )
    }
}

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn format_float(value: f64) -> String {
    let s = format!("{value:.12}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn parse_record(payload: &[u8]) -> Result<SqliteRow> {
    let (header_len, n) = read_varint(payload)?;
    let header_len = header_len as usize;
    if header_len > payload.len() || n > header_len {
        return Err(OpenSlideError::Format(
            "Invalid SQLite record header length".into(),
        ));
    }
    let mut serials = Vec::new();
    let mut pos = n;
    while pos < header_len {
        let (serial, used) = read_varint(&payload[pos..header_len])?;
        serials.push(serial);
        pos += used;
    }

    let mut body = header_len;
    let mut values = Vec::with_capacity(serials.len());
    for serial in serials {
        let (value, used) = parse_sqlite_value(serial, &payload[body..])?;
        body += used;
        values.push(value);
    }
    Ok(SqliteRow { values })
}

fn parse_sqlite_value(serial: u64, data: &[u8]) -> Result<(SqliteValue, usize)> {
    let need = match serial {
        0 | 8 | 9 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 | 7 => 8,
        serial if serial >= 12 => ((serial - 12) / 2) as usize,
        _ => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported SQLite record serial type {serial}"
            )))
        }
    };
    if data.len() < need {
        return Err(OpenSlideError::Format(
            "SQLite record value exceeds payload".into(),
        ));
    }
    let value = match serial {
        0 => SqliteValue::Null,
        1 => SqliteValue::Integer(i8::from_be_bytes([data[0]]) as i64),
        2 => SqliteValue::Integer(i16::from_be_bytes([data[0], data[1]]) as i64),
        3 => {
            let sign = if data[0] & 0x80 == 0 { 0 } else { 0xff };
            SqliteValue::Integer(i32::from_be_bytes([sign, data[0], data[1], data[2]]) as i64)
        }
        4 => SqliteValue::Integer(i32::from_be_bytes([data[0], data[1], data[2], data[3]]) as i64),
        5 => {
            let sign = if data[0] & 0x80 == 0 { 0 } else { 0xff };
            SqliteValue::Integer(i64::from_be_bytes([
                sign, sign, data[0], data[1], data[2], data[3], data[4], data[5],
            ]))
        }
        6 => SqliteValue::Integer(i64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ])),
        7 => SqliteValue::Real(f64::from_bits(u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]))),
        8 => SqliteValue::Integer(0),
        9 => SqliteValue::Integer(1),
        serial if serial >= 12 && serial % 2 == 0 => SqliteValue::Blob(data[..need].to_vec()),
        serial if serial >= 13 && serial % 2 == 1 => {
            SqliteValue::Text(String::from_utf8_lossy(&data[..need]).into_owned())
        }
        _ => unreachable!(),
    };
    Ok((value, need))
}

fn read_varint(data: &[u8]) -> Result<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..9 {
        let Some(&byte) = data.get(i) else {
            return Err(OpenSlideError::Format("Truncated SQLite varint".into()));
        };
        if i == 8 {
            value = (value << 8) | byte as u64;
            return Ok((value, 9));
        }
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    unreachable!()
}

fn read_be_u16(data: &[u8]) -> u16 {
    u16::from_be_bytes([data[0], data[1]])
}

fn read_be_u32(data: &[u8]) -> u32 {
    u32::from_be_bytes([data[0], data[1], data[2], data[3]])
}

fn decode_tile_blob(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let format = detect_image_format(data)?;
    decode::decode_rgb(format, data)
}

fn detect_image_format(data: &[u8]) -> Result<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8]) {
        Ok(ImageFormat::Jpeg)
    } else if data.starts_with(b"\x89PNG") {
        Ok(ImageFormat::Png)
    } else if data.starts_with(b"BM") {
        Ok(ImageFormat::Bmp)
    } else {
        Err(OpenSlideError::UnsupportedFormat(
            "Sakura tile BLOB is not JPEG, PNG, or BMP".into(),
        ))
    }
}

fn blit_rgb_channel(
    rgb: &[u8],
    tile_w: u32,
    tile_h: u32,
    channel: u32,
    output: &mut GrayImage,
    dest_x: f64,
    dest_y: f64,
) {
    let channel = channel as usize;
    let src_x0 = (-dest_x).max(0.0).floor() as u32;
    let src_y0 = (-dest_y).max(0.0).floor() as u32;
    let dst_x0 = dest_x.max(0.0).ceil() as u32;
    let dst_y0 = dest_y.max(0.0).ceil() as u32;
    let copy_w = (tile_w - src_x0).min(output.width.saturating_sub(dst_x0));
    let copy_h = (tile_h - src_y0).min(output.height.saturating_sub(dst_y0));

    for row in 0..copy_h {
        let src_base = ((src_y0 + row) * tile_w + src_x0) as usize * 3;
        let dst_base = ((dst_y0 + row) * output.width + dst_x0) as usize;
        for col in 0..copy_w as usize {
            output.data[dst_base + col] = rgb[src_base + col * 3 + channel];
        }
    }
}

fn blit_gray_channel(tile: &GrayImage, output: &mut GrayImage, dst_x: f64, dst_y: f64) {
    let start_x = dst_x.floor() as i64;
    let start_y = dst_y.floor() as i64;
    let src_x0 = (-start_x).max(0) as u32;
    let src_y0 = (-start_y).max(0) as u32;
    let dst_x0 = start_x.max(0) as u32;
    let dst_y0 = start_y.max(0) as u32;
    if dst_x0 >= output.width || dst_y0 >= output.height {
        return;
    }
    let copy_w = (tile.width.saturating_sub(src_x0)).min(output.width - dst_x0);
    let copy_h = (tile.height.saturating_sub(src_y0)).min(output.height - dst_y0);
    for row in 0..copy_h {
        let src_base = ((src_y0 + row) * tile.width + src_x0) as usize;
        let dst_base = ((dst_y0 + row) * output.width + dst_x0) as usize;
        let src = &tile.data[src_base..src_base + copy_w as usize];
        let dst = &mut output.data[dst_base..dst_base + copy_w as usize];
        dst.copy_from_slice(src);
    }
}

#[derive(Debug, Clone, Copy)]
struct SakuraHeader {
    tile_size: u32,
    width: u64,
    height: u64,
    focal_planes: u32,
}

fn read_header(db: &SqliteDatabase) -> Result<Option<SakuraHeader>> {
    let unique_table_name = get_unique_table_name(db)?;
    let Some(table) = db
        .tables
        .iter()
        .find(|table| table.name == unique_table_name)
    else {
        return Ok(None);
    };
    let Some(id_col) = column_index(table, "id") else {
        return Ok(None);
    };
    let Some(data_col) = column_index(table, "data") else {
        return Ok(None);
    };
    for row in db.read_table_rows(table.root_page)? {
        if row.text(id_col) == Some("Header") {
            let Some(data) = row.bytes(data_col) else {
                return Err(OpenSlideError::Format(
                    "Sakura Header row does not contain data".into(),
                ));
            };
            return Ok(Some(parse_sakura_header(data)?));
        }
    }
    Ok(None)
}

fn parse_sakura_header(data: &[u8]) -> Result<SakuraHeader> {
    if data.len() < 20 {
        return Err(OpenSlideError::Format("Short Sakura Header data".into()));
    }
    let tile_size = read_u32(data, 0);
    if tile_size == 0 || tile_size > i32::MAX as u32 {
        return Err(OpenSlideError::Format(format!(
            "Invalid tile size: {tile_size}"
        )));
    }
    Ok(SakuraHeader {
        tile_size,
        width: read_u32(data, 4) as u64,
        height: read_u32(data, 8) as u64,
        focal_planes: read_u32(data, 16),
    })
}

fn find_header_like_record(path: &Path) -> Result<Option<SakuraHeader>> {
    let file = File::open(path)?;
    let mut data = Vec::new();
    file.take(SCAN_LIMIT).read_to_end(&mut data)?;

    for pos in find_all(&data, b"Header") {
        let start = pos.saturating_sub(32);
        let end = (pos + 256).min(data.len());
        for candidate in start..end.saturating_sub(20) {
            let tile_size = read_u32(&data, candidate);
            let width = read_u32(&data, candidate + 4);
            let height = read_u32(&data, candidate + 8);
            let focal_planes = read_u32(&data, candidate + 16);
            if is_plausible_header(tile_size, width, height, focal_planes) {
                return Ok(Some(SakuraHeader {
                    tile_size,
                    width: width as u64,
                    height: height as u64,
                    focal_planes,
                }));
            }
        }
    }
    Ok(None)
}

fn is_plausible_header(tile_size: u32, width: u32, height: u32, focal_planes: u32) -> bool {
    matches!(tile_size, 128 | 240 | 256 | 512 | 1024)
        && width >= tile_size
        && height >= tile_size
        && focal_planes > 0
        && focal_planes < 10_000
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&data[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn find_all<'a>(haystack: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(move |(i, window)| (window == needle).then_some(i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_raw_sakura_magic_without_sqlite_tables() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_raw_sakura_magic_{}",
            std::process::id()
        ));
        let mut data = Vec::new();
        data.extend_from_slice(SQLITE_MAGIC);
        data.extend_from_slice(b"padding");
        data.extend_from_slice(SAKURA_MAGIC);
        fs::write(&path, data).unwrap();

        assert!(!detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn detects_sakura_magic_from_unique_table_rows() {
        let unique_table = SqliteTable {
            name: "ImageData".into(),
            root_page: 2,
            columns: vec!["id".into(), "data".into()],
        };
        let rows = vec![SqliteRow {
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Text("SVGigaPixelImage".into()),
            ],
        }];

        assert!(has_sakura_magic_bytes(&unique_table, &rows));

        let blob_rows = vec![SqliteRow {
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Blob(b"SVGigaPixelImage".to_vec()),
            ],
        }];
        assert!(has_sakura_magic_bytes(&unique_table, &blob_rows));

        let wrong_rows = vec![SqliteRow {
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Text("not sakura".into()),
            ],
        }];
        assert!(!has_sakura_magic_bytes(&unique_table, &wrong_rows));
    }

    #[test]
    fn parses_sakura_header_from_unique_table_data_layout() {
        let mut data = vec![0; 20];
        data[0..4].copy_from_slice(&240u32.to_le_bytes());
        data[4..8].copy_from_slice(&1234u32.to_le_bytes());
        data[8..12].copy_from_slice(&5678u32.to_le_bytes());
        data[16..20].copy_from_slice(&5u32.to_le_bytes());

        let header = parse_sakura_header(&data).unwrap();
        assert_eq!(header.tile_size, 240);
        assert_eq!(header.width, 1234);
        assert_eq!(header.height, 5678);
        assert_eq!(header.focal_planes, 5);
    }

    #[test]
    fn rejects_invalid_sakura_header_tile_size() {
        let mut data = vec![0; 20];
        data[4..8].copy_from_slice(&1234u32.to_le_bytes());
        data[8..12].copy_from_slice(&5678u32.to_le_bytes());

        let err = parse_sakura_header(&data).unwrap_err();
        assert!(format!("{err}").contains("Invalid tile size"));
    }

    #[test]
    fn reads_sqlite_numeric_values_as_float() {
        let row = SqliteRow {
            values: vec![
                SqliteValue::Real(0.00025),
                SqliteValue::Integer(40),
                SqliteValue::Text("20".into()),
            ],
        };

        assert_eq!(row.float(0), Some(0.00025));
        assert_eq!(row.float(1), Some(40.0));
        assert_eq!(row.float(2), Some(20.0));
    }

    #[test]
    fn parses_upstream_sakura_tile_ids() {
        let parsed = parse_tileid("T;512|256;2;1;0").unwrap().unwrap();

        assert_eq!(
            parsed,
            SakuraTileId {
                x: 512,
                y: 256,
                downsample: 2,
                color: 1,
                focal_plane: 0,
            }
        );
        assert_eq!(parse_tileid("++MagicBytes").unwrap(), None);
        assert!(parse_tileid("T;0512|256;2;1;0").is_err());
        assert!(parse_tileid("T;512|256;2;3;0").is_err());
    }

    #[test]
    fn parses_create_table_columns_with_constraints() {
        let columns = parse_create_table_columns(
            r#"CREATE TABLE "Tiles" (
                "Level" INTEGER,
                [TileX] INTEGER,
                `TileY` INTEGER,
                image BLOB,
                PRIMARY KEY ("Level", [TileX], `TileY`)
            )"#,
        );

        assert_eq!(columns, ["Level", "TileX", "TileY", "image"]);
    }

    #[test]
    fn finds_plausible_tile_source_from_schema() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 4096,
            reserved_bytes: 0,
            tables: vec![SqliteTable {
                name: "TileStore".into(),
                root_page: 3,
                columns: vec![
                    "pyramid_level".into(),
                    "tile_col".into(),
                    "tile_row".into(),
                    "tile_jpeg".into(),
                ],
            }],
        };

        let source = find_tile_source(&db).unwrap();
        assert_eq!(source.table_name, "TileStore");
        assert_eq!(source.root_page, 3);
        assert_eq!(source.level_col, Some(0));
        assert!(matches!(
            source.address,
            TileAddress::Xy { x_col: 1, y_col: 2 }
        ));
        assert_eq!(source.blob_col, 3);
    }

    #[test]
    fn finds_linear_tile_source_from_schema() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 4096,
            reserved_bytes: 0,
            tables: vec![SqliteTable {
                name: "ImageTiles".into(),
                root_page: 5,
                columns: vec!["Resolution".into(), "TileIndex".into(), "ImageData".into()],
            }],
        };

        let source = find_tile_source(&db).unwrap();
        assert_eq!(source.table_name, "ImageTiles");
        assert_eq!(source.level_col, Some(0));
        assert!(matches!(
            source.address,
            TileAddress::Linear { index_col: 1 }
        ));
        assert_eq!(source.blob_col, 2);
    }

    #[test]
    fn prefers_tile_named_source_over_metadata_images() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 4096,
            reserved_bytes: 0,
            tables: vec![
                SqliteTable {
                    name: "PreviewImages".into(),
                    root_page: 2,
                    columns: vec!["x".into(), "y".into(), "ImageData".into()],
                },
                SqliteTable {
                    name: "PyramidTiles".into(),
                    root_page: 7,
                    columns: vec![
                        "PyramidLevel".into(),
                        "TilePosX".into(),
                        "TilePosY".into(),
                        "TileBlob".into(),
                    ],
                },
            ],
        };

        let source = find_tile_source(&db).unwrap();
        assert_eq!(source.table_name, "PyramidTiles");
        assert_eq!(source.level_col, Some(0));
        assert!(matches!(
            source.address,
            TileAddress::Xy { x_col: 1, y_col: 2 }
        ));
    }

    #[test]
    fn records_schema_properties_and_metadata_candidates() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 8192,
            reserved_bytes: 4,
            tables: vec![
                SqliteTable {
                    name: "SlideProperties".into(),
                    root_page: 3,
                    columns: vec!["PropertyName".into(), "PropertyValue".into()],
                },
                SqliteTable {
                    name: "ScanDetails".into(),
                    root_page: 4,
                    columns: vec!["Field".into(), "Value".into()],
                },
            ],
        };
        let mut properties = HashMap::new();
        add_schema_properties(&db, &mut properties);

        assert_eq!(
            properties.get("sakura.sqlite.table[0].columns"),
            Some(&"PropertyName,PropertyValue".to_string())
        );
        let metadata_names = metadata_tables(&db.tables)
            .into_iter()
            .map(|table| table.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(metadata_names, ["ScanDetails", "SlideProperties"]);
        assert_eq!(
            normalize_property_component("Scan Objective Power"),
            "scan-objective-power"
        );
        let row = SqliteRow {
            values: vec![SqliteValue::Real(0.2525)],
        };
        assert_eq!(row.value_as_property(0).as_deref(), Some("0.2525"));
    }

    #[test]
    fn maps_linear_tile_index_to_grid_coordinates() {
        let source = TileSource {
            table_name: "ImageTiles".into(),
            root_page: 5,
            columns: vec!["TileIndex".into(), "ImageData".into()],
            blob_col: 1,
            address: TileAddress::Linear { index_col: 0 },
            level_col: None,
            focal_plane: None,
        };
        let row = SqliteRow {
            values: vec![SqliteValue::Integer(5), SqliteValue::Blob(vec![1, 2, 3])],
        };
        let levels = vec![SakuraLevel {
            width: 768,
            height: 512,
            downsample: 1.0,
            tile_size: 256,
        }];

        assert_eq!(tile_coordinates(&source, &row, None, &levels), Some((2, 1)));
    }

    #[test]
    fn maps_sakura_tile_id_to_grid_coordinates() {
        let source = TileSource {
            table_name: "ImageData".into(),
            root_page: 5,
            columns: vec!["id".into(), "data".into()],
            blob_col: 1,
            address: TileAddress::SakuraTileId { id_col: 0 },
            level_col: None,
            focal_plane: Some(0),
        };
        let row = SqliteRow {
            values: vec![
                SqliteValue::Text("T;512|256;2;1;0".into()),
                SqliteValue::Blob(vec![1, 2, 3]),
            ],
        };
        let levels = vec![SakuraLevel {
            width: 512,
            height: 512,
            downsample: 2.0,
            tile_size: 128,
        }];

        assert_eq!(
            tile_coordinates(&source, &row, Some(2), &levels),
            Some((2, 1))
        );
    }

    #[test]
    fn accepts_integer_like_sqlite_values_for_tile_coordinates() {
        let source = TileSource {
            table_name: "ImageTiles".into(),
            root_page: 5,
            columns: vec!["TileX".into(), "TileY".into(), "ImageData".into()],
            blob_col: 2,
            address: TileAddress::Xy { x_col: 0, y_col: 1 },
            level_col: None,
            focal_plane: None,
        };
        let levels = vec![SakuraLevel {
            width: 768,
            height: 512,
            downsample: 1.0,
            tile_size: 256,
        }];

        let text_row = SqliteRow {
            values: vec![
                SqliteValue::Text(" 12 ".into()),
                SqliteValue::Text("7".into()),
                SqliteValue::Blob(vec![1, 2, 3]),
            ],
        };
        assert_eq!(
            tile_coordinates(&source, &text_row, None, &levels),
            Some((12, 7))
        );

        let real_row = SqliteRow {
            values: vec![
                SqliteValue::Real(12.0),
                SqliteValue::Real(7.5),
                SqliteValue::Blob(vec![1, 2, 3]),
            ],
        };
        assert_eq!(tile_coordinates(&source, &real_row, None, &levels), None);
    }

    #[test]
    fn detects_associated_image_columns_and_names() {
        let columns = vec![
            "ID".into(),
            "MacroImage".into(),
            "LabelJpeg".into(),
            "TileX".into(),
            "Payload".into(),
            "ContentBytes".into(),
            "ImageSize".into(),
            "BinaryMedia".into(),
            "EncodedImage".into(),
            "Base64Image".into(),
            "ByteOffset".into(),
        ];

        assert_eq!(associated_blob_columns(&columns), [1, 2, 4, 5, 7, 8, 9]);
        assert_eq!(
            associated_name_from_text("SlidePreviewImage").as_deref(),
            Some("preview")
        );
        assert_eq!(
            associated_name_from_text("barcode label").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_text("Slide ID").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_text("Slide Identifier Image").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_text("WholeImageSmall").as_deref(),
            Some("overview")
        );
        assert_eq!(
            associated_name_from_text("thumb image").as_deref(),
            Some("thumbnail")
        );
        assert_eq!(
            associated_name_from_text("Localization Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("Localisation Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("Reference Map Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("ReferenceMap").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("Navigation Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("Localiser Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_text("SmallImage").as_deref(),
            Some("overview")
        );

        let row = SqliteRow {
            values: vec![SqliteValue::Text("data:image/bmp;base64,Qk0=".into())],
        };
        assert_eq!(associated_image_bytes(&row, 0).unwrap(), b"BM");

        let octet_stream_row = SqliteRow {
            values: vec![SqliteValue::Text(
                " data:application/octet-stream;base64,Qk0= ".into(),
            )],
        };
        assert_eq!(associated_image_bytes(&octet_stream_row, 0).unwrap(), b"BM");
    }

    #[test]
    fn accepts_more_sakura_tile_schema_aliases() {
        let table = SqliteTable {
            name: "PyramidImageData".into(),
            root_page: 9,
            columns: vec![
                "ResolutionIndex".into(),
                "TilePositionX".into(),
                "TilePositionY".into(),
                "TileContentBytes".into(),
            ],
        };

        let source = tile_source_from_table(&table).unwrap();
        assert_eq!(source.level_col, Some(0));
        assert_eq!(source.blob_col, 3);
        assert!(matches!(
            source.address,
            TileAddress::Xy { x_col: 1, y_col: 2 }
        ));
    }

    #[test]
    fn exposes_and_decodes_associated_images() {
        let mut associated_images = HashMap::new();
        associated_images.insert("label".into(), one_by_one_bmp([7, 11, 13]));
        let slide = SakuraSlide {
            levels: Vec::new(),
            properties: HashMap::new(),
            sqlite: None,
            tile_source: None,
            tile_index: Mutex::new(None),
            associated_images,
        };

        assert_eq!(slide.associated_image_names(), ["label"]);
        let image = slide.read_associated_image("label").unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.pixel(0, 0), [7, 11, 13, 255]);
    }

    fn one_by_one_bmp(rgb: [u8; 3]) -> Vec<u8> {
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&58u32.to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&4u32.to_le_bytes());
        bmp.extend_from_slice(&[0; 16]);
        bmp.extend_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
        bmp
    }
}
