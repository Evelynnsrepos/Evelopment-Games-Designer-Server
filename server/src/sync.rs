//! The app's collaboration protocol v1 (src/core/collab/protocol.ts in the app),
//! spoken over a WebSocket. The server is an always-on member of every project:
//! it keeps the Yjs document, relays edits and presence, and stores asset files.

use crate::App;
use crate::util::{device_id_for_key, find_asset_paths, is_asset_path, now};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use yrs::sync::awareness::AwarenessUpdateEntry;
use yrs::sync::{AwarenessUpdate, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::types::ToJson;
use yrs::{ClientID, Doc, GetString, ReadTxn, StateVector, Transact, Update};

pub const PROTOCOL_VERSION: i64 = 1;

pub mod msg {
    pub const HELLO: u8 = 0;
    pub const WELCOME: u8 = 1;
    pub const REJECT: u8 = 2;
    pub const SYNC: u8 = 3;
    pub const AWARENESS: u8 = 4;
    pub const ASSET_REQUEST: u8 = 5;
    pub const ASSET_CHUNK: u8 = 6;
    pub const CLOSED: u8 = 7;
}

/// Same limits as the app (frames up to 8 MiB, 512 KB asset chunks, 200 MB per file).
pub const MAX_FRAME: usize = 8 * 1024 * 1024 + 1024;
pub const CHUNK_BYTES: usize = 512 * 1024;
pub const MAX_ASSET_BYTES: u64 = 200 * 1024 * 1024;
const MAX_CONNS_PER_KEY: usize = 5;
const MAX_CONNS_PER_IP: u32 = 30;
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const PING_EVERY: Duration = Duration::from_secs(30);
/// View-only connections that keep sending changes are disconnected.
const MAX_STRIKES: u32 = 3;
const PARALLEL_ASSET_REQUESTS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    View,
    Write,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "view" => Some(Role::View),
            "write" => Some(Role::Write),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::View => "view",
            Role::Write => "write",
        }
    }
}

pub enum Out {
    Bin(Vec<u8>),
    /// Control messages outside protocol v1 (plugins).
    Text(String),
    Close,
}

pub struct Peer {
    tx: UnboundedSender<Out>,
    pub key_hash: String,
    pub device_id: String,
    pub role: Role,
    pub name: String,
    /// The project id this app uses (from its hello).
    project_id: String,
    pub since: i64,
    sent_welcome: bool,
    got_welcome: bool,
    ready: bool,
    /// Awareness client ids this connection told us about.
    clients: HashSet<u64>,
    strikes: u32,
}

impl Peer {
    fn send(&self, data: Vec<u8>) {
        let _ = self.tx.send(Out::Bin(data));
    }
    fn close(&self) {
        let _ = self.tx.send(Out::Close);
    }
}

struct Incoming {
    buf: Vec<u8>,
    received: usize,
    from: u64,
}

pub struct Room {
    pub id: String,
    pub name: String,
    schema: Option<i64>,
    /// The app's id for this project; None until someone uploads it.
    app_id: Option<String>,
    doc: Doc,
    /// Presence of everyone connected: client id -> (clock, JSON state). Never saved.
    awareness: HashMap<u64, (u32, Arc<str>)>,
    pub peers: HashMap<u64, Peer>,
    dirty: bool,
    scan: bool,
    incoming: HashMap<String, Incoming>,
    /// Connections that said they don't have a file.
    lacking: HashMap<String, HashSet<u64>>,
    assets_dir: PathBuf,
    quota_bytes: u64,
}

#[derive(Default)]
pub struct Hub {
    rooms: Mutex<HashMap<String, Arc<Mutex<Room>>>>,
    next_conn: AtomicU64,
    /// Open connections per IP, in memory only, for the connection limit. IPs are never written anywhere.
    ip_conns: Mutex<HashMap<IpAddr, u32>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(kind);
    out.extend_from_slice(body);
    out
}

pub fn json_frame(kind: u8, value: &serde_json::Value) -> Vec<u8> {
    frame(kind, value.to_string().as_bytes())
}

