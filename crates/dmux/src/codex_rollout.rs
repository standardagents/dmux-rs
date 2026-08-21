//! Incremental, bounded extraction of images from Codex rollout JSONL files.
//!
//! Rollouts are append-only in normal operation, but a process can observe a
//! partial final line or a file replacement between sweeps. `RolloutTailer`
//! keeps only a byte cursor, one bounded partial line, and event identities;
//! it never logs or persists message content or image bytes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_IMAGE_BYTES: usize = crate::images::MAX_ENCODED_BYTES;
pub const MAX_LINE_BYTES: usize = 30 * 1024 * 1024;
pub const MAX_READ_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ASSETS_PER_MESSAGE: usize = crate::images::MAX_IMAGES_PER_MESSAGE;
const MAX_SEEN_EVENTS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RolloutIdentity {
    pub session_id: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EventIdentity {
    message_id: String,
    content_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutAttachment {
    /// Stable identity: `<message id>:<content array index>`.
    pub event_id: String,
    pub session_id: String,
    pub source_path: PathBuf,
    pub message_id: String,
    pub content_index: usize,
    pub media_type: &'static str,
    pub encoded_png: Vec<u8>,
    pub sha256: [u8; 32],
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct RolloutRead {
    pub attachments: Vec<RolloutAttachment>,
    /// The cursor reached the file length observed at the start of this read.
    pub caught_up: bool,
}

#[derive(Debug, Default)]
struct Cursor {
    offset: u64,
    partial: Vec<u8>,
    seen: HashSet<EventIdentity>,
    seen_order: VecDeque<EventIdentity>,
    file_id: Option<FileId>,
    modified: Option<std::time::SystemTime>,
    discarding_line: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn file_id(metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_id(_metadata: &std::fs::Metadata) -> Option<FileId> {
    None
}

/// Stateful tailer keyed by Codex session identity and the exact source path.
#[derive(Debug, Default)]
pub struct RolloutTailer {
    cursors: HashMap<RolloutIdentity, Cursor>,
}

impl RolloutTailer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn retain(&mut self, identities: &HashSet<RolloutIdentity>) {
        self.cursors
            .retain(|identity, _| identities.contains(identity));
    }

    /// Read newly appended complete JSONL records from `source_path`.
    ///
    /// A shorter file resets the cursor, which handles truncation and atomic
    /// replacement. A final unterminated line stays buffered for the next
    /// call. The source must be a Codex rollout selected by tracking; this
    /// method deliberately does not inspect arbitrary paths.
    pub fn read(
        &mut self,
        session_id: &str,
        source_path: impl AsRef<Path>,
    ) -> io::Result<RolloutRead> {
        let source_path = source_path.as_ref().to_path_buf();
        let identity = RolloutIdentity {
            session_id: session_id.to_owned(),
            source_path: source_path.clone(),
        };
        let mut file = File::open(&source_path)?;
        let metadata = file.metadata()?;
        let len = metadata.len();
        let current_file_id = file_id(&metadata);
        let modified = metadata.modified().ok();
        let cursor = self.cursors.entry(identity.clone()).or_default();
        if len < cursor.offset
            || (len <= cursor.offset
                && cursor
                    .modified
                    .zip(modified)
                    .is_some_and(|(previous, current)| previous != current))
            || cursor
                .file_id
                .zip(current_file_id)
                .is_some_and(|(previous, current)| previous != current)
        {
            cursor.offset = 0;
            cursor.partial.clear();
            cursor.seen.clear();
            cursor.seen_order.clear();
        }
        cursor.file_id = current_file_id;
        cursor.modified = modified;
        file.seek(SeekFrom::Start(cursor.offset))?;
        let remaining = len.saturating_sub(cursor.offset);
        let to_read = remaining.min(MAX_READ_BYTES as u64) as usize;
        let mut bytes = vec![0; to_read];
        file.read_exact(&mut bytes)?;
        cursor.offset += to_read as u64;

        cursor.partial.extend_from_slice(&bytes);
        let complete = take_complete_lines(cursor, MAX_LINE_BYTES);
        let mut attachments = Vec::new();
        for line in complete {
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            decode_record(
                session_id,
                &source_path,
                &mut cursor.seen,
                &mut cursor.seen_order,
                &value,
                &mut attachments,
            );
        }
        Ok(RolloutRead {
            attachments,
            caught_up: cursor.offset >= len,
        })
    }
}

fn take_complete_lines(cursor: &mut Cursor, max_line_bytes: usize) -> Vec<Vec<u8>> {
    let mut complete = Vec::new();
    loop {
        let Some(newline) = cursor.partial.iter().position(|byte| *byte == b'\n') else {
            if cursor.partial.len() > max_line_bytes {
                cursor.partial.clear();
                cursor.discarding_line = true;
            }
            break;
        };
        if cursor.discarding_line || newline > max_line_bytes {
            cursor.partial.drain(..=newline);
            cursor.discarding_line = false;
            continue;
        }
        complete.push(cursor.partial.drain(..=newline).collect());
    }
    complete
}

fn decode_record(
    session_id: &str,
    source_path: &Path,
    seen: &mut HashSet<EventIdentity>,
    seen_order: &mut VecDeque<EventIdentity>,
    value: &Value,
    out: &mut Vec<RolloutAttachment>,
) {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return;
    }
    let payload = &value["payload"];
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return;
    }
    let Some(message_id) = payload.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(content) = payload.get("content").and_then(Value::as_array) else {
        return;
    };
    let mut assets = 0;
    for (content_index, item) in content.iter().enumerate() {
        if assets == MAX_ASSETS_PER_MESSAGE {
            tracing::warn!(
                message_id,
                limit = MAX_ASSETS_PER_MESSAGE,
                "image attachment limit reached"
            );
            break;
        }
        if item.get("type").and_then(Value::as_str) != Some("input_image") {
            continue;
        }
        let event = EventIdentity {
            message_id: message_id.to_owned(),
            content_index,
        };
        if seen.contains(&event) {
            continue;
        }
        let Some(url) = item.get("image_url").and_then(Value::as_str) else {
            diagnostic(&event, "image URL is missing");
            continue;
        };
        let Some(encoded) = url.strip_prefix("data:image/png;base64,") else {
            diagnostic(&event, "only PNG data URLs are supported");
            continue;
        };
        let Ok(png) = decode_base64_bounded(encoded, MAX_IMAGE_BYTES) else {
            diagnostic(&event, "PNG base64 is invalid or exceeds 20 MiB");
            continue;
        };
        let Ok((width, height)) = validate_png_dimensions(&png) else {
            diagnostic(&event, "PNG structure or dimensions are invalid");
            continue;
        };
        seen.insert(event.clone());
        seen_order.push_back(event.clone());
        if seen_order.len() > MAX_SEEN_EVENTS {
            if let Some(expired) = seen_order.pop_front() {
                seen.remove(&expired);
            }
        }
        let sha256 = Sha256::digest(&png).into();
        out.push(RolloutAttachment {
            event_id: format!("{}:{}", event.message_id, event.content_index),
            session_id: session_id.to_owned(),
            source_path: source_path.to_path_buf(),
            message_id: event.message_id,
            content_index: event.content_index,
            media_type: "image/png",
            encoded_png: png,
            sha256,
            width,
            height,
        });
        assets += 1;
    }
}

fn diagnostic(event: &EventIdentity, reason: &'static str) {
    tracing::warn!(
        event_id = %format_args!("{}:{}", event.message_id, event.content_index),
        reason,
        "image attachment rejected"
    );
}

fn decode_base64_bounded(input: &str, max_bytes: usize) -> Result<Vec<u8>, ()> {
    if input.is_empty() || !input.len().is_multiple_of(4) || input.len() / 4 * 3 > max_bytes + 2 {
        return Err(());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(input.as_bytes())
        .map_err(|_| ())?;
    (decoded.len() <= max_bytes).then_some(decoded).ok_or(())
}

/// Validate the PNG signature and IHDR dimensions without decoding pixels.
/// The caller owns the bounded byte buffer; this function never allocates.
pub fn validate_png_dimensions(bytes: &[u8]) -> Result<(u32, u32), ()> {
    crate::images::validate_png_dimensions(bytes).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "dmux-codex-rollout-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn fixture() -> (PathBuf, File) {
        let path = temp_path();
        let file = File::create(&path).unwrap();
        (path, file)
    }

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    fn record(message_id: &str, image_url: &str) -> String {
        serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message", "id": message_id, "role": "user",
                "content": [
                    {"type": "input_text", "text": "<image>"},
                    {"type": "input_image", "image_url": image_url, "detail": "high"},
                    {"type": "input_text", "text": "</image>"}
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn decodes_user_png_and_deduplicates_event_identity() {
        let (path, mut file) = fixture();
        let line = record("msg-1", &format!("data:image/png;base64,{PNG_1X1}"));
        writeln!(file, "{line}").unwrap();
        writeln!(file, "{line}").unwrap();
        let mut tailer = RolloutTailer::new();
        let got = tailer.read("session-1", &path).unwrap().attachments;
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].width, got[0].height), (1, 1));
        assert_eq!(got[0].message_id, "msg-1");
        assert_eq!(got[0].event_id, "msg-1:1");
        assert_eq!(got[0].media_type, "image/png");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn retains_partial_line_until_next_read() {
        let (path, mut file) = fixture();
        let line = record("msg-2", &format!("data:image/png;base64,{PNG_1X1}"));
        file.write_all(line.as_bytes()).unwrap();
        file.flush().unwrap();
        let mut tailer = RolloutTailer::new();
        assert!(tailer
            .read("session-2", &path)
            .unwrap()
            .attachments
            .is_empty());
        writeln!(file).unwrap();
        file.flush().unwrap();
        assert_eq!(
            tailer.read("session-2", &path).unwrap().attachments.len(),
            1
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepts_only_png_data_urls_and_valid_dimensions() {
        let (path, mut file) = fixture();
        writeln!(file, "{}", record("jpeg", "data:image/jpeg;base64,AAAA")).unwrap();
        writeln!(file, "{}", record("bad", "data:image/png;base64,AAAA")).unwrap();
        file.flush().unwrap();
        assert!(RolloutTailer::new()
            .read("session-3", &path)
            .unwrap()
            .attachments
            .is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn truncation_resets_seen_events_and_cursor() {
        let (path, mut file) = fixture();
        let line = record("msg-4", &format!("data:image/png;base64,{PNG_1X1}"));
        writeln!(file, "{line}").unwrap();
        file.flush().unwrap();
        let mut tailer = RolloutTailer::new();
        assert_eq!(
            tailer.read("session-4", &path).unwrap().attachments.len(),
            1
        );
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let replacement = record("m", &format!("data:image/png;base64,{PNG_1X1}"));
        writeln!(file, "{replacement}").unwrap();
        file.flush().unwrap();
        let got = tailer.read("session-4", &path).unwrap().attachments;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].message_id, "m");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_line_is_discarded_between_valid_lines() {
        let mut cursor = Cursor::default();
        cursor.partial.extend_from_slice(b"first\n");
        cursor.partial.extend_from_slice(&[b'x'; 9]);
        cursor.partial.extend_from_slice(b"\nlast\n");
        assert_eq!(
            take_complete_lines(&mut cursor, 8),
            vec![b"first\n".to_vec(), b"last\n".to_vec()]
        );
        assert!(cursor.partial.is_empty());
    }
}
