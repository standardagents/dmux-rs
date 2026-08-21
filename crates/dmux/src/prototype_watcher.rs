//! Long-running local prototype builder. Filesystem observation stays in this
//! external command process while the interactive controller owns reload
//! eligibility and re-exec.

use std::collections::VecDeque;
use std::io;
use std::path::{Component, Path};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};

use crate::{prototype, updater, Cli};

const DEBOUNCE: Duration = Duration::from_millis(750);
const OUTPUT_TAIL: usize = 20;

type WatchEvent = notify::Result<Event>;

pub(crate) fn run(cli: &Cli, raw_worktree: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let worktree = updater::prototype_worktree(raw_worktree).map_err(io::Error::other)?;
    let (_, _, session_name) = crate::startup::resolve_session(cli)?;
    prototype::ensure_controller(cli, &session_name)?;
    let stopped = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stopped))?;
    let (tx, rx) = std::sync::mpsc::channel::<WatchEvent>();
    let mut watcher = notify::recommended_watcher(tx)
        .map_err(|error| io::Error::other(format!("could not start prototype watcher: {error}")))?;
    watcher
        .watch(&worktree, RecursiveMode::Recursive)
        .map_err(|error| {
            io::Error::other(format!("could not watch prototype worktree: {error}"))
        })?;

    let mut generations = Generations::default();
    loop {
        let generation = generations.begin_build();
        eprintln!("prototype watch: building generation {generation}");
        let mut tail = VecDeque::with_capacity(OUTPUT_TAIL);
        let result = updater::build_prototype_with_output(&worktree, |line| {
            remember_output(&mut tail, line);
        });
        if stopped.load(Ordering::Relaxed) {
            eprintln!("prototype watch: stopped");
            return Ok(());
        }

        let changed_at = drain_source_changes(&rx, &worktree);
        if let Err(error) = &result {
            report_build_failure(generation, error, &tail);
        }
        if let Some(last_change) = changed_at {
            generations.source_batch();
            if result.is_ok() {
                eprintln!(
                    "prototype watch: discarded stale generation {generation}; source changed during the build"
                );
            }
            if !wait_until_quiet(&rx, &worktree, last_change, &stopped)? {
                return Ok(());
            }
            continue;
        }

        if let Ok(executable) = result {
            match prototype::handoff(cli, &session_name, &executable) {
                Ok(()) => println!(
                    "prototype watch: requested generation {generation} for '{session_name}'"
                ),
                Err(error) => eprintln!(
                    "prototype watch: handoff failed for generation {generation}: {error}"
                ),
            }
        }

        let Some(first_change) = wait_for_source_change(&rx, &worktree, &stopped)? else {
            return Ok(());
        };
        if !wait_until_quiet(&rx, &worktree, first_change, &stopped)? {
            return Ok(());
        }
        generations.source_batch();
    }
}

fn remember_output(tail: &mut VecDeque<String>, line: String) {
    if tail.len() == OUTPUT_TAIL {
        tail.pop_front();
    }
    tail.push_back(line);
}

#[derive(Default)]
struct Generations {
    requested: u64,
}

impl Generations {
    fn begin_build(&mut self) -> u64 {
        if self.requested == 0 {
            self.requested = 1;
        }
        self.requested
    }

    fn source_batch(&mut self) {
        self.requested = self.requested.saturating_add(1);
    }
}

fn report_build_failure(generation: u64, error: &str, tail: &VecDeque<String>) {
    eprintln!("prototype watch: build failed for generation {generation}: {error}");
    if !tail.is_empty() {
        eprintln!("prototype watch: Cargo output tail:");
        for line in tail {
            eprintln!("  {line}");
        }
    }
}

fn drain_source_changes(rx: &Receiver<WatchEvent>, worktree: &Path) -> Option<Instant> {
    let mut changed_at = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            Ok(event) if relevant_event(&event, worktree) => changed_at = Some(Instant::now()),
            Ok(_) => {}
            Err(error) => eprintln!("prototype watch: filesystem watcher error: {error}"),
        }
    }
    changed_at
}

fn wait_for_source_change(
    rx: &Receiver<WatchEvent>,
    worktree: &Path,
    stopped: &AtomicBool,
) -> Result<Option<Instant>, io::Error> {
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(event)) if relevant_event(&event, worktree) => return Ok(Some(Instant::now())),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("prototype watch: filesystem watcher error: {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("prototype watcher stopped unexpectedly"));
            }
        }
    }
}

