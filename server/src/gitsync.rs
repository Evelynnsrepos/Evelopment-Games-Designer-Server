//! Git sync (server 0.9): writes each project as the app's normal project folder into
//! `<data>/git/<project id>/` and pushes it to a repository in the background.
//! Push only: the server is the project's source of truth, it never pulls or forces.
//! Uses the computer's own `git` and its login; no password or token is stored here.

use crate::App;
use crate::util::{is_asset_path, now, sha256_hex};
use serde_json::{Map as JsonMap, Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use yrs::types::ToJson;
use yrs::updates::decoder::Decode;
use yrs::{Any, Doc, ReadTxn, Transact, Update};

/// One git sync at a time, so a slow push never overlaps the next round or a shutdown.
static RUNNING: Mutex<()> = Mutex::new(());

const QUICK_MS: u64 = 30_000;
const NETWORK_MS: u64 = 120_000;
/// GitHub refuses files over 100 MB.
const MAX_FILE: u64 = 100 * 1024 * 1024;
const ENTITY_FILES: [(&str, &str); 4] = [("item", "items"), ("character", "characters"), ("town", "towns"), ("enemy", "enemies")];

// ---- settings, per project -----------------------------------------------------------

fn key(id: &str, what: &str) -> String {
    format!("git.{id}.{what}")
}

pub fn url(app: &App, id: &str) -> String {
    app.db.get_or(&key(id, "url"), "")
}

pub fn status_json(app: &App, id: &str) -> Value {
    let get = |w: &str| app.db.get_or(&key(id, w), "");
    json!({
        "url": get("url"),
        "branch": branch(app, id),
        "allowPublic": app.db.flag(&key(id, "public")),
        "lastSync": get("last").parse::<i64>().ok(),
        "error": get("error"),
    })
}

fn branch(app: &App, id: &str) -> String {
    let b = app.db.get_or(&key(id, "branch"), "main");
    if valid_branch(&b) { b } else { "main".into() }
}

pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with('-')
        && !b.contains("..")
        && !b.ends_with(['.', '/'])
        && b.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

pub fn configure(app: &App, id: &str, url: &str, branch: &str, allow_public: bool) {
    app.db.set(&key(id, "url"), url.trim());
    app.db.set(&key(id, "branch"), branch.trim());
    app.db.set(&key(id, "public"), if allow_public { "1" } else { "0" });
    // A new address or branch starts a fresh history next to it.
    app.db.set(&key(id, "hash"), "");
    app.db.set(&key(id, "error"), "");
    let _ = std::fs::remove_dir_all(work_dir(app, id));
}

pub fn forget(app: &App, id: &str) {
    for w in ["url", "branch", "public", "hash", "last", "error"] {
        app.db.delete_setting(&key(id, w));
    }
    let _ = std::fs::remove_dir_all(work_dir(app, id));
}

fn work_dir(app: &App, id: &str) -> PathBuf {
    app.data_dir.join("git").join(id)
}

// ---- running git -------------------------------------------------------------------

pub struct GitOut {
    pub code: i32,
    pub text: String,
}

/// `git <args>` in `cwd`, stopped after `timeout_ms` (code -1). Never waits on a password prompt.
pub fn git(cwd: &Path, args: &[&str], timeout_ms: u64) -> Result<GitOut, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("Git is not installed or could not start ({e}). Install Git to use git sync."))?;
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let code = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status.code().unwrap_or(-1);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break -1;
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    Ok(GitOut { code, text: format!("{}{}", out_t.join().unwrap_or_default(), err_t.join().unwrap_or_default()) })
}

fn must(cwd: &Path, args: &[&str], timeout_ms: u64) -> Result<String, String> {
    let r = git(cwd, args, timeout_ms)?;
    if r.code == 0 { Ok(r.text.trim().to_string()) } else { Err(explain(&r)) }
}

