//! The admin page: a local web page to set up and run the server.
//! Bound to 127.0.0.1 by default. Password (Argon2id), HttpOnly SameSite=Strict
//! session cookie, CSRF token on every change, strict CSP, nothing loaded from
//! outside, login rate limiting. Admin actions go to an audit log without IPs.

// Handlers return a Response as their error, the usual axum style.
#![allow(clippy::result_large_err)]

use crate::sync::{Role, dir_size};
use crate::util::{b64, now, random_token, same};
use crate::{App, backup, plugins, tls};
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SESSION_SECS: i64 = 12 * 3600;
const COOKIE: &str = "egd_admin";
const MIN_PASSWORD: usize = 12;

#[derive(Default)]
pub struct Sessions {
    /// token -> (csrf token, expires)
    tokens: Mutex<HashMap<String, (String, i64)>>,
    /// Failed logins in a row and when the next try is allowed.
    fails: Mutex<(u32, i64)>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::to("/admin") }))
        .route("/admin", get(|| async { asset("index.html") }))
        .route("/admin/app.js", get(|| async { asset("app.js") }))
        .route("/admin/app.css", get(|| async { asset("app.css") }))
        .route("/admin/api/state", get(state))
        .route("/admin/api/setup", post(setup))
        .route("/admin/api/login", post(login))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/overview", get(overview))
        .route("/admin/api/projects", post(create_project))
        .route("/admin/api/projects/{id}", post(update_project).delete(delete_project))
        .route("/admin/api/projects/{id}/keys", get(list_keys).post(create_key))
        .route("/admin/api/projects/{id}/backup", get(download_backup))
        .route("/admin/api/projects/{id}/restore", post(restore_backup))
        .route("/admin/api/keys/{hash}", axum::routing::delete(revoke_key))
        .route("/admin/api/settings", post(save_settings))
        .route("/admin/api/password", post(change_password))
        .route("/admin/api/plugins", post(install_plugin))
        .route("/admin/api/plugins/{id}", axum::routing::delete(remove_plugin))
        .route("/admin/api/backup-now", post(backup_now))
        .route("/admin/api/audit", get(audit))
        .layer(DefaultBodyLimit::max(crate::sync::MAX_ASSET_BYTES as usize * 2))
        .layer(axum::middleware::map_response(security_headers))
        .with_state(app)
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn asset(name: &str) -> Response {
    let (body, kind): (&'static str, &'static str) = match name {
        "index.html" => (include_str!("../../admin-ui/index.html"), "text/html; charset=utf-8"),
        "app.js" => (include_str!("../../admin-ui/app.js"), "text/javascript; charset=utf-8"),
        _ => (include_str!("../../admin-ui/app.css"), "text/css; charset=utf-8"),
    };
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}

// ---- sessions -------------------------------------------------------------------

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(String::from))
}

/// The session's CSRF token, if logged in.
fn session(app: &App, headers: &HeaderMap) -> Option<String> {
    let token = cookie_token(headers)?;
    let mut tokens = lock(&app.sessions.tokens);
    tokens.retain(|_, (_, exp)| *exp > now());
    tokens.get(&token).map(|(csrf, _)| csrf.clone())
}

type ApiResult = Result<Response, Response>;

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Logged in, and for changes the CSRF header matches.
fn guard(app: &App, headers: &HeaderMap, change: bool) -> Result<(), Response> {
    let csrf = session(app, headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "Please log in."))?;
    if change {
        let sent = headers.get("x-csrf").and_then(|v| v.to_str().ok()).unwrap_or("");
        if !same(sent.as_bytes(), csrf.as_bytes()) {
            return Err(err(StatusCode::FORBIDDEN, "The page is out of date. Reload it."));
        }
    }
    Ok(())
}

