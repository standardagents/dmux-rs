use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::Theme;

use crate::github::GitHubIssue;

#[derive(Clone, Copy)]
struct Column {
    x: u16,
    width: u16,
}

pub(super) struct IssueTable {
    number: Column,
    title: Column,
    labels: Option<Column>,
    updated: Option<Column>,
}

impl IssueTable {
    pub(super) fn new(width: u16) -> Self {
        let number = Column { x: 6, width: 7 };
        let title_x = 14;
        let mut title_end = width;
        let mut labels = None;
        let mut updated = None;

        if width >= 43 {
            let column = Column {
                x: width - 10,
                width: 10,
            };
            title_end = column.x.saturating_sub(2);
            updated = Some(column);
        }
        if width >= 59 {
            let column = Column {
                x: updated.expect("updated column exists").x - 16,
                width: 14,
            };
            title_end = column.x.saturating_sub(2);
            labels = Some(column);
        }

        Self {
            number,
            title: Column {
                x: title_x,
                width: title_end.saturating_sub(title_x),
            },
            labels,
            updated,
        }
    }

    pub(super) fn draw_header(&self, buf: &mut CellBuffer, row: Rect, theme: &Theme, bg: Color) {
        for (column, label) in [
            (Some(self.number), "#"),
            (Some(self.title), "TITLE"),
            (self.labels, "LABELS"),
            (self.updated, "UPDATED"),
        ] {
            if let Some(column) = column {
                draw_cell(
                    buf,
                    row,
                    column,
                    label,
                    theme.text_faint,
                    bg,
                    AttrFlags::BOLD,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_row(
        &self,
        buf: &mut CellBuffer,
        row: Rect,
        issue: &GitHubIssue,
        theme: &Theme,
        bg: Color,
        focused: bool,
        selected: bool,
    ) {
        buf.fill(
            row,
            &Cell {
                bg,
                ..Cell::default()
            },
        );
        buf.draw_text(
            row.x + 4,
            row.y,
            if selected { "◼" } else { "◻" },
            if selected {
                theme.accent
            } else {
                theme.text_dim
            },
            bg,
            AttrFlags::empty(),
            row,
        );
        draw_cell(
            buf,
            row,
            self.number,
            &format!("#{:<6}", issue.number),
            theme.accent,
            bg,
            AttrFlags::BOLD,
        );
        draw_cell(
            buf,
            row,
            self.title,
            &issue.title,
            if focused { theme.text } else { theme.text_dim },
            bg,
            if focused {
                AttrFlags::BOLD
            } else {
                AttrFlags::empty()
            },
        );
        if let Some(column) = self.labels {
            draw_cell(
                buf,
                row,
                column,
                &issue.labels.join(", "),
                theme.text_dim,
                bg,
                AttrFlags::empty(),
            );
        }
        if let Some(column) = self.updated {
            let value: String = issue.updated_at.chars().take(10).collect();
            draw_cell(
                buf,
                row,
                column,
                &value,
                theme.text_dim,
                bg,
                AttrFlags::empty(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_cell(
    buf: &mut CellBuffer,
    row: Rect,
    column: Column,
    value: &str,
    fg: Color,
    bg: Color,
    attrs: AttrFlags,
) {
    let cell = Rect::new(row.x + column.x, row.y, column.width, 1);
    let clipped = clipped(value, column.width);
    buf.draw_text(cell.x, cell.y, &clipped, fg, bg, attrs, cell);
}

fn clipped(value: &str, width: u16) -> String {
    let width = width as usize;
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, title: &str, label: &str) -> GitHubIssue {
        GitHubIssue {
            repository: "owner/repo".into(),
            number,
            title: title.into(),
            url: format!("https://github.com/owner/repo/issues/{number}"),
            labels: vec![label.into()],
            assignees: vec!["andrew".into()],
            updated_at: "2026-08-19T21:00:00Z".into(),
        }
    }

    fn text(buf: &CellBuffer, y: u16) -> String {
        (0..buf.area().w).map(|x| buf.get(x, y).ch).collect()
    }

    fn column(buf: &CellBuffer, y: u16, value: &str) -> Option<u16> {
        let chars: Vec<_> = value.chars().collect();
        (0..buf.area().w).find(|x| {
            chars
                .iter()
                .enumerate()
                .all(|(offset, ch)| buf.get(*x + offset as u16, y).ch == *ch)
        })
    }

    #[test]
    fn wide_rows_align_metadata_under_shared_headers() {
        let theme = Theme::default();
        let mut buf = CellBuffer::new(96, 3);
        let table = IssueTable::new(96);
        table.draw_header(&mut buf, Rect::new(0, 0, 96, 1), &theme, theme.bg_panel);
        table.draw_row(
            &mut buf,
            Rect::new(0, 1, 96, 1),
            &issue(7, "Short title", "bug"),
            &theme,
            theme.bg_panel,
            true,
            false,
        );
        table.draw_row(
            &mut buf,
            Rect::new(0, 2, 96, 1),
            &issue(1024, "A different title", "performance"),
            &theme,
            theme.bg_panel,
            false,
            false,
        );

        for (header_label, first_value, second_value) in [
            ("LABELS", "bug", "performance"),
            ("UPDATED", "2026-08-19", "2026-08-19"),
        ] {
            let x = column(&buf, 0, header_label).unwrap();
            assert_eq!(column(&buf, 1, first_value), Some(x));
            assert_eq!(column(&buf, 2, second_value), Some(x));
        }
        assert!(!text(&buf, 0).contains("ASSIGNEE"));
        assert!(!text(&buf, 1).contains("@andrew"));
    }

    #[test]
    fn narrow_rows_keep_issue_identity_and_omit_metadata() {
        let theme = Theme::default();
        let mut buf = CellBuffer::new(36, 2);
        let table = IssueTable::new(36);
        table.draw_header(&mut buf, Rect::new(0, 0, 36, 1), &theme, theme.bg_panel);
        table.draw_row(
            &mut buf,
            Rect::new(0, 1, 36, 1),
            &issue(77, "Keep this useful title visible", "performance"),
            &theme,
            theme.bg_panel,
            true,
            true,
        );

        let header = text(&buf, 0);
        let row = text(&buf, 1);
        assert!(header.contains("#       TITLE"));
        assert!(!header.contains("LABELS"));
        assert!(!header.contains("UPDATED"));
        assert!(row.contains("◼ #77"));
        assert!(row.contains("Keep this useful"));
        assert!(row.contains('…'));
        assert!(!row.contains("performance"));
        assert!(!row.contains("2026-08-19"));
    }

    #[test]
    fn long_titles_stop_before_metadata_columns() {
        let theme = Theme::default();
        let mut buf = CellBuffer::new(96, 1);
        let table = IssueTable::new(96);
        table.draw_row(
            &mut buf,
            Rect::new(0, 0, 96, 1),
            &issue(
                1,
                "A title long enough to reach every reserved metadata column in the row",
                "bug",
            ),
            &theme,
            theme.bg_panel,
            true,
            false,
        );

        let row = text(&buf, 0);
        assert!(row.contains("reserved metadata"));
        assert!(row.contains("…  bug"));
        assert!(!row.contains("@andrew"));
        assert!(row.ends_with("2026-08-19"));
    }
}