/// A git failure in plain words.
pub fn explain(r: &GitOut) -> String {
    let t = r.text.as_str();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if r.code == -1 {
        "Git took too long and was stopped. Check the internet connection.".into()
    } else if has(&[
        "terminal prompts disabled",
        "could not read Username",
        "could not read Password",
        "Authentication failed",
        "Permission denied (publickey",
        "Host key verification failed",
    ]) {
        "Git needs a login on this computer. Log in once with Git in a terminal (or GitHub Desktop) as the user the server runs as, then press Sync now.".into()
    } else if has(&["Could not resolve host", "Network is unreachable", "Connection timed out", "Connection refused", "Failed to connect"])
    {
        "Could not reach the repository. Check the internet connection.".into()
    } else if has(&["[rejected]", "non-fast-forward", "fetch first"]) {
        "The repository has changes the server doesn't have (someone else pushed to it). The server never overwrites them: use an empty repository or another branch.".into()
    } else if has(&["not found", "does not appear to be a git repository"]) {
        "Git could not find that repository. Check the address.".into()
    } else {
        t.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("Git failed.").to_string()
    }
}

// ---- private repositories only -------------------------------------------------------

/// Where an anonymous visitor would read the repository: SSH addresses become HTTPS, logins
/// are dropped. None for folders on this computer. Same rules as the app (core/gitsync).
pub fn public_read_url(url: &str) -> Option<String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    // `git@host:owner/repo` (scp style)
    if !lower.contains("://") {
        let (user_host, path) = u.split_once(':')?;
        let host = user_host.rsplit_once('@')?.1;
        if host.is_empty() || path.trim_start_matches('/').is_empty() {
            return None;
        }
        return Some(format!("https://{host}/{}", path.trim_start_matches('/')));
    }
    let (scheme, _) = lower.split_once("://")?;
    if !["ssh", "git", "http", "https"].contains(&scheme) {
        return None;
    }
    let rest = &u[scheme.len() + 3..];
    let (authority, path) = rest.split_once('/')?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = host_port.split_once(':').map_or((host_port, None), |(h, p)| (h, Some(p)));
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let web = scheme == "http" || scheme == "https";
    let port = port.filter(|_| web).map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{}://{host}{port}/{path}", if scheme == "http" { "http" } else { "https" }))
}

/// True when anyone on the internet can read the repository (git reads it without any login).
pub fn is_public(cwd: &Path, url: &str) -> bool {
    let Some(open) = public_read_url(url) else { return false };
    git(cwd, &["-c", "credential.helper=", "-c", "core.askPass=", "ls-remote", "--heads", &open], 15_000).is_ok_and(|r| r.code == 0)
}

pub const PUBLIC_ERROR: &str = "This repository is public: anyone on the internet can see it, including unreleased story and art. Git sync only uses private repositories. Make it private on your Git host's website, or allow a public repository for this project (not recommended).";

// ---- the project folder, built from the server's copy ------------------------------

/// The app's JSON for one top-level shared value (core/collab/bridge.ts `fromY`).
fn from_y(v: &Any) -> Value {
    match v {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(yrs::any::Number::Int(i)) => json!(i),
        // JavaScript writes whole numbers without ".0".
        Any::Number(yrs::any::Number::Float(f)) if f.fract() == 0.0 && f.abs() < 9e15 => json!(*f as i64),
        Any::Number(yrs::any::Number::Float(f)) => json!(f),
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(b) => Value::Array(b.iter().map(|x| json!(x)).collect()),
        Any::Array(a) => Value::Array(a.iter().map(from_y).collect()),
        Any::Map(m) if m.contains_key("__list") => {
            let mut items: Vec<(String, &String, Value)> = m
                .iter()
                .filter(|(k, v)| k.as_str() != "__list" && matches!(v, Any::Map(_)))
                .map(|(k, v)| {
                    let order = match v {
                        Any::Map(inner) => match inner.get("__o") {
                            Some(Any::String(s)) => s.to_string(),
                            _ => String::new(),
                        },
                        _ => String::new(),
                    };
                    (order, k, from_y(v))
                })
                .collect();
            items.sort_by(|a, b| (a.0.as_str(), a.1).cmp(&(b.0.as_str(), b.1)));
            Value::Array(items.into_iter().map(|i| i.2).collect())
        }
        Any::Map(m) => {
            Value::Object(m.iter().filter(|(k, _)| k.as_str() != "__o").map(|(k, v)| (k.clone(), from_y(v))).collect::<JsonMap<_, _>>())
        }
    }
}

