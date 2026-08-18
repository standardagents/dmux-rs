//! Native notifications through the dmux macOS helper daemon (progressive
//! enhancement: if the helper socket isn't there — non-macOS, or the helper
//! was never installed — this is a silent no-op). Protocol matches
//! `DmuxFocusService.ts`: one JSON line per connection.

use std::io::Write;
use std::path::PathBuf;

fn helper_socket() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let path = std::env::var_os("HOME").map(PathBuf::from)?
        .join(".dmux")
        .join("native-helper")
        .join("run")
        .join("dmux-helper.sock");
    path.exists().then_some(path)
}

/// True when the helper is available (used to decide whether toasts should
/// also go native).
pub fn available() -> bool {
    helper_socket().is_some()
}

/// Send a native notification. Blocking but bounded; call from spawn_blocking.
pub fn notify(title: &str, body: &str) -> bool {
    let Some(path) = helper_socket() else { return false };
    let payload = serde_json::json!({
        "type": "notify",
        "title": title,
        "body": body,
        "titleToken": "",
        "bundleId": serde_json::Value::Null,
    });
    match std::os::unix::net::UnixStream::connect(&path) {
        Ok(mut stream) => {
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(1)));
            let mut line = payload.to_string();
            line.push('\n');
            stream.write_all(line.as_bytes()).is_ok()
        }
        Err(err) => {
            tracing::debug!(%err, "helper socket connect failed");
            false
        }
    }
}
