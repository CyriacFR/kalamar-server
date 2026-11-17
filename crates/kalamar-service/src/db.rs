//! SQLite storage: opening, pragmas and versioned migrations embedded in the binary.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

/// Migrations in order; the version of a database is the number of applied entries.
pub const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    ("0002_series", include_str!("../migrations/0002_series.sql")),
    (
        "0003_operators_badges",
        include_str!("../migrations/0003_operators_badges.sql"),
    ),
    ("0004_passwords", include_str!("../migrations/0004_passwords.sql")),
    ("0005_draft", include_str!("../migrations/0005_draft.sql")),
    ("0006_spectators", include_str!("../migrations/0006_spectators.sql")),
    ("0007_control", include_str!("../migrations/0007_control.sql")),
    ("0008_security", include_str!("../migrations/0008_security.sql")),
    ("0009_host_score", include_str!("../migrations/0009_host_score.sql")),
];

/// Opens (creating it if needed) and migrates the database at `path`; `:memory:` for an in-memory one. Only the
/// service's start and `kalamar-service migrate` call it: the admin commands never migrate ([`open_current`]).
pub fn open(path: &Path) -> Result<Connection, String> {
    let conn = if path.as_os_str() == ":memory:" {
        Connection::open_in_memory()
    } else {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        // Security review L-S6: a new database is readable by the service's account only (SQLite gives its journal
        // files the same mode); with the default umask it was 0644. An existing file keeps its mode.
        if !path.exists() {
            match kalamar_relay::roomauth::create_private_file(path, b"") {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Connection::open(path)
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    configure(&conn).map_err(|e| format!("database setup: {e}"))?;
    migrate(&conn).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(conn)
}

/// Opens the database at `path` for the admin commands: it must exist and be exactly at this program's schema. It
/// is never migrated here, so that a new binary's command line cannot change a database that an older service
/// still serves.
pub fn open_current(path: &Path) -> Result<Connection, String> {
    if path.as_os_str() != ":memory:" && !path.exists() {
        return Err(format!(
            "{}: no database yet: start the service once (it creates the database)",
            path.display()
        ));
    }
    let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    configure(&conn).map_err(|e| format!("database setup: {e}"))?;
    let found = schema_version(&conn).map_err(|e| format!("{}: {e}", path.display()))?;
    match found.cmp(&MIGRATIONS.len()) {
        std::cmp::Ordering::Equal => Ok(conn),
        std::cmp::Ordering::Greater => Err(format!("{}: {}", path.display(), too_new(found))),
        std::cmp::Ordering::Less => Err(format!(
            "{}: the database is at schema number {found} and this program needs number {}: a service of an older \
             version may still be using it, and admin commands never migrate. Stop that service, start this version \
             (it migrates at start, after a backup of the database), then run the command again; or, the service \
             stopped, `kalamar-service -c <file> migrate`.",
            path.display(),
            MIGRATIONS.len()
        )),
    }
}

/// The schema number of a database (0: none yet).
pub fn schema_version(conn: &Connection) -> rusqlite::Result<usize> {
    let has: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(0);
    }
    conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|v| v.max(0) as usize)
}

fn too_new(found: usize) -> String {
    format!(
        "this database was created by a newer version of kalamar-service: its schema is number {found}, this program \
         knows number {} at most. Run the newer version again, or restore the backup taken before the update \
         (README, \"Updating and going back\")",
        MIGRATIONS.len()
    )
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
    // WAL lets the admin command line read and write while the service runs (no-op in memory).
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    Ok(())
}

/// Applies the missing migrations, each in its own transaction. Returns the database version. A database of a newer
/// version is refused with a message that says so (it is never touched).
pub fn migrate(conn: &Connection) -> Result<usize, String> {
    let current = schema_version(conn).map_err(|e| format!("database migration: {e}"))?;
    if current > MIGRATIONS.len() {
        return Err(too_new(current));
    }
    let step = || -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)",
        )?;
        for (i, (name, sql)) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                params![(i + 1) as i64, name, now],
            )?;
            tx.commit()?;
        }
        Ok(())
    };
    step().map_err(|e| format!("database migration: {e}"))?;
    Ok(MIGRATIONS.len())
}

