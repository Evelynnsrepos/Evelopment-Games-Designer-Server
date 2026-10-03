//! Plugins installed on the server. The admin uploads a plugin zip (the same
//! format the app installs); apps connected to this server can list and download
//! them with their key. The app never installs one on its own: it shows the
//! plugin warning first (plugins are programs and run unsandboxed).

use crate::App;
use crate::util::{now, sha256_hex};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::Arc;

pub const MAX_PLUGIN_BYTES: usize = 20 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// SHA-256 of the zip, so the app can show exactly what it installs and notice changes.
    pub sha256: String,
    pub size: usize,
    pub added: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    api_version: i64,
    #[serde(default = "default_main")]
    main: String,
}

fn default_main() -> String {
    "index.js".into()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Check a plugin zip the same way the app does: plugin.json at the top (or in one folder), id, apiVersion 1, main file present.
pub fn inspect(zip_bytes: &[u8]) -> Result<PluginInfo, String> {
    if zip_bytes.len() > MAX_PLUGIN_BYTES {
        return Err("The plugin is larger than 20 MB.".into());
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|_| "This is not a zip file.".to_string())?;
    let names: Vec<String> = zip.file_names().map(String::from).collect();
    let manifest_name = names
        .iter()
        .filter(|n| n.ends_with("plugin.json") && n.matches('/').count() <= 1)
        .min_by_key(|n| n.len())
        .ok_or("The zip has no plugin.json.")?
        .clone();
    let prefix = &manifest_name[..manifest_name.len() - "plugin.json".len()];
    let mut text = String::new();
    zip.by_name(&manifest_name)
        .map_err(|e| e.to_string())?
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|_| "plugin.json is not readable text.".to_string())?;
    let m: Manifest = serde_json::from_str(&text).map_err(|e| format!("plugin.json is not valid: {e}"))?;
    if !valid_id(&m.id) {
        return Err("The plugin id may only use lowercase letters, digits and dashes.".into());
    }
    if m.api_version != 1 {
        return Err(format!("The plugin needs plugin API version {}; this app version supports 1.", m.api_version));
    }
    if !names.iter().any(|n| *n == format!("{prefix}{}", m.main)) {
        return Err(format!("The plugin's main file {} is missing.", m.main));
    }
    Ok(PluginInfo {
        id: m.id,
        name: m.name.chars().take(80).collect(),
        version: m.version.chars().take(40).collect(),
        description: m.description.chars().take(300).collect(),
        sha256: sha256_hex(zip_bytes),
        size: zip_bytes.len(),
        added: now(),
    })
}

fn dir(app: &App) -> std::path::PathBuf {
    app.data_dir.join("plugins")
}

pub fn list(app: &App) -> Vec<PluginInfo> {
    let Ok(entries) = std::fs::read_dir(dir(app)) else { return vec![] };
    let mut out: Vec<PluginInfo> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub fn install(app: &App, zip_bytes: &[u8]) -> Result<PluginInfo, String> {
    let info = inspect(zip_bytes)?;
    let d = dir(app);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join(format!("{}.zip", info.id)), zip_bytes).map_err(|e| e.to_string())?;
    std::fs::write(d.join(format!("{}.json", info.id)), serde_json::to_vec_pretty(&info).unwrap()).map_err(|e| e.to_string())?;
    Ok(info)
}

pub fn remove(app: &App, id: &str) -> bool {
    if !valid_id(id) {
        return false;
    }
    let d = dir(app);
    let had = d.join(format!("{id}.json")).exists();
    let _ = std::fs::remove_file(d.join(format!("{id}.json")));
    let _ = std::fs::remove_file(d.join(format!("{id}.zip")));
    had
}

// ---- for apps (sync port, needs a key) ------------------------------------------

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match key.and_then(|k| app.db.find_key(k)) {
        Some(k) => k.expires.is_none_or(|e| e > now()),
        None => false,
    }
}

pub async fn api_list(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(list(&app)).into_response()
}

pub async fn api_download(State(app): State<Arc<App>>, headers: HeaderMap, Path(file): Path<String>) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(id) = file.strip_suffix(".zip").filter(|id| valid_id(id)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read(dir(&app).join(format!("{id}.zip"))) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/zip")], Bytes::from(bytes)).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn checks_plugins_like_the_app() {
        let good = make_zip(&[
            ("dice/plugin.json", r#"{"id":"dice-roller","name":"Dice","version":"1.0.0","apiVersion":1}"#),
            ("dice/index.js", "export default () => ({})"),
        ]);
        let info = inspect(&good).unwrap();
        assert_eq!(info.id, "dice-roller");
        assert_eq!(info.sha256.len(), 64);

        let bad_id = make_zip(&[("plugin.json", r#"{"id":"../x","name":"x","version":"1","apiVersion":1}"#), ("index.js", "")]);
        assert!(inspect(&bad_id).is_err());
        let no_main = make_zip(&[("plugin.json", r#"{"id":"x","name":"x","version":"1","apiVersion":1}"#)]);
        assert!(inspect(&no_main).is_err());
        let new_api = make_zip(&[("plugin.json", r#"{"id":"x","name":"x","version":"1","apiVersion":2}"#), ("index.js", "")]);
        assert!(inspect(&new_api).is_err());
        assert!(inspect(b"not a zip").is_err());
    }
}
