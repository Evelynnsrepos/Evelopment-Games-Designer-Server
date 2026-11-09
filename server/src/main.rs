// No console window when started by double-click on Windows in the tray build.
#![cfg_attr(all(windows, feature = "tray", not(debug_assertions)), windows_subsystem = "windows")]

mod admin;
mod autostart;
mod backup;
mod cli;
mod db;
mod gitsync;
mod plugins;
mod sync;
mod tls;
#[cfg(feature = "tray")]
mod tray;
mod util;

use axum::Router;
use axum::routing::get;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct App {
    pub db: db::Db,
    pub hub: sync::Hub,
    pub sessions: admin::Sessions,
    pub data_dir: PathBuf,
    /// The server's device id in the protocol.
    pub server_id: String,
    pub sync_addr: SocketAddr,
    pub admin_addr: SocketAddr,
    /// The admin page is served over TLS (remote admin), so its cookie is Secure.
    pub admin_tls: bool,
    /// A setting changed that only takes effect after a restart.
    pub restart_needed: Mutex<bool>,
}

impl App {
    pub fn project_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join("projects").join(id)
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.db.get("backup_dir").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| self.data_dir.join("backups"))
    }
    pub fn admin_url(&self) -> String {
        let host = if self.admin_addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { self.admin_addr.ip().to_string() };
        format!("{}://{host}:{}/admin", if self.admin_tls { "https" } else { "http" }, self.admin_addr.port())
    }
}

const HELP: &str = "Evelopment Games Designer Server

Usage: egd-server [options]

  --data <folder>   Where projects and settings are kept (or EGD_DATA)
  --no-browser      Don't open the admin page on start
  --headless        No tray icon, even in the tray build
  --no-admin-page   Don't serve the admin page at all (manage it with the commands below)
  --version         Print the version

  autostart on      Start the server when you log in (Linux: on the first connection)
  autostart off     Stop doing that

Environment: EGD_SYNC_ADDR, EGD_ADMIN_ADDR override the listen addresses
(for Docker: EGD_ADMIN_ADDR=0.0.0.0:8475 and publish the port on 127.0.0.1 only).

Open the admin page (default http://127.0.0.1:8475/admin) to set it up,
or set it up and run it entirely from the command line (see below).";

fn default_data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("egd-server")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}\n\n{}", cli::HELP);
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("egd-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let data_dir = args
        .iter()
        .position(|a| a == "--data")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("EGD_DATA").map(PathBuf::from))
        .unwrap_or_else(default_data_dir);
    // The command, if any: what's left without the server's own options.
    let command: Vec<String> = {
        let mut skip_next = false;
        args.iter()
            .filter(|a| {
                let keep = !skip_next && !["--data", "--no-browser", "--headless", "--no-admin-page"].contains(&a.as_str());
                skip_next = !skip_next && a.as_str() == "--data";
                keep
            })
            .cloned()
            .collect()
    };
    if let Some(c) = command.first().filter(|c| !c.starts_with("--") && c.as_str() != "autostart" && !cli::COMMANDS.contains(&c.as_str())) {
        fail(&format!("Unknown command: {c}. See `egd-server --help`."));
    }
    let open_browser = !args.iter().any(|a| a == "--no-browser") && std::env::var_os("EGD_DATA").is_none() && command.is_empty();
    let headless = args.iter().any(|a| a == "--headless");
    let admin_page = !args.iter().any(|a| a == "--no-admin-page");

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        fail(&format!("Can't create the data folder {}: {e}", data_dir.display()));
    }
    let db = db::Db::open(&data_dir.join("egd-server.db")).unwrap_or_else(|e| fail(&format!("Can't open the database: {e}")));
    let server_id = db.get("server_id").unwrap_or_else(|| {
        let id = format!("server-{}", util::random_token(16));
        db.set("server_id", &id);
        id
    });
    let mode = db.get_or("tls_mode", "proxy");
    let sync_addr = addr("EGD_SYNC_ADDR", &db.get_or("sync_addr", tls::default_addr(&mode)));
    let admin_env = std::env::var("EGD_ADMIN_ADDR").ok();
    let admin_addr = addr("EGD_ADMIN_ADDR", "127.0.0.1:8475");
    // Remote admin: only over TLS, unless the operator set the address explicitly (Docker, published on 127.0.0.1).
    let admin_tls = !admin_addr.ip().is_loopback() && admin_env.is_none();
    if admin_tls && !["self-signed", "files"].contains(&mode.as_str()) {
        fail("The admin page may only be reached from other computers over TLS. Use the self-signed or certificate-file mode.");
    }

    if command.first().map(String::as_str) == Some("autostart") {
        let r = match command.get(1).map(String::as_str) {
            Some("on") => autostart::turn_on(&data_dir, &sync_addr.to_string(), &admin_addr.to_string()),
            Some("off") => autostart::turn_off(),
            _ => Ok(format!("Autostart is {}. Use `egd-server autostart on` or `off`.", if autostart::is_on() { "on" } else { "off" })),
        };
        match r {
            Ok(text) => println!("{text}"),
            Err(e) => fail(&e),
        }
        return;
    }

    let app = Arc::new(App {
        db,
        hub: sync::Hub::default(),
        sessions: admin::Sessions::default(),
        data_dir,
        server_id,
        sync_addr,
        admin_addr,
        admin_tls,
        restart_needed: Mutex::new(false),
    });
    if !command.is_empty() {
        if let Err(e) = cli::run(app, &command) {
            fail(&e);
        }
        return;
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    #[cfg(feature = "tray")]
    if !headless {
        let a = app.clone();
        std::thread::spawn(move || rt.block_on(run(a, open_browser && admin_page, admin_page)));
        tray::run(app);
        return;
    }
    let _ = headless;
    rt.block_on(run(app, open_browser && admin_page, admin_page));
}

