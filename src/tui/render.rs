use crate::{
    download::{TransferState, format_bytes},
    logger::LogLevel,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};

use super::{
    AppTab, BrowserApp, BrowserLayer, BrowserMode, NavigationFocus, PanelFocus,
    ProviderSettingsForm, RemoteItem, Toast, ToastKind, keymap,
    theme::{self, Theme},
    with_cursor,
};

pub(crate) fn render_ui(frame: &mut Frame<'_>, app: &mut BrowserApp) {
    let area = frame.area();
    let provider_theme = theme::for_provider(app.current_provider_id());
    let content_theme = if app.global_settings_open {
        Theme::application()
    } else {
        provider_theme
    };
    let footer_height = footer_height(app, area.width);
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(8),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    render_shell(frame, app, header_area, &provider_theme);

    if app.global_settings_open {
        render_settings(frame, app, body_area, &content_theme);
    } else {
        let navigation_width = if body_area.width < 72 { 15 } else { 20 };
        let [navigation_area, content_area] =
            Layout::horizontal([Constraint::Length(navigation_width), Constraint::Min(24)])
                .areas(body_area);
        render_navigation(frame, app, navigation_area, &provider_theme);

        match app.active_tab {
            AppTab::Accounts if app.account_detail_open => {
                render_provider_settings(frame, app, content_area, &provider_theme)
            }
            AppTab::Accounts => render_accounts(frame, app, content_area, &provider_theme),
            AppTab::Files => render_file_tree(frame, app, content_area, &provider_theme),
            AppTab::Transfers => render_status(frame, app, content_area, &provider_theme),
            AppTab::Logs => {
                let [upper_area, lower_area] =
                    Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)])
                        .areas(content_area);
                render_log_card(frame, app, upper_area, &provider_theme);
                render_log_detail(frame, app, lower_area, &provider_theme);
            }
            AppTab::Settings => render_provider_settings(frame, app, content_area, &provider_theme),
        }
    }
    render_footer(frame, app, footer_area, &content_theme);

    if app.mode == BrowserMode::Confirming {
        let target = app
            .download_target()
            .expect("confirmation always has a download target");
        let form = app
            .download_form
            .as_ref()
            .expect("confirmation always has a download form");
        let kind = if target.is_folder() {
            "recursive folder"
        } else {
            "single file"
        };
        let popup = centered_rect(80, 11, area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Target type: {kind}\nBreadcrumb: {}\n\nGlobal download root: {}\nFile workers: [{}] (1-5)\nTarget: {}\n\nLeft/Right workers | Enter start | Esc cancel",
                app.download_target_breadcrumbs(),
                app.destination.display(),
                form.workers,
                app.download_target_destination().display()
            ))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Download form ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(provider_theme.warn)),
            ),
            popup,
        );
    }
    if let Some(selected) = app.provider_selector {
        let choice = &app.provider_choices[selected.provider];
        let selector_theme = theme::for_provider(Some(&choice.id));
        let selector_hints = provider_selector_hints();
        let popup_width = centered_rect(76, 1, area).width;
        let shortcut_height = shortcut_bar_height(&selector_hints, popup_width.saturating_sub(2));
        let height = (choice.sources.len() as u16 + 8 + shortcut_height)
            .max(10)
            .min(area.height.max(1));
        let popup = centered_rect(76, height, area);
        let block = Block::default()
            .title(" Source ")
            .borders(Borders::ALL)
            .border_style(active_border_style(&selector_theme));
        let inner = block.inner(popup);
        let [provider_area, source_area, help_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(2),
            Constraint::Length(shortcut_height),
        ])
        .areas(inner);
        let rows: Vec<Row<'_>> = if choice.sources.is_empty() {
            vec![Row::new([
                Cell::from("No Sources available"),
                Cell::from(""),
            ])]
        } else {
            choice
                .sources
                .iter()
                .map(|source| {
                    Row::new([
                        Cell::from(single_line(&source.name)),
                        Cell::from(single_line(&source.root.name)),
                    ])
                })
                .collect()
        };
        let source_selected = (!choice.sources.is_empty()).then_some(selected.source);
        let mut state = TableState::default().with_selected(source_selected);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("PROVIDER  ", muted_style(&selector_theme)),
                    Span::styled(
                        single_line(&choice.name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("ACCOUNT   ", muted_style(&selector_theme)),
                    Span::raw(single_line(&choice.profile_name)),
                ]),
                Line::from(vec![
                    Span::styled("STATUS    ", muted_style(&selector_theme)),
                    Span::styled(
                        short_status(&choice.status),
                        status_style(&choice.status, &selector_theme),
                    ),
                ]),
            ]),
            provider_area,
        );
        frame.render_stateful_widget(
            Table::new(rows, [Constraint::Min(12), Constraint::Length(18)])
                .header(table_header(["SOURCE", "ROOT"], &selector_theme))
                .column_spacing(1)
                .row_highlight_style(selection_style(true, &selector_theme))
                .highlight_symbol("> "),
            source_area,
            &mut state,
        );
        render_shortcut_bar(frame, &selector_hints, help_area, &selector_theme);
    }
    if let Some(form) = app.account_form.as_ref() {
        let popup = centered_rect(64, 8, area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from("Create an independent Account for the current Provider."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("NAME  ", muted_style(&provider_theme)),
                    Span::styled(
                        with_cursor(&form.value, form.cursor),
                        Style::default().fg(provider_theme.accent),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    single_line(&form.status),
                    muted_style(&provider_theme),
                )),
                Line::from("Enter create | Esc cancel"),
            ])
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" New Account ")
                    .borders(Borders::ALL)
                    .border_style(active_border_style(&provider_theme)),
            ),
            popup,
        );
    }
    if let Some(toast) = app.toast.as_ref() {
        render_toast(frame, toast, area, &provider_theme);
    }
    if app.quit_confirming {
        let popup = centered_rect(58, 7, area);
        frame.render_widget(Clear, popup);
        let lifecycle = if app.daemon_mode {
            "Downloads continue in the daemon."
        } else {
            "Downloads stop; .part files are kept."
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Exit net-pan?\n\n{lifecycle}\n\ny/Enter exit | n/Esc return"
            ))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" Confirm exit ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(provider_theme.err)),
            ),
            popup,
        );
    }
}