fn wait_until_quiet(
    rx: &Receiver<WatchEvent>,
    worktree: &Path,
    mut last_change: Instant,
    stopped: &AtomicBool,
) -> Result<bool, io::Error> {
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let remaining = DEBOUNCE.saturating_sub(last_change.elapsed());
        if remaining.is_zero() {
            return Ok(true);
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(Ok(event)) if relevant_event(&event, worktree) => last_change = Instant::now(),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("prototype watch: filesystem watcher error: {error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("prototype watcher stopped unexpectedly"));
            }
        }
    }
}

fn relevant_event(event: &Event, worktree: &Path) -> bool {
    event.paths.iter().any(|path| relevant_path(path, worktree))
}

fn relevant_path(path: &Path, worktree: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(worktree) else {
        return false;
    };
    if relative.components().any(|component| {
        matches!(component, Component::Normal(name) if name == ".git" || name == ".dmux" || name == "target")
    }) {
        return false;
    }
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    !(name == ".DS_Store"
        || name.starts_with(".#")
        || (name.starts_with('#') && name.ends_with('#'))
        || name.ends_with('~')
        || ["swp", "swo", "tmp", "temp"]
            .iter()
            .any(|suffix| name.ends_with(&format!(".{suffix}"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn event(path: &str) -> WatchEvent {
        Ok(Event::new(notify::EventKind::Any).add_path(path.into()))
    }

    #[test]
    fn watch_requires_a_prototype_worktree() {
        assert!(Cli::try_parse_from(["dmux-rs", "--watch"]).is_err());
        let cli = Cli::try_parse_from(["dmux-rs", "--prototype-worktree", "/worktree", "--watch"])
            .expect("watch CLI");
        assert!(cli.watch);
    }

    #[test]
    fn source_batches_advance_one_generation_each() {
        let mut state = Generations::default();
        assert_eq!(state.begin_build(), 1);
        state.source_batch();
        assert_eq!(state.begin_build(), 2);
        state.source_batch();
        assert_eq!(state.begin_build(), 3);
    }

    #[test]
    fn an_active_builds_event_burst_becomes_one_followup_generation() {
        let root = Path::new("/worktree");
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(event("/worktree/src/main.rs")).unwrap();
        tx.send(event("/worktree/src/lib.rs")).unwrap();
        tx.send(event("/worktree/target/release/dmux-rs")).unwrap();
        let mut state = Generations::default();
        assert_eq!(state.begin_build(), 1);
        assert!(drain_source_changes(&rx, root).is_some());
        state.source_batch();
        assert_eq!(state.begin_build(), 2);
    }

    #[test]
    fn shutdown_interrupts_an_idle_watcher_without_a_build() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let stopped = AtomicBool::new(true);
        assert_eq!(
            wait_for_source_change(&rx, Path::new("/worktree"), &stopped).unwrap(),
            None
        );
    }

    #[test]
    fn cargo_failure_tail_is_bounded_and_keeps_the_latest_lines() {
        let mut tail = VecDeque::new();
        for line in 0..(OUTPUT_TAIL + 5) {
            remember_output(&mut tail, format!("line {line}"));
        }
        assert_eq!(tail.len(), OUTPUT_TAIL);
        assert_eq!(tail.front().map(String::as_str), Some("line 5"));
        assert_eq!(tail.back().map(String::as_str), Some("line 24"));
    }

    #[test]
    fn generated_and_editor_paths_cannot_rebuild() {
        let root = Path::new("/worktree");
        for ignored in [
            "/worktree/.git/index",
            "/worktree/.dmux/state.json",
            "/worktree/target/debug/dmux-rs",
            "/worktree/src/main.rs.swp",
            "/worktree/src/.#main.rs",
            "/worktree/src/main.rs~",
        ] {
            assert!(!relevant_path(Path::new(ignored), root), "{ignored}");
        }
        assert!(relevant_path(
            Path::new("/worktree/.cargo/config.toml"),
            root
        ));
        assert!(relevant_path(Path::new("/worktree/src/main.rs"), root));
    }

    #[test]
    fn unrelated_paths_are_ignored() {
        assert!(!relevant_path(
            Path::new("/another-worktree/src/main.rs"),
            Path::new("/worktree")
        ));
    }
}
