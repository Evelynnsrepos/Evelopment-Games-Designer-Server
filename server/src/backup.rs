//! Backups: one project as a zip (download / restore in the admin page), or the
//! whole server into the backup folder (button, or daily; kept 14 days).

use crate::App;
use crate::util::{is_asset_path, now};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use yrs::updates::decoder::Decode;

pub const KEEP_DAYS: i64 = 14;

/// `project.json`, `state.bin` (the Yjs document) and `assets/...`.
pub fn project_zip(app: &App, id: &str) -> Result<Vec<u8>, String> {
    let p = app.db.project(id).ok_or("No such project")?;
    let state = app.hub.state_of(app, id).unwrap_or_default();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let meta = serde_json::json!({ "format": 1, "id": p.id, "name": p.name, "schema": p.schema, "appId": p.app_id, "saved": now() });
    let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
        z.start_file(name, opts).map_err(|e| e.to_string())?;
        z.write_all(bytes).map_err(|e| e.to_string())
    };
    add("project.json", meta.to_string().as_bytes())?;
    add("state.bin", &state)?;
    let assets = app.project_dir(id).join("assets");
    for kind in ["images", "audio"] {
        let Ok(entries) = std::fs::read_dir(assets.join(kind)) else { continue };
        for e in entries.filter_map(Result::ok) {
            let name = format!("assets/{kind}/{}", e.file_name().to_string_lossy());
            if is_asset_path(&name) {
                add(&name, &std::fs::read(e.path()).map_err(|e| e.to_string())?)?;
            }
        }
    }
    z.finish().map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

/// Replace a project's state and files with a backup (the project must be closed first).
pub fn restore_project(app: &App, id: &str, zip_bytes: &[u8]) -> Result<(), String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|_| "This is not a backup zip.".to_string())?;
    let mut state = Vec::new();
    z.by_name("state.bin").map_err(|_| "The zip has no state.bin.".to_string())?.read_to_end(&mut state).map_err(|e| e.to_string())?;
    if !state.is_empty() {
        yrs::Update::decode_v1(&state).map_err(|_| "state.bin is damaged.".to_string())?;
    }
    let mut schema = None;
    let mut app_id = None;
    if let Ok(mut f) = z.by_name("project.json") {
        let mut text = String::new();
        let _ = f.read_to_string(&mut text);
        let v = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
        schema = v["schema"].as_i64();
        app_id = v["appId"].as_str().map(String::from);
    }
    let assets = app.project_dir(id).join("assets");
    let _ = std::fs::remove_dir_all(&assets);
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().to_string();
        // Only well-formed asset paths, so a crafted zip can't write anywhere else.
        if !is_asset_path(&name) {
            continue;
        }
        if f.size() > crate::sync::MAX_ASSET_BYTES {
            return Err(format!("{name} is larger than 200 MB."));
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let mut path = assets.clone();
        for part in name.split('/').skip(1) {
            path.push(part);
        }
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    }
    app.db.save_state(id, &state);
    if let Some(s) = schema {
        app.db.set_schema(id, s);
    }
    app.db.set_app_id(id, app_id.as_deref());
    Ok(())
}

/// Copy the database (consistent snapshot), project files and plugins into a dated folder.
pub fn server_backup(app: &App) -> Result<PathBuf, String> {
    app.hub.save_all(app);
    let dir = app.backup_dir().join(format!("egd-server-{}", stamp(now())));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.db.backup_to(&dir.join("egd-server.db")).map_err(|e| e.to_string())?;
    copy_dir(&app.data_dir.join("projects"), &dir.join("projects")).map_err(|e| e.to_string())?;
    copy_dir(&app.data_dir.join("plugins"), &dir.join("plugins")).map_err(|e| e.to_string())?;
    prune(&app.backup_dir());
    Ok(dir)
}

/// Delete server backups older than 14 days.
fn prune(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(KEEP_DAYS as u64 * 86400);
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().to_string();
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff);
        if name.starts_with("egd-server-") && old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(from) else { return Ok(()) };
    std::fs::create_dir_all(to)?;
    for e in entries {
        let e = e?;
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()))?;
        } else {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// `YYYY-MM-DD-HHMMSS` in UTC, for folder names.
pub fn stamp(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}-{:02}{:02}{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamps() {
        assert_eq!(super::stamp(0), "1970-01-01-000000");
        assert_eq!(super::stamp(1_791_000_000), "2026-10-03-040000");
        assert_eq!(super::stamp(951_782_400), "2000-02-29-000000");
    }
}