fn render_toast(frame: &mut Frame<'_>, toast: &Toast, area: Rect, theme: &Theme) {
    let maximum_width = area.width.saturating_sub(4).max(1);
    let width = (toast.message.chars().count() as u16 + 4)
        .max(24.min(maximum_width))
        .min(maximum_width);
    let height = 3.min(area.height.max(1));
    let toast_area = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let color = match toast.kind {
        ToastKind::Success => theme.accent,
        ToastKind::Warning => theme.warn,
    };
    frame.render_widget(Clear, toast_area);
    frame.render_widget(
        Paragraph::new(fit_text(
            &single_line(&toast.message),
            toast_area.width.saturating_sub(2) as usize,
        ))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ),
        toast_area,
    );
}

fn tab_span(label: String, active: bool, theme: &Theme) -> Span<'static> {
    if active {
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {label} "), muted_style(theme))
    }
}

fn focus_style(active: bool, theme: &Theme) -> Style {
    if active {
        active_border_style(theme)
    } else {
        Style::default().fg(theme.comment)
    }
}

fn render_shell(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let block = Block::default()
        .title(Span::styled(
            " net-pan ",
            Style::default()
                .fg(theme.fg_strong)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.comment));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [tabs_area, identity_area, source_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let runtime = if app.daemon_mode {
        "DAEMON"
    } else {
        "EMBEDDED"
    };
    let mut provider_types = Vec::new();
    for choice in &app.provider_choices {
        if !provider_types
            .iter()
            .any(|id: &&str| *id == choice.id.as_str())
        {
            provider_types.push(choice.id.as_str());
        }
    }
    let current_provider = app.current_provider_id().map(|id| id.as_str());
    let mut provider_spans = Vec::new();
    for (index, provider_id) in provider_types.iter().enumerate() {
        if index > 0 {
            provider_spans.push(Span::raw(" "));
        }
        let label = app
            .provider_choices
            .iter()
            .find(|choice| choice.id.as_str() == *provider_id)
            .map(|choice| choice.name.clone())
            .unwrap_or_else(|| (*provider_id).to_owned());
        let tab_theme = theme::for_provider(
            app.provider_choices
                .iter()
                .find(|choice| choice.id.as_str() == *provider_id)
                .map(|choice| &choice.id),
        );
        provider_spans.push(tab_span(
            label,
            current_provider == Some(*provider_id),
            &tab_theme,
        ));
    }
    let provider_width: usize = provider_spans.iter().map(|span| span.width()).sum();
    let runtime_padding = (tabs_area.width as usize).saturating_sub(provider_width + runtime.len());
    provider_spans.push(Span::raw(" ".repeat(runtime_padding)));
    provider_spans.push(Span::styled(
        runtime,
        muted_style(theme).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(Line::from(provider_spans)), tabs_area);
    frame.render_widget(
        Paragraph::new(labeled_pair_line(
            "Provider",
            &app.provider_name,
            "Account",
            &app.profile_name,
            identity_area.width as usize,
            Style::default(),
            theme,
        )),
        identity_area,
    );
    frame.render_widget(
        Paragraph::new(labeled_triplet_line(
            "Source",
            &app.source_name,
            "Status",
            short_status(&app.provider_status),
            "Destination",
            &app.destination.display().to_string(),
            source_area.width as usize,
            status_style(&app.provider_status, theme),
            theme,
        )),
        source_area,
    );
}

fn render_navigation(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let focused = app.navigation_focus == NavigationFocus::Navigation;
    let block = panel_block(" Navigation ", focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = AppTab::ALL
        .iter()
        .map(|tab| Row::new([Cell::from(tab.label())]))
        .collect::<Vec<_>>();
    let selected = AppTab::ALL
        .iter()
        .position(|tab| *tab == app.navigation_tab);
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(
        Table::new(rows, [Constraint::Min(8)])
            .row_highlight_style(selection_style(focused, theme))
            .highlight_symbol("> "),
        inner,
        &mut state,
    );
}

fn render_accounts(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let indices = app.account_indices();
    let rows = indices
        .iter()
        .map(|index| {
            let account = &app.provider_choices[*index];
            Row::new([
                Cell::from(single_line(&account.profile_name)),
                Cell::from(account.profile.to_string()),
                Cell::from(short_status(&account.status).to_owned()),
            ])
            .style(status_style(&account.status, theme))
        })
        .collect::<Vec<_>>();
    let focused = app.navigation_focus == NavigationFocus::Content;
    let title = format!(" Accounts | {} ", app.provider_name);
    let block = panel_block(title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [table_area, status_area] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(inner);
    let mut state =
        TableState::default().with_selected((!rows.is_empty()).then_some(app.account_selected));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Min(14),
                Constraint::Length(12),
                Constraint::Length(15),
            ],
        )
        .header(table_header(["ACCOUNT", "ID", "STATUS"], theme))
        .column_spacing(1)
        .row_highlight_style(selection_style(focused, theme))
        .highlight_symbol("> "),
        table_area,
        &mut state,
    );
    render_panel_status(
        frame,
        &format!("{} Account(s); active: {}", indices.len(), app.profile_name),
        status_area,
        theme,
    );
}

#[allow(clippy::too_many_arguments)]
fn labeled_triplet_line(
    first_label: &str,
    first_value: &str,
    second_label: &str,
    second_value: &str,
    third_label: &str,
    third_value: &str,
    width: usize,
    second_style: Style,
    theme: &Theme,
) -> Line<'static> {
    let first_label = format!("{first_label}: ");
    let second_label = format!(" | {second_label}: ");
    let third_label = format!(" | {third_label}: ");
    let labels = first_label.len() + second_label.len() + third_label.len();
    let available = width.saturating_sub(labels);
    let second_value = single_line(second_value);
    let second_width = second_value
        .chars()
        .count()
        .min(15)
        .min(available.saturating_sub(8));
    let remaining = available.saturating_sub(second_width);
    let first_value = single_line(first_value);
    let first_width = first_value.chars().count().min(remaining / 4);
    let third_width = remaining.saturating_sub(first_width);
    Line::from(vec![
        Span::styled(first_label, muted_style(theme)),
        Span::raw(fit_text(&first_value, first_width)),
        Span::styled(second_label, muted_style(theme)),
        Span::styled(fit_text(&second_value, second_width), second_style),
        Span::styled(third_label, muted_style(theme)),
        Span::raw(fit_middle_text(&single_line(third_value), third_width)),
    ])
}

