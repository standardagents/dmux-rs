//! Filesystem path picker for **Add project** (#32): starts in dmux's
//! launch directory, lists entries (directories first), filters as the user
//! types, traverses with the keyboard, and accepts only directories. The
//! model half is pure and unit-tested; the view half reuses the panel /
//! list / input primitives.

use std::path::{Path, PathBuf};

use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{draw_hint_bar, draw_panel, panel_frame, ClickMap, ListState, PanelStyle, TextInput};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

/// Click-target base for filter-field cursor placement; offset = column.
const TAG_FIELD: u64 = 10_000;

/// Bounded directory read: a single readdir capped at this many entries so a
/// huge or slow (network) directory can't stall input or rendering.
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
}

/// One directory's picker content, dirs first then files, each half sorted
/// case-insensitively. `truncated` marks a listing cut at MAX_ENTRIES.
pub struct DirListing {
    pub entries: Vec<FsEntry>,
    pub truncated: bool,
}

/// Read a directory for the picker. Errors (permissions, vanished paths)
/// come back as a message for the inline error line — never a crash.
pub fn read_dir_listing(dir: &Path) -> Result<DirListing, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for item in rd {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        // Follow symlinks so a linked directory is enterable; a broken link
        // simply lists as a file (not accepted, but visible).
        let is_dir = std::fs::metadata(item.path())
            .map(|m| m.is_dir())
            .unwrap_or(false);
        entries.push(FsEntry { name, is_dir });
    }
    sort_dirs_first(&mut entries);
    Ok(DirListing { entries, truncated })
}

/// Directories first, each group sorted case-insensitively (#32).
pub fn sort_dirs_first(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Case-insensitive prefix filter on the typed segment.
pub fn filter_entries<'a>(entries: &'a [FsEntry], query: &str) -> Vec<&'a FsEntry> {
    let q = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.name.to_lowercase().starts_with(&q))
        .collect()
}

/// Interpret typed text as a jump target: absolute paths and `~` (alone or
/// `~/…`) resolve directly; anything else is a filter, not a jump.
pub fn resolve_typed_path(query: &str, home: &Path) -> Option<PathBuf> {
    let q = query.trim();
    if q == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = q.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    if q.starts_with('/') {
        return Some(PathBuf::from(q));
    }
    None
}

pub struct PathPickerView {
    dir: PathBuf,
    input: TextInput,
    listing: Result<DirListing, String>,
    list: ListState,
    /// Inline error from the last action (invalid path, file accepted…).
    notice: Option<String>,
    home: PathBuf,
}

impl PathPickerView {
    pub fn new(start: PathBuf) -> Self {
        let listing = read_dir_listing(&start);
        Self {
            dir: start,
            input: TextInput::with_value("").placeholder("type to filter · / or ~ jumps · ⏎ opens"),
            listing,
            list: ListState::default(),
            notice: None,
            home: dirs_home(),
        }
    }

    fn reload(&mut self) {
        self.listing = read_dir_listing(&self.dir);
        self.list = ListState::default();
        self.input =
            TextInput::with_value("").placeholder("type to filter · / or ~ jumps · ⏎ opens");
    }

