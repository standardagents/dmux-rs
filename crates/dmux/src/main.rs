//! dmux-rs: a native tmux control-mode renderer for dmux sessions. Attaches
//! (or creates) the project session, runs a terminal emulator per pane, and
//! composites panes + sidebar + native overlays into the host terminal with
//! damage-diffed, synchronized-output frames.

mod agents;
mod bootstrap;
mod git;
mod hooks;

mod input;
mod keys;
mod layout;
mod metrics;
mod notify;
mod render;
mod report;
mod session;
mod sounds;
mod tracking;
mod updater;
mod verify;
mod views;
mod welcome;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser as ClapParser;
use dmux_cc::{CcEvent, Client, PaneId, Reply, ReplyRouter, Routed as CcRouted};
use dmux_compositor::{diff_frame, CellBuffer, Emitter, Rect};
use dmux_core::i18n::{t, tf};
use dmux_core::{
    encode_pane_title, session_name_for_root, DmuxConfig, DmuxPane, PaneKind, SettingsScope,
    SettingsStore,
};
use dmux_host::{HostTerminal, InputEvent};
use dmux_ui::{draw_scrim, ClickMap, Theme};
use input::{MouseKind, Routed};
use session::{LogicalPane, PaneStatus};
use views::{
    AgentSelectView, AppCmd, ClickTarget, ConfirmView, InputPurpose, InputView, MenuItem, MenuView,
    SettingsView, ShortcutsView, View, ViewCtx, ViewResult,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SETTLE_AFTER: Duration = Duration::from_millis(1500);
const HUD_REFRESH: Duration = Duration::from_millis(500);
const ANIM_INTERVAL: Duration = Duration::from_millis(120);
/// The rain runs at a showier frame rate — cheap, and it's a perf demo.
const RAIN_INTERVAL: Duration = Duration::from_millis(33);
const STATUS_LINGER: Duration = Duration::from_secs(4);
/// Flood throttling: see ROADMAP notes; keeps typing latency flat under `yes`.
const FLOOD_WINDOW: Duration = Duration::from_millis(250);
const FLOOD_BYTES_PER_WINDOW: u64 = 1_000_000;
const FLOOD_RESEED_EVERY: Duration = Duration::from_millis(500);
/// Agent process/session tracking sweep cadence (env-overridable for tests).
fn tracking_interval() -> Duration {
    static SECS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    Duration::from_secs(*SECS.get_or_init(|| {
        std::env::var("DMUX_TRACKING_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(30)
    }))
}

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
    /// Replay a render-verify incident file offline and report whether the
    /// recorded stream still diverges from the stored tmux capture.
    #[arg(long, value_name = "FILE")]
    replay_incident: Option<PathBuf>,
}

/// Context for a window dmux-rs created and is waiting on.
#[derive(Debug)]
struct NewWindowCtx {
    slug: String,
    display: String,
    /// Prompt recorded on the pane (drives resume/duplicate).
    prompt: String,
    kind: PaneKind,
    agent: Option<String>,
    launch_cmd: Option<String>,
    /// (prompt, delay) for send-keys transport agents.
    injection: Option<(String, u64)>,
    worktree_path: Option<String>,
    /// Working directory for the new window (default: project root).
    cwd: Option<String>,
    /// Owning project root when not the main project.
    project_root: Option<String>,
    /// Native bootstrap (worktree + hook run by dmux, loader UI in the pane)
    /// with the agent launch deferred until it finishes.
    bootstrap: Option<BootstrapSpec>,
}

#[derive(Debug)]
struct BootstrapSpec {
    plan: bootstrap::Plan,
    launch: bootstrap::Launch,
    agent_label: String,
}

/// Results from background tasks (git merges, later inference) delivered
/// into the main loop.
#[derive(Debug)]
enum AppMsg {
    MergeDone { slug: String, branch: String, result: Result<String, String> },
    /// Async filesystem work finished; recompute anything derived from disk.
    RefreshDerived,
    /// LLM pane classification finished.
    AnalysisDone { pane: PaneId, verdict: Result<dmux_infer::PaneVerdict, String> },
    /// LLM terminal naming produced a candidate name.
    NamingDone { pane: PaneId, name: String },
    /// Native worktree bootstrap progress for a pane (keyed by slug).
    Bootstrap { slug: String, ev: bootstrap::Ev },
    /// Automatic incident report finished (issue filed or failed).
    IssueFiled(Result<report::FiledIssue, String>),
    /// A newer release is downloaded and staged; swap + re-exec.
    UpdateStaged { tag: String, staged: PathBuf },
    /// Agent process tracking sweep finished.
    TrackingDone(Vec<(String, tracking::AgentObservation)>),
    /// Conflicted merge state re-established; launch the resolution pane.
    ConflictsReady { branch: String, files: Result<Vec<String>, String> },
    /// A newer published version exists.
    UpdateAvailable(String),
    /// AI auto-merge finished (Ok = files resolved, merge committed).
    AiMergeDone { branch: String, result: Result<usize, String> },
}

/// Reply tags: every command whose reply matters is matched in stream order.
#[derive(Debug)]
enum Tag {
    ListPanes,
    Seed(PaneId),
    Cursor(PaneId),
    /// Shadow-verifier capture for one pane.
    VerifyCap(PaneId),
    ControllerPid,
    NewWindow(Box<NewWindowCtx>),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if let Some(path) = &cli.replay_incident {
        let text = std::fs::read_to_string(path)?;
        match verify::replay_incident(&text) {
            None => {
                eprintln!("could not parse incident file (wrong format?)");
                std::process::exit(2);
            }
            Some(diffs) if diffs.is_empty() => {
                println!("replay clean: recorded stream matches the stored tmux capture");
                return Ok(());
            }
            Some(diffs) => {
                println!("replay diverges: {} cells", diffs.len());
                for d in diffs.iter().take(20) {
                    println!("  {d}");
                }
                std::process::exit(1);
            }
        }
    }
    init_logging(&cli)?;

    if std::env::var_os("TMUX").is_some() {
        eprintln!("dmux-rs must run OUTSIDE tmux (it renders tmux panes itself).");
        eprintln!("Run it from a plain terminal window.");
        std::process::exit(2);
    }

    let (config, project_root, session_name) = resolve_session(&cli)?;
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
                // A fresh session holds ONLY the keepalive window, so dmux
                // boots straight into the welcome screen, not a bare shell.
                "-n",
                session::KEEPALIVE_NAME,
                "sleep 2147483647",
            ])
            .status()?;
        if !status.success() {
            eprintln!("failed to create tmux session '{session_name}' in {}", project_root.display());
            std::process::exit(1);
        }
        eprintln!("created session '{session_name}' for {}", project_root.display());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run(cli, config, project_root, session_name))
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
    config: DmuxConfig,
    config_path: PathBuf,
    /// Whether a config file existed / has been created on disk.
    config_persisted: bool,
    project_root: PathBuf,
    is_git: bool,
    session_name: String,
    settings: Arc<Mutex<SettingsStore>>,
    installed_agents: std::collections::HashSet<&'static str>,
    keymap: keys::Keymap,
    theme: Theme,
    views: Vec<Box<dyn View>>,
    click_map: ClickMap<ClickTarget>,
    view_cursor: Option<(u16, u16)>,
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
    status_clear_at: Option<Instant>,
    leader_armed: bool,
    /// The sidebar owns navigation keys (entered via `^b ↑/↓` or clicking a
    /// row); Esc or focusing a pane hands the keyboard back.
    sidebar_focused: bool,
    anim: u64,
    /// Prompts waiting to be typed into send-keys-transport agent panes.
    pending_injections: Vec<(PaneId, String, Instant)>,
    /// Active native worktree bootstraps, keyed by pane slug; the pane body
    /// renders a loader card while one exists.
    bootstraps: std::collections::HashMap<String, bootstrap::Ui>,
    /// DMUX_VERIFY=1: shadow-compare settled panes against tmux's grid and
    /// write incident bundles on mismatch.
    verify_enabled: bool,
    /// Fault injection for verifying the verifier (DMUX_FAULT_DROP_BYTES=N):
    /// silently drop the first N pane-output bytes, simulating a
    /// stream-consumption bug the shadow verifier must catch.
    fault_drop: usize,
    /// Issues this user's dmux-rs has filed (sidebar list; ● = this session).
    filed_issues: Vec<report::FiledIssue>,
    new_issue_count: usize,
    version_line: String,
    /// A staged self-update: swap + re-exec after clean shutdown.
    reexec_after: Option<PathBuf>,
    want_exit: bool,
    own_sizing: bool,
    sized_windows: std::collections::HashSet<dmux_cc::WindowId>,
    /// Welcome-screen state (shown when no panes are visible).
    welcome_cards: Vec<welcome::WelcomeCard>,
    welcome_sel: usize,
    welcome_rain: welcome::MatrixRain,
    keepalive_present: bool,
    /// Panes we killed on purpose: never re-adopt while tmux still lists them.
    closing: std::collections::HashSet<PaneId>,
    /// A pane we just created: focus it once adoption lands.
    pending_focus: Option<PaneId>,
    /// Pane index with an active selection drag.
    drag_select: Option<usize>,
    /// Pane index receiving forwarded mouse-drag events (app mouse mode).
    mouse_forward: Option<usize>,
    /// Physical button state: SGR 1002 reports press and drag identically,
    /// so clicks fire only on the press edge.
    mouse_down: bool,
    /// The current drag actually moved (a plain click must not copy).
    drag_moved: bool,
    /// Last press (time, col, row) for double-click detection.
    last_press: Option<(Instant, u16, u16)>,
    last_search: Option<String>,
    log_path: PathBuf,
    app_tx: tokio::sync::mpsc::UnboundedSender<AppMsg>,
    inference_primary: Option<dmux_infer::Target>,
    inference_backup: Option<dmux_infer::Target>,
    update_available: Option<String>,
    tracking_inflight: bool,
    last_tracking: Instant,
}

async fn run(
    cli: Cli,
    config: Option<DmuxConfig>,
    project_root: PathBuf,
    session_name: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = Vec::new();
    if let Some(socket) = &cli.socket {
        args.extend(["-L".into(), socket.clone()]);
    }
    args.extend(["-C".into(), "attach-session".into(), "-t".into(), session_name.clone()]);
    let (client, mut events, router, mut child) = Client::<Tag>::spawn(&cli.tmux, &args)?;

    let settings = Arc::new(Mutex::new(SettingsStore::load(&dirs_home(), Some(&project_root))));
    let installed_agents = agents::detect_installed();
    let is_git = git_main_worktree_root(&project_root).is_some();

    let host = HostTerminal::setup()?;
    let size = host.size();
    let mut input_rx = dmux_host::spawn_input_reader();
    let mut resize_rx = dmux_host::spawn_resize_watcher();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel::<AppMsg>();

    let caps = host.caps();
    tracing::info!(
        ?size,
        sync_output = caps.synchronized_output,
        kitty = caps.kitty_keyboard,
        session = %session_name,
        agents = installed_agents.len(),
        "attached"
    );

    let _ = client.send("refresh-client -f ignore-size,pause-after=1,wait-exit");
    let _ = client.send(format!("refresh-client -C {}x{}", size.0, size.1));
    client.send_tagged(
        format!("show-options -t {} -qv @dmux_controller_pid", dmux_cc::quote_arg(&session_name)),
        Tag::ControllerPid,
    )?;
    client.send_tagged(session::list_panes_command(), Tag::ListPanes)?;

    let (theme, keymap) = {
        let s = settings.lock().unwrap();
        let theme = Theme::named(s.get_str("colorTheme").unwrap_or("violet"));
        let overrides = s
            .get("keybindings")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        (theme, keys::Keymap::from_overrides(&overrides))
    };
    let config_persisted = config.is_some();
    let config = config.unwrap_or_else(|| {
        let name = project_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        DmuxConfig::new(name, project_root.to_string_lossy().into_owned())
    });
    let config_path = DmuxConfig::default_path(&project_root);

    let mut app = App {
        client,
        router,
        host,
        panes: Vec::new(),
        config,
        config_path,
        config_persisted,
        project_root,
        is_git,
        session_name,
        settings,
        installed_agents,
        keymap,
        theme,
        views: Vec::new(),
        click_map: ClickMap::new(),
        view_cursor: None,
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
        status_msg: String::new(),
        status_clear_at: None,
        leader_armed: false,
        sidebar_focused: false,
        anim: 0,
        pending_injections: Vec::new(),
        bootstraps: std::collections::HashMap::new(),
        verify_enabled: std::env::var("DMUX_VERIFY").map(|v| v != "0").unwrap_or(true),
        fault_drop: std::env::var("DMUX_FAULT_DROP_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        filed_issues: report::load_filed(&dirs_home()),
        new_issue_count: 0,
        version_line: updater::version_line(),
        reexec_after: None,
        want_exit: false,
        own_sizing: false,
        sized_windows: std::collections::HashSet::new(),
        welcome_cards: Vec::new(),
        welcome_sel: 0,
        welcome_rain: welcome::MatrixRain::new(size.0.saturating_sub(layout::SIDEBAR_WIDTH + 1), size.1),
        keepalive_present: false,
        closing: std::collections::HashSet::new(),
        pending_focus: None,
        drag_select: None,
        mouse_forward: None,
        mouse_down: false,
        drag_moved: false,
        last_press: None,
        last_search: None,
        log_path: cli.log_file.clone().unwrap_or_else(|| dirs_home().join(".dmux").join("logs").join("dmux-rs.log")),
        app_tx,
        inference_primary: None,
        inference_backup: None,
        update_available: None,
        tracking_inflight: false,
        last_tracking: Instant::now(),
    };
    {
        let s = app.settings.lock().unwrap();
        app.inference_primary = s.get("inferencePrimary").and_then(dmux_infer::Target::from_value);
        app.inference_backup = s.get("inferenceBackup").and_then(dmux_infer::Target::from_value);
        dmux_core::i18n::set_locale(s.get_str("language").unwrap_or("en"));
    }
    // Update check (daily, off-loop, best-effort).
    {
        let tx = app.app_tx.clone();
        tokio::spawn(async move {
            if let Some(latest) = check_latest_version().await {
                if is_newer(&latest, env!("CARGO_PKG_VERSION")) {
                    let _ = tx.send(AppMsg::UpdateAvailable(latest));
                }
            }
        });
    }
    if std::env::var("DMUX_JUST_UPDATED").map(|v| v == "1").unwrap_or(false) {
        app.toast(format!("⬆ updated to {}", updater::version_line()));
    }
    // First-party self-update loop: poll the dmux-rs repo's latest release
    // and stage newer builds for an in-place re-exec (HMR for the mux).
    if updater::enabled() {
        let tx = app.app_tx.clone();
        let repo = {
            let s = app.settings.lock().unwrap();
            s.get_str("dmuxRsRepo").unwrap_or(report::DEFAULT_REPO).to_string()
        };
        let poll_secs: u64 = std::env::var("DMUX_UPDATE_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                let r = repo.clone();
                let tag = tokio::task::spawn_blocking(move || updater::latest_tag(&r)).await;
                let Ok(Ok(tag)) = tag else { continue };
                if tag == updater::BUILD_TAG || tag.is_empty() {
                    continue;
                }
                let r = repo.clone();
                let t = tag.clone();
                match tokio::task::spawn_blocking(move || updater::stage(&r, &t)).await {
                    Ok(Ok(staged)) => {
                        let _ = tx.send(AppMsg::UpdateStaged { tag, staged });
                        break;
                    }
                    Ok(Err(err)) => tracing::warn!(%err, "update stage failed"),
                    Err(err) => tracing::warn!(%err, "update task failed"),
                }
            }
        });
    }
    if app.inference_primary.is_some() {
        tracing::info!(
            provider = %app.inference_primary.as_ref().unwrap().provider_id,
            "inference configured; LLM status escalation active"
        );
    }
    app.refresh_welcome_cards();

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
        let hud_deadline = app.hud.then(|| tokio::time::Instant::from_std(now + HUD_REFRESH));
        let resume_deadline = app
            .panes
            .iter()
            .filter_map(|p| p.resume_at)
            .min()
            .map(tokio::time::Instant::from_std);
        let anim_deadline = app.animating().then(|| {
            let interval = if app.welcome_active() { RAIN_INTERVAL } else { ANIM_INTERVAL };
            tokio::time::Instant::from_std(now + interval)
        });
        let injection_deadline = app
            .pending_injections
            .iter()
            .map(|(_, _, at)| *at)
            .min()
            .map(tokio::time::Instant::from_std);
        let status_deadline = app.status_clear_at.map(tokio::time::Instant::from_std);
        let tracking_deadline = (!app.tracking_inflight
            && app.panes.iter().any(|p| p.agent.is_some() && p.pane_pid > 0))
        .then(|| tokio::time::Instant::from_std(app.last_tracking + tracking_interval()));
        let deadline = [
            render_deadline,
            settle_deadline,
            hud_deadline,
            resume_deadline,
            anim_deadline,
            injection_deadline,
            status_deadline,
            tracking_deadline,
        ]
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
            Some(msg) = app_rx.recv() => {
                app.handle_app_msg(msg);
                if app.want_exit { break; }
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
            _ = timer => {
                app.handle_deadlines();
            }
        }
    }

    let reexec = app.reexec_after.take();
    let result = app.shutdown(&mut child).await;
    if let Some(exe) = reexec {
        // Only returns on error; on success the new build takes over this
        // terminal and reattaches to the same tmux session.
        let err = updater::reexec(&exe);
        eprintln!("dmux-rs self-update re-exec failed: {err}");
    }
    result
}