fn labeled_pair_line(
    left_label: &str,
    left_value: &str,
    right_label: &str,
    right_value: &str,
    width: usize,
    right_style: Style,
    theme: &Theme,
) -> Line<'static> {
    let left_label = format!("{left_label}: ");
    let right_label = format!(" | {right_label}: ");
    let available = width.saturating_sub(left_label.len() + right_label.len());
    let right_value = single_line(right_value);
    let right_width = right_value.chars().count().min(available / 2);
    let left_width = available.saturating_sub(right_width);
    Line::from(vec![
        Span::styled(left_label, muted_style(theme)),
        Span::raw(fit_text(&single_line(left_value), left_width)),
        Span::styled(right_label, muted_style(theme)),
        Span::styled(fit_text(&right_value, right_width), right_style),
    ])
}

fn render_footer(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    render_shortcut_bar(frame, &footer_hints_for_width(app, area.width), area, theme);
}

fn render_shortcut_bar(frame: &mut Frame<'_>, hints: &[FooterHint], area: Rect, theme: &Theme) {
    let lines = shortcut_rows(hints, area.width)
        .into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (index, hint) in row.into_iter().enumerate() {
                if index > 0 {
                    spans.push(Span::raw(" "));
                }
                let style = if hint.primary {
                    Style::default().fg(theme.on_accent).bg(theme.accent)
                } else {
                    Style::default().fg(theme.fg_strong).bg(theme.surface)
                };
                spans.push(Span::styled(
                    format!(" {} ", hint.key),
                    style.add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(format!("{} ", hint.action), style));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn active_border_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.focus)
        .add_modifier(Modifier::BOLD)
}

fn muted_style(theme: &Theme) -> Style {
    Style::default().fg(theme.comment)
}

fn selection_style(active: bool, theme: &Theme) -> Style {
    if active {
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

fn panel_block(title: impl Into<String>, active: bool, theme: &Theme) -> Block<'static> {
    let title = if active {
        format!(">{}", title.into())
    } else {
        title.into()
    };
    Block::default()
        .title(Span::styled(
            title,
            if active {
                active_border_style(theme)
            } else {
                muted_style(theme)
            },
        ))
        .borders(Borders::ALL)
        .border_style(focus_style(active, theme))
}

fn table_header(cells: impl IntoIterator<Item = &'static str>, theme: &Theme) -> Row<'static> {
    Row::new(cells)
        .style(muted_style(theme).add_modifier(Modifier::BOLD))
        .bottom_margin(0)
}

fn short_status(status: &str) -> &str {
    status.split(':').next().unwrap_or(status)
}

fn status_style(status: &str, theme: &Theme) -> Style {
    match short_status(status) {
        "Ready" => Style::default().fg(theme.ok),
        "Connecting" => Style::default().fg(theme.accent),
        "NeedsConfig" => Style::default().fg(theme.warn),
        "Disabled" => muted_style(theme),
        "ConfigError" | "ConnectionError" | "SourceError" => Style::default().fg(theme.err),
        _ => Style::default().fg(theme.fg_strong),
    }
}

fn fit_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut clipped: String = text.chars().take(width - 3).collect();
    clipped.push_str("...");
    clipped
}

fn fit_middle_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let content = width - 3;
    let left = content / 2;
    let right = content - left;
    let prefix: String = text.chars().take(left).collect();
    let suffix: String = text
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}...{suffix}")
}

