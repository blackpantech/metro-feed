//! Decoders from raw SQLite column values to typed model values.
//!
//! Shared by every module in [`crate::store`] and by the feed sync pipeline
//! so out-of-range values are reported the same way everywhere: a negative
//! row id or timestamp can only come from database corruption, so it maps to
//! [`MetroError::Generic`].

use crate::MetroError;
use crate::models::{FeedId, FolderId};

/// Convert a non-negative SQLite row id into a [`FeedId`].
pub(crate) fn feed_id_from_row_id(row_id: i64) -> Result<FeedId, MetroError> {
    u64::try_from(row_id)
        .map(FeedId::new)
        .map_err(|_| MetroError::Generic(format!("feed row id {row_id} is out of range")))
}

/// Convert a non-negative SQLite row id into a [`FolderId`].
///
/// SQLite stores integers as signed 64-bit values while folder IDs are
/// unsigned. Row ids are always non-negative, so this conversion only fails
/// on a corrupted database.
pub(crate) fn folder_id_from_row_id(row_id: i64) -> Result<FolderId, MetroError> {
    u64::try_from(row_id)
        .map(FolderId::new)
        .map_err(|_| MetroError::Generic(format!("folder row id {row_id} is out of range")))
}

/// Convert an optional SQLite timestamp into an optional Unix timestamp.
///
/// Negative timestamps are rejected as database corruption because the model
/// represents timestamps as `u64`.
pub(crate) fn timestamp_from_row(value: Option<i64>) -> Result<Option<u64>, MetroError> {
    value
        .map(|timestamp| {
            u64::try_from(timestamp).map_err(|_| {
                MetroError::Generic(format!("negative timestamp {timestamp} stored in database"))
            })
        })
        .transpose()
}
