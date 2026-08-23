//! Runtime settings helpers — the `settings` table holds jsonb values. The
//! caller-supplied default is used ONLY when the row is absent; stored DB
//! errors propagate as `Err` (the upload path turns those into 500), and
//! negative stored values are clamped to the fallback default.

use sqlx::PgPool;
use toottok_db::error::DbError;
use toottok_db::settings::Setting;

pub async fn numeric_setting(pool: &PgPool, key: &str, default: f64) -> Result<f64, DbError> {
    match Setting::fetch_by_key(pool, key).await {
        Ok(Some(s)) => {
            let value = s.value.as_f64().unwrap_or(default);
            Ok(if value < 0.0 { default } else { value })
        }
        Ok(None) => Ok(default),
        Err(e) => Err(e),
    }
}
