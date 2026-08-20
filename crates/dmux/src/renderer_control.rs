//! Cross-renderer ownership for native dmux control-mode clients.
//!
//! A tmux session option is the inspectable source of truth. Native claims
//! serialize through a session-scoped advisory lock, tmux mutations carry a
//! server-side owner condition, and host effects confirm the same record while
//! holding the lock.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use dmux_host::InputEvent;

use crate::input;

pub const OWNER_OPTION: &str = "@dmux_renderer_owner";
pub const TOKEN_OPTION: &str = "@dmux_renderer_token";
pub const PROTOTYPE_OPTION: &str = "@dmux_prototype_executable";
pub const OWNER_SUBSCRIPTION: &str = "dmux-renderer-owner";
pub const OWNER_DENIED: &str = "DMUX_OWNER_DENIED";
const PRESERVED_TOKEN_ENV: &str = "DMUX_RENDERER_TOKEN";
const REEXEC_ROLE_ENV: &str = "DMUX_RENDERER_REEXEC_ROLE";
const REEXEC_OWNER_ENV: &str = "DMUX_RENDERER_REEXEC_OWNER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionCategory {
    Local,
    Ssh,
}

impl ConnectionCategory {
    fn detect() -> Self {
        if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
            Self::Ssh
        } else {
            Self::Local
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ssh => "SSH",
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ssh => "ssh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRecord {
    pub token: String,
    pub pid: u32,
    pub client_name: String,
    pub connection: ConnectionCategory,
    pub cols: u16,
    pub rows: u16,
    pub claimed_at: u64,
}

impl OwnerRecord {
    pub fn encode(&self) -> String {
        format!(
            "v1|{}|{}|{}|{}|{}|{}|{}",
            self.token,
            self.pid,
            hex_encode(self.client_name.as_bytes()),
            self.connection.wire(),
            self.cols,
            self.rows,
            self.claimed_at
        )
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let mut fields = raw.trim().split('|');
        if fields.next()? != "v1" {
            return None;
        }
        let token = fields.next()?.to_string();
        if !valid_token(&token) {
            return None;
        }
        let pid = fields.next()?.parse().ok()?;
        let client_name = String::from_utf8(hex_decode(fields.next()?)?).ok()?;
        if client_name.len() > 512 || client_name.chars().any(char::is_control) {
            return None;
        }
        let connection = match fields.next()? {
            "local" => ConnectionCategory::Local,
            "ssh" => ConnectionCategory::Ssh,
            _ => return None,
        };
        let cols = fields.next()?.parse().ok()?;
        let rows = fields.next()?.parse().ok()?;
        let claimed_at = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            token,
            pid,
            client_name,
            connection,
            cols,
            rows,
            claimed_at,
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "{} · {}×{} · pid {} · client {}",
            self.connection.label(),
            self.cols,
            self.rows,
            self.pid,
            if self.client_name.is_empty() {
                "unknown"
            } else {
                &self.client_name
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxIdentity {
    pub socket_path: String,
    pub session_id: String,
    pub client_name: String,
}

impl TmuxIdentity {
    pub fn parse(raw: &str) -> Option<Self> {
        let mut fields = raw.trim().split('\u{1}');
        let socket_path = fields.next()?.to_string();
        let session_id = fields.next()?.to_string();
        let client_name = fields.next()?.to_string();
        if socket_path.is_empty() || session_id.is_empty() || fields.next().is_some() {
            return None;
        }
        Some(Self {
            socket_path,
            session_id,
            client_name,
        })
    }

    fn scope(&self) -> String {
        format!("{}\u{1}{}", self.socket_path, self.session_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Startup,
    Claiming,
    Controller,
    Follower(Option<OwnerRecord>),
    LegacyFollower,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimReason {
    Startup,
    Activity,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReexecRole {
    Controller,
    Follower,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReexecContext {
    pub role: ReexecRole,
    pub expected_owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReexecStartup {
    Claim,
    Follow,
    Recover(Option<String>),
}

#[derive(Debug, Clone)]
pub struct CommandContext {
    pub tmux: String,
    pub socket: Option<String>,
    pub session_name: String,
}

impl CommandContext {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.tmux);
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command
    }

    pub fn read_owner(&self) -> io::Result<Option<OwnerRecord>> {
        let output = self
            .command()
            .args([
                "show-options",
                "-t",
                &self.session_name,
                "-qv",
                OWNER_OPTION,
            ])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(OwnerRecord::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    fn set_owner(&self, record: &OwnerRecord) -> io::Result<bool> {
        let owner_set = self
            .command()
            .args([
                "set-option",
                "-t",
                &self.session_name,
                OWNER_OPTION,
                &record.encode(),
            ])
            .status()?
            .success();
        let token_set = self
            .command()
            .args([
                "set-option",
                "-t",
                &self.session_name,
                TOKEN_OPTION,
                &record.token,
            ])
            .status()?
            .success();
        Ok(owner_set && token_set)
    }

    fn clear_owner(&self, record: &OwnerRecord) -> io::Result<bool> {
        let condition = format!("#{{==:#{{{}}},{}}}", TOKEN_OPTION, record.token);
        let body = format!(
            "set-option -u -t {session} {owner} ; set-option -u -t {session} {token}",
            session = dmux_cc::quote_arg(&self.session_name),
            owner = OWNER_OPTION,
            token = TOKEN_OPTION,
        );
        Ok(self
            .command()
            .args([
                "if-shell",
                "-t",
                &self.session_name,
                "-F",
                &condition,
                &body,
            ])
            .status()?
            .success())
    }

    /// Publish a local replacement request only while this owner still holds
    /// the session. The signal is sent by the caller after this succeeds.
    pub fn request_prototype(&self, owner: &OwnerRecord, executable: &Path) -> io::Result<bool> {
        let condition = format!("#{{==:#{{{}}},{}}}", TOKEN_OPTION, owner.token);
        let body = format!(
            "set-option -t {} {} {}",
            dmux_cc::quote_arg(&self.session_name),
            PROTOTYPE_OPTION,
            dmux_cc::quote_arg(&executable.to_string_lossy()),
        );
        self.command()
            .args([
                "if-shell",
                "-t",
                &self.session_name,
                "-F",
                &condition,
                &body,
            ])
            .status()
            .map(|status| status.success())
    }
}

#[derive(Debug, Clone)]
pub struct ClaimLock {
    _file: std::sync::Arc<LockedFile>,
}

#[derive(Debug)]
struct LockedFile {
    file: File,
}

impl ClaimLock {
    pub fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _file: std::sync::Arc::new(LockedFile { file }),
        })
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub struct RendererControl {
    pub state: State,
    pub token: String,
    pub connection: ConnectionCategory,
    pub identity: Option<TmuxIdentity>,
    pub owner: Option<OwnerRecord>,
    pub claim_reason: Option<ClaimReason>,
    pub recovery_expected: Option<String>,
    pub claim_lock: Option<ClaimLock>,
    pub reexec: Option<ReexecContext>,
    pub command: CommandContext,
    pub home: PathBuf,
}

impl RendererControl {
    pub fn new(home: PathBuf, command: CommandContext) -> Self {
        let token = std::env::var(PRESERVED_TOKEN_ENV)
            .ok()
            .filter(|token| valid_token(token))
            .unwrap_or_else(new_token);
        let expected_owner = std::env::var(REEXEC_OWNER_ENV)
            .ok()
            .filter(|token| valid_token(token));
        let reexec = match std::env::var(REEXEC_ROLE_ENV).as_deref() {
            Ok("controller") => Some(ReexecContext {
                role: ReexecRole::Controller,
                expected_owner: expected_owner.clone(),
            }),
            Ok("follower") => Some(ReexecContext {
                role: ReexecRole::Follower,
                expected_owner,
            }),
            _ => None,
        };
        Self {
            state: State::Startup,
            token,
            connection: ConnectionCategory::detect(),
            identity: None,
            owner: None,
            claim_reason: None,
            recovery_expected: None,
            claim_lock: None,
            reexec,
            command,
            home,
        }
    }

    pub fn record(&self, size: (u16, u16)) -> Option<OwnerRecord> {
        Some(OwnerRecord {
            token: self.token.clone(),
            pid: std::process::id(),
            client_name: self.identity.as_ref()?.client_name.clone(),
            connection: self.connection,
            cols: size.0,
            rows: size.1,
            claimed_at: epoch_seconds(),
        })
    }

    pub fn lock_path(&self) -> Option<PathBuf> {
        let scope = self.identity.as_ref()?.scope();
        Some(
            self.home
                .join(".dmux")
                .join("run")
                .join(format!("renderer-{:016x}.lock", fnv1a(scope.as_bytes()))),
        )
    }

    pub fn is_controller(&self) -> bool {
        self.state == State::Controller
    }

    pub fn is_ready(&self) -> bool {
        !matches!(self.state, State::Startup | State::Claiming)
    }

    pub fn begin_claim(&mut self, reason: ClaimReason, expected: Option<String>) -> bool {
        if self.state == State::Claiming {
            return false;
        }
        self.state = State::Claiming;
        self.claim_reason = Some(reason);
        self.recovery_expected = expected;
        true
    }

    pub fn become_controller(&mut self, record: OwnerRecord) {
        self.owner = Some(record);
        self.state = State::Controller;
        self.claim_reason = None;
        self.recovery_expected = None;
    }

    pub fn become_follower(&mut self, owner: Option<OwnerRecord>) {
        self.owner = owner;
        self.state = State::Follower(self.owner.clone());
        self.claim_reason = None;
        self.recovery_expected = None;
        self.claim_lock = None;
    }

    pub fn become_legacy_follower(&mut self) {
        self.owner = None;
        self.state = State::LegacyFollower;
        self.claim_reason = None;
        self.recovery_expected = None;
        self.claim_lock = None;
    }

    pub fn observe_owner(&mut self, owner: Option<OwnerRecord>) -> Option<String> {
        if self.state == State::LegacyFollower || self.state == State::Claiming {
            return None;
        }
        if self.state == State::Startup {
            self.owner = owner;
            return None;
        }
        if owner
            .as_ref()
            .is_some_and(|record| record.token == self.token)
        {
            self.owner = owner;
            self.state = State::Controller;
            return None;
        }
        let departed = self.owner.as_ref().map(|record| record.token.clone());
        self.become_follower(owner);
        departed
    }

    pub fn owner_record(&self) -> Option<&OwnerRecord> {
        self.owner
            .as_ref()
            .filter(|_| self.state == State::Controller)
    }

    pub fn guarded(&self, command: &str) -> Option<String> {
        self.owner_record()
            .map(|record| guarded_command(record, command))
    }

    pub fn confirmed_guard(&self) -> Option<ClaimLock> {
        let record = self.owner_record()?;
        let lock = match &self.claim_lock {
            Some(lock) => lock.clone(),
            None => ClaimLock::acquire(&self.lock_path()?).ok()?,
        };
        (self.command.read_owner().ok().flatten().as_ref() == Some(record)).then_some(lock)
    }

    pub fn status_line(&self, local_size: (u16, u16)) -> String {
        match &self.state {
            State::Startup | State::Claiming => format!(
                "Connecting · {} · {}×{}",
                self.connection.label(),
                local_size.0,
                local_size.1
            ),
            State::Controller => format!(
                "Controlling · {} · {}×{}",
                self.connection.label(),
                local_size.0,
                local_size.1
            ),
            State::Follower(Some(owner)) => format!(
                "Viewing · controller is {} · {}×{}",
                owner.connection.label(),
                owner.cols,
                owner.rows
            ),
            State::Follower(None) => format!(
                "Viewing · controller unavailable · {}×{}",
                local_size.0, local_size.1
            ),
            State::LegacyFollower => format!(
                "Viewing · TypeScript controller · {}×{}",
                local_size.0, local_size.1
            ),
        }
    }

    pub fn reexec_context(&self) -> ReexecContext {
        ReexecContext {
            role: if self.is_controller() {
                ReexecRole::Controller
            } else {
                ReexecRole::Follower
            },
            expected_owner: self.owner.as_ref().map(|owner| owner.token.clone()),
        }
    }

    pub fn take_reexec_startup(&mut self) -> Option<ReexecStartup> {
        let reexec = self.reexec.take()?;
        let current = self.owner.as_ref();
        Some(match reexec.role {
            ReexecRole::Controller => {
                if current.is_none_or(|owner| owner.token == self.token) {
                    ReexecStartup::Claim
                } else {
                    ReexecStartup::Follow
                }
            }
            ReexecRole::Follower => {
                if current.is_some() {
                    ReexecStartup::Follow
                } else {
                    ReexecStartup::Recover(reexec.expected_owner)
                }
            }
        })
    }

    pub fn graceful_release(&self) {
        let Some(record) = self.owner_record() else {
            tracing::debug!(state = ?self.state, "renderer exit has no owned record to release");
            return;
        };
        let Some(path) = self.lock_path() else {
            tracing::warn!("renderer exit has no coordination lock path");
            return;
        };
        let Ok(_lock) = ClaimLock::acquire(&path) else {
            tracing::warn!("renderer exit could not acquire the coordination lock");
            return;
        };
        let Ok(Some(current)) = self.command.read_owner() else {
            tracing::debug!("renderer owner was absent during exit");
            return;
        };
        if current.token != record.token {
            tracing::debug!("replacement renderer owns the session during exit");
            return;
        }
        match self.command.clear_owner(&current) {
            Ok(true) => tracing::info!("renderer control released"),
            Ok(false) => tracing::warn!("renderer control release command failed"),
            Err(err) => tracing::warn!(%err, "renderer control release could not run"),
        }
    }

    pub fn mark_reexec(&self, size: (u16, u16)) {
        let Some(current) = self.owner_record() else {
            return;
        };
        let Some(path) = self.lock_path() else { return };
        let Ok(_lock) = ClaimLock::acquire(&path) else {
            return;
        };
        if self.command.read_owner().ok().flatten().as_ref() != Some(current) {
            return;
        }
        let mut preserved = current.clone();
        preserved.client_name = "reexec".into();
        preserved.cols = size.0;
        preserved.rows = size.1;
        let _ = self.command.set_owner(&preserved);
    }

    pub fn update_size(&mut self, size: (u16, u16)) {
        let Some(current) = self.owner_record().cloned() else {
            return;
        };
        if (current.cols, current.rows) == size {
            return;
        }
        let Some(path) = self.lock_path() else { return };
        let Ok(_lock) = ClaimLock::acquire(&path) else {
            return;
        };
        if self.command.read_owner().ok().flatten().as_ref() != Some(&current) {
            self.become_follower(self.command.read_owner().ok().flatten());
            return;
        }
        let mut updated = current;
        updated.cols = size.0;
        updated.rows = size.1;
        if self.command.set_owner(&updated).unwrap_or(false) {
            self.owner = Some(updated);
        }
    }
}

pub fn identity_command() -> String {
    "display-message -p '#{socket_path}\u{1}#{session_id}\u{1}#{client_name}'".to_string()
}

pub fn claim_check_command(session_name: &str) -> String {
    format!(
        "display-message -p -t {} '#{{@dmux_controller_pid}}\u{1}#{{{}}}'",
        dmux_cc::quote_arg(session_name),
        OWNER_OPTION
    )
}

pub fn parse_claim_check(raw: &str) -> (Option<i32>, Option<OwnerRecord>) {
    let mut fields = raw.trim().splitn(2, '\u{1}');
    let legacy = fields.next().and_then(|value| value.parse().ok());
    let owner = fields.next().and_then(OwnerRecord::parse);
    (legacy, owner)
}

pub fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) == 0 }
}

pub fn owner_subscription_command() -> String {
    format!(
        "refresh-client -B '{}::#{{{}}}'",
        OWNER_SUBSCRIPTION, OWNER_OPTION
    )
}

pub fn prototype_path_command(session_name: &str) -> String {
    format!(
        "show-options -t {} -qv {}",
        dmux_cc::quote_arg(session_name),
        PROTOTYPE_OPTION
    )
}

pub fn clear_prototype_command(session_name: &str) -> String {
    format!(
        "set-option -u -t {} {}",
        dmux_cc::quote_arg(session_name),
        PROTOTYPE_OPTION
    )
}

pub fn parse_owner_subscription(raw: &str) -> Option<Option<OwnerRecord>> {
    let (head, value) = raw.split_once(" : ")?;
    if head.split_whitespace().next()? != OWNER_SUBSCRIPTION {
        return None;
    }
    Some(OwnerRecord::parse(value))
}

pub fn guarded_command(record: &OwnerRecord, command: &str) -> String {
    let condition = format!("#{{==:#{{{}}},{}}}", TOKEN_OPTION, record.token);
    format!(
        "if-shell -F {} {} {}",
        dmux_cc::quote_arg(&condition),
        dmux_cc::quote_arg(command),
        dmux_cc::quote_arg(&format!("display-message -p {OWNER_DENIED}"))
    )
}

pub fn reply_denied(reply: &dmux_cc::Reply) -> bool {
    reply
        .text_lines()
        .iter()
        .any(|line| line.trim() == OWNER_DENIED)
}

pub fn claim_worthy(event: &InputEvent, buttons: &input::MouseButtonState) -> bool {
    match event {
        InputEvent::Key(_) | InputEvent::Paste(_) => true,
        InputEvent::Mouse(mouse) => {
            let kind = input::classify_mouse(mouse, buttons.any_down()).2;
            buttons.would_claim(kind)
        }
        _ => false,
    }
}

pub fn preserved_token_env() -> &'static str {
    PRESERVED_TOKEN_ENV
}

pub fn reexec_role_env() -> &'static str {
    REEXEC_ROLE_ENV
}

pub fn reexec_owner_env() -> &'static str {
    REEXEC_OWNER_ENV
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 96
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn new_token() -> String {
    format!(
        "r{}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::thread::current().name().unwrap_or("main").len()
    )
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hex_decode(encoded: &str) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)? as u8;
            let lo = (pair[1] as char).to_digit(16)? as u8;
            Some((hi << 4) | lo)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::{Modifiers, MouseButtons, MouseEvent};

    fn owner() -> OwnerRecord {
        OwnerRecord {
            token: "r42-abc".into(),
            pid: 42,
            client_name: "/dev/ttys009".into(),
            connection: ConnectionCategory::Ssh,
            cols: 190,
            rows: 53,
            claimed_at: 1_700_000_000,
        }
    }

    fn mouse(buttons: MouseButtons) -> InputEvent {
        InputEvent::Mouse(MouseEvent {
            x: 4,
            y: 8,
            mouse_buttons: buttons,
            modifiers: Modifiers::NONE,
        })
    }

    #[test]
    fn owner_record_round_trips_without_exposing_unencoded_client_text() {
        let encoded = owner().encode();
        assert_eq!(OwnerRecord::parse(&encoded), Some(owner()));
        assert!(!encoded.contains("/dev/ttys009"));
    }

    #[test]
    fn malformed_owner_records_are_rejected() {
        assert!(OwnerRecord::parse("").is_none());
        assert!(OwnerRecord::parse("v2|token").is_none());
        assert!(OwnerRecord::parse("v1|bad token|1||local|80|24|1").is_none());
        assert!(OwnerRecord::parse("v1|token|1|0a|local|80|24|1").is_none());
    }

    #[test]
    fn activity_classification_matches_the_handoff_contract() {
        let mut buttons = input::MouseButtonState::default();
        assert!(claim_worthy(&InputEvent::Paste("x".into()), &buttons));
        assert!(claim_worthy(&mouse(MouseButtons::LEFT), &buttons));
        buttons.update(input::MouseKind::LeftHeld);
        assert!(!claim_worthy(&mouse(MouseButtons::LEFT), &buttons));
        assert!(claim_worthy(&mouse(MouseButtons::RIGHT), &buttons));
        assert!(!claim_worthy(&mouse(MouseButtons::NONE), &buttons));
        assert!(claim_worthy(
            &mouse(MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE),
            &buttons
        ));
        assert!(!claim_worthy(
            &InputEvent::Resized { cols: 90, rows: 30 },
            &buttons
        ));
    }

    #[test]
    fn guarded_commands_compare_the_stable_owner_token() {
        let command = guarded_command(&owner(), "resize-window -t @1 -x 80 -y 24");
        assert!(command.contains(TOKEN_OPTION));
        assert!(command.contains(&owner().token));
        assert!(command.contains(OWNER_DENIED));
    }

    #[test]
    fn prototype_request_commands_target_the_session_option() {
        let read = prototype_path_command("project session");
        assert!(read.contains(PROTOTYPE_OPTION));
        assert!(read.contains("project session"));
        let clear = clear_prototype_command("project session");
        assert!(clear.contains("set-option -u"));
        assert!(clear.contains(PROTOTYPE_OPTION));
    }

    #[test]
    fn owner_subscription_parses_changes_and_clears() {
        let raw = format!("{OWNER_SUBSCRIPTION} $1 @1 0 %1 : {}", owner().encode());
        assert_eq!(parse_owner_subscription(&raw), Some(Some(owner())));
        assert_eq!(
            parse_owner_subscription(&format!("{OWNER_SUBSCRIPTION} $1 @1 0 %1 : ")),
            Some(None)
        );
        assert_eq!(
            parse_owner_subscription("dmux-key-mode $1 @1 0 %1 : vi"),
            None
        );
    }

    #[test]
    fn session_and_socket_scope_lock_names_independently() {
        let base = std::env::temp_dir();
        let command = CommandContext {
            tmux: "tmux".into(),
            socket: None,
            session_name: "s".into(),
        };
        let mut control = RendererControl::new(base, command);
        control.identity = Some(TmuxIdentity {
            socket_path: "/tmp/tmux/default".into(),
            session_id: "$1".into(),
            client_name: "c".into(),
        });
        let first = control.lock_path().unwrap();
        control.identity.as_mut().unwrap().session_id = "$2".into();
        assert_ne!(first, control.lock_path().unwrap());
        control.identity.as_mut().unwrap().session_id = "$1".into();
        control.identity.as_mut().unwrap().socket_path = "/tmp/tmux/test".into();
        assert_ne!(first, control.lock_path().unwrap());
    }

    #[test]
    fn claim_transitions_preserve_the_departed_owner_token() {
        let base = std::env::temp_dir();
        let command = CommandContext {
            tmux: "tmux".into(),
            socket: None,
            session_name: "s".into(),
        };
        let mut control = RendererControl::new(base, command);
        assert!(control.begin_claim(ClaimReason::Startup, None));
        assert!(!control.begin_claim(ClaimReason::Activity, None));

        let current = owner();
        control.token = current.token.clone();
        control.become_controller(current.clone());
        let mut replacement = current.clone();
        replacement.token = "r99-replacement".into();
        assert_eq!(
            control.observe_owner(Some(replacement.clone())),
            Some(current.token)
        );
        assert_eq!(control.state, State::Follower(Some(replacement)));
        assert!(control.begin_claim(ClaimReason::Activity, None));
    }

    #[test]
    fn status_lines_name_local_and_remote_ownership() {
        let base = std::env::temp_dir();
        let command = CommandContext {
            tmux: "tmux".into(),
            socket: None,
            session_name: "s".into(),
        };
        let mut control = RendererControl::new(base, command);
        control.connection = ConnectionCategory::Local;
        let mut remote = owner();
        remote.connection = ConnectionCategory::Ssh;
        control.become_follower(Some(remote));
        assert_eq!(
            control.status_line((120, 40)),
            "Viewing · controller is SSH · 190×53"
        );
        control.token = owner().token;
        control.become_controller(owner());
        assert_eq!(
            control.status_line((120, 40)),
            "Controlling · local · 120×40"
        );
    }

    #[test]
    fn follower_reexec_resumes_without_claiming_a_live_owner() {
        let base = std::env::temp_dir();
        let command = CommandContext {
            tmux: "tmux".into(),
            socket: None,
            session_name: "s".into(),
        };
        let mut control = RendererControl::new(base, command);
        control.owner = Some(owner());
        control.reexec = Some(ReexecContext {
            role: ReexecRole::Follower,
            expected_owner: Some(owner().token),
        });
        assert_eq!(control.take_reexec_startup(), Some(ReexecStartup::Follow));
    }

    #[test]
    fn controller_reexec_reclaims_only_its_preserved_token() {
        let base = std::env::temp_dir();
        let command = CommandContext {
            tmux: "tmux".into(),
            socket: None,
            session_name: "s".into(),
        };
        let mut control = RendererControl::new(base, command);
        control.token = owner().token;
        control.owner = Some(owner());
        control.reexec = Some(ReexecContext {
            role: ReexecRole::Controller,
            expected_owner: Some(control.token.clone()),
        });
        assert_eq!(control.take_reexec_startup(), Some(ReexecStartup::Claim));

        let mut replacement = owner();
        replacement.token = "r99-replacement".into();
        control.owner = Some(replacement);
        control.reexec = Some(ReexecContext {
            role: ReexecRole::Controller,
            expected_owner: Some(control.token.clone()),
        });
        assert_eq!(control.take_reexec_startup(), Some(ReexecStartup::Follow));
    }
}
