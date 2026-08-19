use std::sync::{Arc, Mutex};

use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_core::SettingsStore;
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// Read-only inference status: the configured primary/backup targets plus
/// which provider credentials were detected (env vars or the stored
/// `~/.dmux/inference-credentials.json`). Configuration itself is edited as
/// `inferencePrimary`/`inferenceBackup` in settings.json.
pub struct InferProvidersView {
    primary: String,
    backup: String,
    providers: Vec<(String, String, bool)>,
}

fn target_line(store: &SettingsStore, key: &str) -> String {
    store
        .get(key)
        .and_then(dmux_infer::Target::from_value)
        .map(|t| format!("{} / {}", t.provider_id, t.model_id))
        .unwrap_or_else(|| "(not configured)".to_string())
}

impl InferProvidersView {
    pub fn new(settings: Arc<Mutex<SettingsStore>>) -> Self {
        let (primary, backup) = {
            let store = settings.lock().unwrap();
            (
                target_line(&store, "inferencePrimary"),
                target_line(&store, "inferenceBackup"),
            )
        };
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let providers = dmux_infer::provider_statuses(&home)
            .into_iter()
            .map(|p| (p.id.to_string(), p.env_key.to_string(), p.has_key))
            .collect();
        Self {
            primary,
            backup,
            providers,
        }
    }
}

impl View for InferProvidersView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let h = (self.providers.len() as u16 + 9).min(area.h.saturating_sub(2));
        let rect = centered(area, area.w.min(64), h);
        let inner = draw_panel(
            buf,
            rect,
            "Inference Providers",
            ctx.theme,
            PanelStyle::Modal,
        );
        let bg = ctx.theme.bg_raised;

        buf.draw_text(
            inner.x + 1,
            inner.y,
            "Primary",
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        buf.draw_text(
            inner.x + 11,
            inner.y,
            &self.primary,
            ctx.theme.accent,
            bg,
            AttrFlags::empty(),
            inner,
        );
        buf.draw_text(
            inner.x + 1,
            inner.y + 1,
            "Backup",
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        buf.draw_text(
            inner.x + 11,
            inner.y + 1,
            &self.backup,
            ctx.theme.text_dim,
            bg,
            AttrFlags::empty(),
            inner,
        );

        buf.draw_text(
            inner.x + 1,
            inner.y + 3,
            "Detected credentials",
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        let mut y = inner.y + 4;
        for (id, env_key, has_key) in &self.providers {
            if y >= inner.bottom().saturating_sub(2) {
                break;
            }
            let (mark, color) = if *has_key {
                ("✓", ctx.theme.ok)
            } else {
                ("–", ctx.theme.text_faint)
            };
            buf.draw_text(inner.x + 1, y, mark, color, bg, AttrFlags::BOLD, inner);
            buf.draw_text(
                inner.x + 3,
                y,
                id,
                ctx.theme.text,
                bg,
                AttrFlags::empty(),
                inner,
            );
            buf.draw_text(
                inner.x + 16,
                y,
                env_key,
                ctx.theme.text_faint,
                bg,
                AttrFlags::empty(),
                inner,
            );
            y += 1;
        }
        buf.draw_text(
            inner.x + 1,
            inner.bottom().saturating_sub(2),
            "edit: \"inferencePrimary\" / \"inferenceBackup\" in settings.json",
            ctx.theme.text_faint,
            bg,
            AttrFlags::ITALIC,
            inner,
        );
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) || vkeys::is_enter(key) {
            ViewResult::Close
        } else {
            ViewResult::Stay
        }
    }
}
