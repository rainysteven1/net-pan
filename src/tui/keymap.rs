use ratatui::crossterm::event::KeyCode;

use super::{
    AppTab, BrowserApp, BrowserLayer, DownloadJobState, NavigationFocus, ProviderSettingKind,
    ProviderSettingsForm, RemoteItem,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyHint {
    pub key: &'static str,
    pub action: &'static str,
    pub primary: bool,
}

impl KeyHint {
    const fn secondary(key: &'static str, action: &'static str) -> Self {
        Self {
            key,
            action,
            primary: false,
        }
    }
}

struct Binding<I: Copy> {
    display: &'static str,
    keys: &'static [KeyCode],
    intent: I,
    action: &'static str,
    primary: bool,
    shown: fn(&BrowserApp) -> bool,
}

fn always(_: &BrowserApp) -> bool {
    true
}

fn never(_: &BrowserApp) -> bool {
    false
}

fn job_can_pause_or_resume(app: &BrowserApp) -> bool {
    matches!(
        app.download.state,
        Some(
            DownloadJobState::Queued
                | DownloadJobState::Scanning
                | DownloadJobState::Running
                | DownloadJobState::Paused
        )
    )
}

fn job_can_stop(app: &BrowserApp) -> bool {
    job_can_pause_or_resume(app)
}

fn intent_for<I: Copy>(bindings: &[Binding<I>], key: KeyCode) -> Option<I> {
    bindings
        .iter()
        .find(|binding| binding.keys.contains(&key))
        .map(|binding| binding.intent)
}

fn hints_for<I: Copy>(bindings: &[Binding<I>], app: &BrowserApp) -> Vec<KeyHint> {
    bindings
        .iter()
        .filter(|binding| (binding.shown)(app))
        .map(|binding| KeyHint {
            key: binding.display,
            action: binding.action,
            primary: binding.primary,
        })
        .collect()
}

pub(super) mod navigation {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        Select,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "navigate",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "navigate",
            primary: true,
            shown: never,
        },
        Binding {
            display: "Enter",
            keys: &[KeyCode::Enter],
            intent: Intent::Select,
            action: "select",
            primary: true,
            shown: always,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) mod accounts {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        Open,
        Activate,
        Add,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "account",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "account",
            primary: true,
            shown: never,
        },
        Binding {
            display: "Enter",
            keys: &[KeyCode::Enter, KeyCode::Char('e')],
            intent: Intent::Open,
            action: "open",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Space",
            keys: &[KeyCode::Char(' ')],
            intent: Intent::Activate,
            action: "activate",
            primary: true,
            shown: always,
        },
        Binding {
            display: "a",
            keys: &[KeyCode::Char('a'), KeyCode::Char('n')],
            intent: Intent::Add,
            action: "add",
            primary: true,
            shown: always,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) mod global_settings {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        Edit,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "global setting",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "global setting",
            primary: true,
            shown: never,
        },
        Binding {
            display: "Enter",
            keys: &[KeyCode::Enter],
            intent: Intent::Edit,
            action: "edit",
            primary: true,
            shown: always,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) mod provider_settings {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        Edit,
        Toggle,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "field",
            primary: true,
            shown: has_editable_field,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "field",
            primary: true,
            shown: never,
        },
        Binding {
            display: "Enter",
            keys: &[KeyCode::Enter],
            intent: Intent::Edit,
            action: "edit",
            primary: true,
            shown: has_editable_field,
        },
        Binding {
            display: "Space",
            keys: &[KeyCode::Char(' ')],
            intent: Intent::Toggle,
            action: "toggle",
            primary: false,
            shown: selected_field_is_boolean,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }

    fn has_editable_field(app: &BrowserApp) -> bool {
        app.provider_form
            .as_ref()
            .and_then(ProviderSettingsForm::selected_field)
            .is_some()
    }

    fn selected_field_is_boolean(app: &BrowserApp) -> bool {
        app.provider_form
            .as_ref()
            .and_then(ProviderSettingsForm::selected_field)
            .is_some_and(|field| field.spec.kind == ProviderSettingKind::Boolean)
    }
}

