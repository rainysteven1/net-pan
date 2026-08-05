use crate::{
    download::{TransferState, format_bytes},
    logger::LogLevel,
    provider::RemoteItem,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{
    AppTab, BrowserApp, BrowserMode, FormField, PanelFocus, ProviderSettingsForm, with_cursor,
};

pub(crate) fn render_ui(frame: &mut Frame<'_>, app: &mut BrowserApp) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(10),
            Constraint::Length(2),
        ])
        .split(area);

    let location = match app.active_tab {
        AppTab::Files => app.breadcrumbs(),
        AppTab::Logs => "Download logs".to_owned(),
        AppTab::Settings => "Configuration".to_owned(),
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            tab_span("Files", app.active_tab == AppTab::Files),
            Span::raw("  "),
            tab_span("Logs", app.active_tab == AppTab::Logs),
            Span::raw("  "),
            tab_span("Settings", app.active_tab == AppTab::Settings),
        ]),
        Line::from(format!(
            "Provider: {} | Profile: {} | Source: {} | {}",
            app.provider_name, app.profile_name, app.source_name, app.provider_status
        )),
        Line::from(format!(
            "{location} | {} | {}",
            app.level_count_label(),
            app.message
        )),
    ])
    .block(Block::default().title(" Pan TUI ").borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    match app.active_tab {
        AppTab::Files => {
            render_file_tree(frame, app, chunks[1]);
            render_status(frame, app, chunks[2]);
        }
        AppTab::Logs => {
            render_log_card(frame, app, chunks[1]);
            render_log_detail(frame, app, chunks[2]);
        }
        AppTab::Settings => {
            render_settings(frame, app, chunks[1]);
            render_settings_status(frame, app, chunks[2]);
        }
    }
    frame.render_widget(
        Paragraph::new(footer_text(app)).alignment(Alignment::Center),
        chunks[3],
    );

    if app.mode == BrowserMode::Confirming {
        let target = app
            .download_target()
            .expect("confirmation always has a download target");
        let form = app
            .download_form
            .as_ref()
            .expect("confirmation always has a download form");
        let destination_marker = if form.field == FormField::Destination {
            ">"
        } else {
            " "
        };
        let workers_marker = if form.field == FormField::Workers {
            ">"
        } else {
            " "
        };
        let kind = if target.is_folder() {
            "recursive folder"
        } else {
            "single file"
        };
        let displayed_root = if form.field == FormField::Destination {
            with_cursor(&form.destination_root, form.cursor)
        } else {
            form.destination_root.clone()
        };
        let popup = centered_rect(80, 11, area);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Target type: {kind}\nBreadcrumb: {}\n\n{destination_marker} Destination root: {}\n{workers_marker} File workers: [{}] (1-5)\nTarget: {}\n\nTab/Up/Down field | Enter start | Esc cancel",
                app.download_target_breadcrumbs(),
                displayed_root,
                form.workers,
                app.download_target_destination().display()
            ))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Download form ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
            popup,
        );
    }
    if let Some(selected) = app.provider_selector {
        let choice = &app.provider_choices[selected.provider];
        let height = (choice.sources.len() as u16 + 8)
            .max(9)
            .min(area.height.max(1));
        let popup = centered_rect(66, height, area);
        let block = Block::default()
            .title(" Provider / Profile / Source ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(popup);
        let [provider_area, source_area] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).areas(inner);
        let rows: Vec<ListItem<'_>> = if choice.sources.is_empty() {
            vec![ListItem::new("No Sources available")]
        } else {
            choice
                .sources
                .iter()
                .map(|source| ListItem::new(source.name.clone()))
                .collect()
        };
        let source_selected = (!choice.sources.is_empty()).then_some(selected.source);
        let mut state = ListState::default().with_selected(source_selected);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Provider: < {} >  ({}/{})\nProfile: {}\nStatus: {}",
                choice.name,
                selected.provider + 1,
                app.provider_choices.len(),
                choice.profile,
                choice.status
            ))
            .wrap(Wrap { trim: true }),
            provider_area,
        );
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                .highlight_symbol("> "),
            source_area,
            &mut state,
        );
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
                "Exit pan-tui?\n\n{lifecycle}\n\ny/Enter exit | n/Esc return"
            ))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" Confirm exit ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            ),
            popup,
        );
    }
}

fn tab_span(label: &'static str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            format!("[ {label} ]"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(label)
    }
}

