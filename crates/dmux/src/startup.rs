//! Startup resolution shared by the interactive renderer and command modes.

use std::path::PathBuf;

use dmux_core::{session_name_for_root, DmuxConfig};

pub(crate) fn init_logging(cli: &crate::Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path = cli.log_file.clone().unwrap_or_else(|| {
        let dir = crate::dirs_home().join(".dmux").join("logs");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("dmux-rs.log")
    });
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(file)
        .with_ansi(false)
        .init();
    Ok(())
}

/// Resolve (config, project root, session name). Precedence for the root:
/// an existing `.dmux/dmux.config.json` found by walking up (its
/// `projectRoot` is authoritative — matches TS dmux), else the main git
/// worktree root, else the starting directory itself.
pub(crate) fn resolve_session(
    cli: &crate::Cli,
) -> Result<(Option<DmuxConfig>, PathBuf, String), Box<dyn std::error::Error>> {
    let start = cli
        .project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut dir = start.as_path();
    let config = loop {
        let candidate = DmuxConfig::default_path(dir);
        if candidate.exists() {
            break Some(DmuxConfig::load(&candidate)?);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break None,
        }
    };
    let root = match &config {
        Some(cfg) => PathBuf::from(&cfg.project_root),
        None => crate::git::git_main_worktree_root(&start).unwrap_or(start),
    };
    let session = cli
        .session
        .clone()
        .unwrap_or_else(|| session_name_for_root(&root.to_string_lossy()));
    Ok((config, root, session))
}