fn compact_breadcrumb(components: &[&str], width: usize) -> String {
    let full = components.join(" / ");
    if full.chars().count() <= width {
        return full;
    }
    if components.len() <= 2 {
        return fit_middle_text(&full, width);
    }

    let mut tail = vec![components[components.len() - 1]];
    for component in components[1..components.len() - 1].iter().rev() {
        let mut candidate = vec![components[0], "..."];
        candidate.push(component);
        candidate.extend(tail.iter().copied());
        if candidate.join(" / ").chars().count() > width {
            break;
        }
        tail.insert(0, component);
    }
    let mut compact = vec![components[0], "..."];
    compact.extend(tail);
    fit_middle_text(&compact.join(" / "), width)
}

fn render_file_tree(frame: &mut Frame<'_>, app: &mut BrowserApp, area: Rect, theme: &Theme) {
    let saved_links = app.current().is_shared_root();
    let rows: Vec<Row<'_>> = app
        .current()
        .items
        .iter()
        .map(|item| {
            let (kind, style) = file_item_style(item, saved_links, theme);
            let size = item
                .size
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_owned());
            Row::new([
                Cell::from(kind),
                Cell::from(format!(
                    "{}{}",
                    single_line(&item.name),
                    if item.is_folder() { "/" } else { "" }
                )),
                Cell::from(size),
            ])
            .style(style)
        })
        .collect();
    let focused = app.navigation_focus == NavigationFocus::Content;
    let title_width = area.width.saturating_sub(5) as usize;
    let block = panel_block(
        if saved_links {
            " Sources ".to_owned()
        } else {
            format!(
                " {} ",
                compact_breadcrumb(&app.breadcrumb_components(), title_width)
            )
        },
        focused,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [table_area, status_area] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(inner);
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Min(12),
            Constraint::Length(10),
        ],
    )
    .header(table_header(["TYPE", "NAME", "SIZE"], theme))
    .column_spacing(1)
    .row_highlight_style(selection_style(focused, theme))
    .highlight_symbol("> ");
    let mut state = TableState::default()
        .with_selected(app.current().selected)
        .with_offset(app.current().offset);
    frame.render_stateful_widget(table, table_area, &mut state);
    app.current_mut().offset = state.offset();
    render_panel_status(
        frame,
        &format!("{}  |  {}", app.message, app.level_count_label()),
        status_area,
        theme,
    );
}

pub(super) fn file_item_style(
    item: &RemoteItem,
    saved_links: bool,
    theme: &Theme,
) -> (&'static str, Style) {
    if saved_links {
        ("[LINK]", Style::default().fg(theme.accent))
    } else if item.is_folder() {
        ("[DIR ]", Style::default())
    } else if !item.can_download() {
        ("[SKIP]", muted_style(theme))
    } else {
        ("[FILE]", Style::default())
    }
}

fn render_log_card(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let filtered: Vec<_> = app
        .logs
        .iter()
        .filter(|entry| {
            app.log_filter.matches(entry.level) && app.log_target_filter.matches(&entry.target)
        })
        .collect();
    let selected = filtered.len().checked_sub(
        1 + app
            .log_offset_from_bottom
            .min(filtered.len().saturating_sub(1)),
    );
    let rows: Vec<Row<'_>> = filtered
        .iter()
        .map(|entry| {
            let (level, style) = match entry.level {
                LogLevel::Info => ("INFO", Style::default()),
                LogLevel::Error => ("ERROR", Style::default().fg(theme.err)),
            };
            Row::new([
                Cell::from(entry.timestamp.format("%H:%M:%S%.3f").to_string()),
                Cell::from(level),
                Cell::from(single_line(&entry.target)),
                Cell::from(single_line(&entry.message)),
            ])
            .style(style)
        })
        .collect();
    let title = format!(
        " Runtime logs [{} | {}] ({}) ",
        app.log_filter.label(),
        app.log_target_filter.label(),
        filtered.len()
    );
    let focused =
        app.navigation_focus == NavigationFocus::Content && app.logs_focus == PanelFocus::Upper;
    let block = panel_block(title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [table_area, status_area] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(inner);
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(5),
            Constraint::Length(14),
            Constraint::Min(12),
        ],
    )
    .header(table_header(["TIME", "LEVEL", "TARGET", "MESSAGE"], theme))
    .column_spacing(1)
    .row_highlight_style(selection_style(focused, theme))
    .highlight_symbol("> ");
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, table_area, &mut state);
    render_panel_status(
        frame,
        &format!("{}  |  {} visible logs", app.message, filtered.len()),
        status_area,
        theme,
    );
}