fn focus_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn render_file_tree(frame: &mut Frame<'_>, app: &mut BrowserApp, area: Rect) {
    let rows: Vec<ListItem<'_>> = app
        .current()
        .items
        .iter()
        .map(|item| {
            let kind = if item.is_folder() {
                "[DIR ]"
            } else if !item.can_download() {
                "[SKIP]"
            } else {
                "[FILE]"
            };
            let suffix = item
                .size
                .map(|size| format!("  {}", format_bytes(size)))
                .unwrap_or_default();
            ListItem::new(format!(
                "{kind} {}{}{suffix}",
                item.name,
                if item.is_folder() { "/" } else { "" }
            ))
        })
        .collect();
    let list = List::new(rows)
        .block(
            Block::default()
                .title(" Remote Files ")
                .borders(Borders::ALL)
                .border_style(focus_style(
                    app.active_tab == AppTab::Files && app.files_focus == PanelFocus::Upper,
                )),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default()
        .with_selected(app.current().selected)
        .with_offset(app.current().offset);
    frame.render_stateful_widget(list, area, &mut state);
    app.current_mut().offset = state.offset();
}

fn render_log_card(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    let filtered: Vec<_> = app
        .logs
        .iter()
        .filter(|entry| app.log_filter.matches(entry.level))
        .collect();
    let selected = filtered.len().checked_sub(
        1 + app
            .log_offset_from_bottom
            .min(filtered.len().saturating_sub(1)),
    );
    let rows: Vec<ListItem<'_>> = filtered
        .iter()
        .map(|entry| {
            let style = match entry.level {
                LogLevel::Info => Style::default().fg(Color::White),
                LogLevel::Error => Style::default().fg(Color::Red),
            };
            ListItem::new(entry.render()).style(style)
        })
        .collect();
    let title = format!(
        " Download logs [{}] ({}) ",
        app.log_filter.label(),
        filtered.len()
    );
    let list = List::new(rows)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(focus_style(app.logs_focus == PanelFocus::Upper)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_settings(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    let form = app
        .settings_form
        .as_ref()
        .expect("settings view always has a form");
    let displayed_proxy = if form.editing {
        with_cursor(&form.proxy, form.cursor)
    } else if form.proxy.is_empty() {
        "<direct>".to_owned()
    } else {
        form.proxy.clone()
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Proxy URL\n\n> {displayed_proxy}\n\nExamples: http://127.0.0.1:7890 or http://user:pass@host:port\nClear the field to connect directly; system proxy variables are ignored."
        ))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(if form.editing {
                    " Global settings [editing] "
                } else {
                    " Global settings "
                })
                .borders(Borders::ALL)
                .border_style(if app.settings_focus == PanelFocus::Upper {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Yellow)
                }),
        ),
        area,
    );
}

fn render_log_detail(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    let filtered: Vec<_> = app
        .logs
        .iter()
        .filter(|entry| app.log_filter.matches(entry.level))
        .collect();
    let selected = filtered.len().checked_sub(
        1 + app
            .log_offset_from_bottom
            .min(filtered.len().saturating_sub(1)),
    );
    let detail = selected
        .and_then(|index| filtered.get(index))
        .map(|entry| entry.render())
        .unwrap_or_else(|| "No log entry selected".to_owned());
    frame.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" Log Detail ")
                .borders(Borders::ALL)
                .border_style(focus_style(app.logs_focus == PanelFocus::Lower)),
        ),
        area,
    );
}

fn render_settings_status(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    let Some(form) = app.provider_form.as_ref() else {
        frame.render_widget(
            Paragraph::new("Provider settings are unavailable").block(
                Block::default()
                    .title(" Provider Settings ")
                    .borders(Borders::ALL),
            ),
            area,
        );
        return;
    };
    let block = Block::default()
        .title(format!(
            " {} / {} [{}]{} ",
            app.provider_name,
            app.profile_name,
            app.provider_status,
            if form.editing { " editing" } else { "" }
        ))
        .borders(Borders::ALL)
        .border_style(focus_style(app.settings_focus == PanelFocus::Lower));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [fields_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    let visible = fields_area.height as usize;
    let start = form
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .min(form.fields.len().saturating_sub(visible));
    let rows: Vec<ListItem<'_>> = form
        .fields
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, field)| {
            let value =
                ProviderSettingsForm::display_value(field, form.editing && index == form.selected);
            let required = if field.spec.required { "*" } else { "" };
            ListItem::new(format!("{}{required}: {value}", field.spec.label))
        })
        .collect();
    let selected = (!rows.is_empty()).then_some(form.selected.saturating_sub(start));
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> "),
        fields_area,
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(form.status.clone())
            .style(Style::default().fg(Color::Cyan))
            .wrap(Wrap { trim: true }),
        status_area,
    );
}