pub(super) mod logs {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        PageUp,
        PageDown,
        First,
        Last,
        All,
        Info,
        Error,
        CycleTarget,
        TogglePane,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "select",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "select",
            primary: true,
            shown: never,
        },
        Binding {
            display: "PageUp",
            keys: &[KeyCode::PageUp],
            intent: Intent::PageUp,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "PageDown",
            keys: &[KeyCode::PageDown],
            intent: Intent::PageDown,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "Home",
            keys: &[KeyCode::Home],
            intent: Intent::First,
            action: "first",
            primary: false,
            shown: never,
        },
        Binding {
            display: "End",
            keys: &[KeyCode::End],
            intent: Intent::Last,
            action: "last",
            primary: false,
            shown: never,
        },
        Binding {
            display: "a/i/e",
            keys: &[KeyCode::Char('a')],
            intent: Intent::All,
            action: "filter",
            primary: true,
            shown: always,
        },
        Binding {
            display: "i",
            keys: &[KeyCode::Char('i')],
            intent: Intent::Info,
            action: "filter",
            primary: true,
            shown: never,
        },
        Binding {
            display: "e",
            keys: &[KeyCode::Char('e')],
            intent: Intent::Error,
            action: "filter",
            primary: true,
            shown: never,
        },
        Binding {
            display: "f",
            keys: &[KeyCode::Char('f')],
            intent: Intent::CycleTarget,
            action: "target",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Tab",
            keys: &[KeyCode::Tab, KeyCode::BackTab],
            intent: Intent::TogglePane,
            action: "pane",
            primary: false,
            shown: always,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) mod transfers {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        PageUp,
        PageDown,
        First,
        Last,
        All,
        Active,
        Failed,
        TogglePause,
        Stop,
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "transfer",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "transfer",
            primary: true,
            shown: never,
        },
        Binding {
            display: "PageUp",
            keys: &[KeyCode::PageUp],
            intent: Intent::PageUp,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "PageDown",
            keys: &[KeyCode::PageDown],
            intent: Intent::PageDown,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "Home",
            keys: &[KeyCode::Home],
            intent: Intent::First,
            action: "first",
            primary: false,
            shown: never,
        },
        Binding {
            display: "End",
            keys: &[KeyCode::End],
            intent: Intent::Last,
            action: "last",
            primary: false,
            shown: never,
        },
        Binding {
            display: "a/r/f",
            keys: &[KeyCode::Char('a')],
            intent: Intent::All,
            action: "filter",
            primary: true,
            shown: always,
        },
        Binding {
            display: "r",
            keys: &[KeyCode::Char('r'), KeyCode::Char('v')],
            intent: Intent::Active,
            action: "filter",
            primary: true,
            shown: never,
        },
        Binding {
            display: "f",
            keys: &[KeyCode::Char('f')],
            intent: Intent::Failed,
            action: "filter",
            primary: true,
            shown: never,
        },
        Binding {
            display: "Space",
            keys: &[KeyCode::Char(' ')],
            intent: Intent::TogglePause,
            action: "pause/resume",
            primary: true,
            shown: job_can_pause_or_resume,
        },
        Binding {
            display: "x",
            keys: &[KeyCode::Char('x')],
            intent: Intent::Stop,
            action: "stop",
            primary: true,
            shown: job_can_stop,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) mod files {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::tui) enum Intent {
        Previous,
        Next,
        PageUp,
        PageDown,
        First,
        Last,
        Parent,
        Open,
        Download,
        Reload,
        Sources,
    }

    fn has_parent(app: &BrowserApp) -> bool {
        app.browser_layer() == BrowserLayer::DriveFolder
    }

    fn can_open(app: &BrowserApp) -> bool {
        app.selected_item().is_some_and(RemoteItem::is_folder)
    }

    fn can_download(app: &BrowserApp) -> bool {
        app.selected_download_target().is_some()
    }

    const BINDINGS: &[Binding<Intent>] = &[
        Binding {
            display: "Up/Down",
            keys: &[KeyCode::Up],
            intent: Intent::Previous,
            action: "item",
            primary: true,
            shown: always,
        },
        Binding {
            display: "Down",
            keys: &[KeyCode::Down],
            intent: Intent::Next,
            action: "select",
            primary: true,
            shown: never,
        },
        Binding {
            display: "PageUp",
            keys: &[KeyCode::PageUp],
            intent: Intent::PageUp,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "PageDown",
            keys: &[KeyCode::PageDown],
            intent: Intent::PageDown,
            action: "page",
            primary: false,
            shown: never,
        },
        Binding {
            display: "Home",
            keys: &[KeyCode::Home],
            intent: Intent::First,
            action: "first",
            primary: false,
            shown: never,
        },
        Binding {
            display: "End",
            keys: &[KeyCode::End],
            intent: Intent::Last,
            action: "last",
            primary: false,
            shown: never,
        },
        Binding {
            display: "Backspace",
            keys: &[KeyCode::Backspace],
            intent: Intent::Parent,
            action: "parent",
            primary: true,
            shown: has_parent,
        },
        Binding {
            display: "Enter",
            keys: &[KeyCode::Enter, KeyCode::Right],
            intent: Intent::Open,
            action: "open",
            primary: true,
            shown: can_open,
        },
        Binding {
            display: "d",
            keys: &[KeyCode::Char('d')],
            intent: Intent::Download,
            action: "download",
            primary: true,
            shown: can_download,
        },
        Binding {
            display: "r",
            keys: &[KeyCode::Char('r')],
            intent: Intent::Reload,
            action: "reload",
            primary: false,
            shown: always,
        },
        Binding {
            display: "o",
            keys: &[KeyCode::Char('o'), KeyCode::Char('p')],
            intent: Intent::Sources,
            action: "sources",
            primary: false,
            shown: always,
        },
    ];

    pub(in crate::tui) fn intent(key: KeyCode) -> Option<Intent> {
        intent_for(BINDINGS, key)
    }

    pub(super) fn hints(app: &BrowserApp) -> Vec<KeyHint> {
        hints_for(BINDINGS, app)
    }
}

pub(super) fn page_hints(app: &BrowserApp) -> Vec<KeyHint> {
    if app.global_settings_open {
        let mut hints = global_settings::hints(app);
        hints.push(KeyHint::secondary("Backspace", "back"));
        hints.push(KeyHint::secondary("q", "quit"));
        return hints;
    }
    let mut hints = if app.navigation_focus == NavigationFocus::Navigation {
        navigation::hints(app)
    } else if app.provider_settings_visible() {
        let mut hints = provider_settings::hints(app);
        if app.active_tab == AppTab::Accounts {
            hints.push(KeyHint::secondary("Backspace", "accounts"));
        }
        hints
    } else {
        match app.active_tab {
            AppTab::Accounts => accounts::hints(app),
            AppTab::Files => files::hints(app),
            AppTab::Transfers => transfers::hints(app),
            AppTab::Logs => logs::hints(app),
            AppTab::Settings => provider_settings::hints(app),
        }
    };
    if app.navigation_focus == NavigationFocus::Content {
        hints.push(KeyHint::secondary("Left", "nav"));
    }
    hints.push(KeyHint::secondary("[/]", "provider"));
    hints.push(KeyHint::secondary("l", "logs"));
    hints.push(KeyHint::secondary("g", "global"));
    hints.push(KeyHint::secondary("q", "quit"));
    hints
}