fn render_settings(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let form = app
        .settings_form
        .as_ref()
        .expect("settings view always has a form");
    let displayed_destination =
        if form.editing && form.field == super::GlobalSettingField::Destination {
            with_cursor(&form.destination, form.cursor)
        } else {
            form.destination.clone()
        };
    let displayed_proxy = if form.editing && form.field == super::GlobalSettingField::Proxy {
        with_cursor(&form.proxy, form.cursor)
    } else if form.proxy.is_empty() {
        "<direct>".to_owned()
    } else {
        form.proxy.clone()
    };
    let mode = if form.proxy.is_empty() {
        "Direct"
    } else {
        "Proxy"
    };
    let title = if form.editing {
        " GLOBAL SETTINGS  EDITING "
    } else {
        " GLOBAL SETTINGS "
    };
    let focused = app.navigation_focus == NavigationFocus::Content;
    let editable_rows = vec![
        Row::new([
            Cell::from("Download path"),
            Cell::from(displayed_destination),
        ]),
        Row::new([Cell::from("Proxy URL"), Cell::from(displayed_proxy)]),
    ];
    let block = panel_block(title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [scope_area, editable_area, information_area, status_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(4),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    " APPLICATION ",
                    Style::default()
                        .fg(theme.on_accent)
                        .bg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  global process defaults", muted_style(theme)),
            ]),
            Line::from(vec![
                Span::styled("FILE  ", muted_style(theme)),
                Span::raw(fit_middle_text(
                    &app.settings_path.display().to_string(),
                    scope_area.width.saturating_sub(6) as usize,
                )),
            ]),
        ]),
        scope_area,
    );
    let mut state = TableState::default().with_selected(Some(form.field.row()));
    frame.render_stateful_widget(
        Table::new(editable_rows, [Constraint::Length(18), Constraint::Min(16)])
            .header(table_header(["EDITABLE SETTINGS", "VALUE"], theme))
            .column_spacing(1)
            .row_highlight_style(selection_style(focused, theme))
            .highlight_symbol("> "),
        editable_area,
        &mut state,
    );
    frame.render_widget(
        Table::new(
            vec![
                Row::new([Cell::from("Connection"), Cell::from(mode)]),
                Row::new([
                    Cell::from("Precedence"),
                    Cell::from("Provider proxy > Global proxy > Direct"),
                ]),
                Row::new([
                    Cell::from("Environment"),
                    Cell::from("System proxy variables are ignored"),
                ]),
            ],
            [Constraint::Length(18), Constraint::Min(16)],
        )
        .header(table_header(["READ ONLY", "VALUE"], theme))
        .column_spacing(1),
        information_area,
    );
    render_panel_status(frame, &form.status, status_area, theme);
}

fn render_panel_status(frame: &mut Frame<'_>, status: &str, area: Rect, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(fit_text(
            &format!("STATUS  {}", single_line(status)),
            area.width as usize,
        ))
        .style(Style::default().fg(theme.focus)),
        area,
    );
}

fn render_log_detail(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let filtered: Vec<_> = app
        .logs
        .iter()
        .filter(|entry| {
            app.log_filter.matches(entry.level) && app.log_target_filter.matches(&entry.target)
        })
        .collect();
    let selected = filtered.len().checked_sub(
        1 + app
            .log_offset_from_bottom
            .min(filtered.len().saturating_sub(1)),
    );
    let detail = selected
        .and_then(|index| filtered.get(index))
        .map(|entry| entry.message.clone())
        .unwrap_or_else(|| "No log entry selected".to_owned());
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(panel_block(
                " Log Detail ",
                app.navigation_focus == NavigationFocus::Content
                    && app.logs_focus == PanelFocus::Lower,
                theme,
            )),
        area,
    );
}

