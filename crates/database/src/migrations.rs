//! Database migrations and backup logic.
//!
//! On first run with an existing DB, creates a timestamped backup before
//! applying schema. Also handles legacy ccflare.db detection and copy.

use std::path::Path;

use crate::error::DbError;
use crate::paths;

/// Copy legacy `~/.config/ccflare/ccflare.db` to the new location if it
/// exists and the new database does not yet exist.
pub fn migrate_from_legacy(db_path: &Path) -> Result<bool, DbError> {
    if db_path.exists() {
        return Ok(false);
    }

    let legacy_path = paths::resolve_legacy_db_path();
    if !legacy_path.exists() {
        return Ok(false);
    }

    tracing::info!(
        legacy = %legacy_path.display(),
        target = %db_path.display(),
        "Migrating legacy ccflare database"
    );

    // Ensure target directory exists
    paths::ensure_db_dir(db_path)?;

    // Copy main DB file
    std::fs::copy(&legacy_path, db_path)?;

    // Also copy WAL and SHM if present
    let legacy_wal = legacy_path.with_extension("db-wal");
    let legacy_shm = legacy_path.with_extension("db-shm");
    let target_wal = db_path.with_extension("db-wal");
    let target_shm = db_path.with_extension("db-shm");

    if legacy_wal.exists() {
        let _ = std::fs::copy(&legacy_wal, &target_wal);
    }
    if legacy_shm.exists() {
        let _ = std::fs::copy(&legacy_shm, &target_shm);
    }

    tracing::info!("Legacy database migration complete");
    Ok(true)
}

/// Create a timestamped backup of an existing database file.
///
/// Returns the backup path if a backup was created.
pub fn backup_existing_db(db_path: &Path) -> Result<Option<std::path::PathBuf>, DbError> {
    if !db_path.exists() {
        return Ok(None);
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let backup_name = format!(
        "{}.bak.{timestamp}",
        db_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let backup_path = db_path.with_file_name(backup_name);

    tracing::info!(
        source = %db_path.display(),
        backup = %backup_path.display(),
        "Creating database backup"
    );

    std::fs::copy(db_path, &backup_path)?;
    Ok(Some(backup_path))
}

/// Check whether the database has been initialized (has the accounts table).
pub fn is_initialized(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='accounts'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn is_initialized_empty_db() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(!is_initialized(&conn));
    }

    #[test]
    fn is_initialized_with_tables() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::create_tables(&conn).unwrap();
        assert!(is_initialized(&conn));
    }

    #[test]
    fn backup_nonexistent_db() {
        let path = std::path::PathBuf::from("/tmp/nonexistent_test_db.db");
        assert!(backup_existing_db(&path).unwrap().is_none());
    }

    #[test]
    fn backup_existing_db_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        fs::write(&db_path, b"test data").unwrap();

        let backup = backup_existing_db(&db_path).unwrap();
        assert!(backup.is_some());
        let backup_path = backup.unwrap();
        assert!(backup_path.exists());
        assert!(backup_path.to_string_lossy().contains(".bak."));
    }

    #[test]
    fn migrate_from_legacy_no_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("better-ccflare.db");
        // No legacy DB exists, so migration should return false
        let result = migrate_from_legacy(&db_path).unwrap();
        assert!(!result);
    }

    #[test]
    fn migrate_from_legacy_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("better-ccflare.db");
        fs::write(&db_path, b"existing").unwrap();
        // DB already exists, so migration should return false
        let result = migrate_from_legacy(&db_path).unwrap();
        assert!(!result);
    }
}