/// Every file of the project folder except assets: relative path -> contents.
pub fn project_files(state: &[u8], schema: i64) -> Result<Vec<(String, String)>, String> {
    let doc = Doc::new();
    if !state.is_empty() {
        let u = Update::decode_v1(state).map_err(|_| "The project's stored state is unreadable.".to_string())?;
        doc.transact_mut().apply_update(u).map_err(|e| e.to_string())?;
    }
    let names: Vec<String> = doc.transact().root_refs().map(|(n, _)| n.to_string()).filter(|n| !n.starts_with("text:")).collect();
    let maps: Vec<_> = names.iter().map(|n| (n.clone(), doc.get_or_insert_map(n.as_str()))).collect();
    let txn = doc.transact();
    let wrap = |data: Value| serde_json::to_string_pretty(&json!({ "schemaVersion": schema, "data": data })).unwrap_or_default();
    let mut out = vec![];
    let mut tops = std::collections::HashMap::new();
    for (name, map) in maps {
        let any = map.to_json(&txn);
        let Any::Map(m) = &any else { continue };
        // Empty, or deleted on every device (bridge.ts `REMOVED`).
        if m.is_empty() || matches!(m.get("__removed"), Some(Any::Bool(true))) {
            continue;
        }
        tops.insert(name, from_y(&any));
    }
    // Still empty: nobody moved a project to the server yet, so there is nothing to push.
    let Some(Value::Object(mut meta)) = tops.remove("meta") else { return Ok(vec![]) };
    // Only on each computer, never shared (projectStore `sharedMeta`).
    meta.insert("layout".into(), Value::Null);
    meta.insert("sidebarCollapsed".into(), Value::Bool(false));
    out.push(("project.json".into(), wrap(Value::Object(meta))));
    for (ty, file) in ENTITY_FILES {
        let list = tops.remove(&format!("entities:{ty}")).unwrap_or(json!([]));
        out.push((format!("entities/{file}.json"), wrap(list)));
    }
    out.push(("entities/categories.json".into(), wrap(tops.remove("categories").unwrap_or(json!([])))));
    for (name, value) in tops {
        let Some((ty, id)) = name.strip_prefix("doc:").and_then(|r| r.split_once('/')) else { continue };
        // Names come from the network: only plain file names become paths.
        let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        if plain(ty) && plain(id) {
            out.push((format!("components/{ty}/{id}.json"), wrap(value)));
        }
    }
    out.sort();
    Ok(out)
}

