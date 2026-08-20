//! dmux-rs: a native tmux control-mode renderer for dmux sessions. Attaches
//! (or creates) the project session, runs a terminal emulator per pane, and
//! composites panes + sidebar + native overlays into the host terminal with
//! damage-diffed, synchronized-output frames.

mod agent_launch;
mod agents;
mod audit;
mod bootstrap;
mod command_dispatch;
mod diagnose;
mod git;
mod github;
mod hooks;
mod hover;
mod hud;
mod input;
mod keys;
mod layout;
mod metrics;
mod notify;
mod pane_actions;
mod registry;
mod render;
mod report;
mod session;
mod sidebar;
mod sounds;
mod style;
mod tracking;
mod util;
pub(crate) use util::{
    base64, dirs_home, is_newer, iso_now, shq, slugify, strip_status_glyphs, timestamp,
    trace_palette_enabled, trace_palette_line, update_may_apply, AnimClock, Tooltip,
};
mod updater;
mod verify;
mod view_stack;
mod views;
mod welcome;
mod window_launch;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser as ClapParser;
use dmux_cc::{CcEvent, Client, PaneId, Reply, ReplyRouter, Routed as CcRouted};
use dmux_compositor::{diff_frame, CellBuffer, Emitter};
use dmux_core::i18n::t;
use dmux_core::{
    encode_pane_title, session_name_for_root, DmuxConfig, PaneKind, SettingsScope, SettingsStore,
};
use dmux_host::{HostTerminal, InputEvent};
use dmux_ui::{ClickMap, Theme, VerticalAlign};
use github::{IssueLoadState, SharedIssueState};
use hover::tooltip_rect;
use input::{MouseKind, Routed};
use session::{LogicalPane, PaneStatus};
#[cfg(test)]
use sidebar::{key_action as sidebar_key_action, SidebarKeyAction};
use sidebar::{ProjectSelection, SidebarDrag};
use view_stack::{OverlayOrigin, OverlayStack};
use views::{AppCmd, ClickTarget, ConfirmView, InputPurpose, InputView, MenuItem, MenuView};
use window_launch::NewWindowCtx;

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
        std::env::var("DMUX_TRACKING_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30)
    }))
}

#[derive(ClapParser, Debug)]
#[command(name = "dmux-rs", about = "dmux control-mode renderer prototype", version = updater::cli_version())]
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
    /// Read-only live-session diagnostic snapshot (#78): joins the installed
    /// build, recent events, live tmux panes, and persisted records with
    /// adoption's identity semantics. Mutates nothing.
    #[arg(long)]
    diagnose_session: bool,
}

/// Results from background tasks (git merges, later inference) delivered
/// into the main loop.
#[derive(Debug)]
enum AppMsg {
    MergeDone {
        slug: String,
        branch: String,
        result: Result<String, String>,
    },
    /// Async filesystem work finished; recompute anything derived from disk.
    RefreshDerived,
    /// LLM pane classification finished.
    AnalysisDone {
        pane: PaneId,
        verdict: Result<dmux_infer::PaneVerdict, String>,
    },
    /// LLM terminal naming produced a candidate name.
    NamingDone { pane: PaneId, name: String },
    /// Native worktree bootstrap progress for a pane (keyed by slug).
    Bootstrap { slug: String, ev: bootstrap::Ev },
    /// Automatic incident report finished (issue filed or failed).
    IssueFiled(Result<report::FiledIssue, String>),
    /// A project's GitHub issue state changed in a background task.
    IssuesChanged,
    /// A newer release is downloaded and staged; swap + re-exec.
    UpdateStaged { tag: String, staged: PathBuf },
    /// Agent process tracking sweep finished.
    TrackingDone(Vec<(String, tracking::AgentObservation)>),
    /// Conflicted merge state re-established; launch the resolution pane.
    ConflictsReady {
        branch: String,
        files: Result<Vec<String>, String>,
    },
    /// A newer published version exists.
    UpdateAvailable(String),
    /// AI auto-merge finished (Ok = files resolved, merge committed).
    AiMergeDone {
        branch: String,
        result: Result<usize, String>,
    },
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
    /// Reply-escaping probe (#19): decides whether this server octal-escapes
    /// command-reply payloads (tmux 3.5a) or sends raw bytes (3.7b).
    EscapeProbe,
    NewWindow(Box<NewWindowCtx>),
    /// kill-window round-trip for a closing pane (#29): ok finalizes the
    /// close; an error restores the pane and surfaces the failure.
    KillWindow(PaneId),
    /// Keepalive window creation round-trip (#10): the reply clears the
    /// in-flight flag and pins the window's name against automatic-rename.
    KeepaliveCreated,
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
    if cli.diagnose_session {
        let (config, project_root, session_name) = resolve_session(&cli)?;
        let code = diagnose::run(
            config.as_ref(),
            &project_root,
            &session_name,
            &cli.tmux,
            cli.socket.as_deref(),
        );
        std::process::exit(code);
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
            eprintln!(
                "failed to create tmux session '{session_name}' in {}",
                project_root.display()
            );
            std::process::exit(1);
        }
        eprintln!(
            "created session '{session_name}' for {}",
            project_root.display()
        );
    }
    let session_created = !exists;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(
        cli,
        config,
        project_root,
        session_name,
        session_created,
    ))
}