fn sync_frame(m: SyncMessage) -> Vec<u8> {
    frame(msg::SYNC, &m.encode_v1())
}

pub fn chunk_frame(path: &str, offset: usize, total: usize, missing: bool, bytes: &[u8]) -> Vec<u8> {
    let mut header = json!({ "path": path, "offset": offset, "total": total });
    if missing {
        header["missing"] = json!(true);
    }
    let head = header.to_string().into_bytes();
    let mut out = Vec::with_capacity(5 + head.len() + bytes.len());
    out.push(msg::ASSET_CHUNK);
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(bytes);
    out
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    protocol: i64,
    schema: i64,
    project_id: String,
    device_id: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ChunkHeader {
    path: String,
    offset: usize,
    total: usize,
    #[serde(default)]
    missing: bool,
}

#[derive(Serialize)]
pub struct Connected {
    pub project: String,
    pub name: String,
    pub role: &'static str,
    pub since: i64,
}

impl Hub {
    /// The room of a project, loading it from the database if nobody is connected.
    fn room(&self, app: &App, project_id: &str) -> Option<Arc<Mutex<Room>>> {
        let mut rooms = lock(&self.rooms);
        if let Some(r) = rooms.get(project_id) {
            return Some(r.clone());
        }
        let p = app.db.project(project_id)?;
        let doc = Doc::new();
        if let Some(state) = app.db.load_state(project_id) {
            match Update::decode_v1(&state) {
                Ok(u) => {
                    if let Err(e) = doc.transact_mut().apply_update(u) {
                        eprintln!("project {}: stored state could not be applied: {e}", p.id);
                    }
                }
                Err(e) => eprintln!("project {}: stored state is unreadable: {e}", p.id),
            }
        }
        let room = Arc::new(Mutex::new(Room {
            id: p.id.clone(),
            name: p.name,
            schema: p.schema,
            app_id: p.app_id,
            doc,
            awareness: HashMap::new(),
            peers: HashMap::new(),
            dirty: false,
            scan: true,
            incoming: HashMap::new(),
            lacking: HashMap::new(),
            assets_dir: app.project_dir(&p.id).join("assets"),
            quota_bytes: (p.quota_mb.max(0) as u64) * 1024 * 1024,
        }));
        rooms.insert(p.id, room.clone());
        Some(room)
    }

    /// Someone is connected to the project right now.
    pub fn is_open(&self, project_id: &str) -> bool {
        let room = lock(&self.rooms).get(project_id).cloned();
        room.is_some_and(|r| !lock(&r).peers.is_empty())
    }

    pub fn connected(&self) -> Vec<Connected> {
        let rooms: Vec<_> = lock(&self.rooms).values().cloned().collect();
        let mut out = vec![];
        for room in rooms {
            let r = lock(&room);
            for p in r.peers.values().filter(|p| p.ready) {
                out.push(Connected { project: r.name.clone(), name: p.name.clone(), role: p.role.as_str(), since: p.since });
            }
        }
        out
    }

    /// Disconnect everyone using a key (revoked or expired). They are told they were removed.
    pub fn kick_key(&self, key_hash: &str) {
        let rooms: Vec<_> = lock(&self.rooms).values().cloned().collect();
        for room in rooms {
            let r = lock(&room);
            for p in r.peers.values().filter(|p| p.key_hash == key_hash) {
                p.send(json_frame(msg::REJECT, &json!({ "reason": "not-member" })));
                p.close();
            }
        }
    }

    /// Close a project for everyone (archived or deleted): the app's "host closed" goodbye.
    pub fn close_project(&self, app: &App, project_id: &str) {
        let room = lock(&self.rooms).remove(project_id);
        if let Some(room) = room {
            let mut r = lock(&room);
            r.save(app);
            for p in r.peers.values() {
                p.send(json_frame(msg::CLOSED, &json!({ "reason": "host-closed" })));
                p.close();
            }
            r.peers.clear();
        }
    }

    /// A project's name or quota changed in the admin page.
    pub fn refresh_project(&self, app: &App, project_id: &str) {
        let room = lock(&self.rooms).get(project_id).cloned();
        if let (Some(room), Some(p)) = (room, app.db.project(project_id)) {
            let mut r = lock(&room);
            r.name = p.name;
            r.quota_bytes = (p.quota_mb.max(0) as u64) * 1024 * 1024;
        }
    }

    /// Runs every few seconds: save changed projects, fetch missing assets, unload idle rooms.
    pub fn tick(&self, app: &App) {
        let rooms: Vec<_> = lock(&self.rooms).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (id, room) in rooms {
            let mut r = lock(&room);
            r.save(app);
            if r.scan {
                r.scan = false;
                r.request_missing_assets();
            }
            if r.peers.is_empty() {
                lock(&self.rooms).remove(&id);
            }
        }
    }

    /// Save everything (on shutdown).
    pub fn save_all(&self, app: &App) {
        let rooms: Vec<_> = lock(&self.rooms).values().cloned().collect();
        for room in rooms {
            lock(&room).save(app);
        }
    }

    /// The current Yjs state of a project (for backups), without keeping it loaded.
    pub fn state_of(&self, app: &App, project_id: &str) -> Option<Vec<u8>> {
        let room = lock(&self.rooms).get(project_id).cloned();
        match room {
            Some(room) => Some(lock(&room).doc.transact().encode_state_as_update_v1(&StateVector::default())),
            None => app.db.load_state(project_id),
        }
    }
}

impl Room {
    fn save(&mut self, app: &App) {
        if !self.dirty {
            return;
        }
        // ponytail: whole state per save, debounced by the tick; an update log is only worth it for very large projects.
        let state = self.doc.transact().encode_state_as_update_v1(&StateVector::default());
        app.db.save_state(&self.id, &state);
        self.dirty = false;
    }

    fn broadcast(&self, from: u64, data: &[u8]) {
        for (id, p) in &self.peers {
            if *id != from && p.ready {
                p.send(data.to_vec());
            }
        }
    }

    fn reject(&self, conn: u64, reason: &str) {
        if let Some(p) = self.peers.get(&conn) {
            let schema = self.schema.unwrap_or(0);
            p.send(json_frame(msg::REJECT, &json!({ "reason": reason, "protocol": PROTOCOL_VERSION, "schema": schema })));
            p.close();
        }
    }

    fn handle(&mut self, app: &App, conn: u64, data: &[u8]) -> Result<(), String> {
        let (&kind, payload) = data.split_first().ok_or("empty message")?;
        let ready = self.peers.get(&conn).is_some_and(|p| p.ready);
        match kind {
            msg::HELLO => self.on_hello(app, conn, payload),
            msg::WELCOME => {
                if let Some(p) = self.peers.get_mut(&conn) {
                    p.got_welcome = true;
                }
                self.maybe_ready(conn);
                Ok(())
            }
            msg::REJECT | msg::CLOSED => Err("peer left".into()),
            msg::SYNC if ready => self.on_sync(app, conn, payload),
            msg::AWARENESS if ready => self.on_awareness(conn, payload),
            msg::ASSET_REQUEST if ready => {
                #[derive(Deserialize)]
                struct Req {
                    path: String,
                }
                let req: Req = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                self.serve_asset(conn, req.path);
                Ok(())
            }
            msg::ASSET_CHUNK if ready => self.on_chunk(conn, payload),
            msg::SYNC | msg::AWARENESS | msg::ASSET_REQUEST | msg::ASSET_CHUNK => Ok(()), // before the handshake: ignored
            other => Err(format!("unknown message type {other}")),
        }
    }

    fn on_hello(&mut self, app: &App, conn: u64, payload: &[u8]) -> Result<(), String> {
        let h: Hello = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        if h.protocol != PROTOCOL_VERSION {
            self.reject(conn, "version");
            return Ok(());
        }
        match self.schema {
            None => {
                // The first app to connect decides which data version this project uses.
                self.schema = Some(h.schema);
                app.db.set_schema(&self.id, h.schema);
            }
            Some(s) if s != h.schema => {
                self.reject(conn, "version");
                return Ok(());
            }
            _ => {}
        }
        if self.app_id.as_ref().is_some_and(|id| *id != h.project_id) {
            self.reject(conn, "project");
            return Ok(());
        }
        let Some(peer) = self.peers.get_mut(&conn) else { return Ok(()) };
        if h.device_id != peer.device_id {
            self.reject(conn, "not-member");
            return Ok(());
        }
        peer.project_id = h.project_id.clone();
        peer.name = h.name.chars().filter(|c| !c.is_control()).take(60).collect();
        app.db.set_key_name(&peer.key_hash, &peer.name);
        let server_name = app.db.get_or("server_name", "Evelopment server");
        peer.send(json_frame(
            msg::HELLO,
            &json!({
                "protocol": PROTOCOL_VERSION,
                "schema": h.schema,
                "projectId": h.project_id,
                "deviceId": app.server_id,
                "name": server_name,
                "color": "#868e96",
            }),
        ));
        peer.send(json_frame(msg::WELCOME, &json!({ "projectId": h.project_id, "projectName": self.name })));
        peer.sent_welcome = true;
        self.maybe_ready(conn);
        Ok(())
    }

    fn maybe_ready(&mut self, conn: u64) {
        let Some(p) = self.peers.get_mut(&conn) else { return };
        if p.ready || !p.sent_welcome || !p.got_welcome {
            return;
        }
        p.ready = true;
        let sv = self.doc.transact().state_vector();
        p.send(sync_frame(SyncMessage::SyncStep1(sv)));
        if !self.awareness.is_empty() {
            let clients = self
                .awareness
                .iter()
                .map(|(id, (clock, json))| (ClientID::new(*id), AwarenessUpdateEntry { clock: *clock, json: json.clone() }))
                .collect();
            p.send(frame(msg::AWARENESS, &AwarenessUpdate { clients }.encode_v1()));
        }
        if p.role == Role::Write {
            self.scan = true;
        }
    }

    fn on_sync(&mut self, app: &App, conn: u64, payload: &[u8]) -> Result<(), String> {
        let m = SyncMessage::decode_v1(payload).map_err(|e| e.to_string())?;
        let update = match m {
            SyncMessage::SyncStep1(sv) => {
                let diff = self.doc.transact().encode_diff_v1(&sv);
                if let Some(p) = self.peers.get(&conn) {
                    p.send(sync_frame(SyncMessage::SyncStep2(diff)));
                }
                return Ok(());
            }
            SyncMessage::SyncStep2(u) | SyncMessage::Update(u) => u,
        };
        let decoded = Update::decode_v1(&update).map_err(|e| e.to_string())?;
        let peer = self.peers.get_mut(&conn).ok_or("gone")?;
        if peer.role == Role::View {
            // Enforced here, not only in the app: view keys can never change a project.
            if !decoded.is_empty() {
                peer.strikes += 1;
                if peer.strikes >= MAX_STRIKES {
                    return Err("view-only connection kept sending changes".into());
                }
            }
            return Ok(());
        }
        if decoded.is_empty() {
            return Ok(());
        }
        if self.app_id.is_none() {
            // First upload: this project now belongs to that app project id; anyone else is turned away.
            let id = peer.project_id.clone();
            app.db.set_app_id(&self.id, Some(&id));
            for (other, p) in &self.peers {
                if *other != conn && p.project_id != id {
                    p.send(json_frame(msg::REJECT, &json!({ "reason": "project" })));
                    p.close();
                }
            }
            self.app_id = Some(id);
        }
        self.doc.transact_mut().apply_update(decoded).map_err(|e| e.to_string())?;
        self.dirty = true;
        self.scan = true;
        self.broadcast(conn, &sync_frame(SyncMessage::Update(update)));
        Ok(())
    }

    fn on_awareness(&mut self, conn: u64, payload: &[u8]) -> Result<(), String> {
        let update = AwarenessUpdate::decode_v1(payload).map_err(|e| e.to_string())?;
        let peer = self.peers.get_mut(&conn).ok_or("gone")?;
        for (id, entry) in &update.clients {
            let id = id.get();
            // A connection may only speak for client ids no other connection uses.
            if !peer.clients.contains(&id) && self.awareness.contains_key(&id) {
                return Err("awareness for someone else's client".into());
            }
            if &*entry.json == "null" {
                self.awareness.remove(&id);
                peer.clients.remove(&id);
            } else {
                self.awareness.insert(id, (entry.clock, entry.json.clone()));
                peer.clients.insert(id);
            }
        }
        self.broadcast(conn, &frame(msg::AWARENESS, payload));
        Ok(())
    }

    fn remove_peer(&mut self, conn: u64) {
        let Some(peer) = self.peers.remove(&conn) else { return };
        self.incoming.retain(|_, job| job.from != conn);
        for set in self.lacking.values_mut() {
            set.remove(&conn);
        }
        if peer.clients.is_empty() {
            return;
        }
        let mut clients = HashMap::new();
        for id in &peer.clients {
            if let Some((clock, _)) = self.awareness.remove(id) {
                clients.insert(ClientID::new(*id), AwarenessUpdateEntry { clock: clock + 1, json: "null".into() });
            }
        }
        if !clients.is_empty() {
            self.broadcast(conn, &frame(msg::AWARENESS, &AwarenessUpdate { clients }.encode_v1()));
        }
    }

    // ---- assets -----------------------------------------------------------------

    fn asset_file(&self, path: &str) -> PathBuf {
        // `path` was checked with is_asset_path: two fixed segments and a uuid file name.
        let mut p = self.assets_dir.clone();
        for part in path.split('/').skip(1) {
            p.push(part);
        }
        p
    }

    fn serve_asset(&self, conn: u64, path: String) {
        if !is_asset_path(&path) {
            return;
        }
        let Some(peer) = self.peers.get(&conn) else { return };
        let file = self.asset_file(&path);
        let tx = peer.tx.clone();
        tokio::task::spawn_blocking(move || {
            let send = |d: Vec<u8>| tx.send(Out::Bin(d)).is_ok();
            let bytes = match std::fs::read(&file) {
                Ok(b) if b.len() as u64 <= MAX_ASSET_BYTES => b,
                _ => {
                    send(chunk_frame(&path, 0, 0, true, &[]));
                    return;
                }
            };
            if bytes.is_empty() {
                send(chunk_frame(&path, 0, 0, false, &[]));
                return;
            }
            for (i, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
                if !send(chunk_frame(&path, i * CHUNK_BYTES, bytes.len(), false, chunk)) {
                    return;
                }
            }
        });
    }

    /// Ask people with write access for files the project mentions but the server lacks.
    fn request_missing_assets(&mut self) {
        let mut wanted = HashSet::new();
        {
            // Root types arrive untyped from the app; read them the way the app writes them.
            let txn = self.doc.transact();
            let names: Vec<String> = txn.root_refs().map(|(n, _)| n.to_string()).collect();
            for name in names {
                if name.starts_with("text:") {
                    if let Some(f) = txn.get_xml_fragment(name.as_str()) {
                        find_asset_paths(&f.get_string(&txn), &mut wanted);
                    }
                } else if let Some(m) = txn.get_map(name.as_str()) {
                    collect_strings(&m.to_json(&txn), &mut wanted);
                }
            }
        }
        for path in wanted {
            if self.incoming.len() >= PARALLEL_ASSET_REQUESTS {
                self.scan = true; // more next tick
                break;
            }
            if self.incoming.contains_key(&path) || self.asset_file(&path).exists() {
                continue;
            }
            let lacking = self.lacking.get(&path);
            let from = self
                .peers
                .iter()
                .find(|(id, p)| p.ready && p.role == Role::Write && !lacking.is_some_and(|l| l.contains(id)));
            let Some((&id, peer)) = from else { continue };
            peer.send(json_frame(msg::ASSET_REQUEST, &json!({ "path": path })));
            self.incoming.insert(path, Incoming { buf: vec![], received: 0, from: id });
        }
    }

    fn on_chunk(&mut self, conn: u64, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 4 {
            return Err("short asset chunk".into());
        }
        let len = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        let head = payload.get(4..4 + len).ok_or("bad asset chunk")?;
        let h: ChunkHeader = serde_json::from_slice(head).map_err(|e| e.to_string())?;
        let bytes = &payload[4 + len..];
        // Only files we asked a write connection for are accepted: view keys can't upload.
        let Some(job) = self.incoming.get_mut(&h.path) else { return Ok(()) };
        if job.from != conn {
            return Ok(());
        }
        let give_up = |room: &mut Room| {
            room.incoming.remove(&h.path);
            room.lacking.entry(h.path.clone()).or_default().insert(conn);
        };
        if h.missing {
            give_up(self);
            self.scan = true;
            return Ok(());
        }
        if h.total as u64 > MAX_ASSET_BYTES || h.offset + bytes.len() > h.total {
            give_up(self);
            return Ok(());
        }
        if job.buf.len() != h.total {
            if dir_size(&self.assets_dir) + h.total as u64 > self.quota_bytes {
                eprintln!("project {}: storage quota reached, not storing {}", self.id, h.path);
                give_up(self);
                return Ok(());
            }
            let job = self.incoming.get_mut(&h.path).unwrap();
            job.buf = vec![0; h.total];
            job.received = 0;
        }
        let job = self.incoming.get_mut(&h.path).unwrap();
        job.buf[h.offset..h.offset + bytes.len()].copy_from_slice(bytes);
        job.received += bytes.len();
        if job.received < h.total {
            return Ok(());
        }
        let job = self.incoming.remove(&h.path).unwrap();
        let file = self.asset_file(&h.path);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension("part");
        if std::fs::write(&tmp, &job.buf).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
            eprintln!("project {}: could not save {}", self.id, h.path);
        }
        self.scan = true;
        Ok(())
    }
}