impl App {
    async fn shutdown(&mut self, child: &mut tokio::process::Child) -> Result<(), Box<dyn std::error::Error>> {
        self.host.restore();
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
        eprintln!(
            "dmux-rs detached. Frames: {}, p95 {:.2} ms.",
            self.metrics.frames,
            self.metrics.frame_total_us.value_at_quantile(0.95) as f64 / 1000.0
        );
        Ok(())
    }

    fn visible_pane_count(&self) -> usize {
        self.panes.iter().filter(|p| p.rect.is_some()).count()
    }

    fn welcome_active(&self) -> bool {
        self.visible_pane_count() == 0
    }

    fn refresh_welcome_cards(&mut self) {
        // Worktrees on disk that no live pane is using — candidates to reopen.
        let live_paths: std::collections::HashSet<String> = self
            .config
            .panes
            .iter()
            .filter(|r| self.panes.iter().any(|p| p.slug == r.slug))
            .filter_map(|r| r.worktree_path.clone())
            .collect();
        let mut worktrees: Vec<welcome::WorktreeCard> = Vec::new();
        let wt_dir = self.project_root.join(".dmux").join("worktrees");
        if let Ok(entries) = std::fs::read_dir(&wt_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let p = path.to_string_lossy().into_owned();
                    if !live_paths.contains(&p) {
                        if let Some(name) = path.file_name() {
                            let slug = name.to_string_lossy().into_owned();
                            // The record remembers which agent lived here.
                            let agent = self
                                .config
                                .panes
                                .iter()
                                .find(|r| r.worktree_path.as_deref() == Some(p.as_str()) || r.slug == slug)
                                .and_then(|r| r.agent.clone());
                            worktrees.push(welcome::WorktreeCard { slug, path: p, agent });
                        }
                    }
                }
            }
        }
        worktrees.sort_by(|a, b| a.slug.cmp(&b.slug));
        let project_name = self
            .project_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        self.welcome_cards = welcome::build_cards(&self.installed_agents, &project_name, &worktrees);
        self.welcome_sel = self.welcome_sel.min(self.welcome_cards.len().saturating_sub(1));
    }

    /// Make sure the keepalive window exists so an empty session survives.
    /// Commands are FIFO on the control stream, so calling this before a
    /// kill-window guarantees the session never hits zero windows.
    fn ensure_keepalive(&mut self) {
        if self.keepalive_present || !self.own_sizing {
            return;
        }
        self.keepalive_present = true;
        let _ = self.client.send(format!(
            "new-window -d -n {} 'sleep 2147483647'",
            session::KEEPALIVE_NAME
        ));
    }

    fn animating(&self) -> bool {
        !self.bootstraps.is_empty()
            || self.welcome_active()
            || self
                .panes
                .iter()
                .any(|p| p.status == PaneStatus::Working && !p.hidden)
            || self.views.iter().any(|v| v.animating())
    }

    /// Push text to the host clipboard (OSC 52) and mirror it into a tmux
    /// buffer so plain-attached clients can paste it too.
    fn forward_clipboard(&mut self, text: &str) {
        if text.is_empty() || text.len() > 512 * 1024 {
            return;
        }
        let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
        if let Err(err) = self.host.write_frame(osc.as_bytes()) {
            tracing::warn!(%err, "clipboard forward failed");
        }
        let _ = self
            .client
            .send(format!("set-buffer -b dmux {}", dmux_cc::quote_arg(text)));
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.status_msg = msg.into();
        self.status_clear_at = Some(Instant::now() + STATUS_LINGER);
        self.dirty = true;
    }

    /// Auto-file a GitHub issue for a verified render divergence — once per
    /// pane per process lifetime (a corrupted pane paints many bad frames;
    /// they are one bug). Reload (= update) clears the latch.
    fn maybe_file_issue(
        &mut self,
        pane_id: PaneId,
        incident: Option<std::path::PathBuf>,
        reply: &Reply,
    ) {
        if std::env::var("DMUX_NO_REPORT").map(|v| v == "1").unwrap_or(false) {
            return;
        }
        let Some(incident) = incident else { return };
        let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) else { return };
        if p.issue_filed {
            return;
        }
        p.issue_filed = true;
        let repo = {
            let s = self.settings.lock().unwrap();
            s.get_str("dmuxRsRepo").unwrap_or(report::DEFAULT_REPO).to_string()
        };
        let diffs = verify::compare(p, reply);
        let our_grid: String =
            (0..p.rows).map(|r| p.term.row_text_public(r) + "\n").collect();
        let tmux_grid: String = reply
            .lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).escape_default().to_string() + "\n")
            .collect();
        let (slug, cols, rows, det) = (p.slug.clone(), p.cols, p.rows, !p.ring_truncated);
        let build = updater::version_line();
        let home = dirs_home();
        let dry = std::env::var("DMUX_REPORT_DRY").ok().map(std::path::PathBuf::from);
        let tx = self.app_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = report::file_issue(
                &repo,
                &home,
                &build,
                &slug,
                cols,
                rows,
                &diffs,
                &our_grid,
                &tmux_grid,
                &incident,
                det,
                dry.as_deref(),
            )
            .map(|f| f.issue)
            .map_err(|e| e);
            let _ = tx.send(AppMsg::IssueFiled(result));
        });
    }

    /// Attention that should also reach the OS: sidebar toast + native
    /// notification via the macOS helper (when installed and enabled).
    fn attention_toast(&mut self, msg: String) {
        let native = {
            let s = self.settings.lock().unwrap();
            s.get_bool("enableNotifications", true)
        };
        if native && notify::available() {
            let body = msg.clone();
            // Rotate through the configured helper sounds (TS randomizes;
            // a timestamp pick avoids an rng dependency).
            let sound = {
                let s = self.settings.lock().unwrap();
                sounds::pick_resource(s.get("enabledNotificationSounds"), timestamp())
            };
            tokio::task::spawn_blocking(move || {
                let _ = notify::notify("dmux", &body, sound.as_deref());
            });
        }
        self.toast(msg);
    }

    // ------------------------------------------------------------------
    // Control-mode events

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
                false
            }
        }
    }

    fn handle_notification(&mut self, ev: CcEvent) -> bool {
        match ev {
            CcEvent::Output { pane, data } | CcEvent::ExtendedOutput { pane, data, .. } => {
                self.metrics.record_input(data.len());
                let data = if self.fault_drop > 0 {
                    let n = data.len().min(self.fault_drop);
                    self.fault_drop -= n;
                    data[n..].to_vec()
                } else {
                    data
                };
                let mut clipboard_out: Vec<String> = Vec::new();
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    let now = Instant::now();
                    if now.duration_since(p.window_start) >= FLOOD_WINDOW {
                        p.window_start = now;
                        p.window_bytes = 0;
                    }
                    p.window_bytes += data.len() as u64;

                    if let Some(buffer) = &mut p.reseed_buffer {
                        // Recorded when the seed drains it (finish_reseed).
                        buffer.push(data);
                    } else {
                        let effects = p.advance_recorded(&data);
                        for effect in effects {
                            if let Some(text) = handle_side_effect(&self.client, p, effect) {
                                clipboard_out.push(text);
                            }
                        }
                        p.engine.on_output();
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
                        let _ = self.client.send(format!("refresh-client -A '{pane}:off'"));
                        self.dirty = true;
                    }
                }
                for text in clipboard_out {
                    self.forward_clipboard(&text);
                }
                true
            }
            CcEvent::Pause(pane) => {
                self.metrics.pauses += 1;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.paused = true;
                    p.begin_reseed();
                    let _ = self.client.send(format!("refresh-client -A '{pane}:continue'"));
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
            CcEvent::WindowClose(w) => {
                for p in self.panes.iter_mut().filter(|p| p.tmux_window == w) {
                    p.status = PaneStatus::Dead;
                    p.dirty = true;
                }
                self.request_reconcile();
                true
            }
            CcEvent::UnlinkedWindowClose(_) => {
                // A window closed in a session OURS is not attached to
                // (grouped sessions, other users' sessions). Our windows are
                // always linked to our session, so this is never one of our
                // panes dying — marking Dead here false-kills healthy panes
                // whenever session groups churn. Reconcile picks up any real
                // topology change.
                self.request_reconcile();
                true
            }
            CcEvent::WindowAdd(_) | CcEvent::LayoutChange { .. } | CcEvent::WindowPaneChanged { .. } => {
                self.request_reconcile();
                true
            }
            CcEvent::WindowRenamed { window, name } | CcEvent::UnlinkedWindowRenamed { window, name } => {
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
            CcEvent::ConfigError(err) => {
                tracing::warn!(%err, "tmux config error");
                true
            }
            CcEvent::Unknown(line) => {
                tracing::debug!(%line, "unknown control-mode line");
                true
            }
            _ => true,
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
                // An error reply (e.g. the pane died mid-flight) must never
                // be applied as grid content.
                if reply.ok {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                        p.pending_seed = Some(reply);
                    }
                }
            }
            Tag::VerifyCap(pane_id) => {
                if !reply.ok {
                    return;
                }
                let mut report: Option<(usize, Option<std::path::PathBuf>, String)> = None;
                if let Some(p) = self.panes.iter().find(|p| p.tmux_pane == pane_id) {
                    // Discard if output arrived since the capture was
                    // requested — comparison is only valid at quiescence.
                    let quiet = p
                        .last_output
                        .map(|t| t.elapsed() >= std::time::Duration::from_millis(500))
                        .unwrap_or(true);
                    if quiet && p.reseed_buffer.is_none() && !p.paused {
                        let diffs = verify::compare(p, &reply);
                        if diffs.is_empty() {
                            tracing::debug!(pane = %pane_id, "render verify clean");
                        }
                        if !diffs.is_empty() {
                            let path = verify::write_incident(&dirs_home(), p, &reply, &diffs).ok();
                            report = Some((diffs.len(), path, p.display_title().to_string()));
                        }
                    }
                }
                if let Some((n, path, title)) = report {
                    let loc = path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(incident write failed)".into());
                    tracing::warn!(pane = %pane_id, diffs = n, incident = %loc, "render verify mismatch");
                    self.toast(format!("⚠ render verify: {n} diffs in '{title}' → {loc}"));
                    self.maybe_file_issue(pane_id, path, &reply);
                }
            }
            Tag::Cursor(pane_id) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                    if let Some(seed) = p.pending_seed.take() {
                        let cursor = reply.ok.then(|| session::parse_cursor_reply(&reply)).flatten();
                        p.finish_reseed(&seed, cursor);
                        self.dirty = true;
                    } else {
                        // Seed failed: stop buffering so live output flows again.
                        if let Some(buffered) = p.reseed_buffer.take() {
                            for chunk in buffered {
                                let _ = p.advance_recorded(&chunk);
                            }
                        }
                        self.dirty = true;
                    }
                }
            }
            Tag::ControllerPid => {
                let pid = reply.text_lines().first().and_then(|l| l.trim().parse::<i32>().ok());
                let controller_alive = pid.map(|pid| unsafe { libc::kill(pid, 0) == 0 }).unwrap_or(false);
                self.own_sizing = !controller_alive;
                tracing::info!(?pid, own_sizing = self.own_sizing, "controller check");
                if !self.own_sizing {
                    self.toast("observe mode: TS dmux owns this session");
                }
                // Keepalive creation happens in apply_pane_list, after the
                // pane listing has revealed whether one already exists.
                self.apply_window_sizes();
            }
            Tag::NewWindow(ctx) => {
                self.finish_new_window(*ctx, &reply);
            }
        }
    }

    fn handle_app_msg(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::MergeDone { slug, branch, result } => match result {
                Ok(target) => {
                    self.views.push(Box::new(ConfirmView::new(
                        "Merge complete",
                        format!("'{branch}' merged into '{target}'. Remove worktree and close pane?"),
                        "Clean up",
                        false,
                        AppCmd::MergeCleanup { slug },
                    )));
                    self.dirty = true;
                }
                Err(err) => {
                    tracing::warn!(%err, %branch, "merge failed");
                    if err.contains("conflict") || err.contains("CONFLICT") {
                        let agent_label = self.default_agent_for_conflicts().map(|d| d.name).unwrap_or("an agent");
                        let mut items = Vec::new();
                        if self.inference_primary.is_some() {
                            items.push(MenuItem::new(
                                "AI merge (auto-resolve)",
                                "",
                                AppCmd::AiMerge { branch: branch.clone() },
                            ));
                        }
                        items.push(MenuItem::new(
                            format!("Resolve with {agent_label}…"),
                            "",
                            AppCmd::ResolveConflicts { branch: branch.clone() },
                        ));
                        items.push(MenuItem::new("Leave it (merge aborted)", "", AppCmd::Noop));
                        self.views.push(Box::new(MenuView::new(
                            format!("Merge conflict: {branch}"),
                            items,
                        )));
                        self.dirty = true;
                    } else {
                        let short: String = err.chars().take(80).collect();
                        self.toast(format!("✗ {short}"));
                    }
                }
            },
            AppMsg::RefreshDerived => {
                self.refresh_welcome_cards();
                self.dirty = true;
            }
            AppMsg::AiMergeDone { branch, result } => match result {
                Ok(files) => {
                    // Merge committed; offer the standard cleanup.
                    let slug = self
                        .panes
                        .iter()
                        .find(|p| {
                            p.worktree_path
                                .as_deref()
                                .map(PathBuf::from)
                                .and_then(|w| git::current_branch(&w))
                                .as_deref()
                                == Some(branch.as_str())
                        })
                        .map(|p| p.slug.clone());
                    self.toast(format!("✓ AI merged {files} file(s) from '{branch}'"));
                    if let Some(slug) = slug {
                        self.views.push(Box::new(ConfirmView::new(
                            "Merge complete",
                            format!("'{branch}' AI-merged and committed. Remove worktree and close pane?"),
                            "Clean up",
                            false,
                            AppCmd::MergeCleanup { slug },
                        )));
                        self.dirty = true;
                    }
                }
                Err(err) => {
                    let short: String = err.chars().take(90).collect();
                    self.toast(format!("✗ AI merge: {short}"));
                }
            },
            AppMsg::UpdateAvailable(version) => {
                self.update_available = Some(version.clone());
                self.toast(format!("Update available: dmux-rs {version}"));
            }
            AppMsg::ConflictsReady { branch, files } => match files {
                Ok(files) => {
                    let Some(def) = self.default_agent_for_conflicts() else {
                        self.toast("No agent installed for conflict resolution");
                        return;
                    };
                    let list = if files.is_empty() {
                        "the conflicted files".to_string()
                    } else {
                        files.join(", ")
                    };
                    let prompt = format!(
                        "A git merge of branch '{branch}' has conflicts in: {list}. Resolve every conflict thoughtfully (keep both sides' intent), then stage the files and complete the merge with a commit."
                    );
                    let mode = {
                        let s = self.settings.lock().unwrap();
                        s.get_str("permissionMode").unwrap_or("").to_string()
                    };
                    let dir = self.project_root.join(".dmux").join("prompts");
                    let _ = std::fs::create_dir_all(&dir);
                    let pf = dir.join(format!("conflicts-{}.txt", timestamp()));
                    let _ = std::fs::write(&pf, &prompt);
                    let agent_cmd = agents::compose_launch(def, Some(&pf.to_string_lossy()), &mode);
                    let injection = match def.transport {
                        agents::Transport::SendKeys { ready_delay_ms } => Some((prompt, ready_delay_ms)),
                        _ => None,
                    };
                    let n = 1 + self.panes.iter().filter(|q| q.slug.starts_with("conflicts-")).count();
                    self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                        slug: format!("conflicts-{n}"),
                        display: format!("conflicts: {branch}"),
                        kind: PaneKind::Shell,
                        agent: Some(def.id.to_string()),
                        launch_cmd: Some(format!("clear; {agent_cmd}")),
                        injection,
                        worktree_path: None,
                        cwd: Some(self.project_root.to_string_lossy().into_owned()),
                        project_root: None,
                    });
                    self.toast(format!("Resolving conflicts with {}", def.name));
                }
                Err(err) => {
                    let short: String = err.chars().take(80).collect();
                    self.toast(format!("✗ {short}"));
                }
            },
            AppMsg::TrackingDone(observations) => {
                self.tracking_inflight = false;
                let mut changed = false;
                for (slug, obs) in observations {
                    if let Some(rec) = self.config.panes.iter_mut().find(|r| r.slug == slug) {
                        let mut update = |key: &str, value: serde_json::Value| {
                            if rec.extra.get(key) != Some(&value) {
                                rec.extra.insert(key.to_string(), value);
                                changed = true;
                            }
                        };
                        update("activeAgent", serde_json::Value::String(obs.agent_id.to_string()));
                        update("agentProcessId", serde_json::Value::from(obs.agent_pid));
                        if let Some(session) = &obs.session_id {
                            update("agentSessionId", serde_json::Value::String(session.clone()));
                        }
                    }
                }
                if changed {
                    self.save_config();
                    tracing::debug!("agent tracking updated config records");
                }
            }
            AppMsg::AnalysisDone { pane, verdict } => {
                let focused_pane = self.panes.get(self.focused).map(|p| p.tmux_pane);
                let mut attention: Option<String> = None;
                let mut autopilot_fired = false;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.analysis_inflight = false;
                    let is_focused = focused_pane == Some(pane);
                    match verdict {
                        Ok(dmux_infer::PaneVerdict::OptionDialog) if p.autopilot => {
                            // Autopilot: accept the highlighted option so the
                            // agent keeps moving; no attention needed.
                            p.status = PaneStatus::Working;
                            p.last_output = Some(Instant::now());
                            autopilot_fired = true;
                        }
                        Ok(dmux_infer::PaneVerdict::OptionDialog) => {
                            p.status = PaneStatus::Waiting;
                            if !is_focused && !p.needs_attention {
                                p.needs_attention = true;
                                attention = Some(format!("△ {} needs input", p.display_title()));
                            }
                        }
                        Ok(dmux_infer::PaneVerdict::OpenPrompt) => {
                            p.status = PaneStatus::Idle;
                            if !is_focused && !p.needs_attention {
                                p.needs_attention = true;
                                attention = Some(format!("✓ {} finished", p.display_title()));
                            }
                        }
                        Ok(dmux_infer::PaneVerdict::InProgress) => {
                            if p.status != PaneStatus::Dead {
                                p.status = PaneStatus::Working;
                                p.last_output = Some(Instant::now());
                            }
                        }
                        Err(err) => {
                            tracing::warn!(%err, pane = %pane, "pane analysis failed; keeping heuristic verdict");
                            if !is_focused && !p.needs_attention {
                                p.needs_attention = true;
                                attention = Some(format!("• {} settled", p.display_title()));
                            }
                        }
                    }
                    self.dirty = true;
                }
                if autopilot_fired {
                    let _ = self.client.send(input::send_keys_hex(pane, b"\r"));
                    self.toast("Autopilot accepted an option dialog");
                }
                if let Some(msg) = attention {
                    self.attention_toast(msg);
                }
            }
            AppMsg::Bootstrap { slug, ev } => {
                let mut fail_toast: Option<String> = None;
                let mut launch_now: Option<(PaneId, bootstrap::Launch)> = None;
                if let Some(ui) = self.bootstraps.get_mut(&slug) {
                    match ev {
                        bootstrap::Ev::Step(i) => ui.current = i.min(ui.steps.len().saturating_sub(1)),
                        bootstrap::Ev::Detail(line) => ui.detail = line,
                        bootstrap::Ev::Failed(err) => {
                            fail_toast = Some(format!("Bootstrap failed for '{}': {err}", ui.title));
                            ui.failed = Some(err);
                            ui.done_at = Some(Instant::now());
                        }
                        bootstrap::Ev::Done => {
                            ui.done_at = Some(Instant::now());
                            ui.detail.clear();
                            if let Some(launch) = ui.launch.take() {
                                launch_now = Some((ui.pane, launch));
                            }
                        }
                    }
                    self.dirty = true;
                }
                if let Some((pane, launch)) = launch_now {
                    let cmd = format!(
                        "clear; cd {} 2>/dev/null || cd {}; {}",
                        shq(&launch.wt),
                        shq(&launch.root),
                        launch.agent_cmd
                    );
                    let mut bytes = cmd.into_bytes();
                    bytes.push(b'\r');
                    for chunk in bytes.chunks(256) {
                        let _ = self.client.send(input::send_keys_hex(pane, chunk));
                    }
                    if let Some((prompt, delay_ms)) = launch.injection {
                        self.pending_injections.push((
                            pane,
                            prompt,
                            Instant::now() + Duration::from_millis(delay_ms),
                        ));
                    }
                }
                if let Some(msg) = fail_toast {
                    self.toast(msg);
                }
            }
            AppMsg::IssueFiled(result) => {
                match result {
                    Ok(issue) => {
                        self.toast(format!("🐛 filed issue #{} — {}", issue.number, issue.title));
                        self.filed_issues.push(issue);
                        self.new_issue_count += 1;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "auto-report failed");
                        self.toast(format!("auto-report failed: {err}"));
                    }
                }
                self.dirty = true;
            }
            AppMsg::UpdateStaged { tag, staged } => {
                match updater::apply(&staged) {
                    Ok(exe) => {
                        self.toast(format!("⬆ updating to {tag}…"));
                        self.reexec_after = Some(exe);
                        self.want_exit = true;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "update apply failed");
                        self.toast(format!("update failed: {err}"));
                    }
                }
            }
            AppMsg::NamingDone { pane, name } => {
                let mut apply = false;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.analysis_inflight = false;
                    if !name.is_empty() && p.auto_name {
                        p.llm_named = true;
                        p.llm_named_at = Some(Instant::now());
                        if p.title != name {
                            p.title = name.clone();
                            let encoded = encode_pane_title(&name, &p.slug);
                            let _ = self
                                .client
                                .send(format!("select-pane -t {} -T {}", p.tmux_pane, dmux_cc::quote_arg(&encoded)));
                            apply = true;
                        }
                    }
                }
                if apply {
                    self.dirty = true;
                }
            }
        }
    }

    /// Project root that new panes should target: the selected pane's
    /// project, else the main project.
    fn active_project_root(&self) -> Option<String> {
        self.panes.get(self.selected).and_then(|p| p.project_root.clone())
    }

    /// Best agent to hand conflict resolution to: the configured default if
    /// installed, else the first installed default-enabled agent.
    fn default_agent_for_conflicts(&self) -> Option<&'static agents::AgentDef> {
        let preferred = {
            let s = self.settings.lock().unwrap();
            s.get_str("defaultAgent").unwrap_or("").to_string()
        };
        agents::agent(&preferred)
            .filter(|d| self.installed_agents.contains(d.id))
            .or_else(|| {
                agents::AGENTS
                    .iter()
                    .find(|d| d.default_enabled && self.installed_agents.contains(d.id))
            })
    }

    fn request_reconcile(&mut self) {
        if self.reconcile_in_flight {
            self.reconcile_again = true;
            return;
        }
        self.reconcile_in_flight = true;
        let _ = self.client.send_tagged(session::list_panes_command(), Tag::ListPanes);
    }

    fn apply_pane_list(&mut self, reply: &Reply) {
        let infos = session::parse_pane_list(reply);
        // Track (and dedupe) keepalive windows.
        let keepalives: Vec<_> = infos
            .iter()
            .filter(|i| i.window_name == session::KEEPALIVE_NAME)
            .map(|i| i.window)
            .collect();
        self.keepalive_present = !keepalives.is_empty();
        for extra in keepalives.iter().skip(1) {
            let _ = self.client.send(format!("kill-window -t {extra}"));
        }
        // Forget closing-markers for panes tmux no longer lists.
        let listed: std::collections::HashSet<PaneId> = infos.iter().map(|i| i.pane).collect();
        self.closing.retain(|p| listed.contains(p));
        let infos: Vec<_> = infos.into_iter().filter(|i| !self.closing.contains(&i.pane)).collect();
        let adopted = session::adopt_panes(Some(&self.config), &infos);

        for mut new_pane in adopted {
            new_pane.record_stream = self.verify_enabled;
            match self.panes.iter_mut().find(|p| p.tmux_pane == new_pane.tmux_pane) {
                Some(existing) => {
                    existing.tmux_window = new_pane.tmux_window;
                    // tmux still lists the pane: whatever marked it dead was
                    // wrong (or it recovered) — resurrect.
                    if existing.status == PaneStatus::Dead {
                        existing.status = PaneStatus::Idle;
                        existing.dirty = true;
                    }
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
                    if new_pane.hidden {
                        let _ = self.client.send(format!("refresh-client -A '{}:off'", new_pane.tmux_pane));
                    }
                    self.panes.push(new_pane);
                }
            }
        }
        // Panes whose process exited are gone — tmux semantics, no dead husks.
        let live: std::collections::HashSet<_> = infos.iter().map(|i| i.pane).collect();
        let before = self.panes.len();
        self.panes.retain(|p| live.contains(&p.tmux_pane));
        if self.panes.len() != before {
            // Terminal records die with their pane; worktree records stay so
            // the welcome screen offers to reopen them.
            let live_slugs: std::collections::HashSet<String> =
                self.panes.iter().map(|p| p.slug.clone()).collect();
            let rec_before = self.config.panes.len();
            self.config
                .panes
                .retain(|r| r.kind() != PaneKind::Shell || live_slugs.contains(&r.slug));
            if self.config.panes.len() != rec_before {
                self.save_config();
            }
        }
        self.relayout();
        // Newly created panes take focus once adopted.
        if let Some(pending) = self.pending_focus {
            if let Some(idx) = self.panes.iter().position(|p| p.tmux_pane == pending) {
                self.pending_focus = None;
                let _ = self.execute_cmd(AppCmd::FocusPane(idx));
            }
        }
        self.refresh_welcome_cards();
        // The session must never be one process-exit away from vanishing.
        self.ensure_keepalive();
    }

    fn comfort_band(&self) -> (u16, u16) {
        let s = self.settings.lock().unwrap();
        let min = s.get_u64("minPaneWidth").map(|v| v as u16).unwrap_or(layout::DEFAULT_MIN_WIDTH);
        let max = s.get_u64("maxPaneWidth").map(|v| v as u16).unwrap_or(layout::DEFAULT_MAX_WIDTH);
        (min.clamp(20, 200), max.clamp(min, 400))
    }

    fn relayout(&mut self) {
        let (min_w, max_w) = self.comfort_band();
        let visible: Vec<usize> = (0..self.panes.len()).filter(|&i| !self.panes[i].hidden).collect();
        self.layout = layout::compute_with_band(self.size.0, self.size.1, visible.len(), min_w, max_w);
        for p in self.panes.iter_mut() {
            p.rect = None;
            p.dirty = true;
        }
        for (slot, &idx) in visible.iter().enumerate() {
            self.panes[idx].rect = self.layout.panes.get(slot).copied();
        }
        if self.focused >= self.panes.len() || self.panes.get(self.focused).map(|p| p.hidden).unwrap_or(true) {
            self.focused = visible.first().copied().unwrap_or(0);
        }
        self.selected = self.selected.min(self.panes.len().saturating_sub(1));
        self.dirty = true;
        self.force_full = true;
        self.apply_window_sizes();
    }

    fn apply_window_sizes(&mut self) {
        if !self.own_sizing {
            return;
        }
        let mut per_window: std::collections::HashMap<dmux_cc::WindowId, u32> = std::collections::HashMap::new();
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
                let _ = self.client.send(format!("set-option -w -t {} window-size manual", p.tmux_window));
                // User configs with `pane-border-status` steal a row INSIDE
                // the window, making the pane one row shorter than the window
                // we size — the bottom row of every pane would be invisible.
                // Scoped to our windows; the user's other sessions keep it.
                let _ = self
                    .client
                    .send(format!("set-option -w -t {} pane-border-status off", p.tmux_window));
            }
            let _ = self.client.send(format!("resize-window -t {} -x {} -y {}", p.tmux_window, rect.w, rect.h));
        }
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

    // ------------------------------------------------------------------
    // Input

    /// Returns false to quit.
    fn handle_input(&mut self, ev: InputEvent) -> bool {
        match ev {
            InputEvent::Key(key) => {
                // Overlays swallow all keys.
                if let Some(top) = self.views.last_mut() {
                    let result = top.on_key(&key);
                    self.dirty = true;
                    return self.apply_view_result(result);
                }
                let leader_was_armed = self.leader_armed;
                if leader_was_armed {
                    self.leader_armed = false;
                    self.dirty = true;
                }
                // Welcome screen owns navigation keys when no panes are visible.
                if !leader_was_armed && self.welcome_active() {
                    if let Some(handled) = self.handle_welcome_key(&key) {
                        return handled;
                    }
                }
                // Then the sidebar, while it holds focus.
                if !leader_was_armed && self.sidebar_focused {
                    if let Some(handled) = self.handle_sidebar_key(&key) {
                        return handled;
                    }
                }
                let routed = input::route_key(&key, self.focused_modes(), leader_was_armed, &self.keymap);
                self.execute_routed(routed)
            }
            InputEvent::Mouse(m) => {
                let (col, row, kind, shift) = input::classify_mouse(&m);
                self.handle_mouse(col, row, kind, shift)
            }
            InputEvent::Paste(text) => {
                if self.views.is_empty() {
                    let bytes = input::encode_paste(&text, self.focused_modes());
                    self.send_pane_bytes(&bytes);
                }
                true
            }
            InputEvent::Resized { cols, rows } => {
                self.handle_resize((cols as u16, rows as u16));
                true
            }
            _ => true,
        }
    }

    /// Sidebar-focus navigation. Returns Some(keep_running) when consumed;
    /// any unhandled key drops sidebar focus and routes normally.
    fn handle_sidebar_key(&mut self, key: &dmux_host::KeyEvent) -> Option<bool> {
        use dmux_host::KeyCode;
        if !key.modifiers.is_empty() {
            self.sidebar_focused = false;
            return None;
        }
        let len = self.panes.len();
        match key.key {
            KeyCode::UpArrow | KeyCode::Char('k') if len > 0 => {
                self.selected = (self.selected + len - 1) % len;
            }
            KeyCode::DownArrow | KeyCode::Char('j') if len > 0 => {
                self.selected = (self.selected + 1) % len;
            }
            KeyCode::Enter => {
                self.sidebar_focused = false;
                let i = self.selected;
                if self.panes.get(i).map(|p| p.hidden).unwrap_or(false) {
                    return Some(self.execute_cmd(AppCmd::ToggleHidden(i)));
                }
                return Some(self.execute_cmd(AppCmd::FocusPane(i)));
            }
            KeyCode::Char('m') | KeyCode::Char(' ') => {
                return Some(self.execute_cmd(AppCmd::OpenPaneMenu));
            }
            KeyCode::Char('h') => return Some(self.execute_cmd(AppCmd::ToggleHidden(self.selected))),
            KeyCode::Char('x') => return Some(self.execute_cmd(AppCmd::ConfirmClose(self.selected))),
            KeyCode::Escape => {
                self.sidebar_focused = false;
            }
            _ => {
                self.sidebar_focused = false;
                return None;
            }
        }
        self.dirty = true;
        Some(true)
    }

    /// Welcome-screen navigation. Returns Some(keep_running) when consumed.
    fn handle_welcome_key(&mut self, key: &dmux_host::KeyEvent) -> Option<bool> {
        use dmux_host::KeyCode;
        if !key.modifiers.is_empty() {
            return None;
        }
        let len = self.welcome_cards.len();
        if len == 0 {
            return None;
        }
        match key.key {
            KeyCode::LeftArrow => {
                self.welcome_sel = (self.welcome_sel + len - 1) % len;
            }
            KeyCode::RightArrow | KeyCode::Tab => {
                self.welcome_sel = (self.welcome_sel + 1) % len;
            }
            KeyCode::UpArrow => {
                self.welcome_sel = (self.welcome_sel + len - 2) % len;
            }
            KeyCode::DownArrow => {
                self.welcome_sel = (self.welcome_sel + 2) % len;
            }
            KeyCode::Enter => {
                let cmd = self.welcome_cards[self.welcome_sel].cmd.clone();
                return Some(self.execute_cmd(cmd));
            }
            _ => return None,
        }
        self.dirty = true;
        Some(true)
    }

    fn handle_mouse(&mut self, col: u16, row: u16, kind: MouseKind, shift: bool) -> bool {
        let target = self.click_map.hit(col, row).copied();
        let is_press = kind == MouseKind::LeftHeld && !self.mouse_down;
        match kind {
            MouseKind::LeftHeld => self.mouse_down = true,
            MouseKind::Release => self.mouse_down = false,
            _ => {}
        }
        let is_double = is_press
            && self
                .last_press
                .is_some_and(|(t, c, r)| t.elapsed() < Duration::from_millis(400) && c == col && r == row);
        if is_press {
            self.last_press = Some((Instant::now(), col, row));
        }

        // An active selection drag captures the mouse until release.
        if let Some(i) = self.drag_select {
            match kind {
                MouseKind::LeftHeld => {
                    if let Some(p) = self.panes.get_mut(i) {
                        if let Some(rect) = p.rect {
                            let c = col.clamp(rect.x, rect.right().saturating_sub(1)) - rect.x;
                            let r = row.clamp(rect.y, rect.bottom().saturating_sub(1)) - rect.y;
                            p.term.selection_update(c, r);
                            self.drag_moved = true;
                            p.dirty = true;
                            self.dirty = true;
                        }
                    }
                    return true;
                }
                MouseKind::Release => {
                    self.drag_select = None;
                    if self.drag_moved {
                        let text = self.panes.get(i).and_then(|p| p.term.selection_text());
                        if let Some(text) = text {
                            let chars = text.chars().count();
                            self.forward_clipboard(&text);
                            self.toast(format!("Copied {chars} chars"));
                        }
                    } else if let Some(p) = self.panes.get_mut(i) {
                        // A plain click: no selection, no copy.
                        p.term.selection_clear();
                        p.dirty = true;
                        self.dirty = true;
                    }
                    return true;
                }
                _ => {}
            }
        }
        // An app-mouse drag forwards motion until release.
        if let Some(i) = self.mouse_forward {
            if let Some(p) = self.panes.get(i) {
                if let Some(rect) = p.rect {
                    let pane_id = p.tmux_pane;
                    let c = col.clamp(rect.x, rect.right().saturating_sub(1)) - rect.x;
                    let r = row.clamp(rect.y, rect.bottom().saturating_sub(1)) - rect.y;
                    match kind {
                        MouseKind::LeftHeld => {
                            // Motion-while-held: SGR button 32.
                            let m = input::encode_sgr_mouse(32, true, c, r);
                            let _ = self.client.send(input::send_keys_hex(pane_id, &m));
                            return true;
                        }
                        MouseKind::Release => {
                            self.mouse_forward = None;
                            let up = input::encode_sgr_mouse(0, false, c, r);
                            let _ = self.client.send(input::send_keys_hex(pane_id, &up));
                            return true;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Modal overlays: clicks on overlay regions go to the view; wheel goes
        // to the view; clicks outside dismiss.
        if !self.views.is_empty() {
            match kind {
                MouseKind::WheelUp | MouseKind::WheelDown => {
                    let delta = if kind == MouseKind::WheelUp { -1 } else { 1 };
                    if let Some(top) = self.views.last_mut() {
                        let r = top.on_wheel(delta);
                        self.dirty = true;
                        return self.apply_view_result(r);
                    }
                }
                MouseKind::LeftHeld if is_press => match target {
                    Some(ClickTarget::Overlay(tag)) => {
                        if let Some(top) = self.views.last_mut() {
                            let r = top.on_click(tag);
                            self.dirty = true;
                            return self.apply_view_result(r);
                        }
                    }
                    _ => {
                        self.views.pop();
                        self.dirty = true;
                    }
                },
                MouseKind::LeftHeld | MouseKind::Release => {}
            }
            return true;
        }

        match kind {
            MouseKind::WheelUp | MouseKind::WheelDown => {
                let delta: i32 = if kind == MouseKind::WheelUp { 3 } else { -3 };
                match target {
                    Some(ClickTarget::SidebarRow(_)) | Some(ClickTarget::SidebarNewAgent) | Some(ClickTarget::SidebarNewTerminal) => {
                        let len = self.panes.len();
                        if len > 0 {
                            let step = if delta > 0 { -1i32 } else { 1 };
                            let cur = self.selected as i32;
                            self.selected = (cur + step).rem_euclid(len as i32) as usize;
                            self.dirty = true;
                        }
                    }
                    _ => {
                        // Wheel over a pane scrolls that pane's view.
                        if let Some(ClickTarget::PaneBody(i)) | Some(ClickTarget::PaneTitle(i)) = target {
                            if let Some(p) = self.panes.get_mut(i) {
                                p.term.scroll_view(delta);
                                p.dirty = true;
                                self.dirty = true;
                            }
                        }
                    }
                }
            }
            MouseKind::LeftHeld if is_press => match target {
                Some(ClickTarget::SidebarRow(i)) => {
                    // TS semantics: click selects (sidebar keeps the
                    // keyboard); double-click activates the pane.
                    self.selected = i;
                    if is_double {
                        self.sidebar_focused = false;
                        if self.panes.get(i).map(|p| p.hidden).unwrap_or(false) {
                            return self.execute_cmd(AppCmd::ToggleHidden(i));
                        }
                        return self.execute_cmd(AppCmd::FocusPane(i));
                    }
                    self.sidebar_focused = true;
                    self.dirty = true;
                    return true;
                }
                Some(ClickTarget::SidebarNewAgent) => return self.execute_cmd(AppCmd::OpenNewAgent),
                Some(ClickTarget::SidebarNewTerminal) => return self.execute_cmd(AppCmd::NewTerminal),
                Some(ClickTarget::SidebarNewProject) => return self.execute_cmd(AppCmd::PromptAddProject),
                Some(ClickTarget::SidebarSettings) => return self.execute_cmd(AppCmd::OpenSettings),
                Some(ClickTarget::SidebarHelp) => return self.execute_cmd(AppCmd::OpenShortcuts),
                Some(ClickTarget::SidebarIssues) => {
                    if let Some(issue) = self.filed_issues.last() {
                        let url = issue.url.clone();
                        self.new_issue_count = 0;
                        self.dirty = true;
                        tokio::task::spawn_blocking(move || {
                            let _ = std::process::Command::new("open").arg(url).status();
                        });
                    }
                    return true;
                }
                Some(ClickTarget::PaneTitle(i)) => {
                    // Double-click the title = rename.
                    if is_double {
                        return self.execute_cmd(AppCmd::PromptRename(i));
                    }
                    return self.execute_cmd(AppCmd::FocusPane(i));
                }
                Some(ClickTarget::TitleRename(i)) => return self.execute_cmd(AppCmd::PromptRename(i)),
                Some(ClickTarget::TitleHide(i)) => return self.execute_cmd(AppCmd::ToggleHidden(i)),
                Some(ClickTarget::TitleClose(i)) => return self.execute_cmd(AppCmd::ConfirmClose(i)),
                Some(ClickTarget::WelcomeCard(i)) => {
                    self.welcome_sel = i;
                    let cmd = self.welcome_cards.get(i).map(|c| c.cmd.clone());
                    if let Some(cmd) = cmd {
                        return self.execute_cmd(cmd);
                    }
                }
                Some(ClickTarget::PaneBody(i)) => {
                    self.sidebar_focused = false;
                    let already_focused = self.focused == i;
                    if !already_focused {
                        return self.execute_cmd(AppCmd::FocusPane(i));
                    }
                    if let Some(p) = self.panes.get_mut(i) {
                        let modes = p.term.input_modes();
                        let app_mouse =
                            (modes.mouse_click || modes.mouse_drag || modes.mouse_motion) && modes.sgr_mouse;
                        if let Some(rect) = p.rect {
                            let c = col - rect.x;
                            let r = row - rect.y;
                            if app_mouse && !shift {
                                // Forward the press; drag/release follow via
                                // the mouse_forward capture above.
                                let down = input::encode_sgr_mouse(0, true, c, r);
                                let pane = p.tmux_pane;
                                let _ = self.client.send(input::send_keys_hex(pane, &down));
                                self.mouse_forward = Some(i);
                            } else {
                                // dmux-side text selection (Shift forces it
                                // even when the app wants the mouse); double
                                // click selects the word under the cursor.
                                p.term.selection_clear();
                                p.term.selection_start(c, r, is_double);
                                self.drag_select = Some(i);
                                self.drag_moved = is_double;
                                p.dirty = true;
                                self.dirty = true;
                            }
                        }
                    }
                }
                Some(ClickTarget::Overlay(_)) | None => {}
            },
            MouseKind::LeftHeld | MouseKind::Release => {}
        }
        true
    }

    fn execute_routed(&mut self, routed: Routed) -> bool {
        match routed {
            Routed::Quit | Routed::Detach => return false,
            Routed::LeaderArm => {
                self.leader_armed = true;
                self.dirty = true;
            }
            Routed::ToggleHud => {
                self.hud = !self.hud;
                self.force_full = true;
                self.dirty = true;
            }
            Routed::FocusNext => return self.focus_step(1),
            Routed::FocusPrev => return self.focus_step(-1),
            Routed::FocusIndex(i) => return self.execute_cmd(AppCmd::FocusPane(i)),
            Routed::OpenMenu => return self.execute_cmd(AppCmd::OpenPaneMenu),
            Routed::OpenSettings => return self.execute_cmd(AppCmd::OpenSettings),
            Routed::OpenNewAgent => return self.execute_cmd(AppCmd::OpenNewAgent),
            Routed::OpenShortcuts => return self.execute_cmd(AppCmd::OpenShortcuts),
            Routed::OpenLogs => return self.execute_cmd(AppCmd::OpenLogs),
            Routed::SearchScrollback => {
                let last = self.last_search.clone().unwrap_or_default();
                self.views.push(Box::new(InputView::new(
                    "Search scrollback",
                    &last,
                    "text to find (searches upward)",
                    InputPurpose::SearchScrollback,
                )));
                self.dirty = true;
            }
            Routed::NewTerminal => return self.execute_cmd(AppCmd::NewTerminal),
            Routed::AddProject => return self.execute_cmd(AppCmd::PromptAddProject),
            Routed::RenameFocused => return self.execute_cmd(AppCmd::PromptRename(self.focused)),
            Routed::HideFocused => return self.execute_cmd(AppCmd::ToggleHidden(self.focused)),
            Routed::CloseFocused => return self.execute_cmd(AppCmd::ConfirmClose(self.focused)),
            Routed::PaneBytes(bytes) => self.send_pane_bytes(&bytes),
            Routed::SidebarNav(delta) => {
                self.sidebar_focused = true;
                let len = self.panes.len();
                if len > 0 {
                    self.selected =
                        ((self.selected as i32 + delta).rem_euclid(len as i32)) as usize;
                }
                self.dirty = true;
            }
            Routed::ScrollView(delta) => {
                if let Some(p) = self.panes.get_mut(self.focused) {
                    p.term.scroll_view(delta);
                    p.dirty = true;
                    self.dirty = true;
                }
            }
            Routed::Ignore => {}
        }
        true
    }

    fn focus_step(&mut self, dir: i32) -> bool {
        let visible: Vec<usize> = (0..self.panes.len()).filter(|&i| !self.panes[i].hidden).collect();
        if visible.is_empty() {
            return true;
        }
        let cur = visible.iter().position(|&i| i == self.focused).unwrap_or(0) as i32;
        let next = visible[(cur + dir).rem_euclid(visible.len() as i32) as usize];
        self.execute_cmd(AppCmd::FocusPane(next))
    }

    fn apply_view_result(&mut self, result: ViewResult) -> bool {
        match result {
            ViewResult::Stay => true,
            ViewResult::Close => {
                self.views.pop();
                self.dirty = true;
                true
            }
            ViewResult::Push(view) => {
                self.views.push(view);
                self.dirty = true;
                true
            }
            ViewResult::Cmd(cmd) => self.execute_cmd(cmd),
            ViewResult::CloseAnd(cmd) => {
                self.views.pop();
                self.dirty = true;
                self.execute_cmd(cmd)
            }
        }
    }

    // ------------------------------------------------------------------
    // Commands

    /// Returns false to quit.
    fn execute_cmd(&mut self, cmd: AppCmd) -> bool {
        match cmd {
            AppCmd::Quit => return false,
            AppCmd::FocusPane(i) => {
                self.sidebar_focused = false;
                if i < self.panes.len() && !self.panes[i].hidden {
                    self.focused = i;
                    self.selected = i;
                    self.panes[i].needs_attention = false;
                    let w = self.panes[i].tmux_window;
                    let _ = self.client.send(format!("select-window -t {w}"));
                    self.dirty = true;
                }
            }
            AppCmd::OpenPaneMenu => {
                let idx = self.selected.min(self.panes.len().saturating_sub(1));
                let mut items = Vec::new();
                if let Some(p) = self.panes.get(idx) {
                    let hide_label = if p.hidden { t("menu.show") } else { t("menu.hide") };
                    items.push(MenuItem::new(t("menu.rename"), "^b r", AppCmd::PromptRename(idx)));
                    items.push(MenuItem::new(hide_label, "^b h", AppCmd::ToggleHidden(idx)));
                    if p.worktree_path.is_some() {
                        items.push(MenuItem::new(t("menu.merge"), "", AppCmd::MergeStart(idx)));
                        items.push(MenuItem::new(t("menu.pr"), "", AppCmd::CreatePr(idx)));
                        items.push(MenuItem::new(t("menu.diff"), "", AppCmd::ShowDiff(idx)));
                        if p.agent.is_some() {
                            items.push(MenuItem::new(t("menu.duplicate"), "", AppCmd::DuplicatePane(idx)));
                        }
                    }
                    if p.agent.is_some() {
                        let ap = if p.autopilot { t("menu.autopilot_off") } else { t("menu.autopilot_on") };
                        items.push(MenuItem::new(ap, "", AppCmd::ToggleAutopilot(idx)));
                    }
                    let hook_root = p
                        .project_root
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.project_root.clone());
                    for (hook, label) in [("run_test", t("menu.run_test")), ("run_dev", t("menu.run_dev"))] {
                        if hooks::hook_path(&hook_root, hook).is_some() {
                            items.push(MenuItem::new(label, "", AppCmd::RunHook { idx, name: hook.into() }));
                        }
                    }
                    items.push(MenuItem::new(t("menu.copy_path"), "", AppCmd::CopyPath(idx)));
                    items.push(MenuItem::new(t("menu.editor"), "", AppCmd::OpenInEditor(idx)));
                    items.push(MenuItem::new(t("menu.close"), "^b x", AppCmd::ConfirmClose(idx)).danger());
                }
                items.push(MenuItem::new(t("menu.new_agents"), "^b n", AppCmd::OpenNewAgent));
                items.push(MenuItem::new(t("menu.new_terminal"), "^b t", AppCmd::NewTerminal));
                items.push(MenuItem::new(t("menu.add_project"), "^b p", AppCmd::PromptAddProject));
                items.push(MenuItem::new(t("menu.settings"), "^b s", AppCmd::OpenSettings));
                items.push(MenuItem::new(t("menu.logs"), "^b l", AppCmd::OpenLogs));
                items.push(MenuItem::new(t("menu.shortcuts"), "^b ?", AppCmd::OpenShortcuts));
                items.push(MenuItem::new(t("menu.detach"), "^b d", AppCmd::Quit));
                let title = self
                    .panes
                    .get(idx)
                    .map(|p| p.display_title().to_string())
                    .unwrap_or_else(|| "dmux".into());
                self.views.push(Box::new(MenuView::new(title, items)));
                self.dirty = true;
            }
            AppCmd::OpenSettings => {
                let has_project = self.settings.lock().unwrap().has_project_scope();
                let root = self
                    .active_project_root()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.project_root.clone());
                self.views.push(Box::new(SettingsView::new(self.settings.clone(), has_project, root)));
                self.dirty = true;
            }
            AppCmd::OpenNewAgent => {
                let (default_agent, default_mode, enabled) = {
                    let s = self.settings.lock().unwrap();
                    let enabled = s
                        .get("enabledAgents")
                        .and_then(|v| v.as_array().cloned())
                        .map(|l| l.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
                        .unwrap_or_else(|| {
                            agents::AGENTS.iter().filter(|a| a.default_enabled).map(|a| a.id.to_string()).collect()
                        });
                    (
                        s.get_str("defaultAgent").map(|v| v.to_string()),
                        s.get_str("permissionMode").unwrap_or("").to_string(),
                        enabled,
                    )
                };
                self.views.push(Box::new(AgentSelectView::new(
                    &self.installed_agents,
                    &enabled,
                    default_agent.as_deref(),
                    &default_mode,
                )));
                self.dirty = true;
            }
            AppCmd::OpenShortcuts => {
                self.views.push(Box::new(ShortcutsView::new(
                    self.host.caps().kitty_keyboard,
                    self.keymap.describe(),
                )));
                self.dirty = true;
            }
            AppCmd::OpenLogs => {
                self.views.push(Box::new(views::LogsView::new(self.log_path.clone())));
                self.dirty = true;
            }
            AppCmd::PromptRename(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    self.views.push(Box::new(InputView::new(
                        t("dialog.rename_title"),
                        p.display_title(),
                        "pane name",
                        InputPurpose::RenamePane(idx),
                    )));
                    self.dirty = true;
                }
            }
            AppCmd::ConfirmClose(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    self.views.push(Box::new(ConfirmView::new(
                        t("dialog.close_title"),
                        tf("dialog.close_body", p.display_title()),
                        t("dialog.close_confirm"),
                        true,
                        AppCmd::ClosePane(idx),
                    )));
                    self.dirty = true;
                }
            }
            AppCmd::CopyPath(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    let path = p
                        .worktree_path
                        .clone()
                        .unwrap_or_else(|| self.project_root.to_string_lossy().into_owned());
                    self.forward_clipboard(&path);
                    self.toast(format!("Copied {path}"));
                }
            }
            AppCmd::OpenInEditor(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    let path = p
                        .worktree_path
                        .clone()
                        .unwrap_or_else(|| self.project_root.to_string_lossy().into_owned());
                    let n = 1 + self.panes.iter().filter(|q| q.slug.starts_with("editor-")).count();
                    self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                        slug: format!("editor-{n}"),
                        display: format!("edit: {}", p.display_title()),
                        kind: PaneKind::Shell,
                        agent: None,
                        launch_cmd: Some("${EDITOR:-vi} .".into()),
                        injection: None,
                        worktree_path: Some(path.clone()),
                        cwd: Some(path),
                        project_root: None,
                    });
                }
            }
            AppCmd::MergeStart(idx) => {
                let Some(p) = self.panes.get(idx) else { return true };
                let Some(wt) = p.worktree_path.clone() else { return true };
                let slug = p.slug.clone();
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| slug.clone());
                let root_branch = git::current_branch(&self.project_root).unwrap_or_else(|| "HEAD".into());
                if git::worktree_dirty(&wt_path) {
                    self.views.push(Box::new(InputView::new(
                        format!("Commit & merge '{branch}' into '{root_branch}'"),
                        "",
                        "commit message for uncommitted changes",
                        InputPurpose::MergeCommitMessage { slug },
                    )));
                } else {
                    self.views.push(Box::new(ConfirmView::new(
                        "Merge worktree",
                        format!("Merge '{branch}' into '{root_branch}'?"),
                        "Merge",
                        false,
                        AppCmd::MergeExec { slug, message: None },
                    )));
                }
                self.dirty = true;
            }
            AppCmd::MergeExec { slug, message } => {
                let Some(p) = self.panes.iter().find(|p| p.slug == slug) else { return true };
                let Some(wt) = p.worktree_path.clone() else { return true };
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| slug.clone());
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                self.toast(format!("Merging '{branch}'…"));
                tokio::task::spawn_blocking(move || {
                    let result = git::commit_and_merge(&root, &wt_path, &branch, message.as_deref());
                    let _ = tx.send(AppMsg::MergeDone { slug, branch, result });
                });
            }
            AppCmd::Noop => {}
            AppCmd::ShowDiff(idx) => {
                let Some(p) = self.panes.get(idx) else { return true };
                let Some(wt) = p.worktree_path.clone() else { return true };
                let title = format!("Diff — {}", p.display_title());
                self.views.push(Box::new(views::DiffView::new(title, PathBuf::from(wt))));
                self.dirty = true;
            }
            AppCmd::DuplicatePane(idx) => {
                let Some(p) = self.panes.get(idx) else { return true };
                let (Some(agent), slug) = (p.agent.clone(), p.slug.clone()) else {
                    self.toast("Only agent panes can be duplicated");
                    return true;
                };
                let prompt = self
                    .config
                    .panes
                    .iter()
                    .find(|r| r.slug == slug)
                    .map(|r| r.prompt.clone())
                    .unwrap_or_default();
                let mode = self.settings.lock().unwrap().get_str("permissionMode").unwrap_or("").to_string();
                self.launch_agents(prompt, vec![(agent, 1)], mode);
            }
            AppCmd::ToggleAutopilot(idx) => {
                let Some(p) = self.panes.get_mut(idx) else { return true };
                p.autopilot = !p.autopilot;
                let on = p.autopilot;
                let slug = p.slug.clone();
                let title = p.display_title().to_string();
                if let Some(rec) = self.config.panes.iter_mut().find(|r| r.slug == slug) {
                    rec.autopilot = on.then_some(true);
                    self.save_config();
                }
                self.toast(format!("Autopilot {} for '{title}'", if on { "on" } else { "off" }));
            }
            AppCmd::RunHook { idx, name } => {
                let Some(p) = self.panes.get(idx) else { return true };
                let root = p
                    .project_root
                    .clone()
                    .unwrap_or_else(|| self.project_root.to_string_lossy().into_owned());
                let cwd = p.worktree_path.clone().unwrap_or_else(|| root.clone());
                let n = 1 + self.panes.iter().filter(|q| q.slug.starts_with("hook-")).count();
                let label = if name == "run_test" { "tests" } else { "dev" };
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug: format!("hook-{n}"),
                    display: format!("{label}: {}", p.display_title()),
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: Some(format!(
                        "clear; DMUX_ROOT={r} DMUX_WORKTREE_PATH={w} {r}/.dmux-hooks/{name}; echo; echo '[hook exited — close this pane when finished]'",
                        r = shq(&root),
                        w = shq(&cwd),
                    )),
                    injection: None,
                    worktree_path: None,
                    cwd: Some(cwd),
                    project_root: Some(root),
                });
            }
            AppCmd::SearchScrollback(query) => {
                self.last_search = Some(query.clone());
                if let Some(p) = self.panes.get_mut(self.focused) {
                    match p.term.search_back(&query) {
                        Some(offset) => {
                            p.dirty = true;
                            self.dirty = true;
                            self.toast(format!("Found '{query}' ({offset} lines back) — ⌥PgDn to return"));
                        }
                        None => self.toast(format!("No match for '{query}' above")),
                    }
                }
            }
            AppCmd::AiMerge { branch } => {
                let (Some(primary), backup) = (self.inference_primary.clone(), self.inference_backup.clone()) else {
                    self.toast("No inference provider configured");
                    return true;
                };
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                let b = branch.clone();
                self.toast(format!("AI-merging '{branch}'…"));
                tokio::spawn(async move {
                    let result = ai_merge(&root, &b, &primary, backup.as_ref()).await;
                    let _ = tx.send(AppMsg::AiMergeDone { branch: b, result });
                });
            }
            AppCmd::ResolveConflicts { branch } => {
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                let b = branch.clone();
                self.toast("Re-establishing conflict state…");
                tokio::task::spawn_blocking(move || {
                    let files = git::merge_leaving_conflicts(&root, &b);
                    let _ = tx.send(AppMsg::ConflictsReady { branch: b, files });
                });
            }
            AppCmd::MergeCleanup { slug } => {
                if let Some(idx) = self.panes.iter().position(|p| p.slug == slug) {
                    let wt = self.panes[idx].worktree_path.clone();
                    let branch = wt
                        .as_deref()
                        .map(PathBuf::from)
                        .and_then(|p| git::current_branch(&p))
                        .unwrap_or_else(|| slug.clone());
                    self.close_pane(idx);
                    if let Some(wt) = wt {
                        let root = self.project_root.clone();
                        let wt_path = PathBuf::from(wt);
                        let tx = self.app_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let env = [
                                ("DMUX_WORKTREE_PATH", wt_path.to_string_lossy().into_owned()),
                                ("DMUX_BRANCH", branch.clone()),
                            ];
                            hooks::run_detached(&root, "before_worktree_remove", &root, &env);
                            let _ = git::cleanup_worktree(&root, &wt_path, &branch);
                            hooks::run_detached(&root, "worktree_removed", &root, &env);
                            let _ = tx.send(AppMsg::RefreshDerived);
                        });
                    }
                    self.toast("Worktree merged and cleaned up");
                }
            }
            AppCmd::CreatePr(idx) => {
                let Some(p) = self.panes.get(idx) else { return true };
                let Some(wt) = p.worktree_path.clone() else { return true };
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| p.slug.clone());
                if git::worktree_dirty(&wt_path) {
                    self.toast("Uncommitted changes — merge flow can commit them first");
                    return true;
                }
                // Interactive in a pane so gh auth/questions stay visible.
                let n = 1 + self.panes.iter().filter(|q| q.slug.starts_with("pr-")).count();
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug: format!("pr-{n}"),
                    display: format!("PR: {branch}"),
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: Some(format!(
                        "clear; git push -u origin {b} && gh pr create --head {b} --fill; echo; echo '[done — close this pane when finished]'",
                        b = shq(&branch)
                    )),
                    injection: None,
                    worktree_path: Some(wt.clone()),
                    cwd: Some(wt),
                    project_root: None,
                });
            }
            AppCmd::RenamePane { idx, name } => self.rename_pane(idx, name),
            AppCmd::ToggleHidden(idx) => self.toggle_hidden(idx),
            AppCmd::ClosePane(idx) => self.close_pane(idx),
            AppCmd::NewTerminal => self.new_terminal(),
            AppCmd::PromptAddProject => {
                self.views.push(Box::new(InputView::new(
                    "Add project",
                    "",
                    "path to a project directory (~ ok)",
                    InputPurpose::AddProjectPath,
                )));
                self.dirty = true;
            }
            AppCmd::OpenProjectAt(raw) => {
                let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                    dirs_home().join(rest)
                } else if raw == "~" {
                    dirs_home()
                } else {
                    PathBuf::from(&raw)
                };
                if !expanded.is_dir() {
                    self.toast(format!("Not a directory: {}", expanded.display()));
                } else {
                    let name = expanded
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| raw.clone());
                    let root = expanded.to_string_lossy().into_owned();
                    // Register in sidebarProjects (TS-compatible shape).
                    let entry = serde_json::json!({"projectRoot": root, "projectName": name});
                    let list = self
                        .config
                        .extra
                        .entry("sidebarProjects".to_string())
                        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                    let mut added = false;
                    if let Some(arr) = list.as_array_mut() {
                        if !arr.iter().any(|p| p["projectRoot"].as_str() == Some(root.as_str())) {
                            arr.push(entry);
                            added = true;
                        }
                    }
                    if added {
                        self.save_config();
                        self.toast(format!("Added project '{name}'"));
                    }
                    return self.execute_cmd(AppCmd::NewTerminalAt { path: root, name });
                }
            }
            AppCmd::ResumeWorktree { path, slug, agent } => {
                let mode = {
                    let s = self.settings.lock().unwrap();
                    s.get_str("permissionMode").unwrap_or("").to_string()
                };
                // Prefer the exact captured session id when tracking saved one.
                let session_id = self
                    .config
                    .panes
                    .iter()
                    .find(|r| r.slug == slug)
                    .and_then(|r| r.extra.get("agentSessionId"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let cmd = agents::agent(&agent)
                    .and_then(|def| agents::compose_resume_session(def, session_id.as_deref(), &mode))
                    .unwrap_or_else(|| agent.clone());
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    display: slug.clone(),
                    slug,
                    kind: PaneKind::Worktree,
                    agent: Some(agent),
                    launch_cmd: Some(format!("clear; {cmd}")),
                    injection: None,
                    worktree_path: Some(path.clone()),
                    cwd: Some(path),
                    project_root: None,
                });
                self.toast("Resuming agent session…");
            }
            AppCmd::NewTerminalAt { path, name } => {
                let n = 1 + self.panes.iter().filter(|p| p.slug.starts_with("terminal-")).count();
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug: format!("terminal-{n}"),
                    display: name,
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: None,
                    injection: None,
                    worktree_path: Some(path.clone()),
                    cwd: Some(path.clone()),
                    project_root: (PathBuf::from(&path) != self.project_root).then_some(path),
                });
            }
            AppCmd::LaunchAgents { prompt, allocations, mode } => self.launch_agents(prompt, allocations, mode),
            AppCmd::SetSetting { key, value, scope } => self.set_setting(&key, value, scope),
        }
        true
    }

    fn set_setting(&mut self, key: &str, value: serde_json::Value, scope: SettingsScope) {
        {
            let mut s = self.settings.lock().unwrap();
            let unset = value.as_str().map(|v| v.is_empty()).unwrap_or(false)
                && matches!(key, "baseBranch");
            if unset {
                s.unset(key, scope);
            } else {
                s.set(key, value, scope);
            }
            if let Err(err) = s.save(scope) {
                tracing::warn!(%err, key, "settings save failed");
            }
        }
        match key {
            "colorTheme" => {
                let name = {
                    let s = self.settings.lock().unwrap();
                    s.get_str("colorTheme").unwrap_or("violet").to_string()
                };
                self.theme = Theme::named(&name);
                self.force_full = true;
            }
            "minPaneWidth" | "maxPaneWidth" => self.relayout(),
            "language" => {
                let lang = {
                    let s = self.settings.lock().unwrap();
                    s.get_str("language").unwrap_or("en").to_string()
                };
                dmux_core::i18n::set_locale(&lang);
                self.force_full = true;
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn rename_pane(&mut self, idx: usize, name: String) {
        let Some(p) = self.panes.get_mut(idx) else { return };
        p.title = name.clone();
        p.auto_name = false;
        let encoded = encode_pane_title(&name, &p.slug);
        let _ = self
            .client
            .send(format!("select-pane -t {} -T {}", p.tmux_pane, dmux_cc::quote_arg(&encoded)));
        let slug = p.slug.clone();
        self.update_config_pane(&slug, |rec| {
            rec.display_name = Some(name.clone());
        });
        self.toast(format!("Renamed to '{name}'"));
    }

    fn toggle_hidden(&mut self, idx: usize) {
        let Some(p) = self.panes.get_mut(idx) else { return };
        p.hidden = !p.hidden;
        let hidden = p.hidden;
        let pane_id = p.tmux_pane;
        let slug = p.slug.clone();
        if hidden {
            let _ = self.client.send(format!("refresh-client -A '{pane_id}:off'"));
        } else {
            let _ = self.client.send(format!("refresh-client -A '{pane_id}:on'"));
            p.begin_reseed();
            let _ = self.client.send_tagged(p.seed_command(), Tag::Seed(pane_id));
            let _ = self.client.send_tagged(p.cursor_command(), Tag::Cursor(pane_id));
        }
        self.update_config_pane(&slug, |rec| {
            rec.hidden = hidden.then_some(true);
        });
        self.relayout();
        self.toast(if hidden { t("toast.pane_hidden") } else { t("toast.pane_shown") });
    }

    fn close_pane(&mut self, idx: usize) {
        if idx >= self.panes.len() {
            return;
        }
        // Closing the last pane must not destroy the session: the keepalive
        // window is created FIRST (FIFO command order guarantees it exists
        // before the kill lands).
        if self.panes.len() == 1 {
            self.ensure_keepalive();
        }
        let pane = self.panes.remove(idx);
        self.bootstraps.remove(&pane.slug);
        let hook_root = pane
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let hook_cwd = pane.worktree_path.clone().map(PathBuf::from).unwrap_or_else(|| hook_root.clone());
        let hook_env = [("DMUX_SLUG", pane.slug.clone()), ("DMUX_PANE_ID", pane.tmux_pane.to_string())];
        hooks::run_detached(&hook_root, "before_pane_close", &hook_cwd, &hook_env);
        self.closing.insert(pane.tmux_pane);
        let _ = self.client.send(format!("kill-window -t {}", pane.tmux_window));
        hooks::run_detached(&hook_root, "pane_closed", &hook_root, &hook_env);
        self.config.panes.retain(|r| r.slug != pane.slug);
        self.save_config();
        if self.focused >= self.panes.len() {
            self.focused = self.panes.len().saturating_sub(1);
        }
        self.relayout();
        self.toast(format!("Closed '{}'", pane.display_title()));
    }

    fn new_terminal(&mut self) {
        let n = 1 + self
            .panes
            .iter()
            .filter(|p| p.slug.starts_with("terminal-"))
            .count();
        let slug = format!("terminal-{n}");
        self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
            display: slug.clone(),
            slug,
            kind: PaneKind::Shell,
            agent: None,
            launch_cmd: None,
            injection: None,
            worktree_path: None,
            cwd: None,
            project_root: self.active_project_root(),
        });
    }

    fn launch_agents(&mut self, prompt: String, allocations: Vec<(String, u8)>, mode: String) {
        let total: u32 = allocations.iter().map(|(_, c)| *c as u32).sum();
        if total == 0 {
            return;
        }
        let base_slug = slugify(&prompt);
        let (base_branch, branch_prefix) = {
            let s = self.settings.lock().unwrap();
            (
                s.get_str("baseBranch").unwrap_or("").to_string(),
                s.get_str("branchPrefix").unwrap_or("").to_string(),
            )
        };

        for (agent_id, count) in &allocations {
            let Some(def) = agents::agent(agent_id) else { continue };
            for i in 1..=*count {
                let mut slug = if total == 1 {
                    base_slug.clone()
                } else {
                    format!("{base_slug}-{}-{i}", def.short)
                };
                // Uniquify against existing records.
                let mut n = 1;
                while self.config.panes.iter().any(|p| p.slug == slug) || self.panes.iter().any(|p| p.slug == slug) {
                    n += 1;
                    slug = format!("{base_slug}-{}-{n}", def.short);
                }

                let prompt_file = (!prompt.is_empty()).then(|| {
                    let dir = self.project_root.join(".dmux").join("prompts");
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join(format!("{slug}-{}.txt", timestamp()));
                    let _ = std::fs::write(&path, &prompt);
                    path.to_string_lossy().into_owned()
                });

                let injection = match def.transport {
                    agents::Transport::SendKeys { ready_delay_ms } if !prompt.is_empty() => {
                        Some((prompt.clone(), ready_delay_ms))
                    }
                    _ => None,
                };
                let agent_cmd = agents::compose_launch(def, prompt_file.as_deref(), &mode);

                // Git projects bootstrap natively: the pane opens straight
                // into a loader card while dmux runs worktree add + the
                // worktree_created hook itself, then starts the agent.
                let (launch_cmd, injection, worktree_path, bootstrap) = if self.is_git {
                    let branch = format!("{branch_prefix}{slug}");
                    let wt = self.project_root.join(".dmux").join("worktrees").join(&slug);
                    let wt_str = wt.to_string_lossy().into_owned();
                    let root = self.project_root.to_string_lossy().into_owned();
                    let spec = BootstrapSpec {
                        plan: bootstrap::Plan {
                            root: root.clone(),
                            wt: wt_str.clone(),
                            branch,
                            base_branch: base_branch.clone(),
                            slug: slug.clone(),
                            has_hook: hooks::hook_path(&self.project_root, "worktree_created").is_some(),
                        },
                        launch: bootstrap::Launch { agent_cmd, wt: wt_str.clone(), root, injection },
                        agent_label: def.name.to_string(),
                    };
                    (None, None, Some(wt_str), Some(spec))
                } else {
                    (Some(format!("clear; {agent_cmd}")), injection, None, None)
                };

                self.create_window(NewWindowCtx {
                    bootstrap,
                    prompt: prompt.clone(),
                    display: if total == 1 { base_slug.clone() } else { format!("{base_slug} ({}{i})", def.short) },
                    slug,
                    kind: PaneKind::Worktree,
                    agent: Some(def.id.to_string()),
                    launch_cmd,
                    injection,
                    worktree_path,
                    cwd: None,
                    project_root: self.active_project_root(),
                });
            }
        }
        self.toast(format!(
            "Launching {total} pane{}…",
            if total == 1 { "" } else { "s" }
        ));
    }

    fn create_window(&mut self, ctx: NewWindowCtx) {
        let cwd = ctx
            .cwd
            .clone()
            .or_else(|| ctx.project_root.clone())
            .unwrap_or_else(|| self.project_root.to_string_lossy().into_owned());
        let hook_root = ctx
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        hooks::run_detached(
            &hook_root,
            "before_pane_create",
            &hook_root,
            &[("DMUX_SLUG", ctx.slug.clone())],
        );
        let _ = self.client.send_tagged(
            format!("new-window -d -P -F '#{{window_id}}\u{1}#{{pane_id}}' -c {}", dmux_cc::quote_arg(&cwd)),
            Tag::NewWindow(Box::new(ctx)),
        );
    }

    fn finish_new_window(&mut self, mut ctx: NewWindowCtx, reply: &Reply) {
        let line = reply.text_lines().into_iter().next().unwrap_or_default();
        let mut parts = line.split('\u{1}');
        let (Some(_window), Some(pane_str)) = (parts.next(), parts.next()) else {
            self.toast("Pane creation failed");
            return;
        };
        let Some(pane_id) = pane_str.strip_prefix('%').and_then(|s| s.parse().ok()).map(PaneId) else {
            self.toast("Pane creation failed");
            return;
        };

        let encoded = encode_pane_title(&ctx.display, &ctx.slug);
        let _ = self
            .client
            .send(format!("select-pane -t {pane_id} -T {}", dmux_cc::quote_arg(&encoded)));

        if let Some(spec) = ctx.bootstrap.take() {
            let steps = bootstrap::Ui::step_labels(&spec.agent_label, spec.plan.has_hook);
            self.bootstraps.insert(
                ctx.slug.clone(),
                bootstrap::Ui {
                    pane: pane_id,
                    title: ctx.display.clone(),
                    agent_label: spec.agent_label,
                    branch: spec.plan.branch.clone(),
                    steps,
                    current: 0,
                    detail: String::new(),
                    started: Instant::now(),
                    done_at: None,
                    failed: None,
                    launch: Some(spec.launch),
                },
            );
            let slug = ctx.slug.clone();
            let tx = self.app_tx.clone();
            tokio::task::spawn_blocking(move || {
                bootstrap::run_blocking(&spec.plan, &mut |ev| {
                    let _ = tx.send(AppMsg::Bootstrap { slug: slug.clone(), ev });
                });
            });
        }

        if let Some(cmd) = &ctx.launch_cmd {
            let mut bytes = cmd.clone().into_bytes();
            bytes.push(b'\r');
            for chunk in bytes.chunks(256) {
                let _ = self.client.send(input::send_keys_hex(pane_id, chunk));
            }
        }
        if let Some((prompt, delay_ms)) = &ctx.injection {
            self.pending_injections.push((
                pane_id,
                prompt.clone(),
                Instant::now() + Duration::from_millis(*delay_ms),
            ));
        }

        let hook_root = ctx
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let hook_cwd = ctx.worktree_path.clone().map(PathBuf::from).unwrap_or_else(|| hook_root.clone());
        let mut hook_env = vec![("DMUX_SLUG", ctx.slug.clone()), ("DMUX_PANE_ID", pane_id.to_string())];
        if let Some(wt) = &ctx.worktree_path {
            hook_env.push(("DMUX_WORKTREE_PATH", wt.clone()));
        }
        hooks::run_detached(&hook_root, "pane_created", &hook_cwd, &hook_env);

        // Config record first so reconcile adoption pairs slug → agent.
        // Resumed worktrees reuse their existing record (fresh pane id).
        if let Some(existing) = self.config.panes.iter_mut().find(|r| r.slug == ctx.slug) {
            existing.pane_id = pane_id.to_string();
            existing.agent = ctx.agent.clone().or_else(|| existing.agent.clone());
        } else {
            let mut record = DmuxPane::new_record(
                format!("pane-{}", timestamp()),
                ctx.slug.clone(),
                pane_id.to_string(),
                ctx.kind,
            );
            // A display equal to the slug is a default, not a chosen name —
            // leave it unset so shell panes auto-name from their own titles.
            record.display_name = (ctx.display != ctx.slug).then(|| ctx.display.clone());
            record.prompt = ctx.prompt.clone();
            record.agent = ctx.agent.clone();
            if ctx.agent.is_some() {
                let on = self.settings.lock().unwrap().get_bool("enableAutopilotByDefault", false);
                record.autopilot = on.then_some(true);
            }
            record.worktree_path = ctx.worktree_path.clone();
            record.project_root = ctx.project_root.clone();
            record.project_name = ctx.project_root.as_deref().map(|r| {
                std::path::Path::new(r)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.to_string())
            });
            self.config.panes.push(record);
        }
        self.save_config();

        self.pending_focus = Some(pane_id);
        self.request_reconcile();
    }

    fn update_config_pane(&mut self, slug: &str, f: impl FnOnce(&mut DmuxPane)) {
        if let Some(rec) = self.config.panes.iter_mut().find(|p| p.slug == slug) {
            f(rec);
        } else {
            return;
        }
        self.save_config();
    }

    fn save_config(&mut self) {
        if let Some(obj) = self.config.extra.get_mut("lastUpdated") {
            *obj = serde_json::Value::String(iso_now());
        } else {
            self.config
                .extra
                .insert("lastUpdated".into(), serde_json::Value::String(iso_now()));
        }
        match self.config.save(&self.config_path) {
            Ok(()) => {
                self.config_persisted = true;
            }
            Err(err) => tracing::warn!(%err, "config save failed"),
        }
    }

    fn focused_modes(&self) -> dmux_vt::InputModes {
        self.panes
            .get(self.focused)
            .map(|p| p.term.input_modes())
            .unwrap_or_default()
    }

    fn send_pane_bytes(&mut self, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(self.focused) else { return };
        if p.status == PaneStatus::Dead || p.hidden {
            return;
        }
        if p.term.selection_clear() {
            p.dirty = true;
            self.dirty = true;
        }
        if p.term.display_offset() > 0 {
            p.term.scroll_to_bottom();
            p.dirty = true;
            self.dirty = true;
        }
        for chunk in bytes.chunks(256) {
            let _ = self.client.send(input::send_keys_hex(p.tmux_pane, chunk));
        }
    }

    // ------------------------------------------------------------------
    // Deadlines + rendering

    fn handle_deadlines(&mut self) {
        let now = Instant::now();
        // Settle classification: quiet panes get a heuristic verdict
        // (working spinner text / waiting on the user / idle).
        let focused_pane = self.panes.get(self.focused).map(|p| p.tmux_pane);
        let mut attention: Option<String> = None;
        for p in &mut self.panes {
            if p.status != PaneStatus::Working {
                continue;
            }
            let Some(t) = p.last_output else { continue };
            if now.duration_since(t) < SETTLE_AFTER {
                continue;
            }
            // Working/waiting classification is for agent panes; plain
            // terminals settle straight to idle (shell echoes would pollute
            // the heuristics — and TS never statused terminals either).
            if p.agent.is_none() {
                p.status = PaneStatus::Idle;
                self.dirty = true;
                // LLM terminal naming (TerminalPaneNamingService port):
                // settled terminal output names the pane. Untitled panes name
                // eagerly; already-LLM-named panes re-check on a relaxed
                // cadence; human renames are protected (auto_name = false).
                let due = p.auto_name
                    && match p.llm_named_at {
                        None => true,
                        Some(at) => now.duration_since(at) >= Duration::from_secs(120),
                    };
                if due && !p.analysis_inflight {
                    if let Some(primary) = &self.inference_primary {
                        let tail = p.term.read_tail_text(25);
                        if tail.trim().len() >= 20 {
                            p.analysis_inflight = true;
                            let pane = p.tmux_pane;
                            let primary = primary.clone();
                            let backup = self.inference_backup.clone();
                            let tx = self.app_tx.clone();
                            tokio::spawn(async move {
                                let result = dmux_infer::generate(
                                    &dirs_home(),
                                    Some(&primary),
                                    backup.as_ref(),
                                    "You name terminal panes. Given terminal output, reply with ONLY a lowercase 2-4 word name describing what is happening (e.g. 'vite dev server', 'db migration'). No quotes, no punctuation.",
                                    &tail,
                                    16,
                                )
                                .await;
                                if let Ok(name) = result {
                                    let name: String = name
                                        .trim()
                                        .trim_matches(['"', '\'', '.'])
                                        .chars()
                                        .take(32)
                                        .collect();
                                    if !name.is_empty() && !name.contains('\n') {
                                        let _ = tx.send(AppMsg::NamingDone { pane, name });
                                        return;
                                    }
                                }
                                // Clear the in-flight flag even on failure.
                                let _ = tx.send(AppMsg::NamingDone { pane, name: String::new() });
                            });
                        }
                    }
                }
                continue;
            }
            let tail = p.term.read_tail_text(30);
            let verdict = p.engine.on_settle(&tail, p.agent.as_deref());
            tracing::debug!(pane = %p.tmux_pane, ?verdict, tail_tail = %tail.lines().rev().take(3).collect::<Vec<_>>().join(" | "), "settle verdict");
            let is_focused = focused_pane == Some(p.tmux_pane);
            match verdict {
                dmux_status::Activity::Working => {
                    // Still working without output (thinking); recheck later.
                    p.last_output = Some(now);
                }
                verdict @ (dmux_status::Activity::Waiting | dmux_status::Activity::Idle) => {
                    // Heuristic verdict paints the glyph immediately. When
                    // inference is configured, attention waits for the LLM
                    // (the TS contract: attention only after analysis).
                    p.status = if verdict == dmux_status::Activity::Waiting {
                        PaneStatus::Waiting
                    } else {
                        PaneStatus::Idle
                    };
                    self.dirty = true;
                    if let (Some(primary), false) = (&self.inference_primary, p.analysis_inflight) {
                        p.analysis_inflight = true;
                        let pane = p.tmux_pane;
                        let primary = primary.clone();
                        let backup = self.inference_backup.clone();
                        let tail = tail.clone();
                        let tx = self.app_tx.clone();
                        tokio::spawn(async move {
                            let result = dmux_infer::generate(
                                &dirs_home(),
                                Some(&primary),
                                backup.as_ref(),
                                dmux_infer::STATE_PROMPT,
                                &format!("Analyze this terminal output and return a JSON object with the state:\n\n{tail}"),
                                40,
                            )
                            .await
                            .map(|text| dmux_infer::parse_state(&text))
                            .map_err(|e| e.to_string());
                            let _ = tx.send(AppMsg::AnalysisDone { pane, verdict: result });
                        });
                    } else if !is_focused && !p.needs_attention {
                        // Heuristic-only path (no inference configured).
                        p.needs_attention = true;
                        attention = Some(if p.status == PaneStatus::Waiting {
                            format!("△ {} needs input", p.display_title())
                        } else {
                            format!("✓ {} finished", p.display_title())
                        });
                    }
                }
            }
        }
        if let Some(msg) = attention {
            self.attention_toast(msg);
        }
        // Flood-throttled panes due for a refresh.
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
        // Prompt injections for send-keys transport agents.
        let due: Vec<(PaneId, String)> = {
            let mut d = Vec::new();
            self.pending_injections.retain(|(pane, prompt, at)| {
                if now >= *at {
                    d.push((*pane, prompt.clone()));
                    false
                } else {
                    true
                }
            });
            d
        };
        for (pane, prompt) in due {
            let mut bytes = prompt.into_bytes();
            bytes.push(b'\r');
            for chunk in bytes.chunks(256) {
                let _ = self.client.send(input::send_keys_hex(pane, chunk));
            }
        }
        if !self.tracking_inflight
            && now.duration_since(self.last_tracking) >= tracking_interval()
        {
            let targets: Vec<(String, u32)> = self
                .panes
                .iter()
                .filter(|p| p.agent.is_some() && p.pane_pid > 0 && p.status != PaneStatus::Dead)
                .map(|p| (p.slug.clone(), p.pane_pid))
                .collect();
            if !targets.is_empty() {
                tracing::debug!(count = targets.len(), "tracking sweep starting");
                self.tracking_inflight = true;
                self.last_tracking = now;
                let tx = self.app_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let observations = tracking::observe(&targets);
                    let _ = tx.send(AppMsg::TrackingDone(observations));
                });
            } else {
                self.last_tracking = now;
            }
        }
        // Shadow verifier: compare one settled pane per tick against tmux's
        // authoritative grid (bounded to one capture in flight per sweep).
        if self.verify_enabled {
            if let Some(p) = self
                .panes
                .iter_mut()
                .find(|p| verify::eligible(p, now))
            {
                p.last_verify = Some(now);
                let _ = self
                    .client
                    .send_tagged(p.seed_command_visible(), Tag::VerifyCap(p.tmux_pane));
            }
        }
        // Finished bootstrap loaders linger briefly (success: long enough for
        // the agent to paint under them; failure: long enough to read why).
        let before = self.bootstraps.len();
        self.bootstraps.retain(|_, ui| match (ui.done_at, ui.failed.is_some()) {
            (Some(at), false) => now.duration_since(at) < Duration::from_millis(1500),
            (Some(at), true) => now.duration_since(at) < Duration::from_secs(6),
            _ => true,
        });
        if self.bootstraps.len() != before {
            self.dirty = true;
        }
        if let Some(at) = self.status_clear_at {
            if now >= at {
                self.status_clear_at = None;
                // Empty = idle; render_frame fills it (update notice > tips >
                // static leader hint).
                self.status_msg = match &self.update_available {
                    Some(v) => format!("⬆ dmux-rs {v} available · npm i -g dmux-rs"),
                    None => String::new(),
                };
                self.dirty = true;
            }
        }
        if self.animating() {
            self.anim = self.anim.wrapping_add(1);
            if self.welcome_active() {
                self.welcome_rain.step();
            }
            self.dirty = true;
        }
        if self.hud {
            self.dirty = true;
        }
        if self.dirty && now.duration_since(self.last_frame) >= FRAME_INTERVAL {
            self.render_frame();
        }
    }

    fn render_if_due(&mut self) {
        if self.dirty && self.last_frame.elapsed() >= FRAME_INTERVAL {
            self.render_frame();
        } else if self.dirty {
            self.metrics.coalesced += 1;
        }
    }

    fn render_frame(&mut self) {
        let start = Instant::now();
        self.click_map.clear();
        // Clear the canvas: anything not repainted this frame (welcome
        // remnants after a pane appears, closed dialogs, shrunk layouts)
        // must not survive. The diff still only emits actual changes.
        self.back.fill(self.back.area(), &dmux_compositor::Cell::default());
        let project_name = self
            .project_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        // Footer tips fill the idle footer (rotating every ~15s); any real
        // status message wins.
        let footer_text = if !self.status_msg.is_empty() {
            self.status_msg.clone()
        } else if self.sidebar_focused {
            "sidebar: ↑↓ select · ⏎ open · m menu · h hide · x close · esc back".to_string()
        } else if self.settings.lock().unwrap().get_bool("showFooterTips", true) {
            const TIPS: &[&str] = &[
                "tip: ^b ? shows every shortcut",
                "tip: click a sidebar row to select it, double-click to open",
                "tip: ^b / searches the focused pane's scrollback",
                "tip: shift+drag selects text; double-click selects a word",
                "tip: ^b 1..9 jumps straight to a pane",
                "tip: ^b h hides a pane without killing it",
                "tip: autopilot (pane menu) auto-accepts option dialogs",
            ];
            TIPS[(timestamp() / 15) as usize % TIPS.len()].to_string()
        } else {
            "^b for commands · ^b ? help".to_string()
        };
        let scene = render::Scene {
            panes: &self.panes,
            layout: &self.layout,
            focused: self.focused,
            selected: self.selected,
            session_name: &self.session_name,
            project_name: &project_name,
            hud: self.hud.then_some(&self.metrics),
            status_line: &footer_text,
            theme: &self.theme,
            anim: self.anim,
            leader_armed: self.leader_armed,
            sidebar_focused: self.sidebar_focused,
            version: &self.version_line,
            issues: (self.filed_issues.len(), self.new_issue_count),
        };
        render::compose(&mut self.back, &scene, &mut self.click_map);

        // Bootstrapping panes show the native loader card over their body.
        if !self.bootstraps.is_empty() {
            for p in &self.panes {
                if let (Some(rect), Some(ui)) = (p.rect, self.bootstraps.get(&p.slug)) {
                    bootstrap::draw(&mut self.back, rect, &self.theme, ui, self.anim);
                }
            }
        }

        if self.welcome_active() {
            let content = render::content_area(&self.back, &self.layout);
            self.welcome_rain.resize(content.w, content.h);
            self.welcome_rain.draw(&mut self.back, content, &self.theme, self.anim);
            let wscene = welcome::WelcomeScene {
                cards: &self.welcome_cards,
                selected: self.welcome_sel,
                session_name: &self.session_name,
                project_root: &self.project_root.to_string_lossy(),
                installed: &self.installed_agents,
            };
            welcome::draw(&mut self.back, content, &self.theme, &wscene, &mut self.click_map);
        }

        // Overlays above the scene, with a scrim under the stack.
        self.view_cursor = None;
        if !self.views.is_empty() {
            let area = self.back.area();
            draw_scrim(&mut self.back, area);
            let ctx = ViewCtx { theme: &self.theme, anim: self.anim };
            let full = Rect::new(0, 0, self.size.0, self.size.1);
            let last = self.views.len() - 1;
            for (i, view) in self.views.iter_mut().enumerate() {
                let cursor = view.render(&mut self.back, full, &ctx, &mut self.click_map);
                if i == last {
                    self.view_cursor = cursor;
                }
            }
        }
        let composed = Instant::now();

        let sync = self.host.caps().synchronized_output;
        if sync {
            self.emitter.begin_sync();
        }
        self.emitter.hide_cursor();
        let force = self.force_full;
        diff_frame(&mut self.front, &mut self.back, &mut self.emitter, force);

        // Hardware cursor: overlay input first, else focused pane.
        if let Some((cx, cy)) = self.view_cursor {
            self.emitter.move_to(cx, cy);
            self.emitter.cursor_shape(6);
            self.emitter.show_cursor();
        } else if self.views.is_empty() {
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

/// React to a pane emulator side effect. Returns clipboard text to forward
/// (handled by the caller once the pane borrow ends).
fn handle_side_effect(
    client: &Client<Tag>,
    pane: &mut LogicalPane,
    effect: dmux_vt::TermSideEffect,
) -> Option<String> {
    use dmux_vt::TermSideEffect;
    match effect {
        TermSideEffect::PtyResponse(bytes) => {
            let _ = client.send(input::send_keys_hex(pane.tmux_pane, &bytes));
            None
        }
        TermSideEffect::Title(title) => {
            // Auto-naming: shell panes without a human-chosen name follow the
            // pane's own title reports (zsh's ESC k command/cwd names, OSC 2).
            let title = title.trim();
            // An LLM-chosen name beats raw shell titles (which are usually
            // just the cwd); human renames beat both (auto_name = false).
            if !title.is_empty() && (pane.title.is_empty() || pane.auto_name) && !pane.llm_named {
                let clipped: String = title.chars().take(24).collect();
                if pane.title != clipped {
                    pane.title = clipped;
                    pane.dirty = true;
                }
            }
            None
        }
        TermSideEffect::Clipboard(text) => Some(text),
        TermSideEffect::Bell => {
            pane.needs_attention = true;
            None
        }
    }
}

/// Minimal base64 (standard alphabet, padded) for OSC 52 payloads.
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// AI auto-merge: re-establish the conflicts, resolve each conflicted file
/// with the inference provider, stage, and commit. Aborts the merge on any
/// failure so the root is left clean.
async fn ai_merge(
    root: &std::path::Path,
    branch: &str,
    primary: &dmux_infer::Target,
    backup: Option<&dmux_infer::Target>,
) -> Result<usize, String> {
    let root_owned = root.to_path_buf();
    let b = branch.to_string();
    let files = tokio::task::spawn_blocking(move || git::merge_leaving_conflicts(&root_owned, &b))
        .await
        .map_err(|e| e.to_string())??;
    if files.is_empty() {
        // Clean re-merge: commit already made by git merge.
        return Ok(0);
    }
    const SYSTEM: &str = "You are resolving a git merge conflict. You will receive a file containing conflict markers (<<<<<<<, =======, >>>>>>>). Produce the fully resolved file content, preserving the intent of BOTH sides wherever possible. Output ONLY the raw resolved file content — no code fences, no commentary.";
    for file in &files {
        let path = root.join(file);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            git::abort_merge(root);
            format!("read {file}: {e}")
        })?;
        if content.len() > 48 * 1024 {
            git::abort_merge(root);
            return Err(format!("{file} too large for AI merge"));
        }
        let resolved = dmux_infer::generate(
            &dirs_home(),
            Some(primary),
            backup,
            SYSTEM,
            &format!("File: {file}

{content}"),
            8000,
        )
        .await
        .map_err(|e| {
            git::abort_merge(root);
            format!("{file}: {e}")
        })?;
        // Strip a wrapping code fence if the model added one despite orders.
        let trimmed = resolved.trim();
        let final_text = if trimmed.starts_with("```") {
            let inner = trimmed.trim_start_matches("```");
            let inner = inner.split_once('\n').map(|(_, rest)| rest).unwrap_or(inner);
            inner.trim_end().trim_end_matches("```").trim_end().to_string()
        } else {
            resolved.clone()
        };
        if final_text.contains("<<<<<<<") || final_text.trim().is_empty() {
            git::abort_merge(root);
            return Err(format!("{file}: model left conflicts unresolved"));
        }
        std::fs::write(&path, final_text).map_err(|e| {
            git::abort_merge(root);
            format!("write {file}: {e}")
        })?;
        git::stage_file(root, file).map_err(|e| {
            git::abort_merge(root);
            e
        })?;
    }
    git::commit_merge(root).map_err(|e| {
        git::abort_merge(root);
        e
    })?;
    Ok(files.len())
}

/// Latest published dmux-rs version from the npm registry (best-effort).
async fn check_latest_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .ok()?;
    let resp = client
        .get("https://registry.npmjs.org/dmux-rs/latest")
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v["version"].as_str().map(String::from)
}

