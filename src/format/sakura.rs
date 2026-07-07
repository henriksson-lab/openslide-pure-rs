use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff::OpenslideHash, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;

const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const SAKURA_MAGIC: &[u8] = b"SVGigaPixelImage";

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
    associated_images: HashMap<String, AssociatedImage>,
}

#[derive(Debug, Clone)]
struct AssociatedImage {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone)]
struct TileSource {
    root_page: u32,
    blob_col: usize,
    address: TileAddress,
    focal_plane: Option<i64>,
    rowid_alias_col: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileAddress {
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

        let sqlite = SqliteDatabase::open(path)?;
        let mut tile_source = find_sakura_tile_id_source(&sqlite);
        add_properties(&sqlite, &mut properties);
        let associated_images = find_associated_images(&sqlite)?;

        if let Some(source) = &tile_source {
            if let Some(quickhash1) = compute_quickhash1(&sqlite, source)? {
                properties.insert(properties::PROPERTY_QUICKHASH1.into(), quickhash1);
            }
        }

        let header = require_sakura_header(read_header(&sqlite)?)?;

        let levels = {
            if let Some(source) = &mut tile_source {
                source.focal_plane = Some(chosen_focal_plane(header.focal_planes));
            }
            if let Some(source) = &tile_source {
                build_sakura_tile_id_levels(&sqlite, source, header)?
            } else {
                return Err(OpenSlideError::Format("Couldn't find any tiles".into()));
            }
        };

        for (name, image) in &associated_images {
            properties.insert(properties::associated_width(name), image.width.to_string());
            properties.insert(
                properties::associated_height(name),
                image.height.to_string(),
            );
        }

        Ok(Self {
            levels,
            properties,
            sqlite: Some(sqlite),
            tile_source,
            tile_index: Mutex::new(None),
            associated_images,
        })
    }

    fn cached_tile_blob(
        &self,
        db: &SqliteDatabase,
        source: &TileSource,
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
            level: Some(level_data.downsample as i64),
            color: Some(channel as u8),
            x: tile_x,
            y: tile_y,
        };
        Ok(index.tiles.get(&key).cloned())
    }
}

fn sakura_detect(path: &Path) -> Result<bool> {
    let db = SqliteDatabase::open(path)?;
    let unique_table_name = get_unique_table_name(&db)?;
    let Some(unique_table) = find_table_by_name(&db, &unique_table_name) else {
        return Ok(false);
    };
    let rows = db.read_table_rows(unique_table.root_page)?;
    Ok(has_sakura_magic_bytes(unique_table, &rows))
}

fn get_unique_table_name(db: &SqliteDatabase) -> Result<String> {
    let Some(table) = find_sakura_config_table(db) else {
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
    sakura_unique_table_name_from_rows(&rows, table_name_col)
}

fn sakura_unique_table_name_from_rows(rows: &[SqliteRow], table_name_col: usize) -> Result<String> {
    if rows.len() > 1 {
        return Err(OpenSlideError::Format("Found > 1 unique tables".into()));
    }
    rows.first()
        .and_then(|row| row.text(table_name_col))
        .map(str::to_string)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Missing Sakura unique table name".into()))
}

fn find_sakura_config_table(db: &SqliteDatabase) -> Option<&SqliteTable> {
    find_table_by_name(db, "DataManagerSQLiteConfigXPO")
}

fn has_sakura_magic_bytes(table: &SqliteTable, rows: &[SqliteRow]) -> bool {
    let Some(id_col) = column_index(table, "id") else {
        return false;
    };
    let Some(data_col) = column_index(table, "data") else {
        return false;
    };
    rows.iter()
        .find(|row| row.text(id_col) == Some("++MagicBytes"))
        .and_then(|row| row.bytes(data_col))
        == Some(SAKURA_MAGIC)
}

fn column_index(table: &SqliteTable, name: &str) -> Option<usize> {
    table
        .columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case(name))
}

fn find_table_by_name<'a>(db: &'a SqliteDatabase, name: &str) -> Option<&'a SqliteTable> {
    db.tables
        .iter()
        .find(|table| table.name.eq_ignore_ascii_case(name))
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

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (u64::from(level.tile_size), u64::from(level.tile_size)))
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
                    self.cached_tile_blob(db, source, level_data, channel, col, row)?
                else {
                    continue;
                };
                let tile = decode::decode_channel(ImageFormat::Jpeg, &blob, 0)?;
                blit_gray_channel(
                    &tile,
                    &mut output,
                    col as f64 * tile_size - lx,
                    row as f64 * tile_size - ly,
                );
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

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        let image = self.associated_images.get(name)?;
        Some((u64::from(image.width), u64::from(image.height)))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{name}'"))
        })?;
        decode::decode_to_rgba(ImageFormat::Jpeg, &image.data)
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
    rowid_alias_col: Option<usize>,
}

