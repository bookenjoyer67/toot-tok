//! toottok-db — models + migrations. No axum deps; compiles alone.
pub mod activity;
pub mod actor;
pub mod announce;
pub mod audit;
pub mod clip;
pub mod comment;
pub mod email_token;
pub mod error;
pub mod feed;
pub mod follow;
pub mod hashtag;
pub mod instance;
pub mod job;
pub mod like;
pub mod media_asset;
pub mod password;
pub mod session;
pub mod settings;
pub mod tombstone;
pub mod user;

use error::DbError;

/// Baked at compile time so dev/CI/tests all resolve the same tree.
const MIGRATIONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");

/// Runtime override for containerized installs via TOOTTOK_MIGRATIONS_DIR
/// (Docker image bakes migrations to /usr/local/share/toottok/migrations);
/// falls back to the compile-time dev path.
pub fn migrations_dir() -> std::path::PathBuf {
    std::env::var("TOOTTOK_MIGRATIONS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(MIGRATIONS_DIR))
}

pub async fn connect(url: &str) -> Result<sqlx::PgPool, DbError> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(url)
        .await?;
    Ok(pool)
}

/// Apply pending migrations.
pub async fn migrate(pool: &sqlx::PgPool) -> Result<(), DbError> {
    let migrator = sqlx::migrate::Migrator::new(migrations_dir()).await?;
    migrator.run(pool).await?;
    Ok(())
}
