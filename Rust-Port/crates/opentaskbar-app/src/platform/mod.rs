//! Windows platform interop, app pin discovery, and background icon extraction.

// New C4/C5 modules expose APIs consumed by other workstreams after merge.
#![allow(dead_code)]

pub mod icon_cache;
pub mod icon_pack;
pub mod pins;
pub mod theme_packs;
pub mod toast;
pub mod update;
pub mod win32;