fn render_provider_settings(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let Some(form) = app.provider_form.as_ref() else {
        frame.render_widget(
            Paragraph::new("Provider settings are unavailable").block(
                Block::default()
                    .title(" PROVIDER SETTINGS ")
                    .borders(Borders::ALL)
                    .border_style(active_border_style(theme)),
            ),
            area,
        );
        return;
    };
    let route = if app.active_tab == AppTab::Accounts {
        format!("Accounts / {}", app.profile_name)
    } else {
        format!("Settings / {}", app.profile_name)
    };
    let block = panel_block(
        format!(" {route}{} ", if form.editing { " / Editing" } else { "" }),
        app.navigation_focus == NavigationFocus::Content,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let editable_indices = form.editable_indices();
    let read_only_indices = form.read_only_indices();
    let editable_constraint = if editable_indices.is_empty() {
        Constraint::Length(1)
    } else {
        Constraint::Min(2)
    };
    let read_only_constraint = if editable_indices.is_empty() {
        Constraint::Min(2)
    } else {
        Constraint::Length((read_only_indices.len() as u16 + 1).clamp(1, 3))
    };
    let [
        identity_area,
        editable_area,
        read_only_area,
        help_area,
        status_area,
    ] = Layout::vertical([
        Constraint::Length(4),
        editable_constraint,
        read_only_constraint,
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let identity = app.provider_choices.get(app.active_provider);
    let profile_id = identity
        .map(|choice| choice.profile.as_str())
        .unwrap_or("-");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    " PROVIDER ",
                    Style::default()
                        .fg(theme.on_accent)
                        .bg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  configuration for the current Account",
                    muted_style(theme),
                ),
            ]),
            Line::from(vec![
                Span::styled("ID        ", muted_style(theme)),
                Span::raw(profile_id),
                Span::styled("  STATUS   ", muted_style(theme)),
                Span::styled(
                    short_status(&app.provider_status),
                    status_style(&app.provider_status, theme),
                ),
            ]),
            Line::from(vec![
                Span::styled("FILE      ", muted_style(theme)),
                Span::raw(fit_middle_text(
                    &app.account_config_path().display().to_string(),
                    identity_area.width.saturating_sub(10) as usize,
                )),
            ]),
            Line::from(vec![
                Span::styled("NETWORK   ", muted_style(theme)),
                Span::raw("Provider proxy > Global proxy > Direct"),
            ]),
        ]),
        identity_area,
    );
    let visible = editable_area.height.saturating_sub(1) as usize;
    let selected_position = editable_indices
        .iter()
        .position(|index| *index == form.selected)
        .unwrap_or(0);
    let start = selected_position
        .saturating_sub(visible.saturating_sub(1))
        .min(editable_indices.len().saturating_sub(visible));
    let editable_rows: Vec<Row<'_>> = editable_indices
        .iter()
        .skip(start)
        .take(visible)
        .map(|index| {
            let field = &form.fields[*index];
            let value =
                ProviderSettingsForm::display_value(field, form.editing && *index == form.selected);
            let required = if field.spec.required { "*" } else { "" };
            Row::new([
                Cell::from(format!("{}{required}", field.spec.label)),
                Cell::from(value),
            ])
        })
        .collect();
    let selected = (!editable_rows.is_empty()).then_some(selected_position.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(
        Table::new(editable_rows, [Constraint::Length(22), Constraint::Min(12)])
            .header(table_header(["EDITABLE SETTINGS", "VALUE"], theme))
            .column_spacing(1)
            .row_highlight_style(selection_style(
                app.navigation_focus == NavigationFocus::Content,
                theme,
            ))
            .highlight_symbol("> "),
        editable_area,
        &mut state,
    );
    let read_only_rows = read_only_indices
        .iter()
        .take(read_only_area.height.saturating_sub(1) as usize)
        .map(|index| {
            let field = &form.fields[*index];
            Row::new([
                Cell::from(field.spec.label.to_owned()),
                Cell::from(ProviderSettingsForm::display_value(field, false)),
            ])
            .style(muted_style(theme))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Table::new(
            read_only_rows,
            [Constraint::Length(22), Constraint::Min(12)],
        )
        .header(table_header(["READ ONLY", "VALUE"], theme))
        .column_spacing(1),
        read_only_area,
    );
    let description = form
        .selected_field()
        .map(|field| field.spec.description.as_str())
        .filter(|description| !description.is_empty())
        .unwrap_or("No description is available from the connected daemon.");
    frame.render_widget(
        Paragraph::new(fit_text(
            &format!("HELP    {}", single_line(description)),
            help_area.width as usize,
        ))
        .style(muted_style(theme)),
        help_area,
    );
    frame.render_widget(
        Paragraph::new(fit_text(
            &format!("STATUS  {}", single_line(&form.status)),
            status_area.width as usize,
        ))
        .style(Style::default().fg(theme.accent))
        .wrap(Wrap { trim: false }),
        status_area,
    );
}

fn render_status(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    match app.mode {
        BrowserMode::Browsing | BrowserMode::Confirming => {
            render_idle_transfers(frame, app, area, theme);
        }
        BrowserMode::Scanning => {
            let target = app.download_target();
            let (description, title) = match target {
                Some(target) if target.is_folder() => (
                    format!("Scanning every descendant of {}.", target.name),
                    " Recursive scan ",
                ),
                Some(target) => (
                    format!("Preparing the selected file {}.", target.name),
                    " File plan ",
                ),
                None => ("Preparing the selected item.".to_owned(), " Download plan "),
            };
            frame.render_widget(
                Paragraph::new(format!(
                    "{description}\nNo files are written until the plan is ready.\nDestination: {}",
                    app.download_target_destination().display()
                ))
                .alignment(Alignment::Center)
                .block(panel_block(
                    title,
                    app.navigation_focus == NavigationFocus::Content,
                    theme,
                )),
                area,
            );
        }
        BrowserMode::Downloading | BrowserMode::Paused => render_progress(frame, app, area, theme),
        BrowserMode::Finished => {
            frame.render_widget(
                Paragraph::new(format!(
                    "All: {} | Downloaded: {} | Skipped: {} | Failed: {}\nCompleted bytes: {} | Partial bytes: {}\nDestination: {}\n\nPress Enter/Esc to return to the browser.",
                    app.download.downloaded.saturating_add(app.download.skipped).saturating_add(app.download.failed),
                    app.download.downloaded,
                    app.download.skipped,
                    app.download.failed,
                    format_bytes(app.download.overall_written.saturating_sub(app.download.partial_bytes)),
                    format_bytes(app.download.partial_bytes),
                    app.download_target_destination().display()
                ))
                .block(panel_block(
                    " Download complete ",
                    app.navigation_focus == NavigationFocus::Content,
                    theme,
                ).border_style(
                    Style::default()
                        .fg(theme.ok)
                        .add_modifier(Modifier::BOLD),
                )),
                area,
            );
        }
        BrowserMode::Failed => {
            frame.render_widget(
                Paragraph::new(format!(
                    "{}\n\nPartial files were retained for a later retry.\nPress Enter/Esc to return to the browser.",
                    app.download.fatal.as_deref().unwrap_or("Unknown error")
                ))
                .wrap(Wrap { trim: true })
                .block(panel_block(
                    " Download failed ",
                    app.navigation_focus == NavigationFocus::Content,
                    theme,
                ).border_style(
                    Style::default()
                        .fg(theme.err)
                        .add_modifier(Modifier::BOLD),
                )),
                area,
            );
        }
    }
}

fn render_idle_transfers(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let focused = app.navigation_focus == NavigationFocus::Content;
    let rows = vec![
        Row::new([
            Cell::from("IDLE"),
            Cell::from("No active transfers"),
            Cell::from("-"),
            Cell::from("-"),
        ])
        .style(muted_style(theme)),
    ];
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(6),
                Constraint::Min(12),
                Constraint::Length(18),
                Constraint::Length(10),
            ],
        )
        .header(table_header(["STATE", "PATH", "PROGRESS", "SPEED"], theme))
        .column_spacing(1)
        .block(panel_block(" Transfers [All]  IDLE ", focused, theme)),
        area,
    );
}

