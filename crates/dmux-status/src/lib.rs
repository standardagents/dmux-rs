//! Pane status detection: a faithful port of the TS heuristics
//! (`src/utils/paneAttentionHeuristics.ts`) re-targeted onto live emulator
//! grid text, plus a per-pane settle engine. Event-driven — the app calls
//! `on_settle` only when a pane's output has gone quiet, so idle cost is zero.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Working,
    /// Settled and appears to be asking the user something.
    Waiting,
    Idle,
}

const GENERIC_PROGRESS_WORDS: &[&str] = &[
    "germinating",
    "working",
    "thinking",
    "planning",
    "pondering",
    "crunching",
    "analyzing",
    "building",
    "testing",
    "running",
    "searching",
    "reviewing",
    "understanding",
    "loading",
    "processing",
    "writing",
    "reading",
    "editing",
    "patching",
    "generating",
    "reasoning",
    "compiling",
    "indexing",
    "summarizing",
    "executing",
    "refactoring",
    "fixing",
    "checking",
    "scanning",
];

const SPINNER_PREFIX: &str = "[⠁-⣿◐◓◑◒◴◷◶◵●○◦•·⋯⋮✦✧✶✻✽⏳⌛]";

fn progress_alt() -> String {
    GENERIC_PROGRESS_WORDS.join("|")
}

fn re(cell: &'static OnceLock<Regex>, build: impl FnOnce() -> String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&build()).expect("static regex"))
}

fn esc_to_interrupt() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || {
        r"(?i)\besc\s+to\s+(interrupt|cancel|stop|abort)\b".into()
    })
}

fn spinner_line() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || {
        format!(
            r"(?i)^{SPINNER_PREFIX}\s*(?:{})(?:\b|\.\.\.|…|\s)",
            progress_alt()
        )
    })
}

fn progress_suffix() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || {
        format!(
            r"(?i)\b(?:{})\b.*(?:\.\.\.|…|\d{{1,3}}%|/\d+)",
            progress_alt()
        )
    })
}

fn claude_working() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || r"(?i)claude\s+is\s+working".into())
}

/// A whole line that IS a live status: optional spinner glyph, a progress
/// word, optional ellipsis, optional trailing `(…)` timer. Prose that merely
/// mentions a progress word ("its drainer is running.") must not match —
/// transcript history sits on screen long after the agent settles (#50).
fn status_gerund_line() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || {
        format!(
            r"(?i)^\s*(?:{SPINNER_PREFIX}\s*)?(?:{})(?:\.\.\.|…)?\s*(?:\(.*)?$",
            progress_alt()
        )
    })
}

fn prompt_line() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    // The 16 TS PROMPT_PATTERNS collapsed: optional `│`, one of > $ ❯ ›, then
    // either content or end of line.
    re(&C, || r"^\s*(?:│\s*)?[>$❯›]\s*(?:\S.*)?$".into())
}

fn prompt_continuation() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || r"^\s*(?:│\s{2,}|\s{2,})\S".into())
}

/// Waiting/option-dialog cues used when a pane has settled without working
/// indicators (heuristic stand-in for the TS LLM escalation).
fn waiting_cue() -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    re(&C, || {
        r"(?i)(\by/n\b|\byes/no\b|\bdo you want\b|\bwould you like\b|\bproceed\?|\bcontinue\?|\bpress\s+enter\b|\bwaiting for\b|\bapprove\b|\bpermission\b|\ballow\b.*\?|^\s*❯?\s*\d+[.)]\s+\S)".into()
    })
}

