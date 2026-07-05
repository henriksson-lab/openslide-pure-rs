//! OpenSlide private debug flag helpers.

use std::sync::atomic::{AtomicI32, Ordering};

/// Environment variable used by upstream OpenSlide for private debug flags.
pub const OPENSLIDE_DEBUG_ENV_VAR: &str = "OPENSLIDE_DEBUG";

/// Private OpenSlide debug flag enum from `openslide-private.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OpenSlideDebugFlag {
    Decoding = 0,
    Detection = 1,
    JpegMarkers = 2,
    Performance = 3,
    Search = 4,
    Sql = 5,
    Synthetic = 6,
    Tiles = 7,
}

pub const OPENSLIDE_DEBUG_DECODING: OpenSlideDebugFlag = OpenSlideDebugFlag::Decoding;
pub const OPENSLIDE_DEBUG_DETECTION: OpenSlideDebugFlag = OpenSlideDebugFlag::Detection;
pub const OPENSLIDE_DEBUG_JPEG_MARKERS: OpenSlideDebugFlag = OpenSlideDebugFlag::JpegMarkers;
pub const OPENSLIDE_DEBUG_PERFORMANCE: OpenSlideDebugFlag = OpenSlideDebugFlag::Performance;
pub const OPENSLIDE_DEBUG_SEARCH: OpenSlideDebugFlag = OpenSlideDebugFlag::Search;
pub const OPENSLIDE_DEBUG_SQL: OpenSlideDebugFlag = OpenSlideDebugFlag::Sql;
pub const OPENSLIDE_DEBUG_SYNTHETIC: OpenSlideDebugFlag = OpenSlideDebugFlag::Synthetic;
pub const OPENSLIDE_DEBUG_TILES: OpenSlideDebugFlag = OpenSlideDebugFlag::Tiles;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenSlideDebugOption {
    pub keyword: &'static str,
    pub flag: OpenSlideDebugFlag,
    pub description: &'static str,
}

/// Keyword table copied from upstream `openslide-util.c`.
pub const OPENSLIDE_DEBUG_OPTIONS: &[OpenSlideDebugOption] = &[
    OpenSlideDebugOption {
        keyword: "decoding",
        flag: OPENSLIDE_DEBUG_DECODING,
        description: "log warnings from format decoding libraries",
    },
    OpenSlideDebugOption {
        keyword: "detection",
        flag: OPENSLIDE_DEBUG_DETECTION,
        description: "log format detection errors",
    },
    OpenSlideDebugOption {
        keyword: "jpeg-markers",
        flag: OPENSLIDE_DEBUG_JPEG_MARKERS,
        description: "verify Hamamatsu restart markers",
    },
    OpenSlideDebugOption {
        keyword: "performance",
        flag: OPENSLIDE_DEBUG_PERFORMANCE,
        description: "log conditions causing poor performance",
    },
    OpenSlideDebugOption {
        keyword: "search",
        flag: OPENSLIDE_DEBUG_SEARCH,
        description: "log skipped files when searching directory",
    },
    OpenSlideDebugOption {
        keyword: "sql",
        flag: OPENSLIDE_DEBUG_SQL,
        description: "log SQL queries",
    },
    OpenSlideDebugOption {
        keyword: "synthetic",
        flag: OPENSLIDE_DEBUG_SYNTHETIC,
        description: "openslide_open(\"\") opens a synthetic test slide",
    },
    OpenSlideDebugOption {
        keyword: "tiles",
        flag: OPENSLIDE_DEBUG_TILES,
        description: "render tile outlines",
    },
];

/// Initialize OpenSlide debug flags.
///
/// Upstream stores parsed flags globally.  Rust callers read the current
/// environment directly through `_openslide_debug()`, so this is intentionally
/// a no-op compatibility hook for direct source translations.
pub fn _openslide_debug_init() {}

/// Return whether an OpenSlide private debug flag is enabled.
pub fn _openslide_debug(flag: OpenSlideDebugFlag) -> bool {
    std::env::var(OPENSLIDE_DEBUG_ENV_VAR)
        .ok()
        .is_some_and(|value| debug_flag_enabled_in(&value, flag))
}

pub(crate) fn debug_flag_enabled_in(value: &str, flag: OpenSlideDebugFlag) -> bool {
    value.split(',').map(str::trim).any(|keyword| {
        OPENSLIDE_DEBUG_OPTIONS
            .iter()
            .any(|option| option.flag == flag && keyword.eq_ignore_ascii_case(option.keyword))
    })
}

/// Emit a performance warning when `OPENSLIDE_DEBUG=performance` is enabled.
///
/// If `warned_flag` is provided, the message is emitted only for the first
/// caller that changes the flag from 0 to 1, matching upstream's
/// `g_atomic_int_compare_and_exchange(warned_flag, 0, 1)` guard.
pub fn _openslide_performance_warn_once(warned_flag: Option<&AtomicI32>, message: &str) -> bool {
    _openslide_performance_warn_once_with(warned_flag, message, |message| {
        eprintln!("{message}");
    })
}

