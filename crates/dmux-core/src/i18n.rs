//! Minimal i18n: keyed string catalog with en/ja locales (the TS scheme —
//! `t()` lookups, no ICU machinery). The locale comes from the `language`
//! setting; unknown keys and locales fall back to English.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Ja,
}

static LOCALE: AtomicU8 = AtomicU8::new(0);

pub fn set_locale(name: &str) {
    let value = match name {
        "ja" => 1,
        _ => 0,
    };
    LOCALE.store(value, Ordering::Relaxed);
}

pub fn locale() -> Locale {
    match LOCALE.load(Ordering::Relaxed) {
        1 => Locale::Ja,
        _ => Locale::En,
    }
}

/// (key, en, ja) — the visible chrome strings. Japanese follows the TS
/// catalog's terminology (ペイン, ワークツリー, 設定…).
const CATALOG: &[(&str, &str, &str)] = &[
    ("menu.rename", "Rename pane", "ペインの名前を変更"),
    ("menu.hide", "Hide pane", "ペインを非表示"),
    ("menu.show", "Show pane", "ペインを表示"),
    ("menu.merge", "Merge worktree…", "ワークツリーをマージ…"),
    ("menu.pr", "Create PR…", "PRを作成…"),
    ("menu.copy_path", "Copy path", "パスをコピー"),
    ("menu.editor", "Open in editor", "エディタで開く"),
    ("menu.close", "Close pane", "ペインを閉じる"),
    ("menu.new_agents", "New agents…", "新しいエージェント…"),
    ("menu.new_terminal", "New terminal", "新しいターミナル"),
    ("menu.add_project", "Add project…", "プロジェクトを追加…"),
    ("menu.settings", "Settings…", "設定…"),
    ("menu.logs", "Logs…", "ログ…"),
    ("menu.shortcuts", "Shortcuts…", "ショートカット…"),
    ("menu.detach", "Detach", "デタッチ"),
    ("dialog.rename_title", "Rename pane", "ペインの名前を変更"),
    ("dialog.close_title", "Close pane", "ペインを閉じる"),
    ("dialog.close_body", "Close '{}'? The process will be killed.", "'{}' を閉じますか？プロセスは終了されます。"),
    ("dialog.close_confirm", "Close", "閉じる"),
    ("dialog.cancel", "Cancel", "キャンセル"),
    ("dialog.merge_title", "Merge worktree", "ワークツリーをマージ"),
    ("dialog.merge_confirm", "Merge", "マージ"),
    ("dialog.add_project_title", "Add project", "プロジェクトを追加"),
    ("welcome.new_agents", "New agents", "新しいエージェント"),
    ("welcome.new_terminal", "New terminal", "新しいターミナル"),
    ("welcome.settings", "Settings", "設定"),
    ("welcome.shortcuts", "Shortcuts", "ショートカット"),
    ("welcome.tagline", "The Agent Multiplexer", "The Agent Multiplexer"),
    ("toast.settings_saved", "Settings saved", "設定が保存されました"),
    ("toast.pane_hidden", "Pane hidden (still running)", "ペインを非表示にしました（実行中）"),
    ("toast.pane_shown", "Pane shown", "ペインを表示しました"),
    ("hint.select", "select", "選択"),
    ("hint.run", "run", "実行"),
    ("hint.close", "close", "閉じる"),
    ("hint.save", "save", "保存"),
    ("hint.cancel", "cancel", "キャンセル"),
    ("agent.launch", "Launch", "起動"),
    ("agent.title", "New Agents", "新しいエージェント"),
    ("agent.prompt_label", "Prompt", "プロンプト"),
    ("agent.allocate", "Allocate panes", "ペインを割り当て"),
    ("agent.permissions", "Permissions", "権限"),
];

/// Translate a key in the active locale (en fallback, key as last resort).
pub fn t(key: &str) -> &'static str {
    let entry = CATALOG.iter().find(|(k, _, _)| *k == key);
    match (entry, locale()) {
        (Some((_, _, ja)), Locale::Ja) => ja,
        (Some((_, en, _)), _) => en,
        (None, _) => {
            debug_assert!(false, "missing i18n key: {key}");
            "??"
        }
    }
}

/// Translate with a single `{}` placeholder.
pub fn tf(key: &str, arg: &str) -> String {
    t(key).replacen("{}", arg, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test: the locale is process-global state, so parallel tests would
    /// race each other's `set_locale` calls.
    #[test]
    fn locale_switching_and_fallback() {
        set_locale("en");
        assert_eq!(t("menu.close"), "Close pane");
        set_locale("ja");
        assert_eq!(t("menu.close"), "ペインを閉じる");
        assert_eq!(tf("dialog.close_body", "x"), "'x' を閉じますか？プロセスは終了されます。");
        // Unknown locale falls back to English.
        set_locale("fr");
        assert_eq!(t("menu.settings"), "Settings…");
        set_locale("en");
    }
}
