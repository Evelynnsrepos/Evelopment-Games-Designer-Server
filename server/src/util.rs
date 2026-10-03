use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// `n` bytes from the operating system's random source, base64url encoded.
pub fn random_token(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("operating system random source");
    b64(&buf)
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The device id a key's holder uses in the protocol: stable per key, reveals nothing about it.
pub fn device_id_for_key(key: &str) -> String {
    format!("k-{}", &sha256_hex(key.as_bytes())[..32])
}

/// Constant-time comparison for secrets.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Asset paths the app uses: `assets/<images|audio>/<uuid>.<ext>` (same rule as the app's ASSET_PATH).
pub fn is_asset_path(path: &str) -> bool {
    asset_path_at(path, 0) == Some(path.len())
}

/// Every asset path inside a string (the app stores them as whole strings, rich text inside attributes).
pub fn find_asset_paths(text: &str, out: &mut std::collections::HashSet<String>) {
    let mut from = 0;
    while let Some(i) = text[from..].find("assets/") {
        let start = from + i;
        if let Some(end) = asset_path_at(text, start) {
            out.insert(text[start..end].to_string());
            from = end;
        } else {
            from = start + 7;
        }
    }
}

/// If an asset path starts at `start`, where it ends.
fn asset_path_at(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let after = rest.strip_prefix("assets/images/").or_else(|| rest.strip_prefix("assets/audio/"))?;
    let b = after.as_bytes();
    if b.len() < 38 || !b[..36].iter().all(|c| c.is_ascii_hexdigit() || *c == b'-') || b[36] != b'.' {
        return None;
    }
    let ext = b[37..].iter().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).count();
    if !(1..=5).contains(&ext) {
        return None;
    }
    // Don't accept a longer extension cut short (the app's regex is anchored).
    if b.get(37 + ext).is_some_and(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(start + (rest.len() - after.len()) + 37 + ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_paths() {
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        assert!(is_asset_path(&format!("assets/images/{id}.png")));
        assert!(is_asset_path(&format!("assets/audio/{id}.mp3")));
        assert!(!is_asset_path(&format!("assets/images/{id}.pngxyz")));
        assert!(!is_asset_path(&format!("assets/other/{id}.png")));
        assert!(!is_asset_path(&format!("assets/images/../{id}.png")));
        assert!(!is_asset_path(&format!("assets/images/{id}.png/../../x")));
        let mut found = std::collections::HashSet::new();
        find_asset_paths(&format!(r#"<img src="assets/images/{id}.webp"> and assets/audio/{id}.ogg."#), &mut found);
        assert_eq!(found.len(), 2);
        assert!(found.contains(&format!("assets/images/{id}.webp")));
    }

    #[test]
    fn device_ids_are_stable() {
        assert_eq!(device_id_for_key("egd-key-a"), device_id_for_key("egd-key-a"));
        assert_ne!(device_id_for_key("egd-key-a"), device_id_for_key("egd-key-b"));
    }
}