/// Loose semver comparison: a > b?
fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(|c: char| c == '.' || c == '-')
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Shell-quote a path/branch for the bootstrap command line.
fn shq(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' )) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn slugify(prompt: &str) -> String {
    let mut slug = String::new();
    for word in prompt.split_whitespace().take(4) {
        let clean: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if clean.is_empty() {
            continue;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&clean);
        if slug.len() >= 24 {
            break;
        }
    }
    slug.truncate(32);
    if slug.is_empty() {
        format!("agents-{}", timestamp() % 100_000)
    } else {
        slug
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn iso_now() -> String {
    // Close-enough ISO timestamp without a chrono dependency (UTC seconds).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (mut y, mut rem_days) = (1970u64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if rem_days < len {
            break;
        }
        rem_days -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_lens = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while rem_days >= month_lens[m] {
        rem_days -= month_lens[m];
        m += 1;
    }
    let tod = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        y,
        m + 1,
        rem_days + 1,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_prompts() {
        assert_eq!(slugify("Fix the auth bug"), "fix-the-auth-bug");
        assert_eq!(slugify("Add   OAuth2!! support, please"), "add-oauth2-support-please");
        assert!(slugify("").starts_with("agents-"));
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shq("/tmp/simple-path"), "/tmp/simple-path");
        assert_eq!(shq("a path"), "'a path'");
        assert_eq!(shq("it's"), "'it'\\''s'");
    }

    #[test]
    fn iso_timestamp_shape() {
        let ts = iso_now();
        assert_eq!(ts.len(), 24);
        assert!(ts.ends_with("Z"));
        assert!(ts.starts_with("20"));
    }
}
