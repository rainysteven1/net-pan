use super::*;

pub async fn run_browser(
    mut providers: Vec<BrowserProvider>,
    active_provider: usize,
    runtime: BrowserRuntime,
) -> Result<BrowserOutcome> {
    anyhow::ensure!(
        !providers.is_empty(),
        "no registered cloud provider is available"
    );
    anyhow::ensure!(
        active_provider < providers.len(),
        "active provider index is out of bounds"
    );
    for provider_runtime in &mut providers {
        provider_runtime
            .refresh_sources(runtime.client.as_ref())
            .await;
    }
    let mut logger = SessionLogger::from_home()?;
    let mut terminal = ratatui::try_init().context("failed to initialize terminal")?;
    let mut app = BrowserApp::new(
        Vec::new(),
        runtime.destination.clone(),
        runtime.network.clone(),
    );
    app.set_provider_catalog(&providers, active_provider);
    let result = run_loop(
        &mut terminal,
        providers,
        active_provider,
        app,
        runtime,
        &mut logger,
    )
    .await;
    drop(terminal);
    let restore = ratatui::try_restore().context("failed to restore terminal");
    match (result, restore) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    mut providers: Vec<BrowserProvider>,
    mut active_provider: usize,
    mut app: BrowserApp,
    paths: BrowserRuntime,
    logger: &mut SessionLogger,
) -> Result<BrowserOutcome> {
    if paths.client.is_daemon() {
        app.daemon_mode = true;
        app.message = "Connected to pan-tui daemon".to_owned();
    }
    let initial_source = (!providers[active_provider].sources.is_empty()).then_some(0);
    let initial = app.activate_selection(active_provider, initial_source);
    if let Some(root) = initial.root_to_load {
        load_source_root(terminal, paths.client.as_ref(), &mut app, root).await?;
    }

    loop {
        loop {
            match paths.client.try_next_update().await {
                Some(JobUpdate::Event(event)) => {
                    record_download_event(&mut app, &event, logger)?;
                    app.apply_download_event(event);
                }
                Some(JobUpdate::Snapshot { revision: _, jobs }) => {
                    app.apply_download_snapshots(&jobs);
                }
                Some(JobUpdate::Disconnected(error)) => {
                    app.message = format!("Job backend disconnected: {error}; reconnecting...");
                }
                None => break,
            }
        }
        paths.client.reconnect_if_needed().await;
        terminal.draw(|frame| render_ui(frame, &mut app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if app.quit_confirming {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    return Ok(BrowserOutcome::Quit);
                }
                KeyCode::Char('n') | KeyCode::Esc => app.quit_confirming = false,
                _ => {}
            }
            continue;
        }
        if let Some(selected) = app.provider_selector {
            match key.code {
                KeyCode::Left => app.move_provider_selection(-1),
                KeyCode::Right => app.move_provider_selection(1),
                KeyCode::Up | KeyCode::Char('k') => app.move_source_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => app.move_source_selection(1),
                KeyCode::Esc => app.provider_selector = None,
                KeyCode::Enter => {
                    active_provider = selected.provider;
                    let source_index =
                        (!providers[active_provider].sources.is_empty()).then_some(selected.source);
                    let activation = app.activate_selection(active_provider, source_index);
                    app.refresh_provider_form(&providers[active_provider]);
                    if activation.restored {
                        app.message =
                            format!("Restored {} / {}", app.provider_name, app.source_name);
                    } else if let Some(root) = activation.root_to_load {
                        load_source_root(terminal, paths.client.as_ref(), &mut app, root).await?;
                    }
                }
                _ => {}
            }
            continue;
        }
        if is_quit(key) {
            app.quit_confirming = true;
            continue;
        }
        if app.active_tab == AppTab::Settings
            && app.settings_focus == PanelFocus::Lower
            && app.provider_form.as_ref().is_some_and(|form| form.editing)
        {
            match key.code {
                KeyCode::Esc => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.editing = false;
                    }
                }
                KeyCode::Enter => {
                    let changes = app
                        .provider_form
                        .as_ref()
                        .map(ProviderSettingsForm::changes)
                        .unwrap_or_default();
                    if changes.is_empty() {
                        if let Some(form) = app.provider_form.as_mut() {
                            form.editing = false;
                            form.status = "No Provider settings changed.".to_owned();
                        }
                        continue;
                    }
                    match paths
                        .client
                        .save_provider_settings(providers[active_provider].id(), &changes)
                        .await
                    {
                        Ok(outcome) => {
                            let reconnect_failed = matches!(
                                outcome.provider.status,
                                ProviderStatus::ConfigError(_) | ProviderStatus::ConnectionError(_)
                            );
                            providers[active_provider].update(outcome.provider);
                            providers[active_provider]
                                .refresh_sources(paths.client.as_ref())
                                .await;
                            app.refresh_provider_catalog(&providers);
                            if reconnect_failed {
                                if let Some(form) = app.provider_form.as_mut() {
                                    let retention = if outcome.retained_previous_instance {
                                        "previous instance retained"
                                    } else {
                                        "no previous instance was available"
                                    };
                                    form.status = format!(
                                        "Saved, but reconnect failed; {retention}: {}",
                                        providers[active_provider].status_label()
                                    );
                                    form.editing = false;
                                }
                            } else {
                                app.refresh_provider_form(&providers[active_provider]);
                                if let Some(form) = app.provider_form.as_mut() {
                                    form.status =
                                        "Provider settings saved and instance rebuilt.".to_owned();
                                }
                            }
                        }
                        Err(error) => {
                            if let Some(form) = app.provider_form.as_mut() {
                                form.status = format!("Could not save Provider settings: {error}");
                            }
                        }
                    }
                }
                code => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.handle_edit_key(code);
                    }
                }
            }
            continue;
        }
        if app.active_tab == AppTab::Settings
            && app.settings_form.as_ref().is_some_and(|form| form.editing)
        {
            match key.code {
                KeyCode::Esc => {
                    if let Some(form) = app.settings_form.as_mut() {
                        form.editing = false;
                    }
                }
                KeyCode::Enter => {
                    let proxy = app
                        .settings_form
                        .as_ref()
                        .map(|form| form.proxy.clone())
                        .unwrap_or_default();
                    let candidate = NetworkSettings::from_proxy_input(&proxy);
                    let mut config = match AppConfig::load(&paths.settings) {
                        Ok(config) => config,
                        Err(error) => {
                            if let Some(form) = app.settings_form.as_mut() {
                                form.status = format!("Could not load config: {error:#}");
                            }
                            continue;
                        }
                    };
                    config.network = candidate.clone();
                    if let Err(error) = config.save(&paths.settings) {
                        if let Some(form) = app.settings_form.as_mut() {
                            form.status = format!("Could not save config: {error:#}");
                        }
                        continue;
                    }
                    let catalog = match paths.client.reconfigure_network(candidate.clone()).await {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            if let Some(form) = app.settings_form.as_mut() {
                                form.status = format!("Saved, but reconnect failed: {error:#}");
                            }
                            continue;
                        }
                    };
                    for entry in catalog {
                        if let Some(runtime) = providers.iter_mut().find(|item| item.id == entry.id)
                        {
                            runtime.update(entry);
                            runtime.refresh_sources(paths.client.as_ref()).await;
                        }
                    }
                    app.network_settings = candidate;
                    app.refresh_provider_catalog(&providers);
                    app.message = "Proxy saved; reconnecting...".to_owned();
                    if let Some(form) = app.settings_form.as_mut() {
                        let failures: Vec<_> = providers
                            .iter()
                            .filter(|provider| {
                                matches!(
                                    provider.status,
                                    ProviderStatus::ConfigError(_)
                                        | ProviderStatus::ConnectionError(_)
                                )
                            })
                            .map(|provider| provider.name.clone())
                            .collect();
                        form.status = if failures.is_empty() {
                            "Saved to config.toml; providers reconnected.".to_owned()
                        } else {
                            format!(
                                "Saved; retained previous instances for: {}",
                                failures.join("; ")
                            )
                        };
                        form.editing = false;
                    }
                }
                code => app.handle_settings_edit_key(code),
            }
            continue;
        }

        match app.mode {
            BrowserMode::Confirming => {
                match key.code {
                    KeyCode::Enter => {
                        let Some(form) = app.download_form.as_ref() else {
                            app.cancel_download();
                            continue;
                        };
                        let destination_root = PathBuf::from(&form.destination_root);
                        if form.destination_root.is_empty() || !destination_root.is_absolute() {
                            app.message = "Destination root must be an absolute path".to_owned();
                            continue;
                        }
                        let workers = form.workers.clamp(1, 5);
                        app.refresh_form_destination();
                        if !providers[active_provider].available {
                            app.message = "The selected Provider is not ready".to_owned();
                            app.return_to_browser();
                            continue;
                        }
                        let Some(target) = app.download_target().cloned() else {
                            app.return_to_browser();
                            continue;
                        };
                        let destination = app.download_parent_destination();
                        let job_options = DownloadOptions {
                            concurrency: workers,
                            ..paths.options
                        };
                        app.begin_download();
                        match paths.client.submit(target, destination, job_options).await {
                            Ok(job_id) => app.download.job_id = Some(job_id),
                            Err(error) => {
                                app.mode = BrowserMode::Failed;
                                app.message = format!("Could not submit download Job: {error:#}");
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc => app.cancel_download(),
                    KeyCode::Up | KeyCode::Down => {
                        if let Some(form) = app.download_form.as_mut() {
                            form.toggle_field();
                        }
                    }
                    code => app.handle_form_edit_key(code),
                }
                continue;
            }
            BrowserMode::Scanning | BrowserMode::Downloading => {}
            BrowserMode::Finished | BrowserMode::Failed => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    app.return_to_browser();
                }
            }
            BrowserMode::Browsing => {}
        }

        match key.code {
            KeyCode::Char('p') => {
                app.open_provider_selector();
                continue;
            }
            KeyCode::Char(']') => {
                app.switch_tab(true);
                continue;
            }
            KeyCode::Char('[') => {
                app.switch_tab(false);
                continue;
            }
            KeyCode::Tab => {
                app.switch_panel();
                continue;
            }
            KeyCode::BackTab => {
                app.switch_panel();
                continue;
            }
            _ => {}
        }
        if app.active_tab == AppTab::Settings {
            match app.settings_focus {
                PanelFocus::Upper => {
                    if key.code == KeyCode::Enter
                        && let Some(form) = app.settings_form.as_mut()
                    {
                        form.editing = true;
                    }
                }
                PanelFocus::Lower => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(form) = app.provider_form.as_mut() {
                            form.move_selection(-1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(form) = app.provider_form.as_mut() {
                            form.move_selection(1);
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(form) = app.provider_form.as_mut()
                            && let Some(field) = form.selected_field_mut()
                            && field.spec.kind == ProviderSettingKind::Boolean
                        {
                            field.value = (!matches!(
                                field.value.trim().to_ascii_lowercase().as_str(),
                                "true" | "1" | "yes" | "on"
                            ))
                            .to_string();
                            field.cursor = field.value.len();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(form) = app.provider_form.as_mut() {
                            if let Some(reason) = form.read_only_reason.clone() {
                                form.status = reason;
                            } else if form
                                .selected_field()
                                .is_some_and(|field| field.spec.kind == ProviderSettingKind::Action)
                            {
                                form.status = "This setting is managed by the Source selector or its Provider file.".to_owned();
                            } else {
                                form.editing = true;
                            }
                        }
                    }
                    _ => {}
                },
            }
            continue;
        }
        if app.active_tab == AppTab::Logs {
            app.handle_log_key(&key.code);
            continue;
        }
        if app.mode != BrowserMode::Browsing || app.files_focus != PanelFocus::Upper {
            if app.active_tab == AppTab::Files && app.files_focus == PanelFocus::Lower {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => app.download.move_transfer_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => app.download.move_transfer_selection(1),
                    KeyCode::PageUp => app
                        .download
                        .move_transfer_selection(-(app.page_step as isize)),
                    KeyCode::PageDown => {
                        app.download.move_transfer_selection(app.page_step as isize)
                    }
                    KeyCode::Home => {
                        app.download.transfer_selected = 0;
                        app.download.transfer_offset = 0;
                    }
                    KeyCode::End => {
                        let end = app.download.sorted_transfers().len().saturating_sub(1);
                        app.download.transfer_selected = end;
                        app.download.transfer_offset = end;
                    }
                    KeyCode::Char('a') => app.download.set_transfer_filter(TransferFilter::All),
                    KeyCode::Char('v') => app.download.set_transfer_filter(TransferFilter::Active),
                    KeyCode::Char('f') => app.download.set_transfer_filter(TransferFilter::Failed),
                    _ => {}
                }
            }
            continue;
        }

        if app.handle_selection_key(&key.code) {
            continue;
        }
        if key.code == KeyCode::Char('r') {
            if providers[active_provider].available {
                reload_current_page(terminal, paths.client.as_ref(), &mut app).await?;
            } else {
                app.message = "The selected Provider is not ready".to_owned();
            }
            continue;
        }

        match app.browser_layer() {
            BrowserLayer::SharedLinks => match key.code {
                KeyCode::Enter => {
                    let Some(item) = app.selected_item().cloned() else {
                        continue;
                    };
                    if app
                        .current_source
                        .as_ref()
                        .is_some_and(|key| key.source_id == item.reference.source_id)
                    {
                        open_selected_folder(terminal, paths.client.as_ref(), &mut app).await?;
                        continue;
                    }
                    let Some(source_index) = app.source_index_for_item(&item) else {
                        app.message = "Selected Source is no longer available".to_owned();
                        continue;
                    };
                    let activation = app.activate_selection(active_provider, Some(source_index));
                    if let Some(root) = activation.root_to_load {
                        load_source_root(terminal, paths.client.as_ref(), &mut app, root).await?;
                    }
                }
                KeyCode::Char('d') if app.selected_download_target().is_some() => {
                    app.request_download();
                }
                _ => {}
            },
            BrowserLayer::DriveFolder => match key.code {
                KeyCode::Backspace | KeyCode::Left => app.go_back(),
                KeyCode::Char('d') if app.selected_download_target().is_some() => {
                    app.request_download()
                }
                KeyCode::Enter | KeyCode::Right => {
                    open_selected_folder(terminal, paths.client.as_ref(), &mut app).await?;
                }
                _ => {}
            },
        }
    }
}