fn start_session(app: &App) -> Response {
    let token = random_token(32);
    let csrf = random_token(32);
    lock(&app.sessions.tokens).insert(token.clone(), (csrf.clone(), now() + SESSION_SECS));
    let secure = if app.admin_tls { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/admin; Max-Age={SESSION_SECS}{secure}");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true, "csrf": csrf }))).into_response()
}

// ---- passwords ------------------------------------------------------------------

/// A short list of passwords people pick most; the length rule catches most of the rest.
const COMMON: &[&str] = &[
    "123456789012",
    "1234567890123",
    "password1234",
    "passwort1234",
    "qwertzuiop12",
    "qwertyuiop12",
    "111111111111",
    "000000000000",
    "aaaaaaaaaaaa",
    "abcdefghijkl",
    "iloveyou1234",
    "password123!",
    "passwort123!",
    "adminadmin12",
    "administrator",
    "letmein12345",
    "welcome12345",
    "willkommen12",
    "hallo1234567",
    "sommer202020",
    "changeme1234",
    "qwerty123456",
    "123qweasdzxc",
    "1q2w3e4r5t6y",
    "evelopment12",
    "gamedesigner",
];

fn check_password(pw: &str) -> Result<(), String> {
    if pw.chars().count() < MIN_PASSWORD {
        return Err(format!("The password needs at least {MIN_PASSWORD} characters."));
    }
    let lower = pw.to_lowercase();
    if COMMON.iter().any(|c| lower == *c) || lower.chars().all(|c| c == lower.chars().next().unwrap()) {
        return Err("This password is too common. Pick another one.".into());
    }
    Ok(())
}

fn hash_password(pw: &str) -> String {
    Argon2::default().hash_password(pw.as_bytes()).expect("argon2").to_string()
}

fn verify_password(app: &App, pw: &str) -> bool {
    let Some(stored) = app.db.get("admin_hash") else { return false };
    PasswordHash::new(&stored).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

// ---- connect codes --------------------------------------------------------------

/// Where apps connect: the address set in the wizard, or a sensible guess.
pub fn sync_url(app: &App) -> String {
    if let Some(u) = app.db.get("public_url").filter(|u| !u.is_empty()) {
        return u;
    }
    let mode = tls::mode(app);
    let port = app.sync_addr.port();
    match mode.as_str() {
        "proxy" | "plain" => format!("ws://127.0.0.1:{port}/sync"),
        _ => format!("wss://{}:{port}/sync", lan_ip().unwrap_or_else(|| "127.0.0.1".into())),
    }
}

/// This computer's address in the home network. Connecting a UDP socket sends nothing.
fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// `EGS1-` + base64url JSON: server address, certificate fingerprint (self-signed only),
/// project id and name, server name and the key. Everything the app needs to join.
pub fn connect_code(app: &App, key: &str, project_id: &str, project_name: &str) -> String {
    let body = json!({
        "u": sync_url(app),
        "f": tls::fingerprint(app),
        "p": project_id,
        "n": project_name,
        "s": app.db.get_or("server_name", "Evelopment server"),
        "k": key,
    });
    format!("EGS1-{}", b64(body.to_string().as_bytes()))
}

// ---- handlers -------------------------------------------------------------------

async fn state(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let csrf = session(&app, &headers);
    Json(json!({
        "setup": app.db.get("admin_hash").is_some(),
        "loggedIn": csrf.is_some(),
        "csrf": csrf,
        "serverName": app.db.get_or("server_name", ""),
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Setup {
    server_name: String,
    password: String,
    mode: String,
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    cert_path: String,
    #[serde(default)]
    key_path: String,
    #[serde(default)]
    acme_email: String,
}

async fn setup(State(app): State<Arc<App>>, Json(s): Json<Setup>) -> ApiResult {
    if app.db.get("admin_hash").is_some() {
        return Err(err(StatusCode::CONFLICT, "The server is already set up."));
    }
    check_password(&s.password).map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    if !["self-signed", "proxy", "files", "acme"].contains(&s.mode.as_str()) {
        return Err(err(StatusCode::BAD_REQUEST, "Pick how the server is reached."));
    }
    let name = s.server_name.trim();
    app.db.set("server_name", if name.is_empty() { "Evelopment server" } else { name });
    app.db.set("tls_mode", &s.mode);
    app.db.set("public_url", s.public_url.trim());
    app.db.set("cert_path", s.cert_path.trim());
    app.db.set("key_path", s.key_path.trim());
    app.db.set("acme_email", s.acme_email.trim());
    app.db.set("admin_hash", &hash_password(&s.password));
    app.db.audit("Server set up");
    Ok(start_session(&app))
}

#[derive(Deserialize)]
struct Login {
    password: String,
}

async fn login(State(app): State<Arc<App>>, Json(l): Json<Login>) -> ApiResult {
    {
        let fails = lock(&app.sessions.fails);
        if fails.1 > now() {
            return Err(err(
                StatusCode::TOO_MANY_REQUESTS,
                &format!("Too many wrong passwords. Try again in {} seconds.", fails.1 - now()),
            ));
        }
    }
    let ok = tokio::task::spawn_blocking({
        let app = app.clone();
        move || verify_password(&app, &l.password)
    })
    .await
    .unwrap_or(false);
    let mut fails = lock(&app.sessions.fails);
    if !ok {
        fails.0 += 1;
        if fails.0 >= 5 {
            // 30 s after five tries, doubling up to an hour.
            let wait = (30i64 << (fails.0 - 5).min(7)).min(3600);
            fails.1 = now() + wait;
            app.db.audit("Login locked after repeated wrong passwords");
        }
        return Err(err(StatusCode::UNAUTHORIZED, "Wrong password."));
    }
    *fails = (0, 0);
    drop(fails);
    app.db.audit("Logged in");
    Ok(start_session(&app))
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_token(&headers) {
        lock(&app.sessions.tokens).remove(&t);
    }
    let cookie = format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

fn settings_json(app: &App) -> Value {
    let get = |k: &str| app.db.get_or(k, "");
    json!({
        "serverName": get("server_name"),
        "mode": tls::mode(app),
        "publicUrl": get("public_url"),
        "syncUrl": sync_url(app),
        "certPath": get("cert_path"),
        "keyPath": get("key_path"),
        "acmeEmail": get("acme_email"),
        "trustProxy": app.db.flag("trust_proxy"),
        "dailyBackups": app.db.flag("daily_backups"),
        "backupDir": app.backup_dir().to_string_lossy(),
        "deleteUnusedDays": app.db.get_or("delete_unused_days", "0").parse::<i64>().unwrap_or(0),
        "fingerprint": tls::fingerprint(app),
        "syncAddr": app.sync_addr.to_string(),
        "dataDir": app.data_dir.to_string_lossy(),
    })
}

async fn overview(State(app): State<Arc<App>>, headers: HeaderMap) -> ApiResult {
    guard(&app, &headers, false)?;
    let projects: Vec<Value> = app
        .db
        .projects()
        .into_iter()
        .map(|p| {
            let keys = app.db.keys(&p.id).len();
            let used = dir_size(&app.project_dir(&p.id).join("assets"));
            json!({ "project": p, "keys": keys, "assetBytes": used })
        })
        .collect();
    Ok(Json(json!({
        "projects": projects,
        "connected": app.hub.connected(),
        "plugins": plugins::list(&app),
        "settings": settings_json(&app),
        "restartNeeded": *lock(&app.restart_needed),
    }))
    .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectEdit {
    name: Option<String>,
    quota_mb: Option<i64>,
    archived: Option<bool>,
}

fn clean_name(name: &str) -> Result<String, Response> {
    let n: String = name.trim().chars().filter(|c| !c.is_control()).take(120).collect();
    if n.is_empty() { Err(err(StatusCode::BAD_REQUEST, "Give it a name.")) } else { Ok(n) }
}

async fn create_project(State(app): State<Arc<App>>, headers: HeaderMap, Json(e): Json<ProjectEdit>) -> ApiResult {
    guard(&app, &headers, true)?;
    let name = clean_name(e.name.as_deref().unwrap_or(""))?;
    let id = new_project_id();
    app.db.create_project(&id, &name);
    app.db.audit(&format!("Created project \"{name}\""));
    Ok(Json(json!({ "id": id })).into_response())
}

/// A UUID v4, the same shape the app uses for project ids.
fn new_project_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("random");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn project_or_404(app: &App, id: &str) -> Result<crate::db::Project, Response> {
    app.db.project(id).ok_or_else(|| err(StatusCode::NOT_FOUND, "That project no longer exists."))
}

async fn update_project(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Json(e): Json<ProjectEdit>) -> ApiResult {
    guard(&app, &headers, true)?;
    let p = project_or_404(&app, &id)?;
    let name = e.name.as_deref().map(clean_name).transpose()?;
    let quota = e.quota_mb.map(|q| q.clamp(1, 1024 * 1024));
    app.db.update_project(&id, name.as_deref(), quota, e.archived);
    if e.archived == Some(true) {
        app.hub.close_project(&app, &id);
        app.db.audit(&format!("Closed project \"{}\"", p.name));
    } else if e.archived == Some(false) {
        app.db.audit(&format!("Reopened project \"{}\"", p.name));
    }
    if let Some(n) = &name {
        app.db.audit(&format!("Renamed project \"{}\" to \"{n}\"", p.name));
    }
    if let Some(q) = quota {
        app.db.audit(&format!("Set storage of \"{}\" to {q} MB", p.name));
    }
    app.hub.refresh_project(&app, &id);
    Ok(Json(json!({ "ok": true })).into_response())
}

async fn delete_project(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    guard(&app, &headers, true)?;
    let p = project_or_404(&app, &id)?;
    app.hub.close_project(&app, &id);
    app.db.delete_project(&id);
    let _ = std::fs::remove_dir_all(app.project_dir(&id));
    app.db.audit(&format!("Deleted project \"{}\"", p.name));
    Ok(Json(json!({ "ok": true })).into_response())
}

async fn list_keys(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    guard(&app, &headers, false)?;
    project_or_404(&app, &id)?;
    Ok(Json(app.db.keys(&id)).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewKey {
    label: String,
    role: String,
    expires_days: Option<i64>,
}

async fn create_key(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Json(k): Json<NewKey>) -> ApiResult {
    guard(&app, &headers, true)?;
    let p = project_or_404(&app, &id)?;
    let role = Role::parse(&k.role).ok_or_else(|| err(StatusCode::BAD_REQUEST, "Pick view or write."))?;
    let label = clean_name(&k.label)?;
    let expires = k.expires_days.filter(|d| *d > 0).map(|d| now() + d.min(3650) * 86400);
    let key = format!("egd-key-{}", random_token(32));
    app.db.add_key(&key, &id, role.as_str(), &label, expires);
    app.db.audit(&format!("Made a {} key \"{label}\" for \"{}\"", role.as_str(), p.name));
    // The key is shown once; only its hash is kept.
    Ok(Json(json!({ "key": key, "code": connect_code(&app, &key, &id, &p.name) })).into_response())
}

async fn revoke_key(State(app): State<Arc<App>>, headers: HeaderMap, Path(hash): Path<String>) -> ApiResult {
    guard(&app, &headers, true)?;
    let k = app.db.key_by_hash(&hash).ok_or_else(|| err(StatusCode::NOT_FOUND, "That key no longer exists."))?;
    app.db.delete_key(&hash);
    app.hub.kick_key(&hash);
    app.db.audit(&format!("Revoked key \"{}\"", k.label));
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsEdit {
    server_name: Option<String>,
    mode: Option<String>,
    public_url: Option<String>,
    cert_path: Option<String>,
    key_path: Option<String>,
    acme_email: Option<String>,
    trust_proxy: Option<bool>,
    daily_backups: Option<bool>,
    backup_dir: Option<String>,
    delete_unused_days: Option<i64>,
}

async fn save_settings(State(app): State<Arc<App>>, headers: HeaderMap, Json(s): Json<SettingsEdit>) -> ApiResult {
    guard(&app, &headers, true)?;
    let flag = |b: bool| if b { "1" } else { "0" };
    let mut restart = false;
    if let Some(v) = s.server_name {
        app.db.set("server_name", &clean_name(&v)?);
    }
    if let Some(v) = s.mode {
        if !["self-signed", "proxy", "files", "acme"].contains(&v.as_str()) {
            return Err(err(StatusCode::BAD_REQUEST, "Unknown connection mode."));
        }
        restart |= v != tls::mode(&app);
        app.db.set("tls_mode", &v);
    }
    for (key, value) in [("public_url", s.public_url), ("cert_path", s.cert_path), ("key_path", s.key_path), ("acme_email", s.acme_email)] {
        if let Some(v) = value {
            restart |= key != "public_url" && v.trim() != app.db.get_or(key, "");
            app.db.set(key, v.trim());
        }
    }
    if let Some(v) = s.backup_dir {
        app.db.set("backup_dir", v.trim());
    }
    for (key, value) in [("trust_proxy", s.trust_proxy), ("daily_backups", s.daily_backups)] {
        if let Some(v) = value {
            app.db.set(key, flag(v));
        }
    }
    if let Some(d) = s.delete_unused_days {
        app.db.set("delete_unused_days", &d.clamp(0, 36500).to_string());
    }
    if restart {
        *lock(&app.restart_needed) = true;
    }
    app.db.audit("Changed settings");
    Ok(Json(json!({ "ok": true, "restartNeeded": restart })).into_response())
}

#[derive(Deserialize)]
struct PasswordChange {
    current: String,
    next: String,
}

async fn change_password(State(app): State<Arc<App>>, headers: HeaderMap, Json(p): Json<PasswordChange>) -> ApiResult {
    guard(&app, &headers, true)?;
    if !verify_password(&app, &p.current) {
        return Err(err(StatusCode::UNAUTHORIZED, "The current password is wrong."));
    }
    check_password(&p.next).map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    app.db.set("admin_hash", &hash_password(&p.next));
    // Log out everywhere else.
    lock(&app.sessions.tokens).clear();
    app.db.audit("Changed the admin password");
    Ok(start_session(&app))
}

async fn install_plugin(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> ApiResult {
    guard(&app, &headers, true)?;
    let info = plugins::install(&app, &body).map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    app.db.audit(&format!("Installed plugin \"{}\" {}", info.name, info.version));
    Ok(Json(info).into_response())
}

async fn remove_plugin(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    guard(&app, &headers, true)?;
    if !plugins::remove(&app, &id) {
        return Err(err(StatusCode::NOT_FOUND, "That plugin is not installed."));
    }
    app.db.audit(&format!("Removed plugin {id}"));
    Ok(Json(json!({ "ok": true })).into_response())
}

async fn download_backup(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    guard(&app, &headers, false)?;
    let p = project_or_404(&app, &id)?;
    let zip = tokio::task::spawn_blocking({
        let app = app.clone();
        move || backup::project_zip(&app, &id)
    })
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "Backup failed."))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    app.db.audit(&format!("Downloaded a backup of \"{}\"", p.name));
    let file: String = p.name.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}.egd-server-backup.zip\"")),
        ],
        zip,
    )
        .into_response())
}

async fn restore_backup(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> ApiResult {
    guard(&app, &headers, true)?;
    let p = project_or_404(&app, &id)?;
    // Everyone is disconnected first, so nobody's app merges the old state back in by accident.
    app.hub.close_project(&app, &id);
    tokio::task::spawn_blocking({
        let app = app.clone();
        move || backup::restore_project(&app, &id, &body)
    })
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "Restore failed."))?
    .map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    app.db.audit(&format!("Restored a backup into \"{}\"", p.name));
    Ok(Json(json!({ "ok": true })).into_response())
}

async fn backup_now(State(app): State<Arc<App>>, headers: HeaderMap) -> ApiResult {
    guard(&app, &headers, true)?;
    let dir = tokio::task::spawn_blocking({
        let app = app.clone();
        move || backup::server_backup(&app)
    })
    .await
    .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "Backup failed."))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    app.db.audit("Made a full server backup");
    Ok(Json(json!({ "ok": true, "path": dir.to_string_lossy() })).into_response())
}

async fn audit(State(app): State<Arc<App>>, headers: HeaderMap) -> ApiResult {
    guard(&app, &headers, false)?;
    let rows: Vec<Value> = app.db.audit_log().into_iter().map(|(at, action)| json!({ "at": at, "action": action })).collect();
    Ok(Json(rows).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_rules() {
        assert!(check_password("short").is_err());
        assert!(check_password("password1234").is_err());
        assert!(check_password("aaaaaaaaaaaaaaaa").is_err());
        assert!(check_password("blue horse staple 42").is_ok());
        let h = hash_password("blue horse staple 42");
        assert!(h.starts_with("$argon2id$"));
    }

    #[test]
    fn project_ids_look_like_uuids() {
        let id = new_project_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }
}