fn render_status(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    match app.mode {
        BrowserMode::Browsing | BrowserMode::Confirming => {
            let detail = match app.selected_item() {
                Some(item) => format!(
                    "Name: {}\nType: {}\nSize: {}\nRemote ID: {}\nDownload root: {}",
                    item.name,
                    if item.is_folder() { "folder" } else { "file" },
                    item.size
                        .map(format_bytes)
                        .unwrap_or_else(|| "unknown".to_owned()),
                    item.reference.object_id,
                    app.destination.display()
                ),
                None => format!(
                    "This folder is empty.\nDownload root: {}",
                    app.destination.display()
                ),
            };
            frame.render_widget(
                Paragraph::new(detail).wrap(Wrap { trim: true }).block(
                    Block::default()
                        .title(" Transfers / Selection ")
                        .borders(Borders::ALL)
                        .border_style(focus_style(app.files_focus == PanelFocus::Lower)),
                ),
                area,
            );
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
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(focus_style(app.files_focus == PanelFocus::Lower)),
                ),
                area,
            );
        }
        BrowserMode::Downloading => render_progress(frame, app, area),
        BrowserMode::Finished => {
            frame.render_widget(
                Paragraph::new(format!(
                    "Downloaded: {} | Skipped: {} | Failed: {}\nCompleted bytes: {} | Partial bytes: {}\nDestination: {}\n\nPress Enter/Esc to return to the browser.",
                    app.download.downloaded,
                    app.download.skipped,
                    app.download.failed,
                    format_bytes(app.download.overall_written.saturating_sub(app.download.partial_bytes)),
                    format_bytes(app.download.partial_bytes),
                    app.download_target_destination().display()
                ))
                .block(
                    Block::default()
                        .title(" Download complete ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Green)),
                ),
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
                .block(
                    Block::default()
                        .title(" Download failed ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red)),
                ),
                area,
            );
        }
    }
}

fn render_progress(frame: &mut Frame<'_>, app: &BrowserApp, area: Rect) {
    let running = app.download.status_count(TransferState::Running);
    let queued = app.download.status_count(TransferState::Queued);
    let done = app.download.status_count(TransferState::Completed);
    let failed = app.download.status_count(TransferState::Failed);
    let title = format!(
        " Transfers [{}] - {running} running / {queued} queued / {done} done / {failed} failed - Total {}/s ",
        app.download.transfer_filter.label(),
        format_speed(app.download.aggregate_speed())
    );
    let transfers = app.download.sorted_transfers();
    let selected = app
        .download
        .transfer_selected
        .min(transfers.len().saturating_sub(1));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(focus_style(app.files_focus == PanelFocus::Lower));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [list_area, detail_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    let visible_rows = list_area.height as usize;
    let maximum_offset = transfers.len().saturating_sub(visible_rows.max(1));
    let offset = app.download.transfer_offset.min(maximum_offset);
    let rows: Vec<ListItem<'_>> = transfers
        .iter()
        .copied()
        .skip(offset)
        .take(visible_rows)
        .map(|transfer| {
            let progress = match transfer.status {
                TransferState::Running => match transfer.total {
                    Some(total) if total > 0 => format!(
                        "{:>3.0}%  {} / {}  {}/s",
                        ratio(transfer.written, total) * 100.0,
                        format_bytes(transfer.written),
                        format_bytes(total),
                        format_speed(transfer.bytes_per_second)
                    ),
                    _ => format!(
                        "      {}  {}/s",
                        format_bytes(transfer.written),
                        format_speed(transfer.bytes_per_second)
                    ),
                },
                TransferState::Completed => format!("      {}", format_bytes(transfer.written)),
                TransferState::Failed | TransferState::Skipped | TransferState::Interrupted => {
                    transfer
                        .detail
                        .as_deref()
                        .map(|detail| format!("      {detail}"))
                        .unwrap_or_default()
                }
                TransferState::Queued => transfer
                    .total
                    .map(|total| format!("      {}", format_bytes(total)))
                    .unwrap_or_default(),
            };
            let style = match transfer.status {
                TransferState::Running => Style::default().fg(Color::Yellow),
                TransferState::Failed => Style::default().fg(Color::Red),
                TransferState::Queued => Style::default().fg(Color::Gray),
                TransferState::Skipped => Style::default().fg(Color::DarkGray),
                TransferState::Completed => Style::default().fg(Color::Green),
                TransferState::Interrupted => Style::default().fg(Color::Magenta),
            };
            ListItem::new(format!(
                "{}  {}  {}{}",
                transfer.status.label(),
                transfer.id.0,
                transfer.path.display(),
                progress
            ))
            .style(style)
        })
        .collect();
    let rows = if rows.is_empty() {
        vec![ListItem::new(format!(
            "Waiting for transfer queue | {} / {} items",
            app.download.items_processed, app.download.items_total
        ))]
    } else {
        rows
    };
    let selected_in_view = (!transfers.is_empty() && selected >= offset)
        .then_some(selected.saturating_sub(offset))
        .filter(|index| *index < rows.len());
    let mut state = ListState::default().with_selected(selected_in_view);
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("> "),
        list_area,
        &mut state,
    );
    let selected_detail = transfers
        .get(selected)
        .map(|transfer| {
            let detail = transfer.detail.as_deref().unwrap_or("no error");
            format!(
                "Items {}/{} | {} | {} | {}",
                app.download.items_processed,
                app.download.items_total,
                transfer.status.label(),
                transfer.path.display(),
                detail
            )
        })
        .unwrap_or_else(|| {
            format!(
                "Items {}/{} | No transfers match this filter",
                app.download.items_processed, app.download.items_total
            )
        });
    frame.render_widget(
        Paragraph::new(selected_detail)
            .style(Style::default().fg(Color::Cyan))
            .wrap(Wrap { trim: true }),
        detail_area,
    );
}

