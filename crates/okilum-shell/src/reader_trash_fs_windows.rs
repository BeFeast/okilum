//! Windows Move to Trash: the Recycle Bin transport lives in okilum-core so
//! its native tests run on Windows CI (#1124).
pub use okilum_core::recycle_bin::{move_to_trash, stamp, Trashed};
