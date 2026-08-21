//! Background polling for provider-owned attachment streams.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::codex_rollout::{RolloutAttachment, RolloutIdentity, RolloutTailer};
use crate::images::{decode_asset_message, encode_asset_message, ImageAsset, ImageMessage};
use crate::AppMsg;

#[derive(Debug)]
struct Source {
    session_id: String,
    path: PathBuf,
    pending: Option<ImageMessage>,
}

enum Command {
    Observe {
        pane_slug: String,
        session_id: String,
        path: PathBuf,
    },
    Retain {
        pane_slugs: std::collections::HashSet<String>,
    },
    Shutdown,
}

pub struct Monitor {
    commands: mpsc::Sender<Command>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Monitor {
    pub fn spawn(app: tokio::sync::mpsc::UnboundedSender<AppMsg>) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dmux-image-monitor".to_string())
            .spawn(move || run(receiver, app))?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    pub fn observe(&self, pane_slug: String, session_id: String, path: PathBuf) {
        let _ = self.commands.send(Command::Observe {
            pane_slug,
            session_id,
            path,
        });
    }

    pub fn retain_panes(&self, pane_slugs: Vec<String>) {
        let _ = self.commands.send(Command::Retain {
            pane_slugs: pane_slugs.into_iter().collect(),
        });
    }
}

impl crate::App {
    pub(super) fn apply_image_message(&mut self, pane_slug: String, message: ImageMessage) {
        if self.images.apply(pane_slug, message) {
            self.dirty = true;
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(commands: mpsc::Receiver<Command>, app: tokio::sync::mpsc::UnboundedSender<AppMsg>) {
    let mut sources: HashMap<String, Source> = HashMap::new();
    let mut tailer = RolloutTailer::new();
    let loopback = std::env::var("DMUX_IMAGES_LOOPBACK").is_ok_and(|value| value == "1");
    loop {
        match commands.recv_timeout(Duration::from_millis(400)) {
            Ok(Command::Observe {
                pane_slug,
                session_id,
                path,
            }) => {
                observe_source(&mut sources, pane_slug, session_id, path);
            }
            Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Command::Retain { pane_slugs }) => {
                sources.retain(|slug, _| pane_slugs.contains(slug));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Observe {
                    pane_slug,
                    session_id,
                    path,
                } => {
                    observe_source(&mut sources, pane_slug, session_id, path);
                }
                Command::Shutdown => return,
                Command::Retain { pane_slugs } => {
                    sources.retain(|slug, _| pane_slugs.contains(slug));
                }
            }
        }
        let identities: HashSet<RolloutIdentity> = sources
            .values()
            .map(|source| RolloutIdentity {
                session_id: source.session_id.clone(),
                source_path: source.path.clone(),
            })
            .collect();
        tailer.retain(&identities);
        for (pane_slug, source) in &mut sources {
            match tailer.read(&source.session_id, &source.path) {
                Ok(batch) => {
                    if let Some(message) = newest_message(batch.attachments) {
                        source.pending = Some(message);
                    }
                    if batch.caught_up {
                        if let Some(mut message) = source.pending.take() {
                            if loopback {
                                let encoded = match encode_asset_message(&message) {
                                    Ok(encoded) => encoded,
                                    Err(reason) => {
                                        tracing::warn!(pane = %pane_slug, reason, "image loopback encode failed");
                                        continue;
                                    }
                                };
                                message = match decode_asset_message(&encoded) {
                                    Ok(message) => message,
                                    Err(reason) => {
                                        tracing::warn!(pane = %pane_slug, reason, "image loopback decode failed");
                                        continue;
                                    }
                                };
                            }
                            let _ = app.send(AppMsg::ImagesChanged {
                                pane_slug: pane_slug.clone(),
                                message,
                            });
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    tracing::debug!(pane = %pane_slug, kind = ?err.kind(), "image source read failed");
                }
            }
        }
    }
}

fn observe_source(
    sources: &mut HashMap<String, Source>,
    pane_slug: String,
    session_id: String,
    path: PathBuf,
) {
    let unchanged = sources
        .get(&pane_slug)
        .is_some_and(|source| source.session_id == session_id && source.path == path);
    if !unchanged {
        sources.insert(
            pane_slug,
            Source {
                session_id,
                path,
                pending: None,
            },
        );
    }
}

fn newest_message(attachments: Vec<RolloutAttachment>) -> Option<ImageMessage> {
    let message_id = attachments.last()?.message_id.clone();
    let assets = attachments
        .into_iter()
        .filter(|attachment| attachment.message_id == message_id)
        .map(|attachment| ImageAsset {
            event_id: attachment.event_id,
            media_type: attachment.media_type.to_string(),
            png: attachment.encoded_png.into(),
            pixel_width: attachment.width,
            pixel_height: attachment.height,
            digest: attachment.sha256,
        })
        .collect();
    Some(ImageMessage { message_id, assets })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(message_id: &str, index: usize) -> RolloutAttachment {
        RolloutAttachment {
            event_id: format!("{message_id}:{index}"),
            session_id: "session".to_string(),
            source_path: PathBuf::from("synthetic-rollout.jsonl"),
            message_id: message_id.to_string(),
            content_index: index,
            media_type: "image/png",
            encoded_png: vec![index as u8],
            sha256: [index as u8; 32],
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn keeps_all_assets_from_only_the_newest_message() {
        let message = newest_message(vec![
            attachment("old", 0),
            attachment("new", 0),
            attachment("new", 1),
        ])
        .unwrap();
        assert_eq!(message.message_id, "new");
        assert_eq!(message.assets.len(), 2);
    }
}