pub(crate) fn _openslide_performance_warn_once_with(
    warned_flag: Option<&AtomicI32>,
    message: &str,
    mut emit: impl FnMut(&str),
) -> bool {
    if !_openslide_debug(OPENSLIDE_DEBUG_PERFORMANCE) {
        return false;
    }
    if warned_flag.is_none_or(|flag| {
        flag.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }) {
        emit(message);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    const UPSTREAM_OPENSLIDE_DEBUG_OPTIONS: &[(&str, OpenSlideDebugFlag, &str)] = &[
        (
            "decoding",
            OPENSLIDE_DEBUG_DECODING,
            "log warnings from format decoding libraries",
        ),
        (
            "detection",
            OPENSLIDE_DEBUG_DETECTION,
            "log format detection errors",
        ),
        (
            "jpeg-markers",
            OPENSLIDE_DEBUG_JPEG_MARKERS,
            "verify Hamamatsu restart markers",
        ),
        (
            "performance",
            OPENSLIDE_DEBUG_PERFORMANCE,
            "log conditions causing poor performance",
        ),
        (
            "search",
            OPENSLIDE_DEBUG_SEARCH,
            "log skipped files when searching directory",
        ),
        ("sql", OPENSLIDE_DEBUG_SQL, "log SQL queries"),
        (
            "synthetic",
            OPENSLIDE_DEBUG_SYNTHETIC,
            "openslide_open(\"\") opens a synthetic test slide",
        ),
        ("tiles", OPENSLIDE_DEBUG_TILES, "render tile outlines"),
    ];

    #[test]
    fn debug_option_table_matches_openslide_util_c() {
        let rust_options: Vec<_> = OPENSLIDE_DEBUG_OPTIONS
            .iter()
            .map(|option| (option.keyword, option.flag, option.description))
            .collect();

        assert_eq!(rust_options, UPSTREAM_OPENSLIDE_DEBUG_OPTIONS);
    }

    #[test]
    fn debug_parser_matches_openslide_comma_split_trim_and_casefold() {
        let value = " decoding, SYNTHETIC ,unknown,tiles ";

        assert!(debug_flag_enabled_in(value, OPENSLIDE_DEBUG_DECODING));
        assert!(debug_flag_enabled_in(value, OPENSLIDE_DEBUG_SYNTHETIC));
        assert!(debug_flag_enabled_in(value, OPENSLIDE_DEBUG_TILES));
        assert!(!debug_flag_enabled_in(value, OPENSLIDE_DEBUG_SQL));
        assert!(!debug_flag_enabled_in("", OPENSLIDE_DEBUG_DECODING));
    }

    #[test]
    fn performance_warn_once_matches_openslide_debug_gate_and_atomic_guard() {
        let _guard = env_lock();
        let old = std::env::var(OPENSLIDE_DEBUG_ENV_VAR).ok();
        std::env::set_var(OPENSLIDE_DEBUG_ENV_VAR, "performance");
        let warned = AtomicI32::new(0);
        let mut messages = Vec::new();

        assert!(_openslide_performance_warn_once_with(
            Some(&warned),
            "slow path",
            |message| messages.push(message.to_string())
        ));
        assert!(!_openslide_performance_warn_once_with(
            Some(&warned),
            "slow path again",
            |message| messages.push(message.to_string())
        ));
        assert!(_openslide_performance_warn_once_with(
            None,
            "unguarded",
            |message| messages.push(message.to_string())
        ));

        assert_eq!(messages, vec!["slow path", "unguarded"]);
        if let Some(old) = old {
            std::env::set_var(OPENSLIDE_DEBUG_ENV_VAR, old);
        } else {
            std::env::remove_var(OPENSLIDE_DEBUG_ENV_VAR);
        }
    }

    #[test]
    fn performance_warn_once_suppressed_without_performance_debug_flag() {
        let _guard = env_lock();
        let old = std::env::var(OPENSLIDE_DEBUG_ENV_VAR).ok();
        std::env::set_var(OPENSLIDE_DEBUG_ENV_VAR, "decoding");
        let warned = AtomicI32::new(0);
        let mut messages = Vec::new();

        assert!(!_openslide_performance_warn_once_with(
            Some(&warned),
            "slow path",
            |message| messages.push(message.to_string())
        ));

        assert!(messages.is_empty());
        assert_eq!(warned.load(Ordering::SeqCst), 0);
        if let Some(old) = old {
            std::env::set_var(OPENSLIDE_DEBUG_ENV_VAR, old);
        } else {
            std::env::remove_var(OPENSLIDE_DEBUG_ENV_VAR);
        }
    }
}
