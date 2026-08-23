//! Hashtag extraction and clip linking.

use crate::error::DbError;

/// Hard cap on distinct tags stored per caption.
const MAX_HASHTAGS: usize = 10;

/// Scan `caption` for `#` followed by `[a-zA-Z0-9_]+` runs. Tags are
/// lowercased, deduped preserving first-seen order, capped at
/// [`MAX_HASHTAGS`]. Safe on multi-byte UTF-8: `#` and the tag charset are
/// ASCII, so slices always land on char boundaries.
pub fn extract_hashtags(caption: &str) -> Vec<String> {
    let bytes = caption.as_bytes();
    let mut tags: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() && tags.len() < MAX_HASHTAGS {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end > start {
            let tag = caption[start..end].to_ascii_lowercase();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        i = end.max(start);
    }
    tags
}

/// Extract hashtags from `caption`, upsert each into `hashtags`
/// (`ON CONFLICT DO NOTHING`), resolve its id, and link the clip in
/// `clip_hashtags` (`ON CONFLICT DO NOTHING`). One transaction, idempotent.
pub async fn link_hashtags_to_clip(
    pool: &sqlx::PgPool,
    clip_id: i64,
    caption: &str,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    for tag in extract_hashtags(caption) {
        sqlx::query("INSERT INTO hashtags (tag) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(&tag)
            .execute(&mut *tx)
            .await?;
        let hashtag_id: i64 = sqlx::query_scalar("SELECT id FROM hashtags WHERE tag = $1")
            .bind(&tag)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO clip_hashtags (clip_id, hashtag_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(clip_id)
        .bind(hashtag_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
