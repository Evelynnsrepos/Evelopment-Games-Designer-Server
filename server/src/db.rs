use crate::util::{now, sha256_hex};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

/// Everything the server remembers, in one SQLite file. No activity history is
/// stored on purpose: only what is needed to run (see docs/operators).
pub struct Db(Mutex<Connection>);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub schema: Option<i64>,
    pub quota_mb: i64,
    pub archived: bool,
    pub created: i64,
    pub last_used: i64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    /// SHA-256 of the key; the key itself is never stored.
    pub hash: String,
    pub project_id: String,
    pub role: String,
    pub label: String,
    /// The last name someone typed when connecting with it (self-chosen, unverified).
    pub last_name: String,
    pub expires: Option<i64>,
    pub created: i64,
}

pub const DEFAULT_QUOTA_MB: i64 = 2048;

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS settings (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS projects (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, schema INTEGER, state BLOB,
               quota_mb INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
               created INTEGER NOT NULL, last_used INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS keys (
               hash TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
               role TEXT NOT NULL, label TEXT NOT NULL, last_name TEXT NOT NULL DEFAULT '',
               expires INTEGER, created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS audit (at INTEGER NOT NULL, action TEXT NOT NULL);",
        )?;
        Ok(Db(Mutex::new(conn)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- settings -----------------------------------------------------------

    pub fn get(&self, k: &str) -> Option<String> {
        self.conn().query_row("SELECT v FROM settings WHERE k=?", [k], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn get_or(&self, k: &str, default: &str) -> String {
        self.get(k).unwrap_or_else(|| default.to_string())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k).as_deref() == Some("1")
    }

    pub fn set(&self, k: &str, v: &str) {
        let _ = self.conn().execute("INSERT INTO settings(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", [k, v]);
    }

    // ---- projects -----------------------------------------------------------

    pub fn projects(&self) -> Vec<Project> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id,name,schema,quota_mb,archived,created,last_used FROM projects ORDER BY name COLLATE NOCASE")
            .unwrap();
        stmt.query_map([], project_row).unwrap().filter_map(Result::ok).collect()
    }

    pub fn project(&self, id: &str) -> Option<Project> {
        self.conn()
            .query_row("SELECT id,name,schema,quota_mb,archived,created,last_used FROM projects WHERE id=?", [id], project_row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn create_project(&self, id: &str, name: &str) {
        let t = now();
        let _ = self.conn().execute(
            "INSERT INTO projects(id,name,quota_mb,created,last_used) VALUES(?,?,?,?,?)",
            params![id, name, DEFAULT_QUOTA_MB, t, t],
        );
    }

    pub fn update_project(&self, id: &str, name: Option<&str>, quota_mb: Option<i64>, archived: Option<bool>) {
        let conn = self.conn();
        if let Some(n) = name {
            let _ = conn.execute("UPDATE projects SET name=? WHERE id=?", params![n, id]);
        }
        if let Some(q) = quota_mb {
            let _ = conn.execute("UPDATE projects SET quota_mb=? WHERE id=?", params![q, id]);
        }
        if let Some(a) = archived {
            let _ = conn.execute("UPDATE projects SET archived=? WHERE id=?", params![a, id]);
        }
    }

    pub fn delete_project(&self, id: &str) {
        let _ = self.conn().execute("DELETE FROM projects WHERE id=?", [id]);
    }

    pub fn set_schema(&self, id: &str, schema: i64) {
        let _ = self.conn().execute("UPDATE projects SET schema=? WHERE id=?", params![schema, id]);
    }

    pub fn load_state(&self, id: &str) -> Option<Vec<u8>> {
        self.conn().query_row("SELECT state FROM projects WHERE id=?", [id], |r| r.get(0)).optional().ok().flatten().flatten()
    }

    pub fn save_state(&self, id: &str, state: &[u8]) {
        let _ = self.conn().execute("UPDATE projects SET state=?, last_used=? WHERE id=?", params![state, now(), id]);
    }

    pub fn touch(&self, id: &str) {
        let _ = self.conn().execute("UPDATE projects SET last_used=? WHERE id=?", params![now(), id]);
    }

    // ---- keys ---------------------------------------------------------------

    pub fn add_key(&self, key: &str, project_id: &str, role: &str, label: &str, expires: Option<i64>) -> String {
        let hash = sha256_hex(key.as_bytes());
        let _ = self.conn().execute(
            "INSERT INTO keys(hash,project_id,role,label,expires,created) VALUES(?,?,?,?,?,?)",
            params![hash, project_id, role, label, expires, now()],
        );
        hash
    }

    /// The key behind a secret, if it exists (expiry is checked by the caller).
    pub fn find_key(&self, key: &str) -> Option<Key> {
        let hash = sha256_hex(key.as_bytes());
        self.conn()
            .query_row("SELECT hash,project_id,role,label,last_name,expires,created FROM keys WHERE hash=?", [hash], key_row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn keys(&self, project_id: &str) -> Vec<Key> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT hash,project_id,role,label,last_name,expires,created FROM keys WHERE project_id=? ORDER BY created")
            .unwrap();
        stmt.query_map([project_id], key_row).unwrap().filter_map(Result::ok).collect()
    }

    pub fn key_by_hash(&self, hash: &str) -> Option<Key> {
        self.conn()
            .query_row("SELECT hash,project_id,role,label,last_name,expires,created FROM keys WHERE hash=?", [hash], key_row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn delete_key(&self, hash: &str) {
        let _ = self.conn().execute("DELETE FROM keys WHERE hash=?", [hash]);
    }

    pub fn set_key_name(&self, hash: &str, name: &str) {
        let _ = self.conn().execute("UPDATE keys SET last_name=? WHERE hash=?", params![name, hash]);
    }

    /// Expired keys are deleted with the name stored for them.
    pub fn delete_expired_keys(&self) -> Vec<String> {
        let conn = self.conn();
        let t = now();
        let mut stmt = conn.prepare("SELECT hash FROM keys WHERE expires IS NOT NULL AND expires<=?").unwrap();
        let gone: Vec<String> = stmt.query_map([t], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
        let _ = conn.execute("DELETE FROM keys WHERE expires IS NOT NULL AND expires<=?", [t]);
        gone
    }

    // ---- audit log (admin actions, no IP addresses) ----------------------------

    pub fn audit(&self, action: &str) {
        let conn = self.conn();
        let _ = conn.execute("INSERT INTO audit(at,action) VALUES(?,?)", params![now(), action]);
        let _ = conn.execute("DELETE FROM audit WHERE at<?", [now() - 90 * 86400]);
    }

    pub fn audit_log(&self) -> Vec<(i64, String)> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT at,action FROM audit ORDER BY at DESC LIMIT 500").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().filter_map(Result::ok).collect()
    }

    /// A consistent copy of the whole database (for backups), safe while the server runs.
    pub fn backup_to(&self, path: &Path) -> rusqlite::Result<()> {
        let _ = std::fs::remove_file(path);
        self.conn().execute("VACUUM INTO ?", [path.to_string_lossy()])?;
        Ok(())
    }
}

fn project_row(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        schema: r.get(2)?,
        quota_mb: r.get(3)?,
        archived: r.get::<_, i64>(4)? != 0,
        created: r.get(5)?,
        last_used: r.get(6)?,
    })
}

fn key_row(r: &rusqlite::Row) -> rusqlite::Result<Key> {
    Ok(Key {
        hash: r.get(0)?,
        project_id: r.get(1)?,
        role: r.get(2)?,
        label: r.get(3)?,
        last_name: r.get(4)?,
        expires: r.get(5)?,
        created: r.get(6)?,
    })
}