    fn filtered(&self) -> Vec<FsEntry> {
        match &self.listing {
            Ok(l) => filter_entries(&l.entries, &self.input.value)
                .into_iter()
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Rows shown in the list: a synthetic accept row, then entries.
    fn row_count(&self) -> usize {
        1 + self.filtered().len()
    }

    fn enter_dir(&mut self, name: &str) {
        self.dir = self.dir.join(name);
        self.notice = None;
        self.reload();
    }

    fn go_parent(&mut self) {
        if let Some(parent) = self.dir.parent() {
            self.dir = parent.to_path_buf();
            self.notice = None;
            self.reload();
        }
    }

    fn jump_if_typed_path(&mut self) -> bool {
        if let Some(path) = resolve_typed_path(&self.input.value, &self.home) {
            if path.is_dir() {
                self.dir = path;
                self.notice = None;
                self.reload();
            } else {
                self.notice = Some(format!("not a directory: {}", path.display()));
            }
            return true;
        }
        false
    }

    fn activate(&mut self) -> ViewResult {
        if self.jump_if_typed_path() {
            return ViewResult::Stay;
        }
        if self.list.selected == 0 {
            // Synthetic "use this directory" row accepts the current dir.
            return ViewResult::CloseAnd(AppCmd::OpenProjectAt(
                self.dir.to_string_lossy().into_owned(),
            ));
        }
        let rows = self.filtered();
        let Some(entry) = rows.get(self.list.selected - 1).cloned() else {
            return ViewResult::Stay;
        };
        if entry.is_dir {
            self.enter_dir(&entry.name);
        } else {
            // Files stay visible for orientation but are never project roots.
            self.notice = Some(format!("'{}' is a file — pick a directory", entry.name));
        }
        ViewResult::Stay
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl View for PathPickerView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let w = area.w.saturating_sub(10).clamp(44, 90);
        let h = area.h.saturating_sub(6).clamp(14, 26);
        let rect = ctx.global(area, w, h);
        let inner = draw_panel(buf, rect, "Add project", ctx.theme, PanelStyle::Modal);
        let frame = panel_frame(inner);
        let content = frame.content;

        // Current directory (tail-truncated from the left so the leaf shows).
        let dir_str = self.dir.to_string_lossy();
        let max = content.w.saturating_sub(2) as usize;
        let shown: String = if dir_str.chars().count() > max {
            let tail: String = dir_str
                .chars()
                .rev()
                .take(max.saturating_sub(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("…{tail}")
        } else {
            dir_str.into_owned()
        };
        buf.draw_text(
            content.x + 1,
            content.y,
            &shown,
            ctx.theme.accent,
            ctx.theme.bg_panel,
            AttrFlags::BOLD,
            inner,
        );

        // Filter input, with per-cell click targets for cursor placement
        // (#96): the tag offset is the clicked column.
        let field = Rect::new(content.x + 1, content.y + 1, content.w.saturating_sub(2), 1);
        let cursor = self.input.draw(buf, field, ctx.theme, true);
        for col in 0..field.w {
            clicks.add(
                Rect::new(field.x + col, field.y, 1, 1),
                ClickTarget::Overlay(TAG_FIELD + col as u64),
            );
        }

        // Notice / error line.
        let msg_row = content.y + 2;
        if let Some(msg) =
            self.notice
                .as_deref()
                .or(self.listing.as_ref().err().map(|e| e.as_str()))
        {
            buf.draw_text(
                content.x + 1,
                msg_row,
                msg,
                ctx.theme.warn,
                ctx.theme.bg_panel,
                AttrFlags::empty(),
                inner,
            );
        }

        // Entry list: synthetic accept row, then dirs-first entries.
        let rows = self.filtered();
        let list_top = msg_row + 1;
        let visible = content.bottom().saturating_sub(list_top) as usize;
        self.list.clamp(self.row_count());
        self.list.ensure_visible(visible);
        let mut y = list_top;
        for (row_i, label, is_dir, dim) in
            std::iter::once((0usize, "▸ use this directory".to_string(), true, false))
                .chain(rows.iter().enumerate().map(|(i, e)| {
                    let label = if e.is_dir {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    };
                    (i + 1, label, e.is_dir, !e.is_dir)
                }))
                .skip(self.list.scroll)
                .take(visible)
        {
            let selected = ctx.active_overlay(row_i as u64, row_i == self.list.selected);
            let line = Rect::new(content.x, y, content.w, 1);
            let bg = if selected {
                ctx.theme.bg_selected
            } else {
                ctx.theme.bg_panel
            };
            buf.fill(
                line,
                &dmux_compositor::Cell {
                    bg,
                    ..Default::default()
                },
            );
            let fg = if row_i == 0 {
                ctx.theme.accent
            } else if is_dir {
                ctx.theme.text
            } else if dim {
                ctx.theme.text_faint
            } else {
                ctx.theme.text_dim
            };
            let attrs = if selected || row_i == 0 {
                AttrFlags::BOLD
            } else {
                AttrFlags::empty()
            };
            buf.draw_text(content.x + 2, y, &label, fg, bg, attrs, line);
            clicks.add(line, ClickTarget::Overlay(row_i as u64));
            y += 1;
        }
        if let Ok(l) = &self.listing {
            if l.truncated {
                buf.draw_text(
                    content.x + 2,
                    y.min(content.bottom().saturating_sub(1)),
                    "… listing capped",
                    ctx.theme.text_faint,
                    ctx.theme.bg_panel,
                    AttrFlags::empty(),
                    inner,
                );
            }
        }

        draw_hint_bar(
            buf,
            frame.footer,
            &[
                ("↑↓", "select"),
                ("⏎", "open/accept"),
                ("⌫", "parent"),
                ("esc", "cancel"),
            ],
            ctx.theme,
        );
        cursor
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) && self.input.value.is_empty() {
            self.list.step(-1, self.row_count());
            return ViewResult::Stay;
        }
        if vkeys::is_down(key) && self.input.value.is_empty() {
            self.list.step(1, self.row_count());
            return ViewResult::Stay;
        }
        if matches!(key.key, KeyCode::UpArrow) {
            self.list.step(-1, self.row_count());
            return ViewResult::Stay;
        }
        if matches!(key.key, KeyCode::DownArrow) {
            self.list.step(1, self.row_count());
            return ViewResult::Stay;
        }
        if vkeys::is_enter(key) {
            return self.activate();
        }
        if vkeys::is_left(key) {
            self.go_parent();
            return ViewResult::Stay;
        }
        if vkeys::is_tab(key) {
            // Complete to the first filtered entry; entering it when a dir.
            let rows = self.filtered();
            if let Some(first) = rows.first().cloned() {
                if first.is_dir {
                    self.enter_dir(&first.name);
                } else {
                    self.input = TextInput::with_value(&first.name);
                }
            }
            return ViewResult::Stay;
        }
        if matches!(key.key, KeyCode::Backspace) && self.input.value.is_empty() {
            self.go_parent();
            return ViewResult::Stay;
        }
        if let Some(ik) = vkeys::as_input_key(key) {
            self.input.handle(ik);
            self.list.selected = 0;
            self.notice = None;
            return ViewResult::Stay;
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        if tag >= TAG_FIELD {
            self.input.click_col((tag - TAG_FIELD) as u16);
            return ViewResult::Stay;
        }
        self.list.selected = tag as usize;
        self.activate()
    }

    fn on_hover(&mut self, tag: u64) -> u64 {
        if (tag as usize) < self.row_count() {
            self.list.selected = tag as usize;
        }
        tag
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        self.list.step(delta, self.row_count());
        ViewResult::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> FsEntry {
        FsEntry {
            name: name.into(),
            is_dir,
        }
    }

    #[test]
    fn picker_filter_receives_shared_word_navigation() {
        // Second text-input consumer (#96): the picker's filter field
        // routes through the same translation as Rename Pane.
        use dmux_host::{KeyCode, KeyEvent, Modifiers};
        let key = |code, mods| KeyEvent {
            key: code,
            modifiers: mods,
        };
        let mut v = PathPickerView::new(std::env::temp_dir());
        for c in "some words".chars() {
            v.on_key(&key(KeyCode::Char(c), Modifiers::NONE));
        }
        v.on_key(&key(KeyCode::LeftArrow, Modifiers::ALT));
        v.on_key(&key(KeyCode::Char('X'), Modifiers::NONE));
        assert_eq!(v.input.value, "some Xwords");
        v.on_key(&key(KeyCode::RightArrow, Modifiers::SUPER));
        v.on_key(&key(KeyCode::Char('!'), Modifiers::NONE));
        assert_eq!(v.input.value, "some Xwords!");
    }

    #[test]
    fn dirs_sort_first_case_insensitively() {
        let mut v = vec![
            entry("zeta.txt", false),
            entry("Beta", true),
            entry("alpha", true),
            entry("Apple.md", false),
        ];
        sort_dirs_first(&mut v);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta", "Apple.md", "zeta.txt"]);
    }

    #[test]
    fn filtering_is_prefix_and_case_insensitive() {
        let v = vec![
            entry("Projects", true),
            entry("pictures", true),
            entry("music", false),
        ];
        let hits = filter_entries(&v, "p");
        assert_eq!(hits.len(), 2);
        assert!(filter_entries(&v, "MU")[0].name == "music");
        assert!(filter_entries(&v, "zzz").is_empty());
    }

    #[test]
    fn typed_paths_resolve_absolute_and_home() {
        let home = Path::new("/Users/me");
        assert_eq!(
            resolve_typed_path("~", home),
            Some(PathBuf::from("/Users/me"))
        );
        assert_eq!(
            resolve_typed_path("~/code", home),
            Some(PathBuf::from("/Users/me/code"))
        );
        assert_eq!(
            resolve_typed_path("/tmp/x", home),
            Some(PathBuf::from("/tmp/x"))
        );
        // Relative text is a filter, not a jump.
        assert_eq!(resolve_typed_path("src", home), None);
    }

    #[test]
    fn picker_traverses_filters_and_accepts_dirs_only() {
        let root = std::env::temp_dir().join(format!("dmux-picker-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("proj a")).unwrap();
        std::fs::create_dir_all(root.join("proj-ü")).unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();

        // Initial listing shows the start directory, dirs first.
        let mut v = PathPickerView::new(root.clone());
        let rows = v.filtered();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].is_dir && rows[1].is_dir && !rows[2].is_dir);

        // Selecting the file row refuses with a notice, picker stays open.
        v.list.selected = 3; // 0 = accept row; entries follow dirs-first
        assert!(matches!(v.activate(), ViewResult::Stay));
        assert!(v.notice.as_deref().unwrap_or("").contains("is a file"));

        // Entering a unicode/space directory works; accept row returns it.
        v.list.selected = 2; // "proj-ü"
        assert!(matches!(v.activate(), ViewResult::Stay));
        assert!(v.dir.ends_with("proj-ü"));
        v.list.selected = 0;
        match v.activate() {
            ViewResult::CloseAnd(AppCmd::OpenProjectAt(p)) => assert!(p.ends_with("proj-ü")),
            other => panic!("expected accept, got {:?}", std::mem::discriminant(&other)),
        }

        // Vanished directory: listing becomes an inline error, no panic.
        let gone = root.join("gone");
        let v2 = PathPickerView::new(gone);
        assert!(v2.listing.is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hover_moves_selection_before_keyboard_navigation() {
        let mut view = PathPickerView::new(std::env::current_dir().unwrap());
        assert!(view.row_count() > 2);
        assert_eq!(view.on_hover(1), 1);
        assert_eq!(view.list.selected, 1);
        let key = KeyEvent {
            key: KeyCode::DownArrow,
            modifiers: dmux_host::Modifiers::NONE,
        };
        view.on_key(&key);
        assert_eq!(view.list.selected, 2);
    }
}