fn addr(env: &str, default: &str) -> SocketAddr {
    let text = std::env::var(env).unwrap_or_else(|_| default.to_string());
    text.parse().unwrap_or_else(|_| fail(&format!("{env}: {text} is not an address like 127.0.0.1:8474")))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

async fn run(app: Arc<App>, open_browser: bool, admin_page: bool) {
    let sync_router = Router::new()
        .route("/", get(|| async { "Evelopment Games Designer server. Connect from the app with a connect code.\n" }))
        .route("/health", get(|| async { "ok" }))
        .route("/sync", get(sync::ws_handler))
        .with_state(app.clone());

    if admin_page {
        let admin_listener = tls::listener(app.admin_addr, 1)
            .and_then(|l| tokio::net::TcpListener::from_std(l).map_err(|e| e.to_string()))
            .unwrap_or_else(|e| fail(&format!("Admin page {}: {e} (is the server already running?)", app.admin_addr)));
        let admin = admin::router(app.clone());
        let admin_app = app.clone();
        tokio::spawn(async move {
            let make = admin.into_make_service_with_connect_info::<SocketAddr>();
            if admin_app.admin_tls {
                let config = match tls::admin_config(&admin_app) {
                    Ok(c) => c,
                    Err(e) => fail(&format!("Admin page TLS: {e}")),
                };
                let std_listener = admin_listener.into_std().expect("listener");
                let _ = axum_server::from_tcp_rustls(std_listener, config).expect("admin listener").serve(make).await;
            } else {
                let _ = axum::serve(admin_listener, make).await;
            }
        });
    }

    let sync_app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = tls::serve(sync_app.clone(), sync_router, sync_app.sync_addr).await {
            eprintln!("Sync port {}: {e}", sync_app.sync_addr);
            *sync_app.restart_needed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        }
    });

    admin::forget_sessions_if_password_changed(&app);
    let ticker = app.clone();
    tokio::spawn(async move {
        let mut last_hourly = 0;
        let mut last_busy = util::now();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // Stop when nobody was connected for a while (Settings); systemd starts it again on the next connection.
            let idle_min: i64 = ticker.db.get_or("idle_stop_min", "0").parse().unwrap_or(0);
            if !ticker.hub.connected().is_empty() {
                last_busy = util::now();
            } else if idle_min > 0 && util::now() - last_busy >= idle_min * 60 {
                println!("Nobody connected for {idle_min} min, stopping.");
                STOP.notify_one();
            }
            let a = ticker.clone();
            let _ = tokio::task::spawn_blocking(move || {
                a.hub.reconcile(&a);
                a.hub.tick(&a);
                admin::forget_sessions_if_password_changed(&a);
                let t = util::now();
                if t - last_hourly >= 3600 {
                    hourly(&a);
                    return t;
                }
                last_hourly
            })
            .await
            .map(|t| last_hourly = t);
        }
    });

    let git = app.clone();
    tokio::spawn(async move {
        loop {
            let minutes: u64 = git.db.get_or("git_interval_min", "10").parse().unwrap_or(10);
            tokio::time::sleep(Duration::from_secs(minutes.max(1) * 60)).await;
            let a = git.clone();
            let _ = tokio::task::spawn_blocking(move || gitsync::sync_all(&a)).await;
        }
    });

    println!("Evelopment Games Designer Server {}", env!("CARGO_PKG_VERSION"));
    println!("Data folder: {}", app.data_dir.display());
    println!("Admin page:  {}", if admin_page { app.admin_url() } else { "off (--no-admin-page)".into() });
    println!("Apps connect to: {} ({})", admin::sync_url(&app), tls::mode(&app));
    if open_browser && app.db.get("admin_hash").is_none() {
        open_url(&app.admin_url());
    }

    shutdown_signal().await;
    println!("Saving and stopping…");
    stop(&app);
}

/// Idle stop, from the ticker.
static STOP: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Save every project and push the last changes to git, then exit (also from the tray).
pub fn stop(app: &App) -> ! {
    app.hub.save_all(app);
    gitsync::sync_all(app);
    std::process::exit(0)
}

/// Ctrl+C, or SIGTERM (Docker, systemd).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = STOP.notified() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = STOP.notified() => {}
    }
}

/// Expired keys, optional auto-delete of unused projects, daily backups.
fn hourly(app: &App) {
    for hash in app.db.delete_expired_keys() {
        app.hub.kick_key(&hash);
    }
    let days: i64 = app.db.get_or("delete_unused_days", "0").parse().unwrap_or(0);
    if days > 0 {
        for p in app.db.projects() {
            if p.last_used < util::now() - days * 86400 && !app.hub.is_open(&p.id) {
                app.hub.close_project(app, &p.id);
                app.db.delete_project(&p.id);
                let _ = std::fs::remove_dir_all(app.project_dir(&p.id));
                app.db.audit(&format!("Deleted project \"{}\" after {days} days unused", p.name));
            }
        }
    }
    if app.db.flag("daily_backups") {
        let last: i64 = app.db.get_or("last_backup", "0").parse().unwrap_or(0);
        if util::now() - last >= 86400 - 3600 {
            match backup::server_backup(app) {
                Ok(_) => app.db.set("last_backup", &util::now().to_string()),
                Err(e) => eprintln!("Daily backup failed: {e}"),
            }
        }
    }
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = r;
}