fn init_logging(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path = cli.log_file.clone().unwrap_or_else(|| {
        let dir = dirs_home().join(".dmux").join("logs");
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
        None => git::git_main_worktree_root(&start).unwrap_or(start),
    };
    let session = cli
        .session
        .clone()
        .unwrap_or_else(|| session_name_for_root(&root.to_string_lossy()));
    Ok((config, root, session))
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
    /// Last-persisted pane-record snapshot; the mutation-audit baseline (#79).
    audit_base: Vec<audit::Snap>,
    project_root: PathBuf,
    session_name: String,
    settings: Arc<Mutex<SettingsStore>>,
    installed_agents: std::collections::HashSet<&'static str>,
    keymap: keys::Keymap,
    theme: Theme,
    views: OverlayStack,
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
    /// A per-project action row selected through sidebar keyboard navigation.
    sidebar_project: Option<ProjectSelection>,
    anim: u64,
    anim_clock: AnimClock,
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
    sidebar_groups: Vec<render::SidebarGroup>,
    project_issues: std::collections::HashMap<String, SharedIssueState>,
    pane_accents: Vec<(dmux_compositor::Color, dmux_compositor::Color)>,
    /// A staged self-update: swap + re-exec after clean shutdown.
    reexec_after: Option<PathBuf>,
    want_exit: bool,
    /// Staged self-update held back while a bootstrap or prompt injection
    /// is in flight (#53) — re-exec at the wrong moment strands the route
    /// as an idle shell. (tag, staged path, first deferred at).
    pending_update: Option<(String, PathBuf, Instant)>,
    own_sizing: bool,
    /// Welcome-screen state (shown when no panes are visible).
    welcome_cards: Vec<welcome::WelcomeCard>,
    welcome_sel: usize,
    welcome_rain: welcome::MatrixRain,
    keepalive_present: bool,
    /// A keepalive create command is in flight; never send another until
    /// its reply lands (#10 — unbounded keepalive spawn).
    keepalive_pending: bool,
    /// The tmux session was created by THIS boot (fresh server): the config
    /// is a recovery manifest, not a mirror of live panes (#20).
    session_created: bool,
    /// The one-shot session-recovery offer has been made (#20).
    restore_offered: bool,
    /// Plans accepted by the recovery dialog, executed by RestoreSession.
    pending_restore: Vec<session::RestorePlan>,
    /// Copy-confirmation tooltip (#22).
    tooltip: Option<Tooltip>,
    /// Sidebar drag-reorder gesture state (#26).
    sidebar_drag: Option<SidebarDrag>,
    /// Dragged perf-HUD origin (#103); None = default anchor.
    hud_pos: Option<(u16, u16)>,
    /// Active HUD drag: pointer grab offset within the card.
    hud_drag: Option<(u16, u16)>,
    /// Server octal-escapes command-reply payloads (probed at attach, #19).
    /// None until the probe reply lands; no decoding happens before that,
    /// and the probe is the first tagged command so nothing races it.
    replies_escaped: Option<bool>,
    /// Panes we killed on purpose: never re-adopt while tmux still lists them.
    closing: std::collections::HashSet<PaneId>,
    /// A pane we just created: focus it once adoption lands.
    pending_focus: Option<PaneId>,
    /// Pane index with an active selection drag.
    drag_select: Option<usize>,
    /// Pane index receiving forwarded mouse-drag events (app mouse mode).
    mouse_forward: Option<usize>,
    /// Physical button state disambiguates press, drag, release, and hover.
    mouse_buttons: input::MouseButtonState,
    hovered: Option<ClickTarget>,
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
    session_created: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = Vec::new();
    if let Some(socket) = &cli.socket {
        args.extend(["-L".into(), socket.clone()]);
    }
    args.extend([
        "-C".into(),
        "attach-session".into(),
        "-t".into(),
        session_name.clone(),
    ]);
    let (client, mut events, router, mut child) = Client::<Tag>::spawn(&cli.tmux, &args)?;

    let settings = Arc::new(Mutex::new(SettingsStore::load(
        &dirs_home(),
        Some(&project_root),
    )));
    let installed_agents = agents::detect_installed();
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
    // tmux answers pane OSC 10/11 queries itself, and with only a
    // control-mode client attached it reports black-on-black — apps then
    // mis-detect the theme (codex painted a light composer, #4).
    // window-style feeds tmux the palette's answer; it tints only
    // tmux-client rendering (nothing watches that — dmux is the client)
    // and never reaches capture-pane grids, so the verifier and seed path
    // are unaffected.
    let (default_fg, default_bg) = dmux_vt::palette::default_fg_bg_hex();
    let _ = client.send(format!(
        "set -g window-style 'fg={default_fg},bg={default_bg}'"
    ));
    // window-active-style MERGES OVER window-style for the active pane —
    // where a focused TUI actually runs — so a stale or user-config value
    // there (observed live: bg=colour231, near-white) silently overrides
    // the answer above and re-breaks theme detection (#4 follow-up). Own
    // both options.
    let _ = client.send(format!(
        "set -g window-active-style 'fg={default_fg},bg={default_bg}'"
    ));
    // Mirror pane mode 2 with tmux's CSI-u extended-key format.
    session::configure_extended_keys(&client);
    // Reply-escaping probe must be the FIRST tagged command: its verdict
    // gates decoding of every later reply, and tmux answers in order (#19).
    client.send_tagged(
        "display-message -p 'dmuxprobe\u{1}end'".to_string(),
        Tag::EscapeProbe,
    )?;
    client.send_tagged(
        format!(
            "show-options -t {} -qv @dmux_controller_pid",
            dmux_cc::quote_arg(&session_name)
        ),
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
    let audit_base = audit::snapshot(&config.panes);

    let mut app = App {
        client,
        router,
        host,
        panes: Vec::new(),
        config,
        config_path,
        config_persisted,
        audit_base,
        project_root,
        session_name,
        settings,
        installed_agents,
        keymap,
        theme,
        views: OverlayStack::default(),
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
        sidebar_project: None,
        anim: 0,
        anim_clock: AnimClock::default(),
        pending_injections: Vec::new(),
        bootstraps: std::collections::HashMap::new(),
        verify_enabled: std::env::var("DMUX_VERIFY")
            .map(|v| v != "0")
            .unwrap_or(true),
        fault_drop: std::env::var("DMUX_FAULT_DROP_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        filed_issues: report::load_filed(&dirs_home()),
        new_issue_count: 0,
        version_line: updater::version_line(),
        sidebar_groups: Vec::new(),
        project_issues: std::collections::HashMap::new(),
        pane_accents: Vec::new(),
        reexec_after: None,
        want_exit: false,
        pending_update: None,
        own_sizing: false,
        welcome_cards: Vec::new(),
        welcome_sel: 0,
        welcome_rain: welcome::MatrixRain::new(
            size.0.saturating_sub(layout::SIDEBAR_WIDTH + 1),
            size.1,
        ),
        keepalive_present: false,
        keepalive_pending: false,
        session_created,
        restore_offered: false,
        pending_restore: Vec::new(),
        tooltip: None,
        sidebar_drag: None,
        hud_pos: None,
        hud_drag: None,
        replies_escaped: None,
        closing: std::collections::HashSet::new(),
        pending_focus: None,
        drag_select: None,
        mouse_forward: None,
        mouse_buttons: input::MouseButtonState::default(),
        hovered: None,
        drag_moved: false,
        last_press: None,
        last_search: None,
        log_path: cli
            .log_file
            .clone()
            .unwrap_or_else(|| dirs_home().join(".dmux").join("logs").join("dmux-rs.log")),
        app_tx,
        inference_primary: None,
        inference_backup: None,
        update_available: None,
        tracking_inflight: false,
        last_tracking: Instant::now(),
    };
    {
        let s = app.settings.lock().unwrap();
        app.inference_primary = s
            .get("inferencePrimary")
            .and_then(dmux_infer::Target::from_value);
        app.inference_backup = s
            .get("inferenceBackup")
            .and_then(dmux_infer::Target::from_value);
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
    if std::env::var("DMUX_JUST_UPDATED")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        app.toast(format!("⬆ updated to {}", updater::version_line()));
    }
    // First-party self-update loop: poll the dmux-rs repo's latest release
    // and stage newer builds for an in-place re-exec (HMR for the mux).
    if updater::enabled() {
        let tx = app.app_tx.clone();
        let repo = {
            let s = app.settings.lock().unwrap();
            s.get_str("dmuxRsRepo")
                .unwrap_or(report::DEFAULT_REPO)
                .to_string()
        };
        // Test-ring cadence: super frequent by design — a fresh release
        // should reach every head within about a minute.
        let poll_secs: u64 = std::env::var("DMUX_UPDATE_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        tokio::spawn(async move {
            // First check soon after boot so a stale install converges fast.
            let mut delay = 15;
            loop {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = poll_secs;
                let r = repo.clone();
                let tag = tokio::task::spawn_blocking(move || updater::latest_tag(&r)).await;
                let tag = match tag {
                    Ok(Ok(t)) => t,
                    Ok(Err(err)) => {
                        tracing::debug!(%err, "update check failed");
                        continue;
                    }
                    Err(err) => {
                        tracing::debug!(%err, "update check task failed");
                        continue;
                    }
                };
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
    if let Some(primary) = &app.inference_primary {
        tracing::info!(
            provider = %primary.provider_id,
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
        let hud_deadline = app
            .hud
            .then(|| tokio::time::Instant::from_std(now + HUD_REFRESH));
        let resume_deadline = app
            .panes
            .iter()
            .filter_map(|p| p.resume_at)
            .min()
            .map(tokio::time::Instant::from_std);
        let anim_deadline = if app.animating() {
            let interval = if app.welcome_active() {
                RAIN_INTERVAL
            } else {
                ANIM_INTERVAL
            };
            Some(tokio::time::Instant::from_std(
                app.anim_clock.deadline(now, interval),
            ))
        } else {
            app.anim_clock.disarm();
            None
        };
        let injection_deadline = app
            .next_injection_deadline()
            .map(tokio::time::Instant::from_std);
        let status_deadline = app.status_clear_at.map(tokio::time::Instant::from_std);
        let tooltip_deadline = app
            .tooltip
            .as_ref()
            .map(|t| tokio::time::Instant::from_std(t.until));
        let tracking_deadline = (!app.tracking_inflight
            && app
                .panes
                .iter()
                .any(|p| p.agent.is_some() && p.pane_pid > 0))
        .then(|| tokio::time::Instant::from_std(app.last_tracking + tracking_interval()));
        let deadline = [
            render_deadline,
            settle_deadline,
            hud_deadline,
            resume_deadline,
            anim_deadline,
            injection_deadline,
            status_deadline,
            tooltip_deadline,
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
            // Input outranks pane output (#29): under an %output flood the
            // unbiased select could keep picking the events branch, delaying
            // a keypress — the reported unacknowledged close Enter. Biased
            // order makes every loop pass drain pending input first; the
            // event branch's own 256-message budget already bounds cc work
            // per pass, so nothing starves.
            biased;
            maybe_input = input_rx.recv() => {
                match maybe_input {
                    Some(ev) => {
                        if !app.handle_input(ev) { break; }
                        app.render_if_due();
                    }
                    None => break,
                }
            }
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
    async fn shutdown(
        &mut self,
        child: &mut tokio::process::Child,
    ) -> Result<(), Box<dyn std::error::Error>> {
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
                                .find(|r| {
                                    r.worktree_path.as_deref() == Some(p.as_str()) || r.slug == slug
                                })
                                .and_then(|r| r.agent.clone());
                            worktrees.push(welcome::WorktreeCard {
                                slug,
                                path: p,
                                agent,
                            });
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
        self.welcome_cards =
            welcome::build_cards(&self.installed_agents, &project_name, &worktrees);
        self.welcome_sel = self
            .welcome_sel
            .min(self.welcome_cards.len().saturating_sub(1));
    }

    /// Make sure the keepalive window exists so an empty session survives.
    /// Commands are FIFO on the control stream, so calling this before a
    /// kill-window guarantees the session never hits zero windows.
    fn ensure_keepalive(&mut self) {
        if self.keepalive_present || self.keepalive_pending || !self.own_sizing {
            return;
        }
        // Tagged round-trip: keepalive_pending stays set until tmux confirms,
        // so overlapping reconciles can never each spawn one (#10 leaked
        // hundreds of PTYs this way). Detection is by start command
        // (session::is_keepalive), which survives automatic-rename.
        self.keepalive_present = true;
        self.keepalive_pending = true;
        let _ = self.client.send_tagged(
            format!(
                "new-window -dP -F '#{{window_id}}' -n {} '{}'",
                session::KEEPALIVE_NAME,
                session::KEEPALIVE_CMD
            ),
            Tag::KeepaliveCreated,
        );
    }

    fn animating(&self) -> bool {
        !self.bootstraps.is_empty()
            || self.welcome_active()
            || self
                .panes
                .iter()
                .any(|p| (p.status == PaneStatus::Working || p.closing) && !p.hidden)
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
        // Selected text routinely holds newlines and quotes; inlining it in
        // a control-mode command split the line and desynced the session
        // (#18). load-buffer from a temp file is byte-exact and quote-proof.
        let path = std::env::temp_dir().join(format!("dmux-rs-clip-{}", std::process::id()));
        match std::fs::write(&path, text) {
            Ok(()) => {
                let _ = self.client.send(format!(
                    "load-buffer -b dmux {}",
                    dmux_cc::quote_arg(&path.to_string_lossy())
                ));
            }
            Err(err) => tracing::warn!(%err, "clipboard buffer write failed"),
        }
    }

    /// Apply a deferred self-update once no bootstrap is provisioning and
    /// no prompt injection is queued (#53) — or once the deferral cap
    /// expires, so a wedged hook can't block updates forever.
    fn try_apply_pending_update(&mut self) {
        let Some((_, _, since)) = &self.pending_update else {
            return;
        };
        let active = self.bootstraps.values().any(|ui| ui.done_at.is_none());
        if !update_may_apply(active, self.pending_injection_count(), since.elapsed()) {
            return;
        }
        let (tag, staged, _) = self.pending_update.take().unwrap();
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
        if std::env::var("DMUX_NO_REPORT")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            return;
        }
        let Some(incident) = incident else { return };
        let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) else {
            return;
        };
        if p.issue_filed {
            return;
        }
        p.issue_filed = true;
        let repo = {
            let s = self.settings.lock().unwrap();
            s.get_str("dmuxRsRepo")
                .unwrap_or(report::DEFAULT_REPO)
                .to_string()
        };
        let diffs = verify::compare(p, reply);
        let our_grid: String = (0..p.rows)
            .map(|r| p.term.row_text_public(r) + "\n")
            .collect();
        let tmux_grid: String = reply
            .lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).escape_default().to_string() + "\n")
            .collect();
        let (slug, cols, rows, det) = (p.slug.clone(), p.cols, p.rows, !p.ring_truncated);
        let build = updater::version_line();
        let home = dirs_home();
        let dry = std::env::var("DMUX_REPORT_DRY")
            .ok()
            .map(std::path::PathBuf::from);
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
            .map(|f| f.issue);
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
            CcRouted::Reply(tag, mut reply) => {
                // Decode octal-escaped payloads on servers that escape them;
                // the probe reply itself must stay raw to be judged (#19).
                if !matches!(tag, Tag::EscapeProbe) && self.replies_escaped == Some(true) {
                    reply.unescape_lines();
                }
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
                    let _ = self
                        .client
                        .send(format!("refresh-client -A '{pane}:continue'"));
                    let _ = self.client.send_tagged(p.seed_command(), Tag::Seed(pane));
                    let _ = self
                        .client
                        .send_tagged(p.cursor_command(), Tag::Cursor(pane));
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
            CcEvent::WindowAdd(_)
            | CcEvent::LayoutChange { .. }
            | CcEvent::WindowPaneChanged { .. }
            | CcEvent::PaneModeChanged(_)
            | CcEvent::SubscriptionChanged { .. } => {
                self.request_reconcile();
                true
            }
            CcEvent::WindowRenamed { window, name }
            | CcEvent::UnlinkedWindowRenamed { window, name } => {
                if let Some(p) = self
                    .panes
                    .iter_mut()
                    .find(|p| p.tmux_window == window && p.title.is_empty())
                {
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
                        let cursor = reply
                            .ok
                            .then(|| session::parse_cursor_reply(&reply))
                            .flatten();
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
            Tag::EscapeProbe => {
                // Raw servers echo the literal 0x01 byte; escaping servers
                // turn it into the four bytes \001.
                let escaped = reply
                    .lines
                    .first()
                    .map(|l| !l.contains(&0x01) && l.windows(4).any(|w| w == b"\\001"))
                    .unwrap_or(false);
                self.replies_escaped = Some(escaped);
                tracing::info!(escaped, "reply-escaping probe");
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
            Tag::KillWindow(pane_id) => {
                let err = reply.text_lines().first().cloned().unwrap_or_default();
                self.finish_close(pane_id, reply.ok, err);
            }
            Tag::KeepaliveCreated => {
                self.keepalive_pending = false;
                if reply.ok {
                    // Pin the name so name-based tooling stays readable even
                    // under automatic-rename configs (identity itself is the
                    // start command and does not depend on this).
                    if let Some(win) = reply.text_lines().first().map(|l| l.trim().to_string()) {
                        if win.starts_with('@') {
                            let _ = self
                                .client
                                .send(format!("set-option -w -t {win} automatic-rename off"));
                        }
                    }
                } else {
                    // Creation failed; allow a later reconcile to retry.
                    self.keepalive_present = false;
                }
            }
        }
    }

    fn handle_app_msg(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::MergeDone {
                slug,
                branch,
                result,
            } => {
                match result {
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
                            let agent_label = self
                                .default_agent_for_conflicts()
                                .map(|d| d.name)
                                .unwrap_or("an agent");
                            let mut items = Vec::new();
                            if self.inference_primary.is_some() {
                                items.push(MenuItem::new(
                                    "AI merge (auto-resolve)",
                                    "",
                                    AppCmd::AiMerge {
                                        branch: branch.clone(),
                                    },
                                ));
                            }
                            items.push(MenuItem::new(
                                format!("Resolve with {agent_label}…"),
                                "",
                                AppCmd::ResolveConflicts {
                                    branch: branch.clone(),
                                },
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
                }
            }
            AppMsg::RefreshDerived => {
                self.refresh_welcome_cards();
                self.dirty = true;
            }
            AppMsg::IssuesChanged => {
                self.rebuild_sidebar_groups();
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
                        agents::Transport::SendKeys { ready_delay_ms } => {
                            Some((prompt, ready_delay_ms))
                        }
                        _ => None,
                    };
                    let n = 1 + self
                        .panes
                        .iter()
                        .filter(|q| q.slug.starts_with("conflicts-"))
                        .count();
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
                        update(
                            "activeAgent",
                            serde_json::Value::String(obs.agent_id.to_string()),
                        );
                        update("agentProcessId", serde_json::Value::from(obs.agent_pid));
                        if let Some(session) = &obs.session_id {
                            update("agentSessionId", serde_json::Value::String(session.clone()));
                        }
                    }
                }
                if changed {
                    self.save_config(audit::Reason::AgentTracking);
                    tracing::debug!("agent tracking updated config records");
                }
            }
            AppMsg::AnalysisDone { pane, verdict } => {
                let focused_pane = self.panes.get(self.focused).map(|p| p.tmux_pane);
                let mut attention: Option<String> = None;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.analysis_inflight = false;
                    let is_focused = focused_pane == Some(pane);
                    match verdict {
                        // Option dialogs always take the waiting/needs-input
                        // path — dmux never auto-accepts one (#31; agents
                        // bring their own autonomous modes).
                        Ok(dmux_infer::PaneVerdict::OptionDialog) => {
                            p.status = session::verdict_pane_status(
                                &dmux_infer::PaneVerdict::OptionDialog,
                            );
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
                if let Some(msg) = attention {
                    self.attention_toast(msg);
                }
            }
            AppMsg::Bootstrap { slug, ev } => {
                self.handle_bootstrap_event(slug, ev);
            }
            AppMsg::IssueFiled(result) => {
                match result {
                    Ok(issue) => {
                        self.toast(format!(
                            "🐛 filed issue #{} — {}",
                            issue.number, issue.title
                        ));
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
                // Never re-exec across an in-flight bootstrap or pending
                // prompt injection: the launch state lives only in this
                // process, and dropping it leaves the pane an idle shell in
                // the source repo (#53). Hold the update until the safe
                // boundary; try_apply_pending_update fires there.
                self.pending_update = Some((tag, staged, Instant::now()));
                self.try_apply_pending_update();
                if self.pending_update.is_some() {
                    self.toast("⬆ update staged — waiting for agent setup to finish…");
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
                            let _ = self.client.send(format!(
                                "select-pane -t {} -T {}",
                                p.tmux_pane,
                                dmux_cc::quote_arg(&encoded)
                            ));
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

    /// Commit and persist a sidebar reorder while retaining pane identity.
    fn reorder_pane(&mut self, src: usize, dst: usize) {
        let order_before = registry::pane_order_identities(&self.panes);
        let focused_id = self.panes.get(self.focused).map(|p| p.tmux_pane);
        let selected_id = self.panes.get(self.selected).map(|p| p.tmux_pane);
        if !registry::move_pane(&mut self.panes, src, dst) {
            if src < self.panes.len() && dst < self.panes.len() {
                self.toast("Panes reorder within their project");
            }
            return;
        }
        if let Some(id) = focused_id {
            if let Some(i) = self.panes.iter().position(|p| p.tmux_pane == id) {
                self.focused = i;
            }
        }
        if let Some(id) = selected_id {
            if let Some(i) = self.panes.iter().position(|p| p.tmux_pane == id) {
                self.selected = i;
            }
        }
        registry::order_records(&mut self.config.panes, &self.panes);
        registry::log_pane_order_change("explicit reorder", &order_before, &self.panes);
        self.save_config(audit::Reason::Reorder);
        self.relayout();
    }

    /// Rebuild the sidebar project groups + per-pane colors (TS contract:
    /// main project first, then config `sidebarProjects` order, then
    /// pane-derived; colors from `colorTheme` with TS auto-assignment for
    /// entries that lack one, persisted back to the shared config).
    fn rebuild_sidebar_groups(&mut self) {
        // Canonical identity (#76): aliases of one directory form one group.
        let norm = |r: &str| registry::canon_root(r);
        let main_root = norm(&self.project_root.to_string_lossy());
        let main_name = self
            .project_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());

        // Ordered, deduped project entries.
        let mut order: Vec<(String, String, Option<String>)> = Vec::new();
        let push = |root: String,
                    name: Option<String>,
                    theme: Option<String>,
                    order: &mut Vec<(String, String, Option<String>)>| {
            let root = norm(&root);
            if let Some(existing) = order.iter_mut().find(|(r, ..)| *r == root) {
                if existing.2.is_none() {
                    existing.2 = theme;
                }
                return;
            }
            let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                std::path::Path::new(&root)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "project".into())
            });
            order.push((root, name, theme));
        };
        push(main_root.clone(), Some(main_name), None, &mut order);
        for e in &self.config.sidebar_projects {
            let theme = e
                .color_theme
                .clone()
                .filter(|t| dmux_ui::PROJECT_THEME_NAMES.contains(&t.as_str()));
            push(
                e.project_root.clone(),
                e.project_name.clone(),
                theme,
                &mut order,
            );
        }
        for p in &self.panes {
            let root = p.project_root.clone().unwrap_or_else(|| main_root.clone());
            push(root, None, None, &mut order);
        }

        // Auto-assign missing colors (TS AUTO order: default first).
        let mut used: std::collections::HashSet<String> =
            order.iter().filter_map(|(_, _, t)| t.clone()).collect();
        let mut persisted_change = false;
        for (root, _, theme) in order.iter_mut() {
            if theme.is_some() {
                continue;
            }
            let pick = dmux_ui::project_theme_auto_order()
                .find(|n| !used.contains(*n))
                .unwrap_or(dmux_ui::DEFAULT_PROJECT_THEME)
                .to_string();
            used.insert(pick.clone());
            // Persist onto a matching config entry so TS resolves the same.
            if let Some(entry) = self
                .config
                .sidebar_projects
                .iter_mut()
                .find(|e| norm(&e.project_root) == *root && e.color_theme.is_none())
            {
                entry.color_theme = Some(pick.clone());
                entry.color_theme_source = Some("auto".into());
                persisted_change = true;
            }
            *theme = Some(pick);
        }
        if persisted_change {
            self.save_config(audit::Reason::ProjectTheme);
        }

        let active_root = norm(
            &self
                .active_project_root()
                .unwrap_or_else(|| main_root.clone()),
        );
        self.sidebar_groups = order
            .iter()
            .map(|(root, name, theme)| {
                let (accent, soft) = dmux_ui::project_theme(
                    theme.as_deref().unwrap_or(dmux_ui::DEFAULT_PROJECT_THEME),
                );
                let pane_indices: Vec<usize> = self
                    .panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        norm(&p.project_root.clone().unwrap_or_else(|| main_root.clone())) == *root
                    })
                    .map(|(i, _)| i)
                    .collect();
                render::SidebarGroup {
                    name: name.clone(),
                    root: root.clone(),
                    accent,
                    accent_soft: soft,
                    pane_indices,
                    issue_label: github::issue_state_label(self.project_issues.get(root)),
                    active: *root == active_root,
                }
            })
            .collect();
        if let Some(project) = &mut self.sidebar_project {
            sidebar::normalize_project_action(project, &self.sidebar_groups);
        }
        self.pane_accents = self
            .panes
            .iter()
            .map(|p| {
                let root = norm(&p.project_root.clone().unwrap_or_else(|| main_root.clone()));
                self.sidebar_groups
                    .iter()
                    .find(|g| g.root == root)
                    .map(|g| (g.accent, g.accent_soft))
                    .unwrap_or((self.theme.accent, self.theme.accent_soft))
            })
            .collect();
        self.ensure_project_issue_loads();
    }

    fn ensure_project_issue_loads(&mut self) {
        let roots: Vec<String> = self
            .sidebar_groups
            .iter()
            .map(|group| group.root.clone())
            .filter(|root| !self.project_issues.contains_key(root))
            .collect();
        for root in roots {
            self.refresh_project_issues(root);
        }
    }

    fn refresh_project_issues(&mut self, project_root: String) {
        let state = self
            .project_issues
            .entry(project_root.clone())
            .or_insert_with(|| Arc::new(Mutex::new(IssueLoadState::Loading { repository: None })))
            .clone();
        let tx = self.app_tx.clone();
        github::refresh_issue_state(state, project_root, move || {
            let _ = tx.send(AppMsg::IssuesChanged);
        });
    }

    /// Project root that new panes should target: the selected pane's
    /// project, else the main project.
    fn active_project_root(&self) -> Option<String> {
        if self.sidebar_focused {
            if let Some(project) = &self.sidebar_project {
                return Some(project.root.clone());
            }
        }
        self.panes
            .get(self.selected)
            .and_then(|p| p.project_root.clone())
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
        let _ = self
            .client
            .send_tagged(session::list_panes_command(), Tag::ListPanes);
    }

    fn apply_pane_list(&mut self, reply: &Reply) {
        let order_before = registry::pane_order_identities(&self.panes);
        let infos = session::parse_pane_list(reply);
        // Track (and dedupe) keepalive windows.
        let keepalives: Vec<_> = infos
            .iter()
            .filter(|i| session::is_keepalive(i))
            .map(|i| i.window)
            .collect();
        self.keepalive_present = !keepalives.is_empty();
        for extra in keepalives.iter().skip(1) {
            let _ = self.client.send(format!("kill-window -t {extra}"));
        }
        // Legacy multi-pane windows (splits inherited from older sessions or
        // other clients): dmux's model is one pane per window, so in owner
        // mode every extra pane is broken out into its own window. Without
        // this, apply_window_sizes skips shared windows entirely and stale
        // split layouts survive forever (#7). Idempotent: the reconcile
        // after the breaks lists only single-pane windows.
        if self.own_sizing {
            let extras = panes_to_break_out(&infos);
            for pane in &extras {
                tracing::info!(pane = %pane, "breaking legacy multi-pane window");
                let _ = self.client.send(format!("break-pane -d -s {pane}"));
            }
            if !extras.is_empty() {
                self.request_reconcile();
            }
        }
        // Forget closing-markers for panes tmux no longer lists.
        let listed: std::collections::HashSet<PaneId> = infos.iter().map(|i| i.pane).collect();
        self.closing.retain(|p| listed.contains(p));
        let infos: Vec<_> = infos
            .into_iter()
            .filter(|i| !self.closing.contains(&i.pane))
            .collect();
        let adopted = registry::adopt_panes(Some(&self.config), &infos);

        for mut new_pane in adopted {
            new_pane.record_stream = self.verify_enabled;
            if trace_palette_enabled() {
                new_pane.term.set_trace_palette(true);
            }
            match self
                .panes
                .iter_mut()
                .find(|p| p.tmux_pane == new_pane.tmux_pane)
            {
                Some(existing) => {
                    existing.tmux_window = new_pane.tmux_window;
                    // Keep the alt-screen flag fresh: begin_reseed seeds
                    // onto the grid tmux says the pane is on (#12).
                    existing.alt_screen = new_pane.alt_screen;
                    // Reconciliation established a newer authoritative
                    // ownership association (#76): refresh it.
                    if existing.project_root != new_pane.project_root {
                        tracing::info!(pane = %existing.tmux_pane,
                            from = ?existing.project_root, to = ?new_pane.project_root,
                            "pane ownership refreshed by reconcile");
                        existing.project_root = new_pane.project_root.clone();
                    }
                    existing.extended_keys_mode2 = new_pane.extended_keys_mode2;
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
                        let _ = self
                            .client
                            .send_tagged(existing.seed_command(), Tag::Seed(existing.tmux_pane));
                        let _ = self.client.send_tagged(
                            existing.cursor_command(),
                            Tag::Cursor(existing.tmux_pane),
                        );
                    }
                }
                None => {
                    new_pane.begin_reseed();
                    let _ = self
                        .client
                        .send_tagged(new_pane.seed_command(), Tag::Seed(new_pane.tmux_pane));
                    let _ = self
                        .client
                        .send_tagged(new_pane.cursor_command(), Tag::Cursor(new_pane.tmux_pane));
                    if new_pane.hidden {
                        let _ = self
                            .client
                            .send(format!("refresh-client -A '{}:off'", new_pane.tmux_pane));
                    }
                    self.panes.push(new_pane);
                }
            }
        }
        // Panes whose process exited are gone — tmux semantics, no dead husks.
        let live: std::collections::HashSet<_> = infos.iter().map(|i| i.pane).collect();
        let before = self.panes.len();
        self.panes.retain(|p| live.contains(&p.tmux_pane));
        let mut records_changed = false;
        if self.panes.len() != before {
            // Worktree records remain available for explicit restore.
            let rec_before = self.config.panes.len();
            self.config.panes.retain(|record| {
                record.kind() != PaneKind::Shell
                    || registry::record_has_live_pane(record, &self.panes)
            });
            records_changed = self.config.panes.len() != rec_before;
        }
        records_changed |=
            registry::record_adopted_panes(&mut self.config, &self.panes, &infos, timestamp());
        if records_changed {
            let live: Vec<String> = live.iter().map(|pane| pane.to_string()).collect();
            self.save_config(audit::Reason::Reconcile { live });
        }
        let (focused, selected) = registry::order_panes_preserving(
            &mut self.panes,
            &self.config.panes,
            self.focused,
            self.selected,
        );
        self.focused = focused;
        self.selected = selected;
        registry::log_pane_order_change("reconcile", &order_before, &self.panes);
        self.relayout();
        // Newly created panes take focus once adopted.
        if let Some(pending) = self.pending_focus {
            if let Some(idx) = self.panes.iter().position(|p| p.tmux_pane == pending) {
                self.pending_focus = None;
                let _ = self.execute_cmd(AppCmd::FocusPane(idx));
            }
        }
        self.refresh_welcome_cards();
        // Fresh server + persisted config: offer ONE explicit recovery
        // action (#20). Agent restarts can have external side effects, so
        // nothing restarts without this confirmation.
        if self.session_created && !self.restore_offered && self.own_sizing {
            self.restore_offered = true;
            let root = self.project_root.to_string_lossy().into_owned();
            let (plans, skipped) = session::plan_session_restore(&self.config, &root, &|p| {
                std::path::Path::new(p).is_dir()
            });
            let live: std::collections::HashSet<String> =
                self.panes.iter().map(|p| p.slug.clone()).collect();
            let plans: Vec<_> = plans
                .into_iter()
                .filter(|pl| !live.contains(pl.slug()))
                .collect();
            if !skipped.is_empty() {
                tracing::info!(?skipped, "session restore: unrecoverable records");
            }
            if !plans.is_empty() {
                let agents = plans
                    .iter()
                    .filter(|p| matches!(p, session::RestorePlan::Agent { .. }))
                    .count();
                let shells = plans.len() - agents;
                let mut msg = format!(
                    "Last session had {} agent pane(s) and {} terminal(s). Restore them?",
                    agents, shells
                );
                if !skipped.is_empty() {
                    msg.push_str(&format!(" ({} unrecoverable skipped)", skipped.len()));
                }
                self.pending_restore = plans;
                self.views.push(Box::new(ConfirmView::new(
                    "Restore session",
                    msg,
                    "Restore",
                    false,
                    AppCmd::RestoreSession,
                )));
                self.dirty = true;
            }
        }
        // The session must never be one process-exit away from vanishing.
        self.ensure_keepalive();
    }

    fn comfort_band(&self) -> (u16, u16) {
        let s = self.settings.lock().unwrap();
        let min = s
            .get_u64("minPaneWidth")
            .map(|v| v as u16)
            .unwrap_or(layout::DEFAULT_MIN_WIDTH);
        let max = s
            .get_u64("maxPaneWidth")
            .map(|v| v as u16)
            .unwrap_or(layout::DEFAULT_MAX_WIDTH);
        (min.clamp(20, 200), max.clamp(min, 400))
    }

    fn relayout(&mut self) {
        self.rebuild_sidebar_groups();
        let (min_w, max_w) = self.comfort_band();
        let visible: Vec<usize> = (0..self.panes.len())
            .filter(|&i| !self.panes[i].hidden)
            .collect();
        self.layout =
            layout::compute_with_band(self.size.0, self.size.1, visible.len(), min_w, max_w);
        for p in self.panes.iter_mut() {
            p.rect = None;
            p.dirty = true;
        }
        for (slot, &idx) in visible.iter().enumerate() {
            self.panes[idx].rect = self.layout.panes.get(slot).copied();
        }
        if self.focused >= self.panes.len()
            || self
                .panes
                .get(self.focused)
                .map(|p| p.hidden)
                .unwrap_or(true)
        {
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
            // Re-asserted on EVERY sizing pass, not once per window (#30):
            // a window that reverts to `window-size latest` (option lost,
            // set-option racing window creation, tmux/client interference)
            // tracks some client's geometry — testers saw 442-column,
            // 20-row terminals inside full-height panes (#30), oversized
            // restored widths (#24), and a one-row-short bottom (#25,
            // latest-mode height = client minus status row). The commands
            // are idempotent and only sent while the size is wrong, so the
            // converged steady state sends nothing.
            let _ = self.client.send(format!(
                "set-option -w -t {} window-size manual",
                p.tmux_window
            ));
            // User configs with `pane-border-status` steal a row INSIDE
            // the window, making the pane one row shorter than the window
            // we size — the bottom row of every pane would be invisible.
            // Scoped to our windows; the user's other sessions keep it.
            let _ = self.client.send(format!(
                "set-option -w -t {} pane-border-status off",
                p.tmux_window
            ));
            let _ = self.client.send(format!(
                "resize-window -t {} -x {} -y {}",
                p.tmux_window, rect.w, rect.h
            ));
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
        self.hovered = None;
        let _ = self
            .client
            .send(format!("refresh-client -C {}x{}", new_size.0, new_size.1));
        self.relayout();
    }

    // ------------------------------------------------------------------
    /// Returns false to quit.
    fn handle_input(&mut self, ev: InputEvent) -> bool {
        match ev {
            InputEvent::Key(key) => {
                if self.hovered.take().is_some() {
                    self.dirty = true;
                }
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
                if !leader_was_armed && self.welcome_active() {
                    if let Some(handled) = self.handle_welcome_key(&key) {
                        return handled;
                    }
                }
                if !leader_was_armed && self.sidebar_focused {
                    if let Some(handled) = self.handle_sidebar_key(&key) {
                        return handled;
                    }
                }
                let modes = session::pane_input_modes(&self.panes, self.focused);
                let routed = input::route_key(&key, modes, leader_was_armed, &self.keymap);
                self.execute_routed(routed)
            }
            InputEvent::Mouse(m) => {
                let (col, row, kind, shift) =
                    input::classify_mouse(&m, self.mouse_buttons.any_down());
                self.handle_mouse(col, row, kind, shift)
            }
            InputEvent::Paste(text) => {
                if self.hovered.take().is_some() {
                    self.dirty = true;
                }
                if let Some(top) = self.views.last_mut() {
                    let result = top.on_paste(&text);
                    self.dirty = true;
                    return self.apply_view_result(result);
                }
                let modes = session::pane_input_modes(&self.panes, self.focused);
                self.send_pane_bytes(&input::encode_paste(&text, modes));
                true
            }
            InputEvent::Resized { cols, rows } => {
                self.handle_resize((cols as u16, rows as u16));
                true
            }
            _ => true,
        }
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
        if kind == MouseKind::Hover {
            if let Some(next) = views::hover_target(target, !self.views.is_empty()) {
                self.update_hover_target(next);
            }
            if let Some(ClickTarget::PaneBody(i)) = target {
                if let Some(p) = self.panes.get(i) {
                    let modes = p.term.input_modes();
                    if modes.mouse_motion && modes.sgr_mouse {
                        if let Some(rect) = p.rect {
                            let motion = input::encode_sgr_mouse(
                                35,
                                true,
                                col.saturating_sub(rect.x),
                                row.saturating_sub(rect.y),
                            );
                            let _ = self.client.send(input::send_keys_hex(p.tmux_pane, &motion));
                        }
                    }
                }
            }
            return true;
        }
        let transitions = self.mouse_buttons.update(kind);
        if transitions.right_press {
            return self.open_context_menu(target, col, row);
        }
        if transitions.right_release {
            return true;
        }
        let is_press = transitions.left_press;
        let is_double = is_press
            && self.last_press.is_some_and(|(t, c, r)| {
                t.elapsed() < Duration::from_millis(400) && c == col && r == row
            });
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
                            self.forward_clipboard(&text);
                            // tmux-like copy: highlight clears the moment the
                            // copy lands, and a small tooltip confirms beside
                            // the release point (#22).
                            if let Some(p) = self.panes.get_mut(i) {
                                p.term.selection_clear();
                                p.dirty = true;
                            }
                            self.tooltip = Some(Tooltip {
                                text: "Copied to clipboard".into(),
                                x: col,
                                y: row,
                                until: Instant::now() + Duration::from_secs(2),
                            });
                            self.dirty = true;
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
        // A HUD drag captures the mouse until release (#103).
        if let Some(handled) = self.hud_drag_motion(kind, is_press, col, row) {
            return handled;
        }
        // A sidebar reorder drag captures the mouse until release (#26).
        if let Some(drag) = self.sidebar_drag {
            match kind {
                MouseKind::LeftHeld if !is_press => {
                    self.sidebar_drag = Some(drag.motion(row));
                    self.dirty = true;
                    return true;
                }
                MouseKind::Release => {
                    self.sidebar_drag = None;
                    self.dirty = true;
                    if let Some((src, _)) = drag.reordering() {
                        // Commit only over a sidebar row (release coords own
                        // this event's `target`); anywhere else cancels.
                        if let Some(ClickTarget::SidebarRow(dst)) = target {
                            self.reorder_pane(src, dst);
                        }
                        return true;
                    }
                    // Armed but never crossed a row: a plain click. Beyond
                    // the press-time selection, focus the pane (#55) — the
                    // click-to-focus contract testers expect. Reorder drags
                    // (handled above) and the double-click flyout (a modal
                    // is open by its release) keep their behaviors; hidden
                    // panes toggle visible first, matching Enter.
                    if let SidebarDrag::Armed { src, .. } = drag {
                        if self.views.is_empty() && src < self.panes.len() {
                            self.sidebar_focused = false;
                            if self.panes.get(src).map(|p| p.hidden).unwrap_or(false) {
                                return self.execute_cmd(AppCmd::ToggleHidden(src));
                            }
                            return self.execute_cmd(AppCmd::FocusPane(src));
                        }
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
                        self.hovered = None;
                        self.dirty = true;
                    }
                },
                MouseKind::LeftHeld
                | MouseKind::RightHeld
                | MouseKind::Hover
                | MouseKind::Release => {}
            }
            return true;
        }

        match kind {
            MouseKind::WheelUp | MouseKind::WheelDown => {
                let delta: i32 = if kind == MouseKind::WheelUp { 3 } else { -3 };
                match target {
                    Some(ClickTarget::SidebarRow(_)) => {
                        let len = self.panes.len();
                        if len > 0 {
                            let step = if delta > 0 { -1i32 } else { 1 };
                            let cur = self.selected as i32;
                            self.selected = (cur + step).rem_euclid(len as i32) as usize;
                            self.dirty = true;
                        }
                    }
                    _ => {
                        // Wheel over a pane (#12): apps that own the mouse
                        // get the wheel as SGR events; alt-screen apps
                        // (Claude Code) get arrow keys — the alt screen has
                        // no history, so local view-scrolling there only
                        // shows garbage; everything else scrolls our view.
                        if let Some(ClickTarget::PaneBody(i)) | Some(ClickTarget::PaneTitle(i)) =
                            target
                        {
                            if let Some(p) = self.panes.get_mut(i) {
                                let modes = p.term.input_modes();
                                let app_mouse =
                                    (modes.mouse_click || modes.mouse_drag || modes.mouse_motion)
                                        && modes.sgr_mouse;
                                let up = kind == MouseKind::WheelUp;
                                if app_mouse {
                                    if let Some(rect) = p.rect {
                                        let c = col.saturating_sub(rect.x);
                                        let r = row.saturating_sub(rect.y);
                                        let seq = input::encode_sgr_mouse(
                                            if up { 64 } else { 65 },
                                            true,
                                            c,
                                            r,
                                        );
                                        let _ = self
                                            .client
                                            .send(input::send_keys_hex(p.tmux_pane, &seq));
                                    }
                                } else if modes.alt_screen {
                                    // xterm "alternate scroll" / tmux behavior:
                                    // three arrow presses per wheel tick,
                                    // honoring DECCKM application encoding.
                                    let arrow: &[u8] = match (up, modes.app_cursor) {
                                        (true, false) => b"\x1b[A",
                                        (true, true) => b"\x1bOA",
                                        (false, false) => b"\x1b[B",
                                        (false, true) => b"\x1bOB",
                                    };
                                    let seq = arrow.repeat(3);
                                    let _ =
                                        self.client.send(input::send_keys_hex(p.tmux_pane, &seq));
                                } else {
                                    p.term.scroll_view(delta);
                                    p.dirty = true;
                                    self.dirty = true;
                                }
                            }
                        }
                    }
                }
            }
            MouseKind::LeftHeld if is_press => match target {
                Some(ClickTarget::HudClose) | Some(ClickTarget::HudTitle) => {
                    return self.hud_press(target, col, row);
                }
                Some(ClickTarget::SidebarRow(i)) => {
                    // Click selects (sidebar keeps the keyboard);
                    // double-click opens the row-anchored pane flyout (#14)
                    // WITHOUT activating the pane — Enter still activates.
                    self.selected = i;
                    self.sidebar_project = None;
                    self.rebuild_sidebar_groups();
                    if is_double {
                        return self.execute_cmd(AppCmd::OpenPaneFlyout { idx: i, y: row });
                    }
                    // Arm the reorder gesture (#26): it only engages if the
                    // pointer crosses onto another row before release.
                    self.sidebar_drag = Some(SidebarDrag::Armed {
                        src: i,
                        start_row: row,
                    });
                    self.sidebar_focused = true;
                    self.dirty = true;
                    return true;
                }
                Some(ClickTarget::SidebarNewProject) => {
                    return self.execute_cmd_at(
                        AppCmd::PromptAddProject,
                        OverlayOrigin::SidebarTarget {
                            target: ClickTarget::SidebarNewProject,
                            align: VerticalAlign::Bottom,
                        },
                    )
                }
                Some(ClickTarget::SidebarSettings) => {
                    return self.execute_cmd_at(
                        AppCmd::OpenSettings,
                        OverlayOrigin::SidebarTarget {
                            target: ClickTarget::SidebarSettings,
                            align: VerticalAlign::Bottom,
                        },
                    )
                }
                Some(ClickTarget::SidebarHelp) => {
                    return self.execute_cmd_at(
                        AppCmd::OpenShortcuts,
                        OverlayOrigin::SidebarTarget {
                            target: ClickTarget::SidebarHelp,
                            align: VerticalAlign::Bottom,
                        },
                    )
                }
                Some(ClickTarget::SidebarGroupIssues(gi)) => {
                    return self.open_sidebar_project_issues(gi);
                }
                Some(ClickTarget::SidebarGroupNewAgent(gi)) => {
                    return self.open_sidebar_project_agents(gi);
                }
                Some(ClickTarget::SidebarGroupNewTerminal(gi)) => {
                    return self.open_sidebar_project_terminal(gi);
                }
                Some(
                    ClickTarget::PaneTitle(i)
                    | ClickTarget::TitleRename(i)
                    | ClickTarget::TitleHide(i)
                    | ClickTarget::TitleClose(i),
                ) => {
                    let target = target.unwrap();
                    return self.title_control_press(target, i, is_double);
                }
                Some(ClickTarget::WelcomeCard(i)) => {
                    self.welcome_sel = i;
                    let cmd = self.welcome_cards.get(i).map(|c| c.cmd.clone());
                    if let Some(cmd) = cmd {
                        return self.execute_cmd(cmd);
                    }
                }
                Some(ClickTarget::PaneBody(i)) => {
                    self.sidebar_focused = false;
                    self.sidebar_project = None;
                    self.rebuild_sidebar_groups();
                    let already_focused = self.focused == i;
                    if !already_focused {
                        return self.execute_cmd(AppCmd::FocusPane(i));
                    }
                    if let Some(p) = self.panes.get_mut(i) {
                        let modes = p.term.input_modes();
                        let app_mouse =
                            (modes.mouse_click || modes.mouse_drag || modes.mouse_motion)
                                && modes.sgr_mouse;
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
                Some(ClickTarget::Overlay(_)) | None => {
                    // A click on empty sidebar background still hands the
                    // sidebar the keyboard (#15) — the whole strip is the
                    // input area, not just its rows.
                    if target.is_none()
                        && col <= self.layout.sidebar.right()
                        && !self.sidebar_focused
                    {
                        self.sidebar_focused = true;
                        self.dirty = true;
                    }
                }
            },
            MouseKind::LeftHeld | MouseKind::RightHeld | MouseKind::Hover | MouseKind::Release => {}
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
                self.step_sidebar_selection(delta);
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
        let visible: Vec<usize> = (0..self.panes.len())
            .filter(|&i| !self.panes[i].hidden)
            .collect();
        if visible.is_empty() {
            return true;
        }
        let cur = visible.iter().position(|&i| i == self.focused).unwrap_or(0) as i32;
        let next = visible[(cur + dir).rem_euclid(visible.len() as i32) as usize];
        self.execute_cmd(AppCmd::FocusPane(next))
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
        let Some(p) = self.panes.get_mut(idx) else {
            return;
        };
        p.title = name.clone();
        p.auto_name = false;
        let encoded = encode_pane_title(&name, &p.slug);
        let _ = self.client.send(format!(
            "select-pane -t {} -T {}",
            p.tmux_pane,
            dmux_cc::quote_arg(&encoded)
        ));
        let slug = p.slug.clone();
        self.update_config_pane(&slug, audit::Reason::Rename, |rec| {
            rec.display_name = Some(name.clone());
        });
        self.toast(format!("Renamed to '{name}'"));
    }

    fn toggle_hidden(&mut self, idx: usize) {
        let Some(p) = self.panes.get_mut(idx) else {
            return;
        };
        p.hidden = !p.hidden;
        let hidden = p.hidden;
        let pane_id = p.tmux_pane;
        let slug = p.slug.clone();
        if hidden {
            let _ = self
                .client
                .send(format!("refresh-client -A '{pane_id}:off'"));
        } else {
            let _ = self
                .client
                .send(format!("refresh-client -A '{pane_id}:on'"));
            p.begin_reseed();
            let _ = self
                .client
                .send_tagged(p.seed_command(), Tag::Seed(pane_id));
            let _ = self
                .client
                .send_tagged(p.cursor_command(), Tag::Cursor(pane_id));
        }
        self.update_config_pane(&slug, audit::Reason::Visibility, |rec| {
            rec.hidden = hidden.then_some(true);
        });
        self.relayout();
        self.toast(if hidden {
            t("toast.pane_hidden")
        } else {
            t("toast.pane_shown")
        });
    }

    fn close_pane(&mut self, idx: usize) {
        let Some(pane) = self.panes.get_mut(idx) else {
            return;
        };
        // Idempotent (#29): a pane already closing ignores duplicate close
        // commands (repeated Enter, repeated ^b x).
        if pane.closing {
            return;
        }
        // Immediate, visible acknowledgement: the row switches to its
        // closing state on the next frame; removal happens when tmux
        // confirms the kill (authoritative), and a failed kill restores
        // the pane (#29).
        pane.closing = true;
        pane.dirty = true;
        let pane_id = pane.tmux_pane;
        let window = pane.tmux_window;
        let slug = pane.slug.clone();
        let title = pane.display_title().to_string();
        let hook_root = pane
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let hook_cwd = pane
            .worktree_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| hook_root.clone());
        // Closing the last live pane must not destroy the session: the
        // keepalive window is created FIRST (FIFO command order guarantees
        // it exists before the kill lands).
        let live = self.panes.iter().filter(|p| !p.closing).count();
        if live == 0 {
            self.ensure_keepalive();
        }
        let hook_env = [
            ("DMUX_SLUG", slug.clone()),
            ("DMUX_PANE_ID", pane_id.to_string()),
        ];
        hooks::run_detached(&hook_root, "before_pane_close", &hook_cwd, &hook_env);
        self.closing.insert(pane_id);
        let _ = self
            .client
            .send_tagged(format!("kill-window -t {window}"), Tag::KillWindow(pane_id));
        self.toast(format!("Closing '{title}'…"));
        self.dirty = true;
    }

    /// tmux answered the kill-window for a closing pane (#29).
    fn finish_close(&mut self, pane_id: PaneId, ok: bool, err: String) {
        let Some(idx) = self.panes.iter().position(|p| p.tmux_pane == pane_id) else {
            return;
        };
        if !ok {
            // Restore a usable pane and surface the failure.
            self.closing.remove(&pane_id);
            let p = &mut self.panes[idx];
            p.closing = false;
            p.dirty = true;
            self.dirty = true;
            self.toast(format!("Close failed: {err}"));
            return;
        }
        let order_before = registry::pane_order_identities(&self.panes);
        let pane = self.panes.remove(idx);
        registry::log_pane_order_change("close", &order_before, &self.panes);
        self.bootstraps.remove(&pane.slug);
        let hook_root = pane
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let hook_env = [
            ("DMUX_SLUG", pane.slug.clone()),
            ("DMUX_PANE_ID", pane.tmux_pane.to_string()),
        ];
        hooks::run_detached(&hook_root, "pane_closed", &hook_root, &hook_env);
        if registry::remove_pane_record(&mut self.config.panes, &pane) {
            tracing::info!(pane = %pane.tmux_pane, slug = %pane.slug,
                root = ?pane.project_root, "removing pane record");
        }
        self.save_config(audit::Reason::PaneClosed);
        if self.focused >= self.panes.len() {
            self.focused = self.panes.len().saturating_sub(1);
        }
        if self.selected >= self.panes.len() {
            self.selected = self.panes.len().saturating_sub(1);
        }
        self.relayout();
        self.toast(format!("Closed '{}'", pane.display_title()));
    }

    fn send_pane_bytes(&mut self, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(self.focused) else {
            return;
        };
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
        // Missed-WINCH safety net (#43): the terminal can settle to its
        // final size between the startup ioctl and signal-handler install
        // (e.g. a window still animating to full screen), leaving the first
        // frame one row/column short forever. Re-probe on every deadline
        // pass — the ioctl is microseconds and handle_resize no-ops on an
        // unchanged size.
        self.handle_resize(dmux_host::term_size());
        self.try_apply_pending_update();
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
                                let _ = tx.send(AppMsg::NamingDone {
                                    pane,
                                    name: String::new(),
                                });
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
                            let _ = tx.send(AppMsg::AnalysisDone {
                                pane,
                                verdict: result,
                            });
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
            let _ = self
                .client
                .send(format!("refresh-client -A '{pane_id}:on'"));
            let (seed, cursor) = {
                let p = self.panes.iter().find(|p| p.tmux_pane == pane_id).unwrap();
                (p.seed_command(), p.cursor_command())
            };
            let _ = self.client.send_tagged(seed, Tag::Seed(pane_id));
            let _ = self.client.send_tagged(cursor, Tag::Cursor(pane_id));
            self.dirty = true;
        }
        self.send_due_injections(now);
        if !self.tracking_inflight && now.duration_since(self.last_tracking) >= tracking_interval()
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
            if let Some(p) = self.panes.iter_mut().find(|p| verify::eligible(p, now)) {
                p.last_verify = Some(now);
                let _ = self
                    .client
                    .send_tagged(p.seed_command_visible(), Tag::VerifyCap(p.tmux_pane));
            }
        }
        // Finished bootstrap loaders linger briefly (success: long enough for
        // the agent to paint under them; failure: long enough to read why).
        let before = self.bootstraps.len();
        self.bootstraps
            .retain(|_, ui| match (ui.done_at, ui.failed.is_some()) {
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
        if self.tooltip.as_ref().is_some_and(|t| now >= t.until) {
            self.tooltip = None;
            self.dirty = true;
        }
        if self.animating() {
            let interval = if self.welcome_active() {
                RAIN_INTERVAL
            } else {
                ANIM_INTERVAL
            };
            if self.anim_clock.fire_if_due(now, interval) {
                self.anim = self.anim.wrapping_add(1);
                if self.welcome_active() {
                    self.welcome_rain.step();
                }
                self.dirty = true;
            }
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
        self.back
            .fill(self.back.area(), &dmux_compositor::Cell::default());
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
            if self.sidebar_project.is_some() {
                "sidebar project: ↑↓ select · i issues · n agent · t terminal · esc back"
                    .to_string()
            } else {
                "sidebar pane: ↑↓ select · ⏎ open · m menu · h hide · x close · esc back"
                    .to_string()
            }
        } else if self
            .settings
            .lock()
            .unwrap()
            .get_bool("showFooterTips", true)
        {
            const TIPS: &[&str] = &[
                "tip: ^b ? shows every shortcut",
                "tip: click a sidebar row to select it, double-click to open",
                "tip: ^b / searches the focused pane's scrollback",
                "tip: shift+drag selects text; double-click selects a word",
                "tip: ^b 1..9 jumps straight to a pane",
                "tip: ^b h hides a pane without killing it",
            ];
            crate::style::pick_tip(TIPS, timestamp(), self.layout.sidebar.w as usize).to_string()
        } else {
            "^b for commands · ^b ? help".to_string()
        };
        let scene = render::Scene {
            panes: &self.panes,
            layout: &self.layout,
            focused: self.focused,
            selected: self.selected,
            project_name: &project_name,
            hud: self.hud.then_some(&self.metrics),
            hud_pos: self.hud_pos,
            status_line: &footer_text,
            theme: &self.theme,
            anim: self.anim,
            leader_armed: self.leader_armed,
            sidebar_focused: self.sidebar_focused,
            sidebar_project: self.sidebar_project.as_ref(),
            version: &self.version_line,
            groups: &self.sidebar_groups,
            pane_accents: &self.pane_accents,
            reorder: self.sidebar_drag.as_ref().and_then(|d| d.reordering()),
            hovered: self.hovered,
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
            self.welcome_rain
                .draw(&mut self.back, content, &self.theme, self.anim);
            let wscene = welcome::WelcomeScene {
                cards: &self.welcome_cards,
                selected: self.welcome_sel,
                session_name: &self.session_name,
                project_root: &self.project_root.to_string_lossy(),
                installed: &self.installed_agents,
                hovered: self.hovered,
            };
            welcome::draw(
                &mut self.back,
                content,
                &self.theme,
                &wscene,
                &mut self.click_map,
            );
        }

        // Overlays use the action geometry registered by base composition.
        self.render_overlays();
        if let Some(tip) = &self.tooltip {
            let label = format!(" {} ", tip.text);
            let rect = tooltip_rect(
                self.back.area(),
                (tip.x, tip.y),
                label.chars().count() as u16,
            );
            self.back.fill(
                rect,
                &dmux_compositor::Cell {
                    bg: self.theme.bg_selected,
                    ..Default::default()
                },
            );
            self.back.draw_text(
                rect.x,
                rect.y,
                &label,
                self.theme.accent,
                self.theme.bg_selected,
                dmux_compositor::AttrFlags::BOLD,
                rect,
            );
        }
        let composed = Instant::now();

        let sync = self.host.caps().synchronized_output;
        if sync {
            self.emitter.begin_sync();
        }
        self.emitter.hide_cursor();
        let force = self.force_full;
        diff_frame(&mut self.front, &mut self.back, &mut self.emitter, force);

        render::place_hardware_cursor(
            &mut self.emitter,
            self.view_cursor,
            self.views.is_empty(),
            self.panes.get(self.focused),
        );
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
            // Agents (Claude Code, Codex) prefix titles with their own
            // animated spinner glyph; the sidebar already draws a status
            // glyph, so a verbatim title showed two spinners per row (#9).
            let title = strip_status_glyphs(title.trim());
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
        TermSideEffect::PaletteChange { slot, to } => {
            trace_palette_line(&dirs_home(), pane.tmux_pane, &pane.slug, slot, to);
            None
        }
        TermSideEffect::Clipboard(text) => Some(text),
        TermSideEffect::Bell => {
            pane.needs_attention = true;
            None
        }
    }
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
            &format!(
                "File: {file}

{content}"
            ),
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
            let inner = inner
                .split_once('\n')
                .map(|(_, rest)| rest)
                .unwrap_or(inner);
            inner
                .trim_end()
                .trim_end_matches("```")
                .trim_end()
                .to_string()
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
        git::stage_file(root, file).inspect_err(|_| {
            git::abort_merge(root);
        })?;
    }
    git::commit_merge(root).inspect_err(|_| {
        git::abort_merge(root);
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

/// Every pane after the first in each window: legacy splits that owner mode
/// breaks out into their own windows (one pane per window is dmux's model).
fn panes_to_break_out(infos: &[session::TmuxPaneInfo]) -> Vec<PaneId> {
    let mut seen: std::collections::HashSet<dmux_cc::WindowId> = std::collections::HashSet::new();
    infos
        .iter()
        .filter(|i| !seen.insert(i.window))
        .map(|i| i.pane)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_prompts() {
        assert_eq!(slugify("Fix the auth bug"), "fix-the-auth-bug");
        assert_eq!(
            slugify("Add   OAuth2!! support, please"),
            "add-oauth2-support-please"
        );
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

    #[test]
    fn reported_titles_lose_their_leading_spinners() {
        // Claude Code animates an asterisk-family glyph in its titles.
        assert_eq!(strip_status_glyphs("✳ dmux-rs"), "dmux-rs");
        assert_eq!(strip_status_glyphs("✻ ✳ dmux-rs"), "dmux-rs");
        // Braille spinner frames.
        assert_eq!(strip_status_glyphs("⠹ building"), "building");
        // Plain titles pass through, including mid-title glyphs.
        assert_eq!(
            strip_status_glyphs("cargo build ✳ hot"),
            "cargo build ✳ hot"
        );
        // A title that is ONLY a spinner strips to empty (then ignored).
        assert_eq!(strip_status_glyphs("✳"), "");
    }

    #[test]
    fn legacy_multi_pane_windows_break_out_extras() {
        let mk = |pane: u32, window: u32| session::TmuxPaneInfo {
            pane: PaneId(pane),
            window: dmux_cc::WindowId(window),
            title: String::new(),
            width: 80,
            height: 24,
            alternate_on: false,
            current_command: "bash".into(),
            window_name: "w".into(),
            pane_pid: 1,
            start_command: String::new(),
            extended_keys_mode2: false,
            current_path: String::new(),
        };
        // Window 0 has three panes, window 1 has one: only the two extras
        // of window 0 are broken out; a re-run on the result is a no-op.
        let infos = vec![mk(0, 0), mk(1, 0), mk(2, 0), mk(3, 1)];
        assert_eq!(panes_to_break_out(&infos), vec![PaneId(1), PaneId(2)]);
        let after = vec![mk(0, 0), mk(1, 2), mk(2, 3), mk(3, 1)];
        assert!(panes_to_break_out(&after).is_empty());
    }

    #[test]
    fn sidebar_typing_never_leaks_or_drops_focus() {
        use dmux_host::{KeyCode, Modifiers};
        let keymap = keys::Keymap::from_overrides(&Default::default());
        let key = |k: KeyCode, m: Modifiers| dmux_host::KeyEvent {
            key: k,
            modifiers: m,
        };
        // A word typed while sidebar-focused: every unbound letter is a
        // no-op — never PassThrough (which would reach a pane), never
        // LeaveFocus (#27). Letters with sidebar meanings map to actions.
        for c in "wordy".chars() {
            let action = sidebar_key_action(&key(KeyCode::Char(c), Modifiers::NONE), &keymap);
            assert!(
                !matches!(
                    action,
                    SidebarKeyAction::PassThrough | SidebarKeyAction::LeaveFocus
                ),
                "typed '{c}' must stay in the sidebar, got {action:?}"
            );
        }
        // Unknown modified keys are consumed too.
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Char('z'), Modifiers::CTRL), &keymap),
            SidebarKeyAction::Ignore
        );
        // The leader chord passes through so global bindings keep working.
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Char('b'), Modifiers::CTRL), &keymap),
            SidebarKeyAction::PassThrough
        );
        // Recognized hotkeys, Enter, and Escape keep their meanings.
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Char('j'), Modifiers::NONE), &keymap),
            SidebarKeyAction::Down
        );
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Char('i'), Modifiers::NONE), &keymap),
            SidebarKeyAction::Issues
        );
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Enter, Modifiers::NONE), &keymap),
            SidebarKeyAction::Activate
        );
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Escape, Modifiers::NONE), &keymap),
            SidebarKeyAction::LeaveFocus
        );
        assert_eq!(
            sidebar_key_action(&key(KeyCode::Char('q'), Modifiers::NONE), &keymap),
            SidebarKeyAction::Ignore
        );
    }

    #[test]
    fn sidebar_drag_thresholds_and_follows() {
        // #26: a press+release on the same row is a click, never a reorder.
        let armed = SidebarDrag::Armed {
            src: 2,
            start_row: 5,
        };
        assert_eq!(armed.reordering(), None);
        assert_eq!(armed.motion(5), armed, "same-row motion stays armed");
        // Crossing a row enters reorder mode and then follows the pointer.
        let dragging = armed.motion(6);
        assert_eq!(dragging.reordering(), Some((2, 6)));
        assert_eq!(dragging.motion(9).reordering(), Some((2, 9)));
    }

    #[test]
    fn tooltip_clamps_to_bounds_and_expires() {
        let area = dmux_compositor::Rect::new(0, 0, 100, 30);
        // Interior release: one row above the cursor.
        assert_eq!(
            tooltip_rect(area, (40, 10), 22),
            dmux_compositor::Rect::new(40, 9, 22, 1)
        );
        // Top edge: stays on screen (no row above to use).
        assert_eq!(tooltip_rect(area, (40, 0), 22).y, 0);
        // Right edge: shifted left so the whole box fits.
        let r = tooltip_rect(area, (95, 10), 22);
        assert_eq!(r.right(), 100);
        // Bottom edge: still inside.
        assert!(tooltip_rect(area, (40, 29), 22).bottom() <= 30);
        // Wider than the terminal: clamped to it.
        assert_eq!(tooltip_rect(area, (0, 5), 200).w, 100);
        // Expiry: a later copy restarts the clock; the deadline decides.
        let t = Tooltip {
            text: "Copied to clipboard".into(),
            x: 1,
            y: 1,
            until: Instant::now(),
        };
        assert!(
            Instant::now() >= t.until,
            "an elapsed deadline reads as expired"
        );
    }
}