fn collect_strings(v: &yrs::Any, out: &mut HashSet<String>) {
    match v {
        yrs::Any::String(s) => find_asset_paths(s, out),
        yrs::Any::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        yrs::Any::Map(m) => m.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

pub fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

// ---- WebSocket ------------------------------------------------------------------

/// The client IP, for the per-IP connection limit only. Behind a reverse proxy
/// the operator turns on `trust_proxy` so X-Real-IP / X-Forwarded-For is used.
pub fn client_ip(app: &App, addr: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if app.db.flag("trust_proxy") {
        let header = headers.get("x-real-ip").or_else(|| headers.get("x-forwarded-for"));
        if let Some(ip) = header.and_then(|v| v.to_str().ok()).and_then(|s| s.split(',').next()).and_then(|s| s.trim().parse().ok()) {
            return ip;
        }
    }
    addr.ip()
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    State(app): State<Arc<App>>,
) -> Response {
    let ip = client_ip(&app, addr, &headers);
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |socket| async move {
        {
            let mut ips = lock(&app.hub.ip_conns);
            let n = ips.entry(ip).or_default();
            if *n >= MAX_CONNS_PER_IP {
                return;
            }
            *n += 1;
        }
        connection(socket, app.clone()).await;
        let mut ips = lock(&app.hub.ip_conns);
        if let Some(n) = ips.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                ips.remove(&ip);
            }
        }
    })
}

