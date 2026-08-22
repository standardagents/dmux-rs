//! The app's internal message vocabulary: results from background tasks
//! delivered into the main loop (`AppMsg`), and the reply tags that match
//! tmux control-mode command replies in stream order (`Tag`).

use std::path::PathBuf;

use dmux_cc::PaneId;

use crate::{bootstrap, renderer_control, report, tracking, NewWindowCtx};

/// Results from background tasks (git merges, later inference) delivered
/// into the main loop.
#[derive(Debug)]
pub(crate) enum AppMsg {
    /// A renderer claim acquired its session-scoped advisory lock.
    RendererLock(Result<renderer_control::ClaimLock, String>),
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
    /// A local prototype replacement was requested by another dmux-rs process.
    PrototypeRequested,
    /// A worktree build started from a pane menu completed.
    PrototypeBuildDone(Result<PathBuf, String>),
    /// Add Project's confirmed create action finished (#129).
    ProjectCreated {
        path: String,
        result: Result<(), String>,
    },
    /// AI auto-merge finished (Ok = files resolved, merge committed).
    AiMergeDone {
        branch: String,
        result: Result<usize, String>,
    },
}

/// Reply tags: every command whose reply matters is matched in stream order.
#[derive(Debug)]
pub(crate) enum Tag {
    Input(PaneId, u64),
    ListPanes,
    Seed(PaneId),
    Cursor(PaneId),
    /// Shadow-verifier capture for one pane.
    VerifyCap(PaneId),
    RendererIdentity,
    ClaimCheck,
    ClaimFence,
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
    /// Path published by an external local prototype request.
    PrototypePath,
}