/// The id of the current season, creating "Season 1" on an empty database.
pub fn current_season(conn: &Connection, now: i64) -> rusqlite::Result<(String, String, i64)> {
    if let Some(s) = conn
        .query_row(
            "SELECT id, name, starts_at FROM seasons WHERE ends_at IS NULL ORDER BY number DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    {
        return Ok(s);
    }
    let number: i64 = conn.query_row("SELECT COALESCE(MAX(number), 0) + 1 FROM seasons", [], |r| r.get(0))?;
    let id = format!("s{number}");
    let name = format!("Season {number}");
    conn.execute(
        "INSERT INTO seasons (id, number, name, starts_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, number, name, now],
    )?;
    Ok((id, name, now))
}

pub fn audit(
    conn: &Connection,
    now: i64,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit (at, action, target, detail) VALUES (?1, ?2, ?3, ?4)",
        params![now, action, target, detail],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_are_versioned() {
        let conn = open(Path::new(":memory:")).unwrap();
        assert_eq!(migrate(&conn).unwrap(), MIGRATIONS.len());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, MIGRATIONS.len());
        // Every table of the schema exists and foreign keys are on.
        for t in [
            "users",
            "identities",
            "sessions",
            "friendships",
            "friend_requests",
            "blocks",
            "seasons",
            "matches",
            "match_players",
            "match_spectators",
            "ratings",
            "series",
            "offences",
            "reports",
            "audit",
            "identity_bans",
            "password_logins",
            "password_codes",
            "control",
            "identity_holds",
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{t}");
        }
        let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
        // A database from the future is refused, and the message says so (not "Query is not read-only").
        conn.execute("INSERT INTO schema_migrations VALUES (99, 'future', 0)", [])
            .unwrap();
        let err = migrate(&conn).unwrap_err();
        assert!(
            err.contains("newer version")
                && err.contains("number 99")
                && err.contains(&format!("number {} at most", MIGRATIONS.len())),
            "{err}"
        );
    }

    /// Going back: a database migrated by this version is refused by name by this version's own check, and the
    /// admin commands of a newer binary never migrate a database an older service still serves.
    #[test]
    fn admin_commands_never_migrate() {
        let dir = std::env::temp_dir().join(format!("kalamar-db-admin-{}", crate::util::new_id("t")));
        let path = dir.join("db.sqlite3");
        let err = open_current(&path).unwrap_err();
        assert!(err.contains("no database yet"), "{err}");
        assert!(!path.exists(), "nothing created");
        // A database of the previous version (every migration but the last one, as that version made it).
        std::fs::create_dir_all(&dir).unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            configure(&conn).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)",
            )
            .unwrap();
            for (i, (name, sql)) in MIGRATIONS.iter().enumerate().take(MIGRATIONS.len() - 1) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations VALUES (?1, ?2, 0)",
                    params![(i + 1) as i64, name],
                )
                .unwrap();
            }
        }
        let err = open_current(&path).unwrap_err();
        assert!(
            err.contains(&format!("schema number {}", MIGRATIONS.len() - 1)) && err.contains("never migrate"),
            "{err}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() - 1, "left as it was");
        drop(conn);
        // The service's start (or `migrate`) migrates it; then the admin commands open it.
        drop(open(&path).unwrap());
        assert!(open_current(&path).is_ok());
        // A newer one: refused by both, with the numbers.
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 'future', 0)",
            [(MIGRATIONS.len() + 1) as i64],
        )
        .unwrap();
        drop(conn);
        for err in [open(&path).unwrap_err(), open_current(&path).unwrap_err()] {
            assert!(
                err.contains("newer version") && err.contains(&format!("number {}", MIGRATIONS.len() + 1)),
                "{err}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 1.1.12 (`0008_security`) from the databases of 1.1.8 (schema 6) and 1.1.11 (schema 7), with data: reports keep
    /// everything and are no longer deleted with their target, the stored game declarations lose `build`.
    #[test]
    fn the_databases_of_1_1_8_and_1_1_11_migrate_with_their_data() {
        for (version, at) in [("1.1.8", 6), ("1.1.11", 7)] {
            let conn = Connection::open_in_memory().unwrap();
            configure(&conn).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)",
            )
            .unwrap();
            for (i, (name, sql)) in MIGRATIONS.iter().enumerate().take(at) {
                conn.execute_batch(sql).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations VALUES (?1, ?2, 0)",
                    params![(i + 1) as i64, name],
                )
                .unwrap();
            }
            for (id, code, caps) in [
                (
                    "u_a",
                    "AAAA",
                    r#"{"modes":["turf"],"build":"exe-1234567-1794346556","max_players":8}"#,
                ),
                ("u_b", "BBBB", "not json"),
            ] {
                conn.execute(
                    "INSERT INTO users (id, display_name, friend_code, created_at, game_caps) VALUES (?1, 'n', ?2, 1, ?3)",
                    params![id, code, caps],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO reports (id, reporter, target, match_id, category, comment, created_at, status, resolution, resolved_at)
                 VALUES ('r1', 'u_a', 'u_b', 'm_1', 'cheating', 'edited results', 5, 'resolved', 'warned', 9)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO offences (user_id, kind, at) VALUES ('u_b', 'left', 7)", [])
                .unwrap();
            assert_eq!(migrate(&conn).unwrap(), MIGRATIONS.len(), "{version}");
            assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len(), "{version}");
            let r: (String, String, String, String, String, i64, String, String, i64) = conn
                .query_row(
                    "SELECT reporter, target, match_id, category, comment, created_at, status, resolution, resolved_at FROM reports",
                    [],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(
                r,
                (
                    "u_a".into(),
                    "u_b".into(),
                    "m_1".into(),
                    "cheating".into(),
                    "edited results".into(),
                    5,
                    "resolved".into(),
                    "warned".into(),
                    9
                ),
                "{version}"
            );
            let caps: String = conn
                .query_row("SELECT game_caps FROM users WHERE id = 'u_a'", [], |r| r.get(0))
                .unwrap();
            let caps: serde_json::Value = serde_json::from_str(&caps).unwrap();
            assert_eq!(
                caps,
                serde_json::json!({"modes": ["turf"], "max_players": 8}),
                "{version}"
            );
            let other: String = conn
                .query_row("SELECT game_caps FROM users WHERE id = 'u_b'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(other, "not json", "{version}: left alone");
            // The target's erasure keeps the report (anonymous), and takes the offense.
            conn.execute("DELETE FROM users WHERE id = 'u_b'", []).unwrap();
            let left: (i64, i64) = conn
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM reports WHERE target IS NULL), (SELECT COUNT(*) FROM offences)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(left, (1, 0), "{version}");
            let holds: i64 = conn
                .query_row("SELECT COUNT(*) FROM identity_holds", [], |r| r.get(0))
                .unwrap();
            assert_eq!(holds, 0);
        }
    }

    #[test]
    fn migrations_on_disk_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("kalamar-db-test-{}", crate::util::new_id("t")));
        let path = dir.join("db.sqlite3");
        {
            let conn = open(&path).unwrap();
            conn.execute(
                "INSERT INTO seasons (id, number, name, starts_at) VALUES ('s1', 1, 'Season 1', 0)",
                [],
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(current_season(&conn, 5).unwrap().0, "s1");
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// L-S6: a new database is readable by the service's account only.
    #[cfg(unix)]
    #[test]
    fn a_new_database_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kalamar-db-mode-{}", crate::util::new_id("t")));
        let path = dir.join("db.sqlite3");
        drop(open(&path).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn first_season_is_created() {
        let conn = open(Path::new(":memory:")).unwrap();
        let (id, name, at) = current_season(&conn, 42).unwrap();
        assert_eq!((id.as_str(), name.as_str(), at), ("s1", "Season 1", 42));
        assert_eq!(current_season(&conn, 99).unwrap().0, "s1");
    }
}
