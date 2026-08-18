/// Compute the project-scoped tmux session name, byte-identical to the TS
/// implementation (`src/index.ts buildSessionNameForRoot`):
/// `dmux-<basename>-<md5(projectRoot)[0:8]>` with `[^A-Za-z0-9_-]+` runs
/// collapsed to `-`.
pub fn session_name_for_root(project_root: &str) -> String {
    let base = std::path::Path::new(project_root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| project_root.to_string());
    let digest = md5::compute(project_root.as_bytes());
    let hash = format!("{digest:x}");
    let identifier = format!("{}-{}", base, &hash[..8]);

    let mut sanitized = String::with_capacity(identifier.len());
    let mut last_dash = false;
    for ch in identifier.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            sanitized.push(ch);
            last_dash = ch == '-';
        } else if !last_dash {
            sanitized.push('-');
            last_dash = true;
        }
    }
    format!("dmux-{sanitized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ts_shape() {
        let name = session_name_for_root("/Users/justin/Projects/dmux");
        assert!(name.starts_with("dmux-dmux-"));
        // md5 of the exact path, first 8 hex chars.
        let digest = format!("{:x}", md5::compute("/Users/justin/Projects/dmux"));
        assert!(name.ends_with(&digest[..8]));
    }

    #[test]
    fn sanitizes_special_chars() {
        let name = session_name_for_root("/tmp/my project (v2)");
        assert!(!name.contains(' '));
        assert!(!name.contains('('));
    }
}
