//! dmux-rs: Phase 0 prototype — a tmux control-mode renderer for dmux
//! sessions. Attaches to an existing dmux tmux session as a `-CC`-style
//! client, runs a terminal emulator per pane, and composites panes + sidebar
//! into the host terminal with damage-diffed, synchronized-output frames.

mod input;
mod layout;
mod metrics;
mod render;
mod session;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser as ClapParser;
use dmux_cc::{CcEvent, Client, PaneId, Reply, ReplyRouter, Routed as CcRouted};
use dmux_compositor::{diff_frame, CellBuffer, Emitter};
use dmux_core::{session_name_for_root, DmuxConfig};
use dmux_host::{HostTerminal, InputEvent};
use input::Routed;
use session::{LogicalPane, PaneStatus};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SETTLE_AFTER: Duration = Duration::from_millis(1500);
const HUD_REFRESH: Duration = Duration::from_millis(500);
/// Flood throttling: a pane producing more than this many bytes inside one
/// rate window gets its output turned off at the source and refreshes by
/// reseed until it calms down. Keeps typing latency flat while `yes` runs.
const FLOOD_WINDOW: Duration = Duration::from_millis(250);
const FLOOD_BYTES_PER_WINDOW: u64 = 1_000_000; // ≈4 MB/s sustained
const FLOOD_RESEED_EVERY: Duration = Duration::from_millis(500);

#[derive(ClapParser, Debug)]
#[command(name = "dmux-rs", about = "dmux control-mode renderer prototype")]
struct Cli {
    /// tmux session to attach (default: derived from the project config).
    #[arg(long)]
    session: Option<String>,
    /// Project root containing .dmux/dmux.config.json (default: cwd).
    #[arg(long)]
    project: Option<PathBuf>,
    /// tmux binary.
    #[arg(long, default_value = "tmux")]
    tmux: String,
    /// tmux socket name (tmux -L), mainly for tests.
    #[arg(short = 'L', long)]
    socket: Option<String>,
    /// Start with the perf HUD visible.
    #[arg(long)]
    hud: bool,
    /// Log file (default: ~/.dmux/logs/dmux-rs.log).
    #[arg(long)]
    log_file: Option<PathBuf>,
}

/// Reply tags: every command whose reply matters is matched here, in stream
/// order, by the main loop.
#[derive(Debug)]
enum Tag {
    ListPanes,
    Seed(PaneId),
    Cursor(PaneId),
    ControllerPid,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    init_logging(&cli)?;

    if std::env::var_os("TMUX").is_some() {
        eprintln!("dmux-rs must run OUTSIDE tmux (it renders tmux panes itself).");
        eprintln!("Run it from a plain terminal window.");
        std::process::exit(2);
    }