fn footer_text(app: &BrowserApp) -> &'static str {
    if app.quit_confirming {
        return "y/Enter exit | n/Esc return";
    }
    if app.provider_selector.is_some() {
        return "Left/Right Provider | Up/Down Source | Enter switch | Esc cancel";
    }
    if app.active_tab == AppTab::Logs {
        return "Up/Down | PgUp/PgDn | a/i/e filter | p provider | [ / ] tabs | Tab panel | q quit";
    }
    if app.active_tab == AppTab::Settings {
        return if app.provider_form.as_ref().is_some_and(|form| form.editing) {
            "Edit Provider field | Enter save/rebuild | Esc stop editing"
        } else if app.settings_form.as_ref().is_some_and(|form| form.editing) {
            "Edit proxy | Enter save/reconnect | Esc stop editing | q quit"
        } else if app.settings_focus == PanelFocus::Lower {
            "Up/Down field | Enter edit | Space toggle | p Provider/Source | Tab panel"
        } else {
            "Enter edit | p provider | [ / ] tabs | Tab panel | q quit"
        };
    }
    if app.files_focus == PanelFocus::Lower {
        return "Up/Down scroll | a/v/f filter | p provider | [ / ] tabs | Tab panel | q quit";
    }
    match app.mode {
        BrowserMode::Browsing => {
            if app.current().is_shared_root() {
                if app.selected_download_target().is_some() {
                    "Enter open | d download | r reload | [ / ] tabs | Tab panel | q quit"
                } else {
                    "r reload | [ / ] tabs | Tab panel | q quit"
                }
            } else if app.selected_item().is_some_and(RemoteItem::is_folder) {
                "Enter open | Backspace back | r reload | d download | [ / ] tabs | Tab panel"
            } else if app.selected_download_target().is_some() {
                "Backspace back | r reload | d download | [ / ] tabs | Tab panel | q quit"
            } else {
                "Backspace back | r reload | [ / ] tabs | Tab panel | q quit"
            }
        }
        BrowserMode::Confirming => "Up/Down field | Enter start | Esc cancel | q exit",
        BrowserMode::Scanning | BrowserMode::Downloading => {
            "Download active | [ / ] tabs | Tab panel | q quit and retain partial files"
        }
        BrowserMode::Finished | BrowserMode::Failed => {
            "Enter/Esc return | [ / ] tabs | Tab panel | q quit"
        }
    }
}

fn ratio(written: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (written as f64 / total as f64).clamp(0.0, 1.0)
    }
}

fn format_speed(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        "0 B".to_owned()
    } else {
        format_bytes(bytes_per_second.round().min(u64::MAX as f64) as u64)
    }
}
fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center);
    let horizontal = Layout::horizontal([Constraint::Percentage(width_percent)]).flex(Flex::Center);
    let [vertical_area] = vertical.areas(area);
    let [popup] = horizontal.areas(vertical_area);
    popup
}