#[derive(Deserialize)]
struct Auth {
    #[serde(rename = "type")]
    kind: String,
    key: String,
}

async fn connection(mut socket: WebSocket, app: Arc<App>) {
    // 1. The first message is the key. It is checked before anything about a project is revealed.
    let auth = match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Auth>(&t).ok(),
        _ => None,
    };
    let fail = |reason: &str| Message::Text(json!({ "type": "auth-error", "reason": reason }).to_string().into());
    let Some(auth) = auth.filter(|a| a.kind == "auth") else {
        let _ = socket.send(fail("bad-request")).await;
        return;
    };
    let key = match app.db.find_key(&auth.key) {
        Some(k) if k.expires.is_none_or(|e| e > now()) => k,
        Some(_) => {
            let _ = socket.send(fail("expired")).await;
            return;
        }
        None => {
            // Slow down key guessing a little (keys are 256-bit, so this is only politeness).
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = socket.send(fail("bad-key")).await;
            return;
        }
    };
    let Some(project) = app.db.project(&key.project_id).filter(|p| !p.archived) else {
        let _ = socket.send(fail("closed")).await;
        return;
    };
    let Some(room) = app.hub.room(&app, &project.id) else { return };
    let role = Role::parse(&key.role).unwrap_or(Role::View);
    let conn = app.hub.next_conn.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = unbounded_channel::<Out>();
    let control = tx.clone();
    let (busy, empty) = {
        let mut r = lock(&room);
        let empty = r.doc.transact().state_vector().is_empty();
        let busy = r.peers.values().filter(|p| p.key_hash == key.hash).count() >= MAX_CONNS_PER_KEY;
        if !busy {
            r.peers.insert(
            conn,
            Peer {
                tx: tx.clone(),
                key_hash: key.hash.clone(),
                device_id: device_id_for_key(&auth.key),
                role,
                name: String::new(),
                project_id: String::new(),
                since: now(),
                sent_welcome: false,
                got_welcome: false,
                ready: false,
                clients: HashSet::new(),
                strikes: 0,
            },
            );
        }
        (busy, empty)
    };
    drop(tx);
    if busy {
        let _ = socket.send(fail("busy")).await;
        return;
    }
    app.db.touch(&project.id);
    let ok = json!({
        "type": "auth-ok",
        "serverId": app.server_id,
        "serverName": app.db.get_or("server_name", "Evelopment server"),
        // The app's project id once the project was uploaded (null while it's empty).
        "projectId": project.app_id,
        "serverProjectId": project.id,
        "projectName": project.name,
        "role": role.as_str(),
        // Nothing stored yet: the app may upload an existing project ("Move to server").
        "empty": empty,
    });

    // 2. Protocol v1 in binary frames, with a writer task so the room lock is never held across awaits.
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        if sink.send(Message::Text(ok.to_string().into())).await.is_err() {
            return;
        }
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Out::Bin(b)) => if sink.send(Message::Binary(b.into())).await.is_err() { break },
                    Some(Out::Text(t)) => if sink.send(Message::Text(t.into())).await.is_err() { break },
                    Some(Out::Close) | None => { let _ = sink.send(Message::Close(None)).await; break }
                },
                _ = ping.tick() => if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break },
            }
        }
    });

    loop {
        let next = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await;
        let data = match next {
            Ok(Some(Ok(Message::Binary(b)))) => b,
            Ok(Some(Ok(Message::Text(t)))) => {
                control_message(&app, &control, &t).await;
                continue;
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            _ => break, // closed, error, or idle too long
        };
        let failed = {
            let mut r = lock(&room);
            let result = r.handle(&app, conn, &data);
            if result.is_err() {
                if let Some(p) = r.peers.get(&conn) {
                    p.close();
                }
            }
            result.err()
        };
        if let Some(e) = failed {
            if e != "peer left" {
                eprintln!("dropping a connection: {e}");
            }
            break;
        }
    }
    lock(&room).remove_peer(conn);
    let _ = writer.await;
}

/// Text messages next to protocol v1: the plugins this server offers.
/// `{"type":"plugins"}` lists them; `{"type":"plugin","id":...}` sends one zip (base64).
async fn control_message(app: &App, out: &UnboundedSender<Out>, text: &str) {
    let Ok(m) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let reply = match m["type"].as_str() {
        Some("plugins") => json!({ "type": "plugins", "plugins": crate::plugins::list(app) }),
        Some("plugin") => {
            let id = m["id"].as_str().unwrap_or("");
            match crate::plugins::read(app, id).await {
                Some((info, zip)) => json!({ "type": "plugin", "id": id, "sha256": info.sha256, "zip": crate::util::b64(&zip) }),
                None => json!({ "type": "plugin", "id": id, "missing": true }),
            }
        }
        _ => return,
    };
    let _ = out.send(Out::Text(reply.to_string()));
}