fn trim_empty_lines(lines: &[&str]) -> Vec<String> {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    match (start, end) {
        (Some(s), Some(e)) => lines[s..=e].iter().map(|l| l.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn recent_relevant_lines(content: &str, max: usize) -> Vec<String> {
    let ws = Regex::new(r"\s+").unwrap();
    let all: Vec<String> = content
        .lines()
        .map(|l| ws.replace_all(l.trim(), " ").into_owned())
        .filter(|l| !l.is_empty())
        .collect();
    let skip = all.len().saturating_sub(max);
    all.into_iter().skip(skip).collect()
}

/// Stable fingerprint of the visually meaningful tail of a pane.
pub fn activity_fingerprint(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let trimmed = trim_empty_lines(&lines);
    let skip = trimmed.len().saturating_sub(max_lines);
    trimmed
        .into_iter()
        .skip(skip)
        .map(|l| l.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Port of `hasAgentWorkingIndicators`, tightened (#50): live status shapes
/// only. Idle transcripts keep prose and stale progress lines on screen, so
/// bare progress-word mentions anywhere in the tail must never count.
pub fn has_working_indicators(content: &str, agent: Option<&str>) -> bool {
    let lines = recent_relevant_lines(content, 10);
    if lines.is_empty() {
        return false;
    }
    let recent = lines.join("\n");
    // Agents show an interrupt hint only while actually running.
    if esc_to_interrupt().is_match(&recent) {
        return true;
    }
    // Everything softer counts only near the bottom, where live status
    // lives; higher rows are transcript history.
    let skip = lines.len().saturating_sub(6);
    let bottom = &lines[skip..];
    if bottom.iter().any(|l| {
        spinner_line().is_match(l)
            || progress_suffix().is_match(l)
            || status_gerund_line().is_match(l)
    }) {
        return true;
    }
    matches!(agent, Some("claude")) && bottom.iter().any(|l| claude_working().is_match(l))
}

/// Port of `isLikelyUserTyping`: bottom-of-screen prompt-shaped edits only.
pub fn is_likely_user_typing(previous: &str, current: &str) -> bool {
    if current.is_empty() || previous == current {
        return false;
    }

    {
        let (prev_block, cur_block) = (
            extract_prompt_block(previous),
            extract_prompt_block(current),
        );
        if prev_block.is_some() || cur_block.is_some() {
            let prev_prefix = normalize_for_comparison(
                prev_block
                    .as_ref()
                    .map(|b| b.0.clone())
                    .unwrap_or_else(|| previous.lines().map(String::from).collect()),
            );
            let cur_prefix = normalize_for_comparison(
                cur_block
                    .as_ref()
                    .map(|b| b.0.clone())
                    .unwrap_or_else(|| current.lines().map(String::from).collect()),
            );
            if prev_prefix == cur_prefix {
                let prev_prompt = normalize_prompt_block(
                    prev_block.as_ref().map(|b| b.1.as_slice()).unwrap_or(&[]),
                );
                let cur_prompt = normalize_prompt_block(
                    cur_block.as_ref().map(|b| b.1.as_slice()).unwrap_or(&[]),
                );
                if prev_prompt != cur_prompt {
                    return true;
                }
            }
        }
    }

    let prev_lines: Vec<&str> = previous.lines().collect();
    let cur_lines: Vec<&str> = current.lines().collect();
    if prev_lines.len().abs_diff(cur_lines.len()) > 6 {
        return false;
    }
    let max_len = prev_lines.len().max(cur_lines.len());
    let changed: Vec<usize> = (0..max_len)
        .filter(|&i| {
            prev_lines.get(i).copied().unwrap_or("") != cur_lines.get(i).copied().unwrap_or("")
        })
        .collect();
    if changed.is_empty() || changed.len() > 6 {
        return false;
    }
    let bottom = max_len.saturating_sub(6);
    if changed.iter().any(|&i| i < bottom) {
        return false;
    }
    changed.iter().any(|&i| {
        let prev = prev_lines.get(i).copied().unwrap_or("");
        let cur = cur_lines.get(i).copied().unwrap_or("");
        let prefix = prev
            .bytes()
            .zip(cur.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let line_max = prev.len().max(cur.len());
        let mostly_shared = line_max > 0 && prefix as f32 / line_max as f32 >= 0.7;
        let probe = if cur.is_empty() { prev } else { cur };
        let prompt_like = prompt_line().is_match(probe) || prompt_continuation().is_match(probe);
        prompt_like && (cur.starts_with(prev) || prev.starts_with(cur) || mostly_shared)
    })
}

type PromptBlock = (Vec<String>, Vec<String>);

fn extract_prompt_block(content: &str) -> Option<PromptBlock> {
    let lines_ref: Vec<&str> = content.lines().collect();
    let lines = trim_empty_lines(&lines_ref);
    if lines.is_empty() {
        return None;
    }
    let search_start = lines.len().saturating_sub(12);
    for index in (search_start..lines.len()).rev() {
        if !prompt_line().is_match(&lines[index]) {
            continue;
        }
        let trailing = &lines[index + 1..];
        if trailing.iter().all(|l| prompt_continuation().is_match(l)) {
            return Some((lines[..index].to_vec(), lines[index..].to_vec()));
        }
    }
    None
}

fn normalize_for_comparison(lines: Vec<String>) -> String {
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    trim_empty_lines(&refs)
        .iter()
        .map(|l| l.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_prompt_block(lines: &[String]) -> String {
    let bar = Regex::new(r"^\s*│\s*").unwrap();
    let marker = Regex::new(r"^(?:[>$❯›])\s?").unwrap();
    let indent = Regex::new(r"^\s{2,}").unwrap();
    lines
        .iter()
        .map(|l| {
            let l = bar.replace(l, "");
            let l = marker.replace(&l, "");
            let l = indent.replace(&l, "");
            l.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does the settled tail look like a question/option dialog? Only the bottom
/// few lines count — a question that has scrolled up under newer output is no
/// longer waiting on anyone.
pub fn has_waiting_cues(content: &str) -> bool {
    let lines = recent_relevant_lines(content, 6);
    lines.iter().any(|l| waiting_cue().is_match(l))
}

/// Per-pane settle engine. Feed it the grid tail whenever output has been
/// quiet for the settle interval; it debounces flapping with fingerprints.
#[derive(Debug, Default)]
pub struct PaneStatusEngine {
    last_fingerprint: String,
    prev_tail: String,
    /// Set when a settled verdict was already produced for this fingerprint.
    settled_verdict: Option<Activity>,
}

impl PaneStatusEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// New output arrived: any settled verdict is stale.
    pub fn on_output(&mut self) {
        self.settled_verdict = None;
    }

    /// Output has been quiet: classify from the tail text.
    pub fn on_settle(&mut self, tail: &str, agent: Option<&str>) -> Activity {
        let fingerprint = activity_fingerprint(tail, 12);
        let same_as_last = fingerprint == self.last_fingerprint;
        if same_as_last {
            if let Some(verdict) = self.settled_verdict {
                return verdict;
            }
        } else {
            self.settled_verdict = None;
        }

        // User typing at a prompt must not flap status.
        let typing = is_likely_user_typing(&self.prev_tail, tail);
        self.prev_tail = tail.to_string();
        self.last_fingerprint = fingerprint;

        let verdict = if has_working_indicators(tail, agent) {
            Activity::Working
        } else if typing {
            self.settled_verdict.unwrap_or(Activity::Idle)
        } else if has_waiting_cues(tail) {
            Activity::Waiting
        } else {
            Activity::Idle
        };
        self.settled_verdict = Some(verdict);
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_indicators() {
        assert!(has_working_indicators("✻ Thinking…\n", Some("claude")));
        assert!(has_working_indicators("esc to interrupt", Some("claude")));
        assert!(has_working_indicators("Compiling foo v0.1 ... 42%", None));
        assert!(!has_working_indicators("❯ ", Some("claude")));
        assert!(!has_working_indicators("Done. All tests passed.", None));
    }

    #[test]
    fn prose_mentions_of_progress_words_are_not_working() {
        // #50: transcript history sits on screen while an agent is idle —
        // sentences that merely mention a progress word kept panes spinning.
        assert!(!has_working_indicators(
            "The completion comment was queued; its drainer is running.\n❯ ",
            Some("claude")
        ));
        assert!(!has_working_indicators(
            "checking connectivity was fine yesterday\n$ ",
            None
        ));
        // A stale status line scrolled above the bottom window is history.
        let tail = "● Running 1 shell command…\na\nb\nc\nd\ne\nf\n❯ ";
        assert!(!has_working_indicators(tail, Some("claude")));
        // A live whole-line status at the bottom still counts.
        assert!(has_working_indicators("thinking…", Some("claude")));
        assert!(has_working_indicators(
            "✻ Germinating… (12s)",
            Some("claude")
        ));
    }

    #[test]
    fn settled_prose_transcript_classifies_idle_and_stays_idle() {
        // #50: the fingerprint cache must cache Idle, not a false Working.
        let mut e = PaneStatusEngine::new();
        let tail = "Ran 2 shell commands\nthe issue drainer is running.\n❯ ";
        assert_eq!(e.on_settle(tail, Some("claude")), Activity::Idle);
        assert_eq!(e.on_settle(tail, Some("claude")), Activity::Idle);
    }

    #[test]
    fn waiting_cues() {
        assert!(has_waiting_cues("Do you want to apply this edit? (y/n)"));
        assert!(has_waiting_cues("❯ 1. Yes\n  2. No"));
        assert!(!has_waiting_cues("build finished in 3s"));
    }

    #[test]
    fn typing_detection() {
        let before = "output line\n╰─ ❯ ";
        let after = "output line\n╰─ ❯ gi";
        // A prompt-line-only bottom edit is typing…
        assert!(is_likely_user_typing("❯ ", "❯ gi"));
        let _ = (before, after);
        // …while new output above the prompt is not.
        assert!(!is_likely_user_typing(
            "a\nb\n❯ ",
            "a\nb\nc\nd\ne\nf\ng\nh\n❯ "
        ));
    }

    #[test]
    fn engine_flow() {
        let mut e = PaneStatusEngine::new();
        assert_eq!(
            e.on_settle("✻ Thinking…", Some("claude")),
            Activity::Working
        );
        e.on_output();
        assert_eq!(
            e.on_settle("Do you want to proceed? (y/n)", Some("claude")),
            Activity::Waiting
        );
        e.on_output();
        assert_eq!(e.on_settle("All done.\n❯ ", Some("claude")), Activity::Idle);
        // Same fingerprint returns cached verdict without reclassifying.
        assert_eq!(e.on_settle("All done.\n❯ ", Some("claude")), Activity::Idle);
    }

    #[test]
    fn fingerprint_trims() {
        assert_eq!(activity_fingerprint("\n\na  \nb\n\n", 12), "a\nb");
    }
}
