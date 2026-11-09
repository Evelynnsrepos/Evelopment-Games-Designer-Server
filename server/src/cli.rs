//! Everything the admin page does, from the command line, for headless servers.
//! Each command goes through the admin page's own handlers (same checks, same admin log),
//! working on the data folder directly. A running server picks the changes up within a
//! few seconds (`Hub::reconcile`), so commands work whether it runs or not.

use crate::{App, admin};
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use std::io::Read;
use std::sync::Arc;
use tower::ServiceExt;

pub const HELP: &str = "Commands (add --data <folder> as for the server; --json prints raw JSON):

  setup --mode self-signed|proxy|files|acme [--name N] [--public-url wss://…/sync]
        [--cert FILE --key FILE] [--acme-email E]
                                    First setup. Admin password from EGD_ADMIN_PASSWORD or stdin.
  status                            Settings, projects, plugins
  password                          New admin password (from EGD_ADMIN_PASSWORD or stdin)

  projects                          List projects
  project new NAME
  project rename P NAME
  project storage P MB              Storage for images and audio
  project close P | project reopen P
  project delete P --yes
  project backup P FILE.zip
  project restore P FILE.zip        The project must be closed first

  keys P                            List keys of a project
  key new P LABEL [--view] [--expires-days N]
                                    Prints the connect code (shown only once)
  key revoke HASH                   The start of the hash is enough

  git P                             Git sync status
  git set P URL [--branch B] [--allow-public \"PROJECT NAME\"]
  git off P
  git sync P                        Push now

  settings                          Show settings
  settings set NAME=VALUE …         e.g. idleStopMin=30 gitIntervalMin=10 dailyBackups=true
  plugins | plugin install FILE.zip | plugin remove ID
  backup                            Full server backup now
  log                               Admin log

  autostart on|off                  Start when you log in (Linux: on the first connection)

P is a project's name or id.";

pub const COMMANDS: &[&str] =
    &["setup", "status", "password", "projects", "project", "keys", "key", "git", "settings", "plugins", "plugin", "backup", "log"];

struct Cli {
    app: Arc<App>,
    cookie: String,
    csrf: String,
}

impl Cli {
    async fn call(&self, method: &str, path: &str, body: Option<Vec<u8>>, json_body: bool) -> Result<(Value, Vec<u8>), String> {
        let mut req = Request::builder()
            .method(method)
            .uri(format!("/admin/api/{path}"))
            .header(header::COOKIE, format!("{}={}", admin::COOKIE_NAME, self.cookie))
            .header("x-csrf", &self.csrf);
        if json_body {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        let mut req = req.body(body.map(Body::from).unwrap_or_else(Body::empty)).map_err(|e| e.to_string())?;
        // Like a browser on this computer.
        req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 0))));
        let res = admin::router(self.app.clone()).oneshot(req).await.map_err(|e| e.to_string())?;
        let ok = res.status().is_success();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.map_err(|e| e.to_string())?.to_vec();
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        if !ok {
            return Err(value["error"].as_str().unwrap_or("Something went wrong.").to_string());
        }
        Ok((value, bytes))
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        Ok(self.call("GET", path, None, false).await?.0)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        Ok(self.call("POST", path, Some(body.to_string().into_bytes()), true).await?.0)
    }

    /// A project by exact id or name (or the start of an id).
    async fn project(&self, p: &str) -> Result<Value, String> {
        let o = self.get("overview").await?;
        let all = o["projects"].as_array().cloned().unwrap_or_default();
        let hits: Vec<Value> = all
            .into_iter()
            .filter(|e| {
                e["project"]["id"] == p
                    || e["project"]["name"] == p
                    || e["project"]["id"].as_str().is_some_and(|id| p.len() >= 6 && id.starts_with(p))
            })
            .collect();
        match hits.len() {
            1 => Ok(hits[0].clone()),
            0 => Err(format!("No project called \"{p}\". `egd-server projects` lists them.")),
            _ => Err(format!("Several projects match \"{p}\". Use the id.")),
        }
    }
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> Result<&'a str, String> {
    args.get(i).map(String::as_str).filter(|a| !a.starts_with("--")).ok_or_else(|| format!("Missing {what}. See `egd-server --help`."))
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// A password from EGD_ADMIN_PASSWORD, or the first line of stdin (prompted when it's a terminal).
fn read_password() -> Result<String, String> {
    if let Ok(p) = std::env::var("EGD_ADMIN_PASSWORD") {
        return Ok(p);
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprint!("Admin password (at least 12 characters, shown while typing): ");
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// `true`/`false`, numbers, else text.
fn setting_value(v: &str) -> Value {
    match v {
        "true" => json!(true),
        "false" => json!(false),
        _ => v.parse::<i64>().map(Value::from).unwrap_or_else(|_| json!(v)),
    }
}

fn when(t: &Value) -> String {
    t.as_i64()
        .map(|t| {
            let s = crate::backup::stamp(t);
            format!("{} {}:{} UTC", &s[..10], &s[11..13], &s[13..15])
        })
        .unwrap_or_else(|| "never".into())
}

pub fn run(app: Arc<App>, args: &[String]) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(run_async(app, args))
}

async fn run_async(app: Arc<App>, args: &[String]) -> Result<(), String> {
    let raw = has(args, "--json");
    let (cookie, csrf) = admin::cli_session(&app);
    let cli = Cli { app: app.clone(), cookie, csrf };
    let print = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    let cmd = args[0].as_str();
    let sub = args.get(1).map(String::as_str).unwrap_or("");

    if cmd == "setup" {
        let mode = opt(args, "--mode").ok_or("Pick how the server is reached: --mode self-signed|proxy|files|acme")?;
        let password = read_password()?;
        cli.post(
            "setup",
            json!({
                "serverName": opt(args, "--name").unwrap_or_default(),
                "password": password,
                "mode": mode,
                "publicUrl": opt(args, "--public-url").unwrap_or_default(),
                "certPath": opt(args, "--cert").unwrap_or_default(),
                "keyPath": opt(args, "--key").unwrap_or_default(),
                "acmeEmail": opt(args, "--acme-email").unwrap_or_default(),
            }),
        )
        .await?;
        println!("Set up. Start the server (or restart it), then make a project: egd-server project new \"My game\"");
        return Ok(());
    }
    if app.db.get("admin_hash").is_none() {
        return Err("The server is not set up yet. Run `egd-server setup --mode …` first (or use the admin page).".into());
    }

    match (cmd, sub) {
        ("status", _) | ("settings", "") => {
            let o = cli.get("overview").await?;
            if raw {
                print(&o);
            } else {
                let s = &o["settings"];
                println!("Server:      {} ({})", s["serverName"].as_str().unwrap_or(""), env!("CARGO_PKG_VERSION"));
                println!(
                    "Apps connect to {} ({} mode, listening on {})",
                    s["syncUrl"].as_str().unwrap_or(""),
                    s["mode"].as_str().unwrap_or(""),
                    s["syncAddr"].as_str().unwrap_or("")
                );
                if let Some(f) = s["fingerprint"].as_str() {
                    println!("Certificate fingerprint: {f}");
                }
                println!("Data folder: {}", s["dataDir"].as_str().unwrap_or(""));
                println!("Autostart:   {}", if s["autostart"] == true { "on" } else { "off" });
                println!();
                println!("Settings (change with `egd-server settings set NAME=VALUE`):");
                for k in [
                    "serverName",
                    "mode",
                    "publicUrl",
                    "certPath",
                    "keyPath",
                    "acmeEmail",
                    "trustProxy",
                    "dailyBackups",
                    "backupDir",
                    "deleteUnusedDays",
                    "idleStopMin",
                    "gitIntervalMin",
                ] {
                    println!("  {k} = {}", s[k]);
                }
                if cmd == "status" {
                    println!();
                    println!(
                        "{} project(s), {} plugin(s). `egd-server projects` lists them.",
                        o["projects"].as_array().map_or(0, Vec::len),
                        o["plugins"].as_array().map_or(0, Vec::len)
                    );
                }
                if o["restartNeeded"] == true {
                    println!("Restart the server so the new connection settings take effect.");
                }
            }
        }
        ("settings", "set") => {
            let mut body = serde_json::Map::new();
            for pair in &args[2..] {
                let (k, v) = pair.split_once('=').ok_or_else(|| format!("{pair}: use NAME=VALUE"))?;
                body.insert(k.to_string(), setting_value(v));
            }
            if body.is_empty() {
                return Err("Nothing to set. Example: egd-server settings set idleStopMin=30".into());
            }
            let r = cli.post("settings", Value::Object(body)).await?;
            println!("Saved.");
            if r["restartNeeded"] == true {
                println!("Restart the server so the new connection settings take effect.");
            }
        }
        ("password", _) => {
            admin::set_password(&app, &read_password()?)?;
            println!("Password changed. Admin page sessions are logged out.");
        }
        ("projects", _) => {
            let o = cli.get("overview").await?;
            if raw {
                print(&o["projects"]);
            }
            for e in o["projects"].as_array().into_iter().flatten().filter(|_| !raw) {
                let p = &e["project"];
                let mut tags = vec![];
                if p["archived"] == true {
                    tags.push("closed".to_string());
                }
                if e["git"]["url"].as_str().is_some_and(|u| !u.is_empty()) {
                    tags.push(if e["git"]["allowPublic"] == true { "git sync, PUBLIC".into() } else { "git sync".into() });
                }
                println!(
                    "{}  {}{}  · {} key(s) · last used {}",
                    p["id"].as_str().unwrap_or(""),
                    p["name"].as_str().unwrap_or(""),
                    if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) },
                    e["keys"],
                    when(&p["lastUsed"])
                );
            }
        }
        ("project", "new") => {
            let r = cli.post("projects", json!({ "name": arg(args, 2, "the project name")? })).await?;
            println!(
                "Made project {}. Next: egd-server key new {} \"Who it's for\"",
                r["id"].as_str().unwrap_or(""),
                r["id"].as_str().unwrap_or("")
            );
        }
        ("project", "rename" | "storage" | "close" | "reopen") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            let id = p["project"]["id"].as_str().unwrap_or("").to_string();
            let body = match sub {
                "rename" => json!({ "name": arg(args, 3, "the new name")? }),
                "storage" => {
                    json!({ "quotaMb": arg(args, 3, "the size in MB")?.parse::<i64>().map_err(|_| "Storage is a number of MB.")? })
                }
                "close" => json!({ "archived": true }),
                _ => json!({ "archived": false }),
            };
            cli.post(&format!("projects/{id}"), body).await?;
            println!("Done.");
        }
        ("project", "delete") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            if !has(args, "--yes") {
                return Err(format!(
                    "This deletes \"{}\", its keys and its files from the server. Add --yes to do it.",
                    p["project"]["name"].as_str().unwrap_or("")
                ));
            }
            cli.call("DELETE", &format!("projects/{}", p["project"]["id"].as_str().unwrap_or("")), None, false).await?;
            println!("Deleted.");
        }
        ("project", "backup") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            let file = arg(args, 3, "the zip file to write")?;
            let (_, bytes) =
                cli.call("GET", &format!("projects/{}/backup", p["project"]["id"].as_str().unwrap_or("")), None, false).await?;
            std::fs::write(file, bytes).map_err(|e| format!("{file}: {e}"))?;
            println!("Saved {file}.");
        }
        ("project", "restore") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            if p["project"]["archived"] != true {
                return Err("Close the project first (egd-server project close …), so nobody's app merges the old state back in. Reopen it afterwards.".into());
            }
            let file = arg(args, 3, "the backup zip")?;
            let mut bytes = vec![];
            std::fs::File::open(file).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("{file}: {e}"))?;
            cli.call("POST", &format!("projects/{}/restore", p["project"]["id"].as_str().unwrap_or("")), Some(bytes), false).await?;
            println!("Restored. Reopen it with: egd-server project reopen {}", p["project"]["id"].as_str().unwrap_or(""));
        }
        ("keys", _) => {
            let p = cli.project(arg(args, 1, "the project")?).await?;
            let keys = cli.get(&format!("projects/{}/keys", p["project"]["id"].as_str().unwrap_or(""))).await?;
            if raw {
                print(&keys);
            }
            for k in keys.as_array().into_iter().flatten().filter(|_| !raw) {
                println!(
                    "{}  {}  {}  used by: {}  expires: {}",
                    &k["hash"].as_str().unwrap_or("")[..12.min(k["hash"].as_str().unwrap_or("").len())],
                    k["role"].as_str().unwrap_or(""),
                    k["label"].as_str().unwrap_or(""),
                    k["lastName"].as_str().filter(|n| !n.is_empty()).unwrap_or("not used yet"),
                    when(&k["expires"])
                );
            }
        }
        ("key", "new") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            let label = arg(args, 3, "who the key is for")?;
            let days = opt(args, "--expires-days").map(|d| d.parse::<i64>().map_err(|_| "--expires-days is a number")).transpose()?;
            let r = cli
                .post(
                    &format!("projects/{}/keys", p["project"]["id"].as_str().unwrap_or("")),
                    json!({ "label": label, "role": if has(args, "--view") { "view" } else { "write" }, "expiresDays": days }),
                )
                .await?;
            if raw {
                print(&r);
            } else {
                println!(
                    "Connect code for {label} (shown only once; they paste it into Join project):\n\n{}\n",
                    r["code"].as_str().unwrap_or("")
                );
            }
        }
        ("key", "revoke") => {
            let prefix = arg(args, 2, "the key's hash")?;
            if prefix.len() < 8 {
                return Err("Give at least the first 8 characters of the hash (egd-server keys P).".into());
            }
            let mut hits = vec![];
            for p in app.db.projects() {
                hits.extend(app.db.keys(&p.id).into_iter().filter(|k| k.hash.starts_with(prefix)));
            }
            let [k] = hits.as_slice() else { return Err(format!("{} keys match {prefix}.", hits.len())) };
            cli.call("DELETE", &format!("keys/{}", k.hash), None, false).await?;
            println!("Revoked \"{}\". Whoever used it is disconnected.", k.label);
        }
        ("git", "set" | "off" | "sync") => {
            let p = cli.project(arg(args, 2, "the project")?).await?;
            let id = p["project"]["id"].as_str().unwrap_or("").to_string();
            match sub {
                "set" => {
                    let public = opt(args, "--allow-public");
                    if public.is_some() {
                        eprintln!(
                            "Warning: a public repository lets anyone on the internet read this project, including unreleased story and art."
                        );
                    }
                    cli.post(
                        &format!("projects/{id}/git"),
                        json!({ "url": arg(args, 3, "the repository address")?, "branch": opt(args, "--branch"), "allowPublic": public.is_some(), "confirmName": public }),
                    )
                    .await?;
                    println!("Git sync is on. Test it now with: egd-server git sync {id}");
                }
                "off" => {
                    cli.post(&format!("projects/{id}/git"), json!({ "url": "" })).await?;
                    println!("Git sync is off. The repository stays as it is.");
                }
                _ => {
                    let r = cli.post(&format!("projects/{id}/git/sync"), json!({})).await?;
                    println!("{}", if r["changed"] == true { "Pushed." } else { "Nothing new to push." });
                }
            }
        }
        ("git", _) => {
            let p = cli.project(arg(args, 1, "the project")?).await?;
            let g = &p["git"];
            if raw {
                print(g);
            } else if g["url"].as_str().unwrap_or("").is_empty() {
                println!("Git sync is off. Turn it on with: egd-server git set P <address> [--branch main]");
            } else {
                println!(
                    "Repository: {}  branch {}{}",
                    g["url"].as_str().unwrap_or(""),
                    g["branch"].as_str().unwrap_or(""),
                    if g["allowPublic"] == true { "  (PUBLIC allowed)" } else { "" }
                );
                println!("Last pushed: {}", when(&g["lastSync"]));
                if let Some(e) = g["error"].as_str().filter(|e| !e.is_empty()) {
                    println!("Problem: {e}");
                }
            }
        }
        ("plugins", _) => {
            let o = cli.get("overview").await?;
            if raw {
                print(&o["plugins"]);
            }
            for p in o["plugins"].as_array().into_iter().flatten().filter(|_| !raw) {
                println!(
                    "{}  {} {}",
                    p["id"].as_str().unwrap_or(""),
                    p["name"].as_str().unwrap_or(""),
                    p["version"].as_str().unwrap_or("")
                );
            }
        }
        ("plugin", "install") => {
            let file = arg(args, 2, "the plugin zip")?;
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let (r, _) = cli.call("POST", "plugins", Some(bytes), false).await?;
            println!(
                "Installed {} {}. Apps are offered it and ask before installing.",
                r["name"].as_str().unwrap_or(""),
                r["version"].as_str().unwrap_or("")
            );
        }
        ("plugin", "remove") => {
            cli.call("DELETE", &format!("plugins/{}", arg(args, 2, "the plugin id")?), None, false).await?;
            println!("Removed.");
        }
        ("backup", _) => {
            let r = cli.post("backup-now", json!({})).await?;
            println!("Saved to {}", r["path"].as_str().unwrap_or(""));
        }
        ("log", _) => {
            let rows = cli.get("audit").await?;
            if raw {
                print(&rows);
            }
            for r in rows.as_array().into_iter().flatten().filter(|_| !raw) {
                println!("{}  {}", when(&r["at"]), r["action"].as_str().unwrap_or(""));
            }
        }
        _ => return Err(format!("Unknown command: {}. See `egd-server --help`.", args.join(" "))),
    }
    Ok(())
}