pub(super) fn record_download_event(
    app: &mut BrowserApp,
    event: &DownloadEvent,
    logger: &mut SessionLogger,
) -> Result<()> {
    let message = match event {
        DownloadEvent::Scanning { job_id, provider } => Some((
            LogLevel::Info,
            format!(
                "planning provider={} profile={} job={} target={}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                app.download_target_destination().display()
            ),
        )),
        DownloadEvent::PlanReady {
            job_id,
            provider,
            files,
            items,
            bytes,
            pre_skipped,
            workers,
        } => Some((
            LogLevel::Info,
            format!(
                "plan_ready provider={} profile={} job={} items={items} downloadable={files} skipped={pre_skipped} known_bytes={bytes} workers={workers}",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
        DownloadEvent::TransferQueued { .. } => None,
        DownloadEvent::FileStarted {
            job_id,
            provider,
            transfer_id,
            index,
            total,
            path,
            size,
        } => Some((
            LogLevel::Info,
            format!(
                "started provider={} profile={} job={} transfer={} index={index}/{total} path={} size={}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                transfer_id.0,
                app.absolute_event_path(path).display(),
                size.map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string())
            ),
        )),
        DownloadEvent::Progress { .. } => None,
        DownloadEvent::FileSkipped {
            job_id,
            provider,
            transfer_id,
            path,
            reason,
        } => Some((
            LogLevel::Info,
            format!(
                "skipped provider={} profile={} job={} transfer={} path={} reason={reason}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                transfer_id.0,
                app.absolute_event_path(path).display()
            ),
        )),
        DownloadEvent::FileCompleted {
            job_id,
            provider,
            transfer_id,
            path,
            bytes,
        } => Some((
            LogLevel::Info,
            format!(
                "completed provider={} profile={} job={} transfer={} path={} bytes={bytes}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                transfer_id.0,
                app.absolute_event_path(path).display()
            ),
        )),
        DownloadEvent::FileFailed {
            job_id,
            provider,
            transfer_id,
            path,
            bytes,
            error,
        } => Some((
            LogLevel::Error,
            format!(
                "failed provider={} profile={} job={} transfer={} path={} partial_bytes={bytes} reason={error}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                transfer_id.0,
                app.absolute_event_path(path).display()
            ),
        )),
        DownloadEvent::Finished {
            job_id,
            provider,
            summary,
        } => Some((
            LogLevel::Info,
            format!(
                "finished provider={} profile={} job={} downloaded={} skipped={} failed={} completed_bytes={} partial_bytes={}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                summary.downloaded,
                summary.skipped,
                summary.failed,
                summary.bytes_written,
                summary.partial_bytes
            ),
        )),
        DownloadEvent::Fatal {
            job_id,
            provider,
            error,
        } => Some((
            LogLevel::Error,
            format!(
                "fatal provider={} profile={} job={} reason={error}",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
        DownloadEvent::Cancelled { job_id, provider } => Some((
            LogLevel::Info,
            format!(
                "cancelled provider={} profile={} job={}",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
    };
    if let Some((level, message)) = message {
        let entry = logger.write(level, "download", message)?;
        if app.log_offset_from_bottom > 0 && app.log_filter.matches(entry.level) {
            app.log_offset_from_bottom = app.log_offset_from_bottom.saturating_add(1);
        }
        app.logs.push(entry);
    }
    Ok(())
}

async fn reload_current_page(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
) -> Result<()> {
    let layer = app.browser_layer();
    app.message = match layer {
        BrowserLayer::SharedLinks => "Reloading saved links...".to_owned(),
        BrowserLayer::DriveFolder => "Reloading current folder...".to_owned(),
    };
    terminal.draw(|frame| render_ui(frame, app))?;

    let result = match layer {
        BrowserLayer::SharedLinks => client
            .sources(
                &app.current_source
                    .as_ref()
                    .map(|key| key.identity.clone())
                    .unwrap_or_else(|| {
                        let choice = &app.provider_choices[app.active_provider];
                        ProviderIdentity::new(choice.id.clone(), choice.profile.clone())
                    }),
            )
            .await
            .map(|sources| sources.into_iter().map(|source| source.root).collect()),
        BrowserLayer::DriveFolder => {
            let folder = app
                .current_drive_folder()
                .expect("Drive folder layer always has a folder")
                .clone();
            client.list_children(&folder.reference).await
        }
    };
    match result {
        Ok(items) => {
            let count = items.len();
            app.replace_current_items(items);
            app.message = format!("Reloaded {count} items");
        }
        Err(error) => app.message = format!("Reload failed: {error:#}"),
    }
    Ok(())
}

async fn open_selected_folder(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
) -> Result<()> {
    let Some(item) = app.selected_item().cloned() else {
        app.message = "This folder is empty".to_owned();
        return Ok(());
    };
    if !item.is_folder() {
        app.message = "The selected item is not a folder".to_owned();
        return Ok(());
    }
    app.message = format!("Loading {}...", item.name);
    terminal.draw(|frame| render_ui(frame, app))?;
    match client.list_children(&item.reference).await {
        Ok(children) => app.enter_loaded(item, children),
        Err(error) => app.message = format!("Failed to open folder: {error:#}"),
    }
    Ok(())
}

async fn load_source_root(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
    root: RemoteItem,
) -> Result<()> {
    terminal.draw(|frame| render_ui(frame, app))?;
    match client.list_children(&root.reference).await {
        Ok(children) => {
            let count = children.len();
            app.enter_loaded(root, children);
            app.message = format!("Loaded {count} items from {}", app.source_name);
        }
        Err(error) => {
            app.message = format!("Source load failed: {error}");
        }
    }
    Ok(())
}

fn is_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}
