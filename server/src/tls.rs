//! How the sync port is reachable. Three modes (plus plain for testing):
//! - `self-signed`: PC or home network. The server makes its own certificate and
//!   puts its fingerprint into connect codes, so the app trusts exactly this server.
//! - `proxy`: plain WebSocket on 127.0.0.1 only; nginx (or similar) in front does TLS.
//! - `files`: a certificate you already have (e.g. from certbot).
//! - `acme`: public server, certificates from Let's Encrypt (needs port 443 and a domain).

use crate::App;
use crate::util::sha256_hex;
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use futures_util::StreamExt;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub fn mode(app: &App) -> String {
    app.db.get_or("tls_mode", "proxy")
}

/// Default sync address for a mode: plain WebSocket never leaves this computer.
pub fn default_addr(mode: &str) -> &'static str {
    match mode {
        "proxy" => "127.0.0.1:8474",
        "acme" => "0.0.0.0:443",
        _ => "0.0.0.0:8474",
    }
}

/// TLS 1.3 only, HTTP/1.1 (WebSocket upgrade), per BSI TR-02102-2.
fn server_config(certs: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut config = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn load_pem(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs: Vec<_> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
    server_config(certs, key)
}

/// The self-signed certificate (made on first use) and its SHA-256 fingerprint.
pub fn self_signed(app: &App) -> Result<(Arc<rustls::ServerConfig>, String), String> {
    let dir = app.data_dir.join("tls");
    let (cert_path, key_path) = (dir.join("self-signed.crt"), dir.join("self-signed.key"));
    if !cert_path.exists() || !key_path.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut names = vec!["localhost".to_string()];
        if let Some(host) = public_host(app) {
            names.push(host);
        }
        let made = rcgen::generate_simple_self_signed(names).map_err(|e| e.to_string())?;
        std::fs::write(&cert_path, made.cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_path, made.signing_key.serialize_pem()).map_err(|e| e.to_string())?;
    }
    let der = CertificateDer::from_pem_file(&cert_path).map_err(|e| e.to_string())?;
    Ok((load_pem(&cert_path, &key_path)?, sha256_hex(&der)))
}

/// Fingerprint for connect codes (self-signed mode only; real certificates are checked normally).
pub fn fingerprint(app: &App) -> Option<String> {
    (mode(app) == "self-signed").then(|| self_signed(app).ok().map(|(_, f)| f)).flatten()
}

fn public_host(app: &App) -> Option<String> {
    let url = app.db.get("public_url")?;
    let rest = url.split("://").nth(1)?;
    let host = rest.split(['/', ':']).next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// TLS for a remote admin page: the same certificate as the sync port.
pub fn admin_config(app: &App) -> Result<RustlsConfig, String> {
    let config = match mode(app).as_str() {
        "self-signed" => self_signed(app)?.0,
        "files" => {
            let cert = app.db.get("cert_path").ok_or("No certificate file is set.")?;
            let key = app.db.get("key_path").ok_or("No key file is set.")?;
            load_pem(Path::new(&cert), Path::new(&key))?
        }
        _ => return Err("Remote admin needs the self-signed or certificate-file mode.".into()),
    };
    Ok(RustlsConfig::from_config(config))
}

pub async fn serve(app: Arc<App>, router: Router, addr: SocketAddr) -> Result<(), String> {
    let make = router.into_make_service_with_connect_info::<SocketAddr>();
    match mode(&app).as_str() {
        "proxy" | "plain" => {
            let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("{addr}: {e}"))?;
            axum::serve(listener, make).await.map_err(|e| e.to_string())
        }
        "self-signed" => {
            let (config, _) = self_signed(&app)?;
            axum_server::bind_rustls(addr, RustlsConfig::from_config(config)).serve(make).await.map_err(|e| e.to_string())
        }
        "files" => {
            let cert = app.db.get("cert_path").ok_or("No certificate file is set.")?;
            let key = app.db.get("key_path").ok_or("No key file is set.")?;
            let config = load_pem(Path::new(&cert), Path::new(&key))?;
            axum_server::bind_rustls(addr, RustlsConfig::from_config(config)).serve(make).await.map_err(|e| e.to_string())
        }
        "acme" => {
            let domain = public_host(&app).ok_or("Set the public address (your domain) first.")?;
            let email = app.db.get_or("acme_email", "");
            let mut state = rustls_acme::AcmeConfig::new([domain])
                .contact(if email.is_empty() { vec![] } else { vec![format!("mailto:{email}")] })
                .cache(rustls_acme::caches::DirCache::new(app.data_dir.join("tls").join("acme")))
                .directory_lets_encrypt(true)
                .state();
            let acceptor = state.axum_acceptor(state.default_rustls_config());
            tokio::spawn(async move {
                while let Some(event) = state.next().await {
                    if let Err(e) = event {
                        eprintln!("Let's Encrypt: {e}");
                    }
                }
            });
            axum_server::bind(addr).acceptor(acceptor).serve(make).await.map_err(|e| e.to_string())
        }
        other => Err(format!("Unknown TLS mode {other}")),
    }
}
