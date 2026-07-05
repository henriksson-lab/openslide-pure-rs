/// Well-known OpenSlide property names.
pub const PROPERTY_COMMENT: &str = "openslide.comment";
pub const PROPERTY_VENDOR: &str = "openslide.vendor";
pub const PROPERTY_MPP_X: &str = "openslide.mpp-x";
pub const PROPERTY_MPP_Y: &str = "openslide.mpp-y";
pub const PROPERTY_OBJECTIVE_POWER: &str = "openslide.objective-power";
pub const PROPERTY_BACKGROUND_COLOR: &str = "openslide.background-color";
pub const PROPERTY_BOUNDS_X: &str = "openslide.bounds-x";
pub const PROPERTY_BOUNDS_Y: &str = "openslide.bounds-y";
pub const PROPERTY_BOUNDS_WIDTH: &str = "openslide.bounds-width";
pub const PROPERTY_BOUNDS_HEIGHT: &str = "openslide.bounds-height";
pub const PROPERTY_QUICKHASH1: &str = "openslide.quickhash-1";
pub const PROPERTY_ICC_SIZE: &str = "openslide.icc-size";
pub const PROPERTY_LEVEL_COUNT: &str = "openslide.level-count";

/// Exact aliases for OpenSlide's public `OPENSLIDE_PROPERTY_NAME_*` macros.
pub const OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR: &str = PROPERTY_BACKGROUND_COLOR;
pub const OPENSLIDE_PROPERTY_NAME_BOUNDS_HEIGHT: &str = PROPERTY_BOUNDS_HEIGHT;
pub const OPENSLIDE_PROPERTY_NAME_BOUNDS_WIDTH: &str = PROPERTY_BOUNDS_WIDTH;
pub const OPENSLIDE_PROPERTY_NAME_BOUNDS_X: &str = PROPERTY_BOUNDS_X;
pub const OPENSLIDE_PROPERTY_NAME_BOUNDS_Y: &str = PROPERTY_BOUNDS_Y;
pub const OPENSLIDE_PROPERTY_NAME_COMMENT: &str = PROPERTY_COMMENT;
pub const OPENSLIDE_PROPERTY_NAME_ICC_SIZE: &str = PROPERTY_ICC_SIZE;
pub const OPENSLIDE_PROPERTY_NAME_MPP_X: &str = PROPERTY_MPP_X;
pub const OPENSLIDE_PROPERTY_NAME_MPP_Y: &str = PROPERTY_MPP_Y;
pub const OPENSLIDE_PROPERTY_NAME_OBJECTIVE_POWER: &str = PROPERTY_OBJECTIVE_POWER;
pub const OPENSLIDE_PROPERTY_NAME_QUICKHASH1: &str = PROPERTY_QUICKHASH1;
pub const OPENSLIDE_PROPERTY_NAME_VENDOR: &str = PROPERTY_VENDOR;

/// Exact aliases for OpenSlide's private `_OPENSLIDE_PROPERTY_NAME_*` macros.
pub const _OPENSLIDE_PROPERTY_NAME_LEVEL_COUNT: &str = PROPERTY_LEVEL_COUNT;
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_WIDTH: &str = "openslide.level[%d].width";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_HEIGHT: &str = "openslide.level[%d].height";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_DOWNSAMPLE: &str =
    "openslide.level[%d].downsample";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_TILE_WIDTH: &str =
    "openslide.level[%d].tile-width";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_TILE_HEIGHT: &str =
    "openslide.level[%d].tile-height";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_X: &str = "openslide.region[%d].x";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_Y: &str = "openslide.region[%d].y";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_WIDTH: &str = "openslide.region[%d].width";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_HEIGHT: &str = "openslide.region[%d].height";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_WIDTH: &str =
    "openslide.associated.%s.width";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_HEIGHT: &str =
    "openslide.associated.%s.height";
pub const _OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_ICC_SIZE: &str =
    "openslide.associated.%s.icc-size";

pub fn level_width(level: impl std::fmt::Display) -> String {
    format!("openslide.level[{level}].width")
}

pub fn level_height(level: impl std::fmt::Display) -> String {
    format!("openslide.level[{level}].height")
}

pub fn level_downsample(level: impl std::fmt::Display) -> String {
    format!("openslide.level[{level}].downsample")
}

pub fn level_tile_width(level: impl std::fmt::Display) -> String {
    format!("openslide.level[{level}].tile-width")
}

pub fn level_tile_height(level: impl std::fmt::Display) -> String {
    format!("openslide.level[{level}].tile-height")
}

pub fn associated_width(name: &str) -> String {
    format!("openslide.associated.{name}.width")
}

pub fn associated_height(name: &str) -> String {
    format!("openslide.associated.{name}.height")
}

pub fn associated_icc_size(name: &str) -> String {
    format!("openslide.associated.{name}.icc-size")
}

pub fn region_x(region: impl std::fmt::Display) -> String {
    format!("openslide.region[{region}].x")
}

pub fn region_y(region: impl std::fmt::Display) -> String {
    format!("openslide.region[{region}].y")
}

pub fn region_width(region: impl std::fmt::Display) -> String {
    format!("openslide.region[{region}].width")
}

pub fn region_height(region: impl std::fmt::Display) -> String {
    format!("openslide.region[{region}].height")
}
