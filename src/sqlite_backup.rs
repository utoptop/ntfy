use crate::{error::Result, model::id};
use rusqlite::Connection;
use std::{path::Path, time::Duration};

pub(crate) fn secure_create(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    Ok(())
}

// Publish only a complete SQLite snapshot, including committed WAL pages. A crash
// cannot leave a partially written file that the next startup mistakes for a backup.
pub(crate) fn backup(db: &Connection, path: &Path, label: &str) -> Result<()> {
    if path.as_os_str() == ":memory:" || path.as_os_str().is_empty() {
        return Ok(());
    }
    let canonical = path.with_extension(format!("db.pre-rust-{label}.bak"));
    let target = if canonical.exists() {
        path.with_extension(format!("db.pre-rust-{label}-{}.bak", id()))
    } else {
        canonical
    };
    let temporary = path.with_extension(format!("db.backup-{}.tmp", id()));
    secure_create(&temporary)?;
    let result = (|| -> Result<()> {
        let mut destination = Connection::open(&temporary)?;
        rusqlite::backup::Backup::new(db, &mut destination)?.run_to_completion(
            100,
            Duration::from_millis(10),
            None,
        )?;
        drop(destination);
        std::fs::File::open(&temporary)?.sync_all()?;
        // Hard-link creation is atomic and will never overwrite another backup.
        std::fs::hard_link(&temporary, &target)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