#[derive(Debug, Clone)]
struct SqliteRow {
    rowid: Option<i64>,
    values: Vec<SqliteValue>,
}

#[derive(Debug, Clone)]
struct TableSchema {
    columns: Vec<String>,
    rowid_alias_col: Option<usize>,
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
        let mut file = crate::util::_openslide_fopen(path)?;
        let mut header = [0u8; 100];
        crate::util::_openslide_fread_exact(&mut file, &mut header)?;
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
            let Some(name) = schema_user_table_name(&row) else {
                continue;
            };
            let Some(root_page) = row.integer(3).and_then(|value| u32::try_from(value).ok()) else {
                continue;
            };
            let Some(sql) = row.text(4) else {
                continue;
            };
            let schema = parse_create_table_schema(sql);
            if !schema.columns.is_empty() {
                tables.push(SqliteTable {
                    name: name.to_string(),
                    root_page,
                    columns: schema.columns,
                    rowid_alias_col: schema.rowid_alias_col,
                });
            }
        }
        Ok(tables)
    }

    fn read_table_rows(&self, root_page: u32) -> Result<Vec<SqliteRow>> {
        let mut rows = Vec::new();
        self.read_btree_page(root_page, &mut rows, 0)?;
        Ok(rows)
    }

    fn build_tile_index(
        &self,
        source: &TileSource,
        levels: &[SakuraLevel],
    ) -> Result<TileBlobIndex> {
        let mut index = TileBlobIndex::default();
        for mut row in self.read_table_rows(source.root_page)? {
            source.apply_rowid_alias(&mut row);
            let Some(blob) = row.blob(source.blob_col) else {
                continue;
            };
            let Some((x, y)) = tile_coordinates(source, &row, levels)? else {
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
            };
            index
                .tiles
                .insert(TileKey { level, color, x, y }, blob.to_vec());
        }
        Ok(index)
    }

    fn read_btree_page(&self, page_no: u32, rows: &mut Vec<SqliteRow>, depth: u32) -> Result<()> {
        if page_no == 0 || depth > 64 {
            return Err(OpenSlideError::Format(
                "Invalid or excessively deep SQLite btree".into(),
            ));
        }
        let page = self.read_page(page_no)?;
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
                    rows.push(self.parse_leaf_table_cell(&page, ptr)?);
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
                    self.read_btree_page(child, rows, depth + 1)?;
                }
                let rightmost = read_be_u32(&page[header_offset + 8..header_offset + 12]);
                self.read_btree_page(rightmost, rows, depth + 1)?;
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Sakura SQLite table uses unsupported btree page type 0x{other:02x}"
                )))
            }
        }
        Ok(())
    }

    fn read_page(&self, page_no: u32) -> Result<Vec<u8>> {
        let offset = (page_no as u64 - 1) * self.page_size as u64;
        crate::util::read_file_range(&self.path, offset, self.page_size as u64)
    }

    fn parse_leaf_table_cell(&self, page: &[u8], offset: usize) -> Result<SqliteRow> {
        let (payload_len, n1) = read_varint(&page[offset..])?;
        let (rowid, n2) = read_varint(&page[offset + n1..])?;
        let start = offset + n1 + n2;
        let payload = self.read_cell_payload(page, start, payload_len as usize)?;
        let mut row = parse_record(&payload)?;
        row.rowid = Some(i64::try_from(rowid).map_err(|_| {
            OpenSlideError::Format(format!("SQLite rowid {rowid} does not fit i64"))
        })?);
        Ok(row)
    }

    fn read_cell_payload(&self, page: &[u8], start: usize, payload_len: usize) -> Result<Vec<u8>> {
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
            let overflow = self.read_page(overflow_page)?;
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

fn schema_user_table_name(row: &SqliteRow) -> Option<&str> {
    if !row.text(0)?.eq_ignore_ascii_case("table") {
        return None;
    }
    let name = row.text(1)?;
    (!name
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sqlite_")))
    .then_some(name)
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

    fn sqlite3_column_double(&self, index: usize) -> f64 {
        match self.values.get(index) {
            Some(SqliteValue::Real(value)) if value.is_finite() => *value,
            Some(SqliteValue::Real(_)) => 0.0,
            Some(SqliteValue::Integer(value)) => *value as f64,
            Some(SqliteValue::Text(value)) => sqlite3_text_to_double(value.as_bytes()),
            Some(SqliteValue::Blob(value)) => sqlite3_text_to_double(value),
            _ => 0.0,
        }
    }

    fn text_as_upstream_property(&self, index: usize) -> Option<String> {
        match self.values.get(index) {
            Some(SqliteValue::Text(value)) if !value.is_empty() => Some(value.clone()),
            Some(SqliteValue::Integer(value)) => Some(value.to_string()),
            Some(SqliteValue::Real(value)) if value.is_finite() => Some(format_float(*value)),
            Some(SqliteValue::Blob(value)) => {
                let bytes = value.split(|byte| *byte == 0).next().unwrap_or(value);
                (!bytes.is_empty())
                    .then(|| std::str::from_utf8(bytes).ok().map(str::to_string))
                    .flatten()
            }
            _ => None,
        }
    }
}

impl TileSource {
    fn apply_rowid_alias(&self, row: &mut SqliteRow) {
        let Some(col) = self.rowid_alias_col else {
            return;
        };
        let Some(rowid) = row.rowid else {
            return;
        };
        if matches!(row.values.get(col), Some(SqliteValue::Null)) {
            row.values[col] = SqliteValue::Integer(rowid);
        }
    }
}

fn find_sakura_tile_id_source(db: &SqliteDatabase) -> Option<TileSource> {
    let unique_table_name = get_unique_table_name(db).ok()?;
    let table = find_table_by_name(db, &unique_table_name)?;
    Some(TileSource {
        root_page: table.root_page,
        blob_col: column_index(table, "data")?,
        address: TileAddress::SakuraTileId {
            id_col: column_index(table, "id")?,
        },
        focal_plane: None,
        rowid_alias_col: table.rowid_alias_col,
    })
}

fn tile_coordinates(
    source: &TileSource,
    row: &SqliteRow,
    levels: &[SakuraLevel],
) -> Result<Option<(i64, i64)>> {
    match source.address {
        TileAddress::SakuraTileId { id_col } => {
            let Some(tileid) = row.text(id_col) else {
                return Ok(None);
            };
            let Some(parsed) = parse_tileid(tileid)? else {
                return Ok(None);
            };
            if Some(parsed.focal_plane) != source.focal_plane {
                return Ok(None);
            }
            let Some(first_level) = levels.first() else {
                return Ok(None);
            };
            let tile_span = parsed.downsample.checked_mul(first_level.tile_size as i64);
            let Some(tile_span) = tile_span else {
                return Ok(None);
            };
            if tile_span <= 0 || parsed.x % tile_span != 0 || parsed.y % tile_span != 0 {
                return Ok(None);
            }
            Ok(Some((parsed.x / tile_span, parsed.y / tile_span)))
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
        .map(parse_tileid_int64)
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

fn parse_tileid_int64(value: &str) -> std::result::Result<i64, ()> {
    crate::util::_openslide_parse_int64(value).ok_or(())
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
    let rows = db.read_table_rows(source.root_page)?;
    let downsamples = sakura_tile_id_downsamples(&rows, source)?;
    sakura_levels_from_downsamples(downsamples, header)
}

fn sakura_levels_from_downsamples(
    downsamples: BTreeSet<i64>,
    header: SakuraHeader,
) -> Result<Vec<SakuraLevel>> {
    if downsamples.is_empty() {
        return Err(OpenSlideError::Format("Couldn't find any tiles".into()));
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

fn sakura_tile_id_downsamples(rows: &[SqliteRow], source: &TileSource) -> Result<BTreeSet<i64>> {
    let mut downsamples = BTreeSet::new();
    let TileAddress::SakuraTileId { id_col } = source.address;
    for row in rows {
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
    Ok(downsamples)
}

fn compute_quickhash1(db: &SqliteDatabase, source: &TileSource) -> Result<Option<String>> {
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
    let Some(table) = find_table_by_name(db, table_name) else {
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
    let Some(table) = find_table_by_name(db, &unique_table_name) else {
        return Ok(false);
    };
    let Some(id_col) = column_index(table, "id") else {
        return Ok(false);
    };
    let Some(data_col) = column_index(table, "data") else {
        return Ok(false);
    };
    let rows = db.read_table_rows(table.root_page)?;
    let matching = unique_table_rows_by_rowid(&rows, id_col, id);
    for row in &matching {
        if let Some(bytes) = row.bytes(data_col) {
            quickhash1.openslide_hash_data(bytes);
        }
        if nul_terminate {
            quickhash1.openslide_hash_data(&[0]);
        }
    }
    Ok(!matching.is_empty())
}

fn unique_table_rows_by_rowid<'a>(
    rows: &'a [SqliteRow],
    id_col: usize,
    id: &str,
) -> Vec<&'a SqliteRow> {
    let mut matching = rows
        .iter()
        .filter(|row| row.text(id_col) == Some(id))
        .collect::<Vec<_>>();
    matching.sort_by_key(|row| row.rowid.unwrap_or(i64::MAX));
    matching
}

fn hash_sakura_tiles(
    quickhash1: &mut OpenslideHash,
    db: &SqliteDatabase,
    source: &TileSource,
) -> Result<bool> {
    let TileAddress::SakuraTileId { id_col } = source.address;
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

fn find_associated_images(db: &SqliteDatabase) -> Result<HashMap<String, AssociatedImage>> {
    let mut images = HashMap::new();
    add_upstream_associated_images(db, &mut images)?;
    Ok(images)
}

fn add_upstream_associated_images(
    db: &SqliteDatabase,
    images: &mut HashMap<String, AssociatedImage>,
) -> Result<()> {
    let Some(slide_table) = find_table_by_name(db, "SVSlideDataXPO") else {
        return Ok(());
    };
    let Ok(slide_rows) = db.read_table_rows(slide_table.root_page) else {
        return Ok(());
    };
    let Some(slide_oid_col) = column_index(slide_table, "OID") else {
        return Ok(());
    };

    if let Some(scanned_table) = find_table_by_name(db, "SVScannedImageDataXPO") {
        if let Ok(scanned_rows) = db.read_table_rows(scanned_table.root_page) {
            if let (Some(image_oid_col), Some(image_col)) = (
                column_index(scanned_table, "OID"),
                column_index(scanned_table, "Image"),
            ) {
                insert_joined_associated_image(
                    images,
                    "label",
                    &slide_rows,
                    column_index(slide_table, "m_labelScan"),
                    &scanned_rows,
                    image_oid_col,
                    image_col,
                );
                insert_joined_associated_image(
                    images,
                    "macro",
                    &slide_rows,
                    column_index(slide_table, "m_overviewScan"),
                    &scanned_rows,
                    image_oid_col,
                    image_col,
                );
            }
        }
    }

    if let Some(scan_table) = find_table_by_name(db, "SVHRScanDataXPO") {
        if let Ok(scan_rows) = db.read_table_rows(scan_table.root_page) {
            if let (Some(parent_col), Some(thumbnail_col)) = (
                column_index(scan_table, "ParentSlide"),
                column_index(scan_table, "ThumbnailImage"),
            ) {
                insert_joined_associated_image(
                    images,
                    "thumbnail",
                    &slide_rows,
                    Some(slide_oid_col),
                    &scan_rows,
                    parent_col,
                    thumbnail_col,
                );
            }
        }
    }

    Ok(())
}

fn insert_joined_associated_image(
    images: &mut HashMap<String, AssociatedImage>,
    name: &str,
    left_rows: &[SqliteRow],
    left_ref_col: Option<usize>,
    right_rows: &[SqliteRow],
    right_oid_col: usize,
    right_data_col: usize,
) {
    let Some(left_ref_col) = left_ref_col else {
        return;
    };
    let mut matches = 0usize;
    let mut first_match = None;
    for left in left_rows {
        let Some(reference) = left.integer(left_ref_col) else {
            continue;
        };
        for right in right_rows {
            if right.integer(right_oid_col) == Some(reference) {
                matches += 1;
                first_match.get_or_insert_with(|| right.bytes(right_data_col));
            }
        }
    }
    if matches == 1 {
        if let Some(Some(bytes)) = first_match {
            if let Some(image) = decode_jpeg_associated_image_metadata(bytes) {
                images.insert(name.to_string(), image);
            }
        }
    }
}

fn add_properties(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    let Some(slide_table) = find_table_by_name(db, "SVSlideDataXPO") else {
        add_version_property(db, properties);
        return;
    };
    let Some(scan_table) = find_table_by_name(db, "SVHRScanDataXPO") else {
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
        if let Some(mmpp) = column_index(scan_table, "ResolutionMmPerPix")
            .map(|col| scan_row.sqlite3_column_double(col))
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
        crate::util::_openslide_duplicate_double_prop(
            properties,
            "sakura.NominalLensMagnification",
            crate::properties::PROPERTY_OBJECTIVE_POWER,
        );
    }

    add_version_property(db, properties);
}

fn add_text_property(
    properties: &mut HashMap<String, String>,
    table: &SqliteTable,
    row: &SqliteRow,
    column: &str,
) {
    let Some(value) =
        column_index(table, column).and_then(|col| row.text_as_upstream_property(col))
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
    let Some(value) = column_index(table, column).map(|col| row.sqlite3_column_double(col)) else {
        return;
    };
    properties.insert(format!("sakura.{column}"), format_float(value));
}

fn add_version_property(db: &SqliteDatabase, properties: &mut HashMap<String, String>) {
    let Ok(unique_table_name) = get_unique_table_name(db) else {
        return;
    };
    let Some(table) = find_table_by_name(db, &unique_table_name) else {
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
            if let Some(value) = row.text_as_upstream_property(data_col) {
                properties.insert("sakura.VersionBytes".into(), value);
            }
            return;
        }
    }
}

#[cfg(test)]
fn parse_create_table_columns(sql: &str) -> Vec<String> {
    parse_create_table_schema(sql).columns
}

fn parse_create_table_schema(sql: &str) -> TableSchema {
    let Some(open) = sql.find('(') else {
        return TableSchema {
            columns: Vec::new(),
            rowid_alias_col: None,
        };
    };
    let mut depth = 0usize;
    let mut current = String::new();
    let mut parts = Vec::new();
    let mut quote: Option<char> = None;
    let mut chars = sql[open + 1..].chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(close) = quote {
            current.push(ch);
            if ch == close {
                if chars.peek() == Some(&close) {
                    current.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                current.push(ch);
            }
            '[' => {
                quote = Some(']');
                current.push(ch);
            }
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

    let mut columns = Vec::new();
    let mut rowid_alias_col = None;
    for part in parts {
        let part = part.trim();
        let Some(name) = first_identifier(part) else {
            continue;
        };
        if matches!(
            normalize_identifier(&name).as_str(),
            "constraint" | "primary" | "foreign" | "unique" | "check"
        ) {
            continue;
        }
        let column_index = columns.len();
        if is_integer_primary_key_column(part) && rowid_alias_col.is_none() {
            rowid_alias_col = Some(column_index);
        }
        columns.push(name);
    }
    TableSchema {
        columns,
        rowid_alias_col,
    }
}

fn first_identifier(value: &str) -> Option<String> {
    let trimmed = value.trim_start();
    let first = trimmed.chars().next()?;
    if matches!(first, '"' | '\'' | '`' | '[') {
        let close = if first == '[' { ']' } else { first };
        let mut out = String::new();
        let mut chars = trimmed[1..].chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == close {
                if chars.peek() == Some(&close) {
                    out.push(ch);
                    chars.next();
                    continue;
                }
                return Some(out);
            }
            out.push(ch);
        }
        None
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

fn is_integer_primary_key_column(definition: &str) -> bool {
    let normalized = normalize_identifier(definition);
    normalized.contains("integerprimarykey") && !normalized.contains("integerprimarykeydesc")
}

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn sqlite3_text_to_double(value: &[u8]) -> f64 {
    let mut start = 0usize;
    while value
        .get(start)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        start += 1;
    }
    let Some(end) = sqlite_numeric_prefix_end(&value[start..]) else {
        return 0.0;
    };
    std::str::from_utf8(&value[start..start + end])
        .ok()
        .and_then(|text| text.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
}

fn sqlite_numeric_prefix_end(value: &[u8]) -> Option<usize> {
    let mut pos = 0usize;
    if value
        .get(pos)
        .is_some_and(|byte| matches!(byte, b'+' | b'-'))
    {
        pos += 1;
    }
    let mut mantissa_digits = 0usize;
    while value.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
        mantissa_digits += 1;
    }
    if value.get(pos) == Some(&b'.') {
        pos += 1;
        while value.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
            mantissa_digits += 1;
        }
    }
    if mantissa_digits == 0 {
        return None;
    }
    let mantissa_end = pos;
    if value
        .get(pos)
        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
    {
        let exponent_marker = pos;
        pos += 1;
        if value
            .get(pos)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            pos += 1;
        }
        let mut exponent_digits = 0usize;
        while value.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
            exponent_digits += 1;
        }
        if exponent_digits == 0 {
            return Some(mantissa_end.min(exponent_marker));
        }
    }
    Some(pos)
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
    Ok(SqliteRow {
        rowid: None,
        values,
    })
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

fn is_jpeg(data: &[u8]) -> bool {
    data.starts_with(&[0xff, 0xd8])
}

fn decode_jpeg_associated_image_metadata(data: &[u8]) -> Option<AssociatedImage> {
    if !is_jpeg(data) {
        return None;
    }
    let decoded = decode::decode_to_rgba(ImageFormat::Jpeg, data).ok()?;
    Some(AssociatedImage {
        data: data.to_vec(),
        width: decoded.width,
        height: decoded.height,
    })
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
    let Some(table) = find_table_by_name(db, &unique_table_name) else {
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

fn require_sakura_header(header: Option<SakuraHeader>) -> Result<SakuraHeader> {
    header.ok_or_else(|| OpenSlideError::Format("Missing Sakura Header row".into()))
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

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&data[offset..offset + 4]);
    u32::from_le_bytes(bytes)
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
            rowid_alias_col: None,
        };
        let rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Text("SVGigaPixelImage".into()),
            ],
        }];

        assert!(has_sakura_magic_bytes(&unique_table, &rows));

        let blob_rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Blob(b"SVGigaPixelImage".to_vec()),
            ],
        }];
        assert!(has_sakura_magic_bytes(&unique_table, &blob_rows));

        let wrong_rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Text("++MagicBytes".into()),
                SqliteValue::Text("not sakura".into()),
            ],
        }];
        assert!(!has_sakura_magic_bytes(&unique_table, &wrong_rows));

        let duplicate_rows = vec![
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Text("++MagicBytes".into()),
                    SqliteValue::Text("not sakura".into()),
                ],
            },
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Text("++MagicBytes".into()),
                    SqliteValue::Text("SVGigaPixelImage".into()),
                ],
            },
        ];
        assert!(!has_sakura_magic_bytes(&unique_table, &duplicate_rows));
    }

    #[test]
    fn finds_unique_table_case_insensitively() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 4096,
            reserved_bytes: 0,
            tables: vec![SqliteTable {
                name: "imagedata".into(),
                root_page: 2,
                columns: vec!["id".into(), "data".into()],
                rowid_alias_col: None,
            }],
        };

        let table = find_table_by_name(&db, "ImageData").unwrap();
        assert_eq!(table.root_page, 2);
    }

    #[test]
    fn finds_sakura_config_table_case_insensitively() {
        let db = SqliteDatabase {
            path: PathBuf::from("dummy.svslide"),
            page_size: 4096,
            reserved_bytes: 0,
            tables: vec![SqliteTable {
                name: "datamanagersqliteconfigxpo".into(),
                root_page: 7,
                columns: vec!["TableName".into()],
                rowid_alias_col: None,
            }],
        };

        let table = find_sakura_config_table(&db).unwrap();
        assert_eq!(table.root_page, 7);
    }

    #[test]
    fn unique_table_config_reports_multiple_rows_like_upstream() {
        let rows = vec![
            SqliteRow {
                rowid: Some(1),
                values: vec![SqliteValue::Text("ImageData".into())],
            },
            SqliteRow {
                rowid: Some(2),
                values: vec![SqliteValue::Text("OtherImageData".into())],
            },
        ];

        let err = sakura_unique_table_name_from_rows(&rows, 0).unwrap_err();
        assert!(format!("{err}").contains("Found > 1 unique tables"));
    }

    #[test]
    fn schema_user_table_filter_is_case_insensitive() {
        let user_row = SqliteRow {
            rowid: Some(1),
            values: vec![
                SqliteValue::Text("TABLE".into()),
                SqliteValue::Text("TileStore".into()),
            ],
        };
        let internal_row = SqliteRow {
            rowid: Some(2),
            values: vec![
                SqliteValue::Text("table".into()),
                SqliteValue::Text("SQLITE_AutoIndex_TileStore_1".into()),
            ],
        };

        assert_eq!(schema_user_table_name(&user_row), Some("TileStore"));
        assert_eq!(schema_user_table_name(&internal_row), None);
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
    fn requires_sakura_header_like_upstream() {
        let err = require_sakura_header(None).unwrap_err();

        assert!(format!("{err}").contains("Missing Sakura Header row"));
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
    fn reads_sqlite_numeric_values_like_sqlite_column_double() {
        let row = SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Real(0.00025),
                SqliteValue::Integer(40),
                SqliteValue::Text("20x".into()),
            ],
        };

        assert_eq!(row.sqlite3_column_double(0), 0.00025);
        assert_eq!(row.sqlite3_column_double(1), 40.0);
        assert_eq!(row.sqlite3_column_double(2), 20.0);
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(format_float(1.2345678901234567), "1.2345678901234567");
    }

    #[test]
    fn unique_table_rows_are_ordered_by_rowid_for_hashing() {
        let rows = vec![
            SqliteRow {
                rowid: Some(9),
                values: vec![
                    SqliteValue::Text("Header".into()),
                    SqliteValue::Blob(b"late".to_vec()),
                ],
            },
            SqliteRow {
                rowid: Some(3),
                values: vec![
                    SqliteValue::Text("Header".into()),
                    SqliteValue::Blob(b"early".to_vec()),
                ],
            },
            SqliteRow {
                rowid: Some(1),
                values: vec![
                    SqliteValue::Text("Other".into()),
                    SqliteValue::Blob(b"ignored".to_vec()),
                ],
            },
        ];

        let ordered = unique_table_rows_by_rowid(&rows, 0, "Header")
            .into_iter()
            .map(|row| row.bytes(1).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(ordered, vec![b"early".as_slice(), b"late".as_slice()]);
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
        assert!(format!("{}", parse_tileid("T;+512|256;2;1;0").unwrap_err())
            .contains("Couldn't round-trip tile ID"));
        assert!(format!("{}", parse_tileid("T; 512|256;2;1;0").unwrap_err())
            .contains("Couldn't round-trip tile ID"));
        assert!(format!("{}", parse_tileid("T;512 |256;2;1;0").unwrap_err())
            .contains("Bad field value in tile ID"));
    }

    #[test]
    fn tile_coordinates_propagates_bad_sakura_tile_ids_like_upstream() {
        let source = TileSource {
            root_page: 1,
            blob_col: 1,
            address: TileAddress::SakuraTileId { id_col: 0 },
            focal_plane: Some(0),
            rowid_alias_col: None,
        };
        let levels = vec![SakuraLevel {
            width: 1024,
            height: 1024,
            downsample: 1.0,
            tile_size: 256,
        }];
        let non_tile = SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Text("++MagicBytes".into())],
        };
        let malformed_tile = SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Text("T;0256|0;1;0;0".into())],
        };

        assert_eq!(tile_coordinates(&source, &non_tile, &levels).unwrap(), None);
        assert!(tile_coordinates(&source, &malformed_tile, &levels).is_err());
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
    fn parses_escaped_quoted_create_table_identifiers() {
        let columns = parse_create_table_columns(
            r#"CREATE TABLE "Tile "" Store" (
                "Tile "" X" INTEGER,
                `Tile `` Y` INTEGER,
                'Image '' Data' BLOB,
                [Bracket ]] Name] TEXT
            )"#,
        );

        assert_eq!(
            columns,
            ["Tile \" X", "Tile ` Y", "Image ' Data", "Bracket ] Name"]
        );
    }

    #[test]
    fn parses_create_table_columns_with_quoted_commas_and_parentheses() {
        let columns = parse_create_table_columns(
            r#"CREATE TABLE Tiles (
                TileX INTEGER DEFAULT "literal, with comma",
                TileY INTEGER CHECK (TileY IN (0, 1)),
                ImageData BLOB DEFAULT 'literal ) close'
            )"#,
        );

        assert_eq!(columns, ["TileX", "TileY", "ImageData"]);
    }

    #[test]
    fn detects_integer_primary_key_rowid_alias() {
        let schema = parse_create_table_schema(
            r#"CREATE TABLE ImageTiles (
                TileIndex INTEGER PRIMARY KEY,
                ImageData BLOB
            )"#,
        );

        assert_eq!(schema.columns, ["TileIndex", "ImageData"]);
        assert_eq!(schema.rowid_alias_col, Some(0));
    }

    #[test]
    fn converts_fixed_sakura_text_properties_like_upstream() {
        let text_row = SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Text("  specimen  ".into()),
                SqliteValue::Blob(b"v1\0ignored".to_vec()),
            ],
        };
        assert_eq!(
            text_row.text_as_upstream_property(0).as_deref(),
            Some("  specimen  ")
        );
        assert_eq!(text_row.text_as_upstream_property(1).as_deref(), Some("v1"));
    }

    #[test]
    fn converts_sakura_float_columns_like_sqlite_column_double() {
        let row = SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Text(" \t+0.00025mm".into()),
                SqliteValue::Text("not numeric".into()),
                SqliteValue::Text("7e".into()),
                SqliteValue::Blob(b"-2.5tail".to_vec()),
                SqliteValue::Null,
            ],
        };

        assert_eq!(row.sqlite3_column_double(0), 0.00025);
        assert_eq!(row.sqlite3_column_double(1), 0.0);
        assert_eq!(row.sqlite3_column_double(2), 7.0);
        assert_eq!(row.sqlite3_column_double(3), -2.5);
        assert_eq!(row.sqlite3_column_double(4), 0.0);
        assert_eq!(row.sqlite3_column_double(99), 0.0);
    }

    #[test]
    fn emits_sakura_float_property_even_for_sqlite_non_numeric_value() {
        let table = SqliteTable {
            name: "SVHRScanDataXPO".into(),
            root_page: 1,
            columns: vec!["ResolutionMmPerPix".into()],
            rowid_alias_col: None,
        };
        let row = SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Text("not numeric".into())],
        };
        let mut props = HashMap::new();

        add_float_property(&mut props, &table, &row, "ResolutionMmPerPix");

        assert_eq!(
            props.get("sakura.ResolutionMmPerPix"),
            Some(&"0".to_string())
        );
    }

    #[test]
    fn maps_sakura_tile_id_to_grid_coordinates() {
        let source = TileSource {
            root_page: 5,
            blob_col: 1,
            address: TileAddress::SakuraTileId { id_col: 0 },
            focal_plane: Some(0),
            rowid_alias_col: None,
        };
        let row = SqliteRow {
            rowid: None,
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
            tile_coordinates(&source, &row, &levels).unwrap(),
            Some((2, 1))
        );
    }

    #[test]
    fn discovers_sakura_tile_id_levels_from_plane_zero() {
        let source = TileSource {
            root_page: 5,
            blob_col: 1,
            address: TileAddress::SakuraTileId { id_col: 0 },
            focal_plane: Some(2),
            rowid_alias_col: None,
        };
        let rows = vec![
            SqliteRow {
                rowid: None,
                values: vec![SqliteValue::Text("T;0|0;1;0;0".into())],
            },
            SqliteRow {
                rowid: None,
                values: vec![SqliteValue::Text("T;0|0;1;0;2".into())],
            },
            SqliteRow {
                rowid: None,
                values: vec![SqliteValue::Text("T;256|0;2;1;2".into())],
            },
        ];

        let downsamples = sakura_tile_id_downsamples(&rows, &source).unwrap();

        assert_eq!(downsamples, BTreeSet::from([1]));
    }

    #[test]
    fn rejects_sakura_without_focal_plane_zero_levels_like_upstream() {
        let source = TileSource {
            root_page: 5,
            blob_col: 1,
            address: TileAddress::SakuraTileId { id_col: 0 },
            focal_plane: Some(1),
            rowid_alias_col: None,
        };
        let rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Text("T;0|0;1;0;1".into())],
        }];

        let downsamples = sakura_tile_id_downsamples(&rows, &source).unwrap();
        let err = sakura_levels_from_downsamples(
            downsamples,
            SakuraHeader {
                tile_size: 256,
                width: 1024,
                height: 1024,
                focal_planes: 3,
            },
        )
        .unwrap_err();

        assert!(format!("{err}").contains("Couldn't find any tiles"));
    }

    #[test]
    fn upstream_associated_image_joins_take_precedence() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Integer(11),
                SqliteValue::Blob(ONE_PIXEL_JPEG.to_vec()),
            ],
        }];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        let image = images.get("label").unwrap();
        assert_eq!(image.data, ONE_PIXEL_JPEG);
        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn upstream_associated_image_join_rejects_non_jpeg_like_upstream() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Integer(11),
                SqliteValue::Blob(one_by_one_bmp([31, 37, 41])),
            ],
        }];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        assert!(!images.contains_key("label"));
    }

    #[test]
    fn upstream_associated_image_join_rejects_truncated_jpeg_like_upstream() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![SqliteRow {
            rowid: None,
            values: vec![
                SqliteValue::Integer(11),
                SqliteValue::Blob(vec![0xff, 0xd8]),
            ],
        }];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        assert!(!images.contains_key("label"));
    }

    #[test]
    fn upstream_associated_image_join_rejects_multiple_valid_rows() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(ONE_PIXEL_JPEG.to_vec()),
                ],
            },
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(ONE_PIXEL_JPEG.to_vec()),
                ],
            },
        ];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        assert!(!images.contains_key("label"));
    }

    #[test]
    fn upstream_associated_image_join_rejects_extra_invalid_row_like_upstream() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(ONE_PIXEL_JPEG.to_vec()),
                ],
            },
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(one_by_one_bmp([31, 37, 41])),
                ],
            },
        ];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        assert!(!images.contains_key("label"));
    }

    #[test]
    fn upstream_associated_image_join_uses_first_row_for_validation_like_upstream() {
        let mut images = HashMap::new();
        let slide_rows = vec![SqliteRow {
            rowid: None,
            values: vec![SqliteValue::Integer(7), SqliteValue::Integer(11)],
        }];
        let scanned_rows = vec![
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(one_by_one_bmp([31, 37, 41])),
                ],
            },
            SqliteRow {
                rowid: None,
                values: vec![
                    SqliteValue::Integer(11),
                    SqliteValue::Blob(ONE_PIXEL_JPEG.to_vec()),
                ],
            },
        ];

        insert_joined_associated_image(
            &mut images,
            "label",
            &slide_rows,
            Some(1),
            &scanned_rows,
            0,
            1,
        );

        assert!(!images.contains_key("label"));
    }

    #[test]
    fn exposes_and_decodes_associated_images() {
        let mut associated_images = HashMap::new();
        associated_images.insert(
            "label".into(),
            AssociatedImage {
                data: ONE_PIXEL_JPEG.to_vec(),
                width: 17,
                height: 19,
            },
        );
        let slide = SakuraSlide {
            levels: Vec::new(),
            properties: HashMap::new(),
            sqlite: None,
            tile_source: None,
            tile_index: Mutex::new(None),
            associated_images,
        };

        assert_eq!(slide.associated_image_names(), ["label"]);
        assert_eq!(slide.associated_image_dimensions("label"), Some((17, 19)));
        assert_eq!(slide.associated_image_dimensions("missing"), None);
        let image = slide.read_associated_image("label").unwrap();
        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn exposes_header_tile_size_as_level_tile_dimensions() {
        let slide = SakuraSlide {
            levels: vec![SakuraLevel {
                width: 1024,
                height: 512,
                downsample: 2.0,
                tile_size: 240,
            }],
            properties: HashMap::new(),
            sqlite: None,
            tile_source: None,
            tile_index: Mutex::new(None),
            associated_images: HashMap::new(),
        };

        assert_eq!(slide.level_tile_dimensions(0), Some((240, 240)));
        assert_eq!(slide.level_tile_dimensions(1), None);
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

    const ONE_PIXEL_JPEG: &[u8] = &[
        0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07,
        0x07, 0x07, 0x09, 0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
        0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20, 0x24, 0x2e, 0x27,
        0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29, 0x2c, 0x30, 0x31, 0x34, 0x34, 0x34,
        0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32, 0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xc0, 0x00, 0x11,
        0x08, 0x00, 0x01, 0x00, 0x01, 0x03, 0x52, 0x11, 0x00, 0x47, 0x11, 0x00, 0x42, 0x11, 0x00,
        0xff, 0xc4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0xda, 0x00, 0x0c, 0x03, 0x52, 0x00, 0x47, 0x00, 0x42, 0x00, 0x00, 0x3f, 0x00, 0x7f, 0x3f,
        0x9f, 0xdf, 0xff, 0xd9,
    ];
}