fn render_progress(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect, theme: &Theme) {
    let running = app.download.status_count(TransferState::Running);
    let queued = app.download.status_count(TransferState::Queued);
    let done = app.download.status_count(TransferState::Completed);
    let failed = app
        .download
        .status_count(TransferState::Failed)
        .saturating_add(app.download.status_count(TransferState::Interrupted));
    let paused = app.download.status_count(TransferState::Paused);
    let title = format!(
        " Transfers [{}]  R{running} P{paused} Q{queued} D{done} F{failed}  {}/s ",
        app.download.transfer_filter.label(),
        format_speed(app.download.aggregate_speed())
    );
    let transfers = app.download.sorted_transfers();
    let selected = app
        .download
        .transfer_selected
        .min(transfers.len().saturating_sub(1));
    let focused = app.navigation_focus == NavigationFocus::Content;
    let block = panel_block(title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [list_area, detail_area] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(5)]).areas(inner);
    let (columns, show_speed, progress_width) = transfer_columns(list_area.width);
    let visible_rows = list_area.height.saturating_sub(1) as usize;
    let maximum_offset = transfers.len().saturating_sub(visible_rows.max(1));
    let offset = app.download.transfer_offset.min(maximum_offset);
    let rows: Vec<Row<'_>> = transfers
        .iter()
        .copied()
        .skip(offset)
        .take(visible_rows)
        .map(|transfer| {
            let progress = transfer_progress(transfer, progress_width);
            let speed = if transfer.status == TransferState::Running {
                format!("{}/s", format_speed(transfer.bytes_per_second))
            } else {
                "-".to_owned()
            };
            let style = match transfer.status {
                TransferState::Running | TransferState::Queued => Style::default(),
                TransferState::Paused => Style::default().fg(theme.warn),
                TransferState::Failed | TransferState::Interrupted => {
                    Style::default().fg(theme.err)
                }
                TransferState::Skipped => muted_style(theme),
                TransferState::Completed => Style::default().fg(theme.ok),
            };
            let mut cells = vec![
                Cell::from(transfer.status.label()),
                Cell::from(single_line(&transfer.path.display().to_string())),
                Cell::from(progress),
            ];
            if show_speed {
                cells.push(Cell::from(speed));
            }
            Row::new(cells).style(style)
        })
        .collect();
    let rows = if rows.is_empty() {
        let mut cells = vec![
            Cell::from("WAIT"),
            Cell::from("Waiting for transfer queue"),
            Cell::from(format!(
                "[ ] --% {}/{} items",
                app.download.items_processed, app.download.items_total
            )),
        ];
        if show_speed {
            cells.push(Cell::from("-"));
        }
        vec![Row::new(cells).style(muted_style(theme))]
    } else {
        rows
    };
    let selected_in_view = (!transfers.is_empty() && selected >= offset)
        .then_some(selected.saturating_sub(offset))
        .filter(|index| *index < rows.len());
    let mut state = TableState::default().with_selected(selected_in_view);
    frame.render_stateful_widget(
        Table::new(rows, columns)
            .header(table_header(
                if show_speed {
                    vec!["STATE", "PATH", "PROGRESS", "SPEED"]
                } else {
                    vec!["STATE", "PATH", "PROGRESS"]
                },
                theme,
            ))
            .column_spacing(1)
            .row_highlight_style(selection_style(focused, theme))
            .highlight_symbol("> "),
        list_area,
        &mut state,
    );
    let selected_detail = transfers
        .get(selected)
        .map(|transfer| {
            let detail = transfer.detail.as_deref().unwrap_or("no error");
            format!(
                "JOB {} | TRANSFER {} | {} | {}/{}\nPATH  {}\nDETAIL  {}",
                app.download.job_id.map(|id| id.0).unwrap_or_default(),
                transfer.id.0,
                transfer.status.label(),
                format_bytes(transfer.written),
                transfer
                    .total
                    .map(format_bytes)
                    .unwrap_or_else(|| "unknown".to_owned()),
                transfer.path.display(),
                detail
            )
        })
        .unwrap_or_else(|| {
            format!(
                "JOB {}  |  items {}/{}  |  No transfers match this filter",
                app.download.job_id.map(|id| id.0).unwrap_or_default(),
                app.download.items_processed,
                app.download.items_total
            )
        });
    frame.render_widget(
        Paragraph::new(selected_detail)
            .style(Style::default().fg(theme.accent))
            .wrap(Wrap { trim: false }),
        detail_area,
    );
}

#[derive(Clone, Copy)]
struct FooterHint {
    key: &'static str,
    action: &'static str,
    primary: bool,
}

impl FooterHint {
    const fn primary(key: &'static str, action: &'static str) -> Self {
        Self {
            key,
            action,
            primary: true,
        }
    }

    const fn secondary(key: &'static str, action: &'static str) -> Self {
        Self {
            key,
            action,
            primary: false,
        }
    }

    fn width(self) -> usize {
        self.key.chars().count() + self.action.chars().count() + 3
    }
}

pub(super) fn footer_height(app: &BrowserApp, width: u16) -> u16 {
    shortcut_bar_height(&footer_hints_for_width(app, width), width)
}

fn footer_hints_for_width(app: &BrowserApp, width: u16) -> Vec<FooterHint> {
    let mut hints = footer_hints(app);
    if width < 72 && app.active_tab == AppTab::Files {
        hints.retain(|hint| !(hint.key == "Left" && hint.action == "nav"));
        if app.browser_layer() == BrowserLayer::DriveFolder {
            hints.retain(|hint| !(hint.key == "[/]" && hint.action == "provider"));
        }
    }
    hints
}

fn shortcut_bar_height(hints: &[FooterHint], width: u16) -> u16 {
    shortcut_rows(hints, width).len().max(1) as u16
}

