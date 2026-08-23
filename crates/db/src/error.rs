#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("unique violation")]
    UniqueViolation,
}

impl DbError {
    /// True when the wrapped SQL error is a Postgres unique_violation
    /// (SQLSTATE 23505), e.g. from a duplicate key or partial unique index.
    pub fn is_unique_violation(&self) -> bool {
        match self {
            DbError::Sql(sqlx::Error::Database(db)) => {
                db.is_unique_violation() || db.code().as_deref() == Some("23505")
            }
            _ => false,
        }
    }
}
