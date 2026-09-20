use base64::Engine;

fn build_service(app_name: &str) -> String {
    format!("{app_name}-vault")
}

pub fn load_from_keyring(app_name: &str, entry_key: &str) -> Option<[u8; 32]> {
    let service = build_service(app_name);
    let entry = keyring::Entry::new(&service, entry_key).ok()?;
    let secret = entry.get_password().ok()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret.as_bytes())
        .ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn store_in_keyring(app_name: &str, entry_key: &str, dek: &[u8; 32]) {
    let service = build_service(app_name);
    if let Ok(entry) = keyring::Entry::new(&service, entry_key) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(dek);
        let _ = entry.set_password(&encoded);
    }
}

pub fn clear_keyring(app_name: &str, entry_key: &str) {
    let service = build_service(app_name);
    if let Ok(entry) = keyring::Entry::new(&service, entry_key) {
        let _ = entry.delete_credential();
    }
}