    let (config, project_root, session_name) = resolve_session(&cli)?;
    // Attach if the project's session exists; otherwise create it there.
    // Session names are a pure function of the project root, so running from
    // the same location always finds its way back to the same session.
    let tmux_base = |cli: &Cli| {
        let mut cmd = std::process::Command::new(&cli.tmux);
        if let Some(socket) = &cli.socket {
            cmd.args(["-L", socket]);
        }
        cmd
    };
    let exists = tmux_base(&cli)
        .args(["has-session", "-t", &session_name])
        .stderr(std::process::Stdio::null())
        .status()?
        .success();
    if !exists {
        let (cols, rows) = dmux_host::term_size();
        let status = tmux_base(&cli)
            .args([
                "new-session",
                "-d",
                "-s",
                &session_name,
                "-c",
                &project_root.to_string_lossy(),
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .status()?;
        if !status.success() {
            eprintln!("failed to create tmux session '{session_name}' in {}", project_root.display());
            std::process::exit(1);
        }
        // A stable dmux-style title so the pane adopts with a sane name.
        let _ = tmux_base(&cli)
            .args(["select-pane", "-t", &format!("{session_name}:0.0"), "-T", "terminal-1"])
            .status();
        eprintln!(
            "created session '{session_name}' for {}",
            project_root.display()
        );
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run(cli, config, session_name))
}

fn init_logging(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path = cli.log_file.clone().unwrap_or_else(|| {
        let dir = dirs_home().join(".dmux").join("logs");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("dmux-rs.log")
    });
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
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

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Resolve (config, project root, session name). Precedence for the root:
/// an existing `.dmux/dmux.config.json` found by walking up (its
/// `projectRoot` is authoritative — matches TS dmux), else the main git
/// worktree root, else the starting directory itself.
fn resolve_session(
    cli: &Cli,
) -> Result<(Option<DmuxConfig>, PathBuf, String), Box<dyn std::error::Error>> {
    let start = cli
        .project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Walk up to the first directory holding .dmux/dmux.config.json.
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
        None => git_main_worktree_root(&start).unwrap_or(start),
    };
    let session = cli
        .session
        .clone()
        .unwrap_or_else(|| session_name_for_root(&root.to_string_lossy()));
    Ok((config, root, session))
}

/// The main git worktree root for `dir` (first entry of `git worktree list`),
/// so every worktree of a repo maps to one session — same rule as TS dmux.
fn git_main_worktree_root(dir: &std::path::Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
}

struct App {
    client: Client<Tag>,
    router: ReplyRouter<Tag>,
    host: HostTerminal,
    panes: Vec<LogicalPane>,
    config: Option<DmuxConfig>,
    session_name: String,
    focused: usize,
    selected: usize,
    front: CellBuffer,
    back: CellBuffer,
    emitter: Emitter,
    metrics: metrics::Metrics,
    hud: bool,
    size: (u16, u16),
    layout: layout::Layout,
    dirty: bool,
    force_full: bool,
    last_frame: Instant,
    reconcile_in_flight: bool,
    reconcile_again: bool,
    status_msg: String,
    /// True when no live TS dmux controller owns the session: we size each
    /// tmux window to its compositor rect (`window-size manual`). When a TS
    /// controller is alive we observe without touching topology.
    own_sizing: bool,
    sized_windows: std::collections::HashSet<dmux_cc::WindowId>,
}

async fn run(cli: Cli, config: Option<DmuxConfig>, session_name: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = Vec::new();
    if let Some(socket) = &cli.socket {
        args.extend(["-L".into(), socket.clone()]);
    }
    args.extend(["-C".into(), "attach-session".into(), "-t".into(), session_name.clone()]);
    let (client, mut events, router, mut child) = Client::<Tag>::spawn(&cli.tmux, &args)?;

    // Terminal takeover happens only after tmux spawned successfully.
    let host = HostTerminal::setup()?;
    let size = host.size();
    let mut input_rx = dmux_host::spawn_input_reader();
    let mut resize_rx = dmux_host::spawn_resize_watcher();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // SIGHUP arrives when our own terminal/pane closes; the tmux -C child
    // MUST die with us or its unread pipe stalls the whole tmux server.
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    let caps = host.caps();
    tracing::info!(?size, sync_output = caps.synchronized_output, session = %session_name, "attached");

    // Client flags FIRST: ignore-size keeps our attachment from resizing
    // windows sized by other clients (a live TS dmux session), and
    // pause-after arms server-side flow control. Errors non-fatal.
    let _ = client.send("refresh-client -f ignore-size,pause-after=1,wait-exit");
    let _ = client.send(format!("refresh-client -C {}x{}", size.0, size.1));
    // Coexistence check: a live TS dmux controller means observe-only mode.
    client.send_tagged(
        format!("show-options -t {} -qv @dmux_controller_pid", dmux_cc::quote_arg(&session_name)),
        Tag::ControllerPid,
    )?;
    // Initial pane discovery.
    client.send_tagged(session::list_panes_command(), Tag::ListPanes)?;

    let mut app = App {
        client,
        router,
        host,
        panes: Vec::new(),
        config,
        session_name,
        focused: 0,
        selected: 0,
        front: CellBuffer::new(size.0, size.1),
        back: CellBuffer::new(size.0, size.1),
        emitter: Emitter::new(),
        metrics: metrics::Metrics::new(),
        hud: cli.hud,
        size,
        layout: layout::Layout::default(),
        dirty: true,
        force_full: true,
        last_frame: Instant::now() - FRAME_INTERVAL,
        reconcile_in_flight: true,
        reconcile_again: false,
        status_msg: "dmux-rs · ^Q quit · ^Y hud · ⌥←→ focus".into(),
        own_sizing: false,
        sized_windows: std::collections::HashSet::new(),
    };

    loop {
        let now = Instant::now();
        let render_deadline = app.dirty.then(|| {
            let earliest = app.last_frame + FRAME_INTERVAL;
            tokio::time::Instant::from_std(if earliest > now { earliest } else { now })
        });
        let settle_deadline = app
            .panes
            .iter()
            .filter(|p| p.status == PaneStatus::Working)
            .filter_map(|p| p.last_output)
            .min()
            .map(|t| tokio::time::Instant::from_std(t + SETTLE_AFTER));
        let hud_deadline = app
            .hud
            .then(|| tokio::time::Instant::from_std(now + HUD_REFRESH));
        let resume_deadline = app
            .panes
            .iter()
            .filter_map(|p| p.resume_at)
            .min()
            .map(tokio::time::Instant::from_std);
        let deadline = [render_deadline, settle_deadline, hud_deadline, resume_deadline]
            .into_iter()
            .flatten()
            .min();

        let timer = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            maybe_ev = events.recv() => {
                match maybe_ev {
                    Some(ev) => {
                        if !app.handle_cc(ev) { break; }
                        // Drain a BOUNDED batch: an unbounded drain starves
                        // input and rendering whenever events arrive faster
                        // than we process them (a `yes` firehose).
                        let mut budget = 256;
                        while budget > 0 {
                            match events.try_recv() {
                                Ok(ev) => {
                                    if !app.handle_cc(ev) { return app.shutdown(&mut child).await; }
                                    budget -= 1;
                                }
                                Err(_) => break,
                            }
                        }
                        app.render_if_due();
                    }
                    None => break,
                }
            }
            maybe_input = input_rx.recv() => {
                match maybe_input {
                    Some(ev) => {
                        if !app.handle_input(ev) { break; }
                        app.render_if_due();
                    }
                    None => break,
                }
            }
            Some(new_size) = resize_rx.recv() => {
                app.handle_resize(new_size);
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
            _ = timer => {
                app.handle_deadlines();
            }
        }
    }

    app.shutdown(&mut child).await
}

impl App {
    async fn shutdown(&mut self, child: &mut tokio::process::Child) -> Result<(), Box<dyn std::error::Error>> {
        self.host.restore();
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
        eprintln!("dmux-rs detached. Frames: {}, p95 {:.2} ms.",
            self.metrics.frames,
            self.metrics.frame_total_us.value_at_quantile(0.95) as f64 / 1000.0);
        Ok(())
    }

    /// Handle one control-mode event. Returns false to quit.
    fn handle_cc(&mut self, ev: CcEvent) -> bool {
        match self.router.route(ev) {
            CcRouted::Notification(ev) => self.handle_notification(ev),
            CcRouted::Reply(tag, reply) => {
                self.handle_reply(tag, reply);
                true
            }
            CcRouted::Consumed => true,
            CcRouted::Desync => {
                tracing::error!("protocol desync — exiting (restart to reattach)");
                self.status_msg = "protocol desync — restart dmux-rs".into();
                false
            }
        }
    }

    fn handle_notification(&mut self, ev: CcEvent) -> bool {
        match ev {
            CcEvent::Output { pane, data } | CcEvent::ExtendedOutput { pane, data, .. } => {
                self.metrics.record_input(data.len());
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    // Flood detection: meter the rate window.
                    let now = Instant::now();
                    if now.duration_since(p.window_start) >= FLOOD_WINDOW {
                        p.window_start = now;
                        p.window_bytes = 0;
                    }
                    p.window_bytes += data.len() as u64;

                    if let Some(buffer) = &mut p.reseed_buffer {
                        buffer.push(data);
                    } else {
                        let effects = p.term.advance(&data);
                        for effect in effects {
                            handle_side_effect(&self.client, p, effect);
                        }
                        p.dirty = true;
                        p.last_output = Some(now);
                        if p.status != PaneStatus::Dead {
                            p.status = PaneStatus::Working;
                        }
                        self.dirty = true;
                    }

                    if !p.throttled && p.window_bytes > FLOOD_BYTES_PER_WINDOW {
                        tracing::info!(pane = %pane, "flood detected; throttling output at source");
                        p.throttled = true;
                        p.resume_at = Some(now + FLOOD_RESEED_EVERY);
                        let _ = self.client.send(format!("refresh-client -A '{}:off'", pane));
                        self.dirty = true;
                    }
                }
                true
            }
            CcEvent::Pause(pane) => {
                self.metrics.pauses += 1;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    tracing::info!(pane = %pane, "paused; reseeding");
                    p.paused = true;
                    p.begin_reseed();
                    let _ = self.client.send(format!("refresh-client -A '{}:continue'", pane));
                    let _ = self.client.send_tagged(p.seed_command(), Tag::Seed(pane));
                    let _ = self.client.send_tagged(p.cursor_command(), Tag::Cursor(pane));
                    self.dirty = true;
                }
                true
            }
            CcEvent::Continue(pane) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.paused = false;
                    p.dirty = true;
                    self.dirty = true;
                }
                true
            }
            CcEvent::WindowClose(w) | CcEvent::UnlinkedWindowClose(w) => {
                for p in self.panes.iter_mut().filter(|p| p.tmux_window == w) {
                    p.status = PaneStatus::Dead;
                    p.dirty = true;
                }
                self.request_reconcile();
                true
            }
            CcEvent::WindowAdd(_) | CcEvent::LayoutChange { .. } | CcEvent::WindowPaneChanged { .. } => {
                self.request_reconcile();
                true
            }
            CcEvent::WindowRenamed { window, name } | CcEvent::UnlinkedWindowRenamed { window, name } => {
                // Window name only matters for panes with no explicit title.
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_window == window && p.title.is_empty()) {
                    p.title = name;
                    self.dirty = true;
                }
                true
            }
            CcEvent::Exit(reason) => {
                tracing::info!(?reason, "server closed control connection");
                false
            }
            CcEvent::ClientDetached { .. }
            | CcEvent::SessionChanged { .. }
            | CcEvent::SessionRenamed { .. }
            | CcEvent::SessionsChanged
            | CcEvent::SessionWindowChanged { .. }
            | CcEvent::ClientSessionChanged { .. }
            | CcEvent::PaneModeChanged(_)
            | CcEvent::PasteBufferChanged { .. }
            | CcEvent::PasteBufferDeleted { .. }
            | CcEvent::SubscriptionChanged { .. }
            | CcEvent::Message(_) => true,
            CcEvent::ConfigError(err) => {
                tracing::warn!(%err, "tmux config error");
                true
            }
            CcEvent::Unknown(line) => {
                tracing::debug!(%line, "unknown control-mode line");
                true
            }
            CcEvent::ReplyBegin { .. } | CcEvent::ReplyLine(_) | CcEvent::ReplyEnd { .. } => true,
        }
    }

    fn handle_reply(&mut self, tag: Tag, reply: Reply) {
        match tag {
            Tag::ListPanes => {
                self.reconcile_in_flight = false;
                self.apply_pane_list(&reply);
                if self.reconcile_again {
                    self.reconcile_again = false;
                    self.request_reconcile();
                }
            }
            Tag::Seed(pane_id) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                    // Held until the cursor reply lands (they arrive adjacent,
                    // ordered); stash the seed on the reseed buffer's side.
                    p.pending_seed = Some(reply);
                }
            }
            Tag::Cursor(pane_id) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                    if let Some(seed) = p.pending_seed.take() {
                        let cursor = session::parse_cursor_reply(&reply);
                        p.finish_reseed(&seed, cursor);
                        self.dirty = true;
                    }
                }
            }
            Tag::ControllerPid => {
                let pid = reply
                    .text_lines()
                    .first()
                    .and_then(|l| l.trim().parse::<i32>().ok());
                let controller_alive = pid
                    .map(|pid| unsafe { libc::kill(pid, 0) == 0 })
                    .unwrap_or(false);
                self.own_sizing = !controller_alive;
                tracing::info!(?pid, own_sizing = self.own_sizing, "controller check");
                if self.own_sizing {
                    self.status_msg = "dmux-rs (owner) · ^Q quit · ^Y hud · ⌥←→ focus".into();
                    self.apply_window_sizes();
                } else {
                    self.status_msg = "dmux-rs (observe) · ^Q quit · ^Y hud".into();
                }
            }
        }
    }

    /// In own-sizing mode, make each pane's tmux window exactly its rect size
    /// so nothing clips. tmux answers with %layout-change → reconcile →
    /// reseed at the new size.
    fn apply_window_sizes(&mut self) {
        if !self.own_sizing {
            return;
        }
        // Legacy dmux sessions keep several panes in one window; sizing such a
        // window to one pane's rect would fight the others. Until break-pane
        // migration lands, only size single-pane windows.
        let mut per_window: std::collections::HashMap<dmux_cc::WindowId, u32> =
            std::collections::HashMap::new();
        for p in &self.panes {
            *per_window.entry(p.tmux_window).or_default() += 1;
        }
        for p in &self.panes {
            if per_window.get(&p.tmux_window).copied().unwrap_or(0) != 1 {
                continue;
            }
            let Some(rect) = p.rect else { continue };
            if (p.cols, p.rows) == (rect.w, rect.h) || rect.is_empty() {
                continue;
            }
            if self.sized_windows.insert(p.tmux_window) {
                let _ = self
                    .client
                    .send(format!("set-option -w -t {} window-size manual", p.tmux_window));
            }
            let _ = self.client.send(format!(
                "resize-window -t {} -x {} -y {}",
                p.tmux_window, rect.w, rect.h
            ));
        }
    }

    fn request_reconcile(&mut self) {
        if self.reconcile_in_flight {
            self.reconcile_again = true;
            return;
        }
        self.reconcile_in_flight = true;
        let _ = self.client.send_tagged(session::list_panes_command(), Tag::ListPanes);
    }

    /// Merge a fresh pane listing into the model: new panes adopted + seeded,
    /// size changes reseeded, vanished panes marked dead.
    fn apply_pane_list(&mut self, reply: &Reply) {
        let infos = session::parse_pane_list(reply);
        let adopted = session::adopt_panes(self.config.as_ref(), &infos);

        for mut new_pane in adopted {
            match self.panes.iter_mut().find(|p| p.tmux_pane == new_pane.tmux_pane) {
                Some(existing) => {
                    existing.title = new_pane.title.clone();
                    existing.tmux_window = new_pane.tmux_window;
                    if (existing.cols, existing.rows) != (new_pane.cols, new_pane.rows) {
                        existing.cols = new_pane.cols;
                        existing.rows = new_pane.rows;
                        existing.begin_reseed();
                        let _ = self.client.send_tagged(existing.seed_command(), Tag::Seed(existing.tmux_pane));
                        let _ = self.client.send_tagged(existing.cursor_command(), Tag::Cursor(existing.tmux_pane));
                    }
                }
                None => {
                    new_pane.begin_reseed();
                    let _ = self.client.send_tagged(new_pane.seed_command(), Tag::Seed(new_pane.tmux_pane));
                    let _ = self.client.send_tagged(new_pane.cursor_command(), Tag::Cursor(new_pane.tmux_pane));
                    self.panes.push(new_pane);
                }
            }
        }
        // Mark vanished panes dead (windows may close without events during churn).
        let live: std::collections::HashSet<_> = infos.iter().map(|i| i.pane).collect();
        for p in &mut self.panes {
            if !live.contains(&p.tmux_pane) {
                p.status = PaneStatus::Dead;
            }
        }
        self.relayout();
    }

    fn relayout(&mut self) {
        let visible = self.panes.len();
        self.layout = layout::compute(self.size.0, self.size.1, visible);
        for (i, p) in self.panes.iter_mut().enumerate() {
            p.rect = self.layout.panes.get(i).copied();
            p.dirty = true;
        }
        if self.focused >= self.panes.len() {
            self.focused = self.panes.len().saturating_sub(1);
        }
        self.selected = self.selected.min(self.panes.len().saturating_sub(1));
        self.dirty = true;
        self.force_full = true;
        self.apply_window_sizes();
    }

    fn handle_resize(&mut self, new_size: (u16, u16)) {
        if new_size == self.size {
            return;
        }
        self.size = new_size;
        self.front = CellBuffer::new(new_size.0, new_size.1);
        self.back = CellBuffer::new(new_size.0, new_size.1);
        self.emitter.invalidate();
        self.emitter.clear_screen();
        let _ = self.client.send(format!("refresh-client -C {}x{}", new_size.0, new_size.1));
        self.relayout();
    }

    /// Returns false to quit.
    fn handle_input(&mut self, ev: InputEvent) -> bool {
        let routed = match &ev {
            InputEvent::Key(key) => {
                let modes = self.focused_modes();
                input::route_key(key, modes)
            }
            InputEvent::Mouse(m) => input::route_mouse(m, layout::SIDEBAR_WIDTH),
            InputEvent::Paste(text) => {
                let modes = self.focused_modes();
                Routed::PaneBytes(input::encode_paste(text, modes))
            }
            InputEvent::Resized { cols, rows } => {
                self.handle_resize((*cols as u16, *rows as u16));
                Routed::Ignore
            }
            _ => Routed::Ignore,
        };

        match routed {
            Routed::Quit => return false,
            Routed::ToggleHud => {
                self.hud = !self.hud;
                self.force_full = true;
                self.dirty = true;
            }
            Routed::FocusNext => self.focus(self.focused.wrapping_add(1)),
            Routed::FocusPrev => self.focus(self.focused.checked_sub(1).unwrap_or(self.panes.len().saturating_sub(1))),
            Routed::FocusIndex(i) => self.focus(i),
            Routed::PaneBytes(bytes) => self.send_pane_bytes(&bytes),
            Routed::ScrollView(delta) => {
                if let Some(p) = self.panes.get_mut(self.focused) {
                    if delta < 0 && p.term.display_offset() == 0 {
                        // Already live; nothing to do.
                    } else {
                        p.term.scroll_view(delta);
                        p.dirty = true;
                        self.dirty = true;
                    }
                }
            }
            Routed::SidebarClick { row, .. } => {
                // Pane rows start at row 2 in the sidebar.
                if row >= 2 {
                    let idx = (row - 2) as usize;
                    if idx < self.panes.len() {
                        self.selected = idx;
                        self.focus(idx);
                    }
                }
            }
            Routed::SidebarWheel(delta) => {
                let len = self.panes.len();
                if len > 0 {
                    let cur = self.selected as i32;
                    self.selected = (cur + delta).rem_euclid(len as i32) as usize;
                    self.dirty = true;
                }
            }
            Routed::PaneClick { col, row } => {
                if let Some(idx) = self.pane_at(col, row) {
                    self.focus(idx);
                    // Forward the click if the app asked for mouse events.
                    let p = &self.panes[idx];
                    let modes = p.term.input_modes();
                    if (modes.mouse_click || modes.mouse_drag || modes.mouse_motion) && modes.sgr_mouse {
                        if let Some(rect) = p.rect {
                            let bytes = input::encode_sgr_mouse(0, true, col - rect.x, row - rect.y);
                            let up = input::encode_sgr_mouse(0, false, col - rect.x, row - rect.y);
                            let pane = p.tmux_pane;
                            let _ = self.client.send(input::send_keys_hex(pane, &bytes));
                            let _ = self.client.send(input::send_keys_hex(pane, &up));
                        }
                    }
                }
            }
            Routed::Ignore => {}
        }
        true
    }

    fn focused_modes(&self) -> dmux_vt::InputModes {
        self.panes
            .get(self.focused)
            .map(|p| p.term.input_modes())
            .unwrap_or_default()
    }

    fn focus(&mut self, idx: usize) {
        if idx < self.panes.len() && idx != self.focused {
            self.focused = idx;
            self.selected = idx;
            // Mirror focus to tmux so plain clients and tooling agree.
            let w = self.panes[idx].tmux_window;
            let _ = self.client.send(format!("select-window -t {w}"));
            self.dirty = true;
        }
    }

    fn send_pane_bytes(&mut self, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(self.focused) else { return };
        if p.status == PaneStatus::Dead {
            return;
        }
        // Typing snaps scrollback to live, like every terminal.
        if p.term.display_offset() > 0 {
            p.term.scroll_to_bottom();
            p.dirty = true;
            self.dirty = true;
        }
        // Chunk to keep command lines short.
        for chunk in bytes.chunks(256) {
            let _ = self.client.send(input::send_keys_hex(p.tmux_pane, chunk));
        }
    }

    fn pane_at(&self, col: u16, row: u16) -> Option<usize> {
        self.panes.iter().position(|p| {
            p.rect
                .map(|r| {
                    // Title bar counts as part of the pane's click target.
                    let hit = dmux_compositor::Rect::new(r.x, r.y.saturating_sub(layout::TITLE_ROWS), r.w, r.h + layout::TITLE_ROWS);
                    hit.contains(col, row)
                })
                .unwrap_or(false)
        })
    }

    fn render_if_due(&mut self) {
        if self.dirty && self.last_frame.elapsed() >= FRAME_INTERVAL {
            self.render_frame();
        } else if self.dirty {
            self.metrics.coalesced += 1;
        }
    }

    fn handle_deadlines(&mut self) {
        let now = Instant::now();
        // Settle: Working panes with quiet output flip to Idle.
        for p in &mut self.panes {
            if p.status == PaneStatus::Working {
                if let Some(t) = p.last_output {
                    if now.duration_since(t) >= SETTLE_AFTER {
                        p.status = PaneStatus::Idle;
                        self.dirty = true;
                    }
                }
            }
        }
        // Throttled panes due for a refresh: re-enable output and reseed. A
        // continuing flood re-trips the meter within one window.
        let mut resumed = Vec::new();
        for p in &mut self.panes {
            if let Some(at) = p.resume_at {
                if now >= at {
                    p.resume_at = None;
                    p.throttled = false;
                    p.window_start = now;
                    p.window_bytes = 0;
                    p.begin_reseed();
                    resumed.push(p.tmux_pane);
                }
            }
        }
        for pane_id in resumed {
            let _ = self.client.send(format!("refresh-client -A '{pane_id}:on'"));
            let (seed, cursor) = {
                let p = self.panes.iter().find(|p| p.tmux_pane == pane_id).unwrap();
                (p.seed_command(), p.cursor_command())
            };
            let _ = self.client.send_tagged(seed, Tag::Seed(pane_id));
            let _ = self.client.send_tagged(cursor, Tag::Cursor(pane_id));
            self.dirty = true;
        }
        if self.hud {
            self.dirty = true;
        }
        if self.dirty && now.duration_since(self.last_frame) >= FRAME_INTERVAL {
            self.render_frame();
        }
    }

    fn render_frame(&mut self) {
        let start = Instant::now();
        let scene = render::Scene {
            panes: &self.panes,
            layout: &self.layout,
            focused: self.focused,
            selected: self.selected,
            session_name: &self.session_name,
            hud: self.hud.then_some(&self.metrics),
            status_line: &self.status_msg,
        };
        render::compose(&mut self.back, &scene);
        let composed = Instant::now();

        let sync = self.host.caps().synchronized_output;
        if sync {
            self.emitter.begin_sync();
        }
        self.emitter.hide_cursor();
        let force = self.force_full;
        diff_frame(&mut self.front, &mut self.back, &mut self.emitter, force);
        // Hardware cursor: focused pane's cursor, when visible and live.
        if let Some(p) = self.panes.get(self.focused) {
            if let (Some(rect), cur) = (p.rect, p.term.cursor()) {
                if let Some((cx, cy)) = cur.position {
                    if cx < rect.w && cy < rect.h {
                        self.emitter.move_to(rect.x + cx, rect.y + cy);
                        self.emitter.cursor_shape(cur.shape);
                        self.emitter.show_cursor();
                    }
                }
            }
        }
        if sync {
            self.emitter.end_sync();
        }
        let diffed = Instant::now();

        let bytes = self.emitter.take();
        let byte_count = bytes.len();
        if let Err(err) = self.host.write_frame(&bytes) {
            tracing::error!(%err, "frame write failed");
        }
        let done = Instant::now();

        for p in &mut self.panes {
            p.dirty = false;
            let _ = p.term.take_damage();
        }
        self.metrics.record_frame(
            done.duration_since(start),
            diffed.duration_since(composed),
            done.duration_since(diffed),
            byte_count,
            force,
        );
        self.force_full = false;
        self.dirty = false;
        self.last_frame = done;
    }
}

/// React to a pane emulator side effect.
fn handle_side_effect(client: &Client<Tag>, pane: &mut LogicalPane, effect: dmux_vt::TermSideEffect) {
    use dmux_vt::TermSideEffect;
    match effect {
        TermSideEffect::PtyResponse(bytes) => {
            // Query answers (DA1/CPR/OSC color…) go back into the pane's pty.
            let _ = client.send(input::send_keys_hex(pane.tmux_pane, &bytes));
        }
        TermSideEffect::Title(title) => {
            if !title.is_empty() && pane.title.is_empty() {
                pane.title = title;
            }
            // Full naming-service behavior is Phase 1.
        }
        TermSideEffect::Clipboard(_text) => {
            // OSC 52 forwarding to the host is a follow-up; ignore for now.
        }
        TermSideEffect::Bell => {
            // Attention plumbing is Phase 1.
        }
    }
}