/// Write the folder: JSON files replaced, assets copied when new, removed ones deleted.
fn write_folder(app: &App, id: &str, dir: &Path, files: &[(String, String)]) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    for sub in ["entities", "components"] {
        let _ = std::fs::remove_dir_all(dir.join(sub));
    }
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(io)?;
        std::fs::write(&path, text).map_err(io)?;
    }
    std::fs::write(dir.join(".gitignore"), ".backups/\ncollab/\n").map_err(io)?;
    let from = app.project_dir(id).join("assets");
    for kind in ["images", "audio"] {
        let (src, dst) = (from.join(kind), dir.join("assets").join(kind));
        std::fs::create_dir_all(&dst).map_err(io)?;
        let names: Vec<String> = std::fs::read_dir(&src)
            .map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        for name in &names {
            if !is_asset_path(&format!("assets/{kind}/{name}")) {
                continue;
            }
            let size = std::fs::metadata(src.join(name)).map(|m| m.len()).unwrap_or(0);
            if size > MAX_FILE {
                return Err(format!("assets/{kind}/{name} is over 100 MB, which GitHub refuses. Remove or shrink it in the app."));
            }
            // Asset files are named by their id and never change, so a copy that exists is current.
            if std::fs::metadata(dst.join(name)).map(|m| m.len()).ok() != Some(size) {
                std::fs::copy(src.join(name), dst.join(name)).map_err(io)?;
            }
        }
        for e in std::fs::read_dir(&dst).map_err(io)?.filter_map(Result::ok) {
            if !names.contains(&e.file_name().to_string_lossy().to_string()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

// ---- syncing -------------------------------------------------------------------------

/// Sync one project if it changed since its last sync (or always, with `force`).
/// Errors are stored for the admin page as well as returned.
pub fn sync_project(app: &App, id: &str, force: bool) -> Result<bool, String> {
    let _one = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let result = sync_inner(app, id, force);
    match &result {
        Ok(_) => app.db.set(&key(id, "error"), ""),
        Err(e) => app.db.set(&key(id, "error"), e),
    }
    result
}

fn sync_inner(app: &App, id: &str, force: bool) -> Result<bool, String> {
    let remote = url(app, id);
    if remote.is_empty() {
        return Ok(false);
    }
    let p = app.db.project(id).ok_or("No such project")?;
    let state = app.hub.state_of(app, id).unwrap_or_default();
    let assets = crate::sync::dir_size(&app.project_dir(id).join("assets"));
    let hash = sha256_hex(&[state.as_slice(), &assets.to_le_bytes()].concat());
    if !force && app.db.get(&key(id, "hash")).as_deref() == Some(hash.as_str()) {
        return Ok(false);
    }
    let files = project_files(&state, p.schema.unwrap_or(1))?;
    if files.is_empty() {
        return Ok(false);
    }
    let dir = work_dir(app, id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if !app.db.flag(&key(id, "public")) && is_public(&dir, &remote) {
        return Err(PUBLIC_ERROR.into());
    }
    let b = branch(app, id);
    if !dir.join(".git").exists() {
        must(&dir, &["init", "-q"], QUICK_MS)?;
        must(&dir, &["remote", "add", "origin", &remote], QUICK_MS)?;
        must(&dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{b}")], QUICK_MS)?;
        // An existing repository (say the desktop's git sync of this project): continue its history.
        if git(&dir, &["fetch", "-q", "origin", &b], NETWORK_MS)?.code == 0 {
            must(&dir, &["reset", "-q", "--soft", "FETCH_HEAD"], QUICK_MS)?;
        }
    }
    write_folder(app, id, &dir, &files)?;
    must(&dir, &["add", "-A"], QUICK_MS)?;
    let changed = git(&dir, &["diff", "--cached", "--quiet"], QUICK_MS)?.code != 0;
    if changed {
        let msg = format!("EGD server sync {}", crate::backup::stamp(now()));
        must(&dir, &["-c", "user.name=EGD Server", "-c", "user.email=egd-server@localhost", "commit", "-q", "-m", &msg], QUICK_MS)?;
    }
    if git(&dir, &["rev-parse", "-q", "--verify", "HEAD"], QUICK_MS)?.code == 0 {
        must(&dir, &["push", "-q", "origin", &format!("HEAD:refs/heads/{b}")], NETWORK_MS)?;
    }
    app.db.set(&key(id, "hash"), &hash);
    app.db.set(&key(id, "last"), &now().to_string());
    Ok(changed)
}

/// Every project with git sync on that changed. Errors stay next to each project.
pub fn sync_all(app: &App) {
    for p in app.db.projects() {
        if !url(app, &p.id).is_empty()
            && let Err(e) = sync_project(app, &p.id, false)
        {
            eprintln!("Git sync of \"{}\": {e}", p.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Map, MapPrelim};

    #[test]
    fn public_urls() {
        assert_eq!(public_read_url("git@github.com:me/game.git").as_deref(), Some("https://github.com/me/game.git"));
        assert_eq!(public_read_url("ssh://git@host:2222/me/game").as_deref(), Some("https://host/me/game"));
        assert_eq!(public_read_url("https://user:tok@gitea.lan:3000/me/g").as_deref(), Some("https://gitea.lan:3000/me/g"));
        assert_eq!(public_read_url("http://gitea.lan/me/g").as_deref(), Some("http://gitea.lan/me/g"));
        assert_eq!(public_read_url("/home/me/repo.git"), None);
        assert_eq!(public_read_url("file:///home/me/repo.git"), None);
        assert_eq!(public_read_url("C:\\repos\\game"), None);
    }

    #[test]
    fn branches() {
        assert!(valid_branch("main") && valid_branch("egd/server"));
        assert!(!valid_branch("-x") && !valid_branch("a..b") && !valid_branch("a/") && !valid_branch("a b"));
    }

    /// The same shapes the app's bridge writes: plain maps, keyed lists with order keys, removed docs.
    fn sample_state() -> Vec<u8> {
        let d = Doc::new();
        let meta = d.get_or_insert_map("meta");
        let chars = d.get_or_insert_map("entities:character");
        let doc = d.get_or_insert_map("doc:writer/w1");
        let gone = d.get_or_insert_map("doc:writer/old");
        let evil = d.get_or_insert_map("doc:../x/y");
        let mut t = d.transact_mut();
        meta.insert(&mut t, "name", "Game");
        meta.insert(&mut t, "version", 1.0);
        chars.insert(&mut t, "__list", 1.0);
        let b = chars.insert(&mut t, "b", MapPrelim::default());
        b.insert(&mut t, "id", "b");
        b.insert(&mut t, "__o", "a1");
        let a = chars.insert(&mut t, "a", MapPrelim::default());
        a.insert(&mut t, "id", "a");
        a.insert(&mut t, "__o", "a0");
        doc.insert(&mut t, "title", "Ch 1");
        gone.insert(&mut t, "__removed", true);
        evil.insert(&mut t, "x", 1.5);
        drop(t);
        d.transact().encode_state_as_update_v1(&yrs::StateVector::default())
    }

    #[test]
    fn builds_the_apps_project_folder() {
        let files: std::collections::HashMap<_, _> = project_files(&sample_state(), 1).unwrap().into_iter().collect();
        let read = |p: &str| serde_json::from_str::<Value>(&files[p]).unwrap();
        let meta = read("project.json");
        assert_eq!(meta["schemaVersion"], 1);
        assert_eq!(meta["data"]["name"], "Game");
        assert_eq!(meta["data"]["layout"], Value::Null);
        assert!(files["project.json"].contains("\"version\": 1\n") || files["project.json"].contains("\"version\": 1,"));
        let chars = read("entities/characters.json");
        assert_eq!(chars["data"], json!([{ "id": "a" }, { "id": "b" }]));
        assert_eq!(read("entities/items.json")["data"], json!([]));
        assert_eq!(read("components/writer/w1.json")["data"], json!({ "title": "Ch 1" }));
        assert!(!files.contains_key("components/writer/old.json"));
        assert_eq!(files.len(), 7, "{:?}", files.keys());
    }

    #[test]
    fn empty_project_has_no_files() {
        assert!(project_files(&[], 1).unwrap().is_empty());
    }

    #[test]
    fn pushes_to_a_local_repository() {
        let root = std::env::temp_dir().join(format!("egd-git-test-{}", crate::util::random_token(8)));
        let bare = root.join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        if git(&bare, &["init", "-q", "--bare"], QUICK_MS).map(|r| r.code) != Ok(0) {
            eprintln!("git not installed, skipping");
            return;
        }
        let app = crate::sync::tests::test_app_in(&root.join("data"));
        app.db.create_project("p1", "Game");
        app.db.save_state("p1", &sample_state());
        configure(&app, "p1", bare.to_str().unwrap(), "main", false);
        assert_eq!(sync_project(&app, "p1", false), Ok(true));
        // Unchanged: nothing to do.
        assert_eq!(sync_project(&app, "p1", false), Ok(false));
        let files = must(&bare, &["ls-tree", "-r", "--name-only", "main"], QUICK_MS).unwrap();
        assert!(files.contains("project.json") && files.contains("components/writer/w1.json") && files.contains(".gitignore"), "{files}");
        // Someone else pushed: the server refuses to overwrite.
        let other = root.join("other");
        must(&root, &["clone", "-q", "-b", "main", bare.to_str().unwrap(), other.to_str().unwrap()], QUICK_MS).unwrap();
        std::fs::write(other.join("x.txt"), "x").unwrap();
        must(&other, &["add", "x.txt"], QUICK_MS).unwrap();
        must(&other, &["-c", "user.name=a", "-c", "user.email=a@b", "commit", "-q", "-m", "x"], QUICK_MS).unwrap();
        must(&other, &["push", "-q", "origin", "HEAD:main"], QUICK_MS).unwrap();
        let d = Doc::new();
        d.transact_mut().apply_update(Update::decode_v1(&sample_state()).unwrap()).unwrap();
        let meta = d.get_or_insert_map("meta");
        meta.insert(&mut d.transact_mut(), "name", "Game 2");
        app.db.save_state("p1", &d.transact().encode_state_as_update_v1(&yrs::StateVector::default()));
        let err = sync_project(&app, "p1", false).unwrap_err();
        assert!(err.contains("never overwrites"), "{err}");
        assert_eq!(status_json(&app, "p1")["error"], err);
        let _ = std::fs::remove_dir_all(&root);
    }
}