fn shortcut_rows(hints: &[FooterHint], width: u16) -> Vec<Vec<FooterHint>> {
    let maximum_width = usize::from(width).max(1);
    let mut rows = vec![Vec::new()];
    let mut row_width = 0;

    for &hint in hints {
        let separator_width = usize::from(!rows.last().is_some_and(Vec::is_empty));
        let required_width = separator_width + hint.width();
        if row_width + required_width > maximum_width && rows.len() < 2 {
            rows.push(Vec::new());
            row_width = 0;
        }
        let separator_width = usize::from(!rows.last().is_some_and(Vec::is_empty));
        row_width += separator_width + hint.width();
        rows.last_mut().expect("footer always has a row").push(hint);
    }

    rows
}

fn footer_hints(app: &BrowserApp) -> Vec<FooterHint> {
    if app.quit_confirming {
        return vec![
            FooterHint::primary("y/Enter", "exit"),
            FooterHint::secondary("n/Esc", "return"),
        ];
    }
    if app.account_form.is_some() {
        return vec![
            FooterHint::primary("Enter", "create"),
            FooterHint::secondary("Esc", "cancel"),
        ];
    }
    if app.global_settings_open && app.settings_form.as_ref().is_some_and(|form| form.editing) {
        return vec![
            FooterHint::primary("Ctrl-S", "save"),
            FooterHint::primary("Esc", "stop editing"),
            FooterHint::secondary("Ctrl-C", "quit"),
        ];
    }
    if app.provider_settings_visible()
        && app.provider_form.as_ref().is_some_and(|form| form.editing)
    {
        return vec![
            FooterHint::primary("Ctrl-S", "save/rebuild"),
            FooterHint::primary("Esc", "stop editing"),
            FooterHint::secondary("Ctrl-C", "quit"),
        ];
    }
    if app.active_tab == AppTab::Files && app.mode != BrowserMode::Browsing {
        return match app.mode {
            BrowserMode::Confirming => vec![
                FooterHint::primary("Left/Right", "workers"),
                FooterHint::primary("Enter", "start"),
                FooterHint::secondary("Esc", "cancel"),
                FooterHint::secondary("q", "exit"),
            ],
            BrowserMode::Scanning | BrowserMode::Downloading | BrowserMode::Paused => vec![
                FooterHint::secondary("Esc", "navigation"),
                FooterHint::secondary("[/]", "provider"),
                FooterHint::secondary("g", "global"),
                FooterHint::secondary("q", "quit; keep partial"),
            ],
            BrowserMode::Finished | BrowserMode::Failed => vec![
                FooterHint::primary("Enter/Esc", "return"),
                FooterHint::secondary("[/]", "provider"),
                FooterHint::secondary("q", "quit"),
            ],
            BrowserMode::Browsing => unreachable!(),
        };
    }
    keymap::page_hints(app)
        .into_iter()
        .map(|hint| FooterHint {
            key: hint.key,
            action: hint.action,
            primary: hint.primary,
        })
        .collect()
}

fn provider_selector_hints() -> Vec<FooterHint> {
    vec![
        FooterHint::primary("Up/Down", "source"),
        FooterHint::secondary("Enter", "open"),
        FooterHint::secondary("Esc", "close"),
    ]
}

fn ratio(written: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (written as f64 / total as f64).clamp(0.0, 1.0)
    }
}

fn transfer_columns(width: u16) -> (Vec<Constraint>, bool, usize) {
    let width = width.saturating_sub(2); // Stateful table selection marker.
    let show_speed = width >= 64;
    let spacing = if show_speed { 3 } else { 2 };
    let fixed = 6 + spacing + if show_speed { 10 } else { 0 };
    let flexible = width.saturating_sub(fixed);
    let path_width = (flexible / 3).max(10).min(flexible);
    let progress_width = flexible.saturating_sub(path_width).max(1);
    let mut columns = vec![
        Constraint::Length(6),
        Constraint::Length(path_width),
        Constraint::Length(progress_width),
    ];
    if show_speed {
        columns.push(Constraint::Length(10));
    }
    (columns, show_speed, progress_width as usize)
}

fn transfer_progress(transfer: &super::TransferDisplay, width: usize) -> String {
    let (percent, total) = match transfer.total {
        Some(total) if total > 0 => (
            format!("{:>3.0}%", ratio(transfer.written, total) * 100.0),
            compact_bytes(total),
        ),
        _ => (" --%".to_owned(), "?".to_owned()),
    };
    let sizes = format!("{}/{}", compact_bytes(transfer.written), total);
    let suffix = format!(" {percent} {sizes}");
    let bar_width = width.saturating_sub(suffix.chars().count() + 2);
    let filled = transfer
        .total
        .filter(|total| *total > 0)
        .map(|total| (ratio(transfer.written, total) * bar_width as f64).round() as usize)
        .unwrap_or(0)
        .min(bar_width);
    format!(
        "[{}{}]{}",
        "#".repeat(filled),
        "-".repeat(bar_width.saturating_sub(filled)),
        suffix
    )
}

fn compact_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn format_speed(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        "0 B".to_owned()
    } else {
        format_bytes(bytes_per_second.round().min(u64::MAX as f64) as u64)
    }
}

fn single_line(value: &str) -> String {
    value.replace('\r', "\\r").replace('\n', "\\n")
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center);
    let horizontal = Layout::horizontal([Constraint::Percentage(width_percent)]).flex(Flex::Center);
    let [vertical_area] = vertical.areas(area);
    let [popup] = horizontal.areas(vertical_area);
    popup
}
