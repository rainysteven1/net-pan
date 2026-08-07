use super::*;

pub(super) fn persist_global_settings(
    settings_path: &Path,
    destination: &Path,
    network: &NetworkSettings,
) -> Result<()> {
    validate_download_destination(destination)?;
    let mut config = AppConfig::load(settings_path)?;
    config.download.destination = Some(destination.to_path_buf());
    config.network = network.clone();
    config.save(settings_path)
}

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
    let mut logger = SessionLogger::from_home()?;
    let mut app = BrowserApp::new(
        Vec::new(),
        runtime.destination.clone(),
        runtime.network.clone(),
    );
    app.settings_path = runtime.settings.clone();
    record_runtime_log(
        &mut app,
        &mut logger,
        LogLevel::Info,
        "runtime",
        format!(
            "session started backend={} destination={}",
            if runtime.client.is_daemon() {
                "daemon"
            } else {
                "embedded"
            },
            runtime.destination.display()
        ),
    )?;
    for provider_runtime in &mut providers {
        provider_runtime
            .refresh_sources(runtime.client.as_ref())
            .await;
        record_provider_refresh(&mut app, &mut logger, provider_runtime)?;
    }
    app.set_provider_catalog(&providers, active_provider);
    let mut terminal = ratatui::try_init().context("failed to initialize terminal")?;
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
    #[cfg(unix)]
    let termination_signals = TerminationSignals::new()?;
    if paths.client.is_daemon() {
        app.daemon_mode = true;
        app.message = "Connected to net-pan daemon".to_owned();
    }
    if !providers[active_provider].available {
        let _ = app.activate_selection(active_provider, None);
        app.open_provider_settings_page();
    }

    loop {
        #[cfg(unix)]
        if termination_signals.received() {
            return Ok(BrowserOutcome::Quit);
        }
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
                    record_runtime_log(
                        &mut app,
                        logger,
                        LogLevel::Error,
                        "daemon",
                        format!("disconnected reason={error}"),
                    )?;
                }
                None => break,
            }
        }
        paths.client.reconnect_if_needed().await;
        app.expire_toast();
        terminal.draw(|frame| render_ui(frame, &mut app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            tokio::time::sleep(Duration::from_millis(25)).await;
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.quit_confirming = true;
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
        if app.account_form.is_some() {
            match key.code {
                KeyCode::Esc => app.account_form = None,
                KeyCode::Enter => {
                    let name = app
                        .account_form
                        .as_ref()
                        .map(|form| validate_account_name(&form.value));
                    let name = match name {
                        Some(Ok(name)) => name,
                        Some(Err(error)) => {
                            if let Some(form) = app.account_form.as_mut() {
                                form.status = error.to_owned();
                            }
                            continue;
                        }
                        None => continue,
                    };
                    let Some(provider_id) = app.current_provider_id().cloned() else {
                        app.account_form = None;
                        continue;
                    };
                    match paths.client.create_profile(&provider_id, &name).await {
                        Ok(entry) => {
                            let identity = entry.identity.clone();
                            let mut provider = BrowserProvider::new(entry);
                            provider.refresh_sources(paths.client.as_ref()).await;
                            record_provider_refresh(&mut app, logger, &provider)?;
                            record_runtime_log(
                                &mut app,
                                logger,
                                LogLevel::Info,
                                "provider",
                                format!(
                                    "account created provider={} profile={}",
                                    identity.provider_id, identity.profile_id
                                ),
                            )?;
                            providers.push(provider);
                            providers.sort_by(|left, right| left.identity.cmp(&right.identity));
                            active_provider = providers
                                .iter()
                                .position(|provider| provider.identity == identity)
                                .expect("new Account remains in the Provider catalog");
                            app.refresh_provider_catalog(&providers);
                            let _ = app.activate_selection(active_provider, None);
                            app.refresh_provider_form(&providers[active_provider]);
                            app.active_tab = AppTab::Accounts;
                            app.navigation_tab = AppTab::Accounts;
                            app.navigation_focus = NavigationFocus::Content;
                            app.account_form = None;
                            app.message = format!("Created Account {name}");
                        }
                        Err(error) => {
                            record_runtime_log(
                                &mut app,
                                logger,
                                LogLevel::Error,
                                "provider",
                                format!(
                                    "account creation failed provider={provider_id} reason={error:#}"
                                ),
                            )?;
                            if let Some(form) = app.account_form.as_mut() {
                                form.status = format!("Could not create Account: {error:#}");
                            }
                        }
                    }
                }
                code => {
                    if let Some(form) = app.account_form.as_mut() {
                        form.handle_key(code);
                    }
                }
            }
            continue;
        }
        if let Some(selected) = app.provider_selector {
            match key.code {
                KeyCode::Up => app.move_source_selection(-1),
                KeyCode::Down => app.move_source_selection(1),
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
                        load_source_root(terminal, paths.client.as_ref(), &mut app, logger, root)
                            .await?;
                    }
                }
                _ => {}
            }
            continue;
        }
        let editing_text = app.provider_form.as_ref().is_some_and(|form| form.editing)
            || app.settings_form.as_ref().is_some_and(|form| form.editing);
        if is_quit(key) && !(editing_text && key.code == KeyCode::Char('q')) {
            app.quit_confirming = true;
            continue;
        }
        if app.provider_settings_visible()
            && app.provider_form.as_ref().is_some_and(|form| form.editing)
        {
            match key.code {
                KeyCode::Esc => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.editing = false;
                    }
                }
                _ if is_save(key) => {
                    let changes = app
                        .provider_form
                        .as_ref()
                        .map(ProviderSettingsForm::changes)
                        .unwrap_or_default();
                    if changes.is_empty() {
                        if let Some(form) = app.provider_form.as_mut() {
                            form.editing = false;
                            form.status = "Ready".to_owned();
                        }
                        app.show_toast(ToastKind::Success, "No Provider settings changed");
                        continue;
                    }
                    match paths
                        .client
                        .save_provider_settings(&providers[active_provider].identity(), &changes)
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
                            record_provider_refresh(&mut app, logger, &providers[active_provider])?;
                            app.refresh_provider_catalog(&providers);
                            if reconnect_failed {
                                let retention = if outcome.retained_previous_instance {
                                    "previous instance retained"
                                } else {
                                    "no previous instance was available"
                                };
                                let message = format!(
                                    "Saved, but reconnect failed; {retention}: {}",
                                    providers[active_provider].status_label()
                                );
                                if let Some(form) = app.provider_form.as_mut() {
                                    form.status = message.clone();
                                    form.editing = false;
                                }
                                record_runtime_log(
                                    &mut app,
                                    logger,
                                    LogLevel::Error,
                                    "settings",
                                    format!(
                                        "provider rebuild failed provider={} profile={} retention={retention}",
                                        providers[active_provider].identity.provider_id,
                                        providers[active_provider].identity.profile_id
                                    ),
                                )?;
                                app.show_toast(ToastKind::Warning, message);
                            } else {
                                app.refresh_provider_form(&providers[active_provider]);
                                app.show_toast(
                                    ToastKind::Success,
                                    "Provider settings saved and rebuilt",
                                );
                                record_runtime_log(
                                    &mut app,
                                    logger,
                                    LogLevel::Info,
                                    "settings",
                                    format!(
                                        "provider settings saved provider={} profile={} rebuilt=true",
                                        providers[active_provider].identity.provider_id,
                                        providers[active_provider].identity.profile_id
                                    ),
                                )?;
                            }
                        }
                        Err(error) => {
                            record_runtime_log(
                                &mut app,
                                logger,
                                LogLevel::Error,
                                "settings",
                                format!(
                                    "provider settings save failed provider={} profile={} reason={error:#}",
                                    providers[active_provider].identity.provider_id,
                                    providers[active_provider].identity.profile_id
                                ),
                            )?;
                            if let Some(form) = app.provider_form.as_mut() {
                                form.status = format!("Could not save Provider settings: {error}");
                            }
                        }
                    }
                }
                KeyCode::Enter => {}
                code => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.handle_edit_key(code);
                    }
                }
            }
            continue;
        }
        if app.global_settings_open && app.settings_form.as_ref().is_some_and(|form| form.editing) {
            match key.code {
                KeyCode::Esc => {
                    if let Some(form) = app.settings_form.as_mut() {
                        form.editing = false;
                    }
                }
                _ if is_save(key) => {
                    let Some(form) = app.settings_form.as_ref() else {
                        continue;
                    };
                    let destination = PathBuf::from(&form.destination);
                    if let Err(error) = validate_download_destination(&destination) {
                        if let Some(form) = app.settings_form.as_mut() {
                            form.status = format!("Could not save: {error}");
                        }
                        continue;
                    }
                    let proxy = form.proxy.clone();
                    let candidate = NetworkSettings::from_proxy_input(&proxy);
                    let network_mode = if candidate.proxy.is_some() {
                        "proxy"
                    } else {
                        "direct"
                    };
                    let network_changed = candidate != app.network_settings;
                    let active_job_keeps_destination = destination != app.destination
                        && app.download.state.is_some_and(|state| !state.is_terminal());
                    if let Err(error) =
                        persist_global_settings(&paths.settings, &destination, &candidate)
                    {
                        record_runtime_log(
                            &mut app,
                            logger,
                            LogLevel::Error,
                            "settings",
                            format!("global settings save failed reason={error:#}"),
                        )?;
                        if let Some(form) = app.settings_form.as_mut() {
                            form.status = format!("Could not save global settings: {error:#}");
                        }
                        continue;
                    }
                    app.destination = destination;
                    let destination_label = app.destination.display().to_string();
                    if !network_changed {
                        record_runtime_log(
                            &mut app,
                            logger,
                            LogLevel::Info,
                            "settings",
                            format!("global settings saved destination={destination_label}"),
                        )?;
                        finish_global_settings_save(
                            &mut app,
                            ToastKind::Success,
                            settings_save_message(
                                "Global settings saved",
                                active_job_keeps_destination,
                            ),
                        );
                        continue;
                    }
                    let catalog = match paths.client.reconfigure_network(candidate.clone()).await {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            record_runtime_log(
                                &mut app,
                                logger,
                                LogLevel::Error,
                                "settings",
                                format!("network reconfigure failed reason={error:#}"),
                            )?;
                            finish_global_settings_save(
                                &mut app,
                                ToastKind::Warning,
                                settings_save_message(
                                    format!("Settings saved; reconnect failed: {error:#}"),
                                    active_job_keeps_destination,
                                ),
                            );
                            continue;
                        }
                    };
                    for entry in catalog {
                        if let Some(runtime) = providers
                            .iter_mut()
                            .find(|item| item.identity == entry.identity)
                        {
                            runtime.update(entry);
                            runtime.refresh_sources(paths.client.as_ref()).await;
                            record_provider_refresh(&mut app, logger, runtime)?;
                        }
                    }
                    app.network_settings = candidate;
                    app.refresh_provider_catalog(&providers);
                    let failures: Vec<_> = providers
                        .iter()
                        .filter(|provider| {
                            matches!(
                                provider.status,
                                ProviderStatus::ConfigError(_) | ProviderStatus::ConnectionError(_)
                            )
                        })
                        .map(|provider| provider.name.clone())
                        .collect();
                    if failures.is_empty() {
                        record_runtime_log(
                            &mut app,
                            logger,
                            LogLevel::Info,
                            "settings",
                            format!(
                                "global settings saved destination={} network={} providers_reconnected=true",
                                destination_label, network_mode
                            ),
                        )?;
                        finish_global_settings_save(
                            &mut app,
                            ToastKind::Success,
                            settings_save_message(
                                "Global settings saved; providers reconnected",
                                active_job_keeps_destination,
                            ),
                        );
                    } else {
                        record_runtime_log(
                            &mut app,
                            logger,
                            LogLevel::Error,
                            "settings",
                            format!(
                                "global settings saved destination={} network={} provider_failures={}",
                                destination_label,
                                network_mode,
                                failures.join(",")
                            ),
                        )?;
                        finish_global_settings_save(
                            &mut app,
                            ToastKind::Warning,
                            settings_save_message(
                                format!(
                                    "Settings saved; previous instances retained for {}",
                                    failures.join(", ")
                                ),
                                active_job_keeps_destination,
                            ),
                        );
                    }
                }
                KeyCode::Enter => {}
                code => app.handle_settings_edit_key(code),
            }
            continue;
        }
        if app.global_settings_open {
            match key.code {
                KeyCode::Backspace => app.global_settings_open = false,
                code => match keymap::global_settings::intent(code) {
                    Some(
                        keymap::global_settings::Intent::Previous
                        | keymap::global_settings::Intent::Next,
                    ) => {
                        if let Some(form) = app.settings_form.as_mut() {
                            form.select_adjacent();
                        }
                    }
                    Some(keymap::global_settings::Intent::Edit) => {
                        if let Some(form) = app.settings_form.as_mut() {
                            form.editing = true;
                            form.cursor = form.selected_value().chars().count();
                        }
                    }
                    None => {}
                },
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
                        let workers = form.workers.clamp(1, 5);
                        if let Err(error) = validate_download_destination(&app.destination) {
                            app.message = format!("Invalid destination root: {error}");
                            continue;
                        }
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
                                record_runtime_log(
                                    &mut app,
                                    logger,
                                    LogLevel::Error,
                                    "download",
                                    format!("job submission failed reason={error:#}"),
                                )?;
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc => app.cancel_download(),
                    code => app.handle_form_edit_key(code),
                }
                continue;
            }
            BrowserMode::Scanning | BrowserMode::Downloading | BrowserMode::Paused => {}
            BrowserMode::Finished | BrowserMode::Failed => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    app.return_to_browser();
                }
            }
            BrowserMode::Browsing => {}
        }

        match key.code {
            KeyCode::Char('l') => {
                app.active_tab = AppTab::Logs;
                app.navigation_tab = AppTab::Logs;
                app.navigation_focus = NavigationFocus::Content;
                app.logs_focus = PanelFocus::Upper;
                app.message = "Runtime logs".to_owned();
                continue;
            }
            KeyCode::Char('g') => {
                app.open_global_settings();
                continue;
            }
            KeyCode::Char(']') => {
                if let Some(index) = app.switch_provider_type(1) {
                    active_provider = index;
                    let _ = app.activate_selection(active_provider, None);
                    app.refresh_provider_form(&providers[active_provider]);
                    app.message = format!("Switched to {}", app.provider_name);
                }
                continue;
            }
            KeyCode::Char('[') => {
                if let Some(index) = app.switch_provider_type(-1) {
                    active_provider = index;
                    let _ = app.activate_selection(active_provider, None);
                    app.refresh_provider_form(&providers[active_provider]);
                    app.message = format!("Switched to {}", app.provider_name);
                }
                continue;
            }
            KeyCode::Right if app.navigation_focus == NavigationFocus::Navigation => {
                app.navigation_focus = NavigationFocus::Content;
                continue;
            }
            KeyCode::Esc => {
                if app.navigation_focus == NavigationFocus::Content {
                    app.navigation_focus = NavigationFocus::Navigation;
                } else {
                    app.quit_confirming = true;
                }
                continue;
            }
            KeyCode::Backspace
                if app.navigation_focus == NavigationFocus::Content
                    && app.active_tab == AppTab::Accounts
                    && app.account_detail_open =>
            {
                app.account_detail_open = false;
                continue;
            }
            KeyCode::Left | KeyCode::Char('h')
                if app.navigation_focus == NavigationFocus::Content =>
            {
                app.navigation_focus = NavigationFocus::Navigation;
                continue;
            }
            _ => {}
        }
        if app.navigation_focus == NavigationFocus::Navigation {
            match keymap::navigation::intent(key.code) {
                Some(keymap::navigation::Intent::Previous) => app.move_navigation(false),
                Some(keymap::navigation::Intent::Next) => app.move_navigation(true),
                Some(keymap::navigation::Intent::Select) => app.activate_navigation(),
                None => {}
            }
            continue;
        }
        if app.provider_settings_visible() {
            match keymap::provider_settings::intent(key.code) {
                Some(keymap::provider_settings::Intent::Previous) => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.move_selection(-1);
                    }
                }
                Some(keymap::provider_settings::Intent::Next) => {
                    if let Some(form) = app.provider_form.as_mut() {
                        form.move_selection(1);
                    }
                }
                Some(keymap::provider_settings::Intent::Toggle) => {
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
                Some(keymap::provider_settings::Intent::Edit) => {
                    if let Some(form) = app.provider_form.as_mut() {
                        if let Some(reason) = form.read_only_reason.clone() {
                            form.status = reason;
                        } else if form.selected_field().is_none() {
                            form.status = "This Provider has no editable settings.".to_owned();
                        } else {
                            form.editing = true;
                        }
                    }
                }
                None => {}
            }
            continue;
        }
        if app.active_tab == AppTab::Accounts {
            match keymap::accounts::intent(key.code) {
                Some(keymap::accounts::Intent::Previous) => app.move_account_selection(-1),
                Some(keymap::accounts::Intent::Next) => app.move_account_selection(1),
                Some(keymap::accounts::Intent::Add) => app.open_account_form(),
                Some(keymap::accounts::Intent::Open) => {
                    let Some(index) = app.selected_account_provider_index() else {
                        continue;
                    };
                    if index != active_provider {
                        active_provider = index;
                        let _ = app.activate_selection(active_provider, None);
                    }
                    app.refresh_provider_form(&providers[active_provider]);
                    app.open_account_detail();
                }
                Some(keymap::accounts::Intent::Activate) => {
                    let Some(index) = app.selected_account_provider_index() else {
                        continue;
                    };
                    active_provider = index;
                    let _ = app.activate_selection(active_provider, None);
                    app.refresh_provider_form(&providers[active_provider]);
                    app.message = format!("Activated Account {}", app.profile_name);
                }
                None => {}
            }
            continue;
        }
        if app.active_tab == AppTab::Logs {
            app.handle_log_intent(keymap::logs::intent(key.code));
            continue;
        }
        if app.active_tab == AppTab::Transfers {
            match keymap::transfers::intent(key.code) {
                Some(keymap::transfers::Intent::Previous) => {
                    app.download.move_transfer_selection(-1)
                }
                Some(keymap::transfers::Intent::Next) => app.download.move_transfer_selection(1),
                Some(keymap::transfers::Intent::PageUp) => app
                    .download
                    .move_transfer_selection(-(app.page_step as isize)),
                Some(keymap::transfers::Intent::PageDown) => {
                    app.download.move_transfer_selection(app.page_step as isize)
                }
                Some(keymap::transfers::Intent::First) => {
                    app.download.transfer_selected = 0;
                    app.download.transfer_offset = 0;
                }
                Some(keymap::transfers::Intent::Last) => {
                    let end = app.download.sorted_transfers().len().saturating_sub(1);
                    app.download.transfer_selected = end;
                    app.download.transfer_offset = end;
                }
                Some(keymap::transfers::Intent::All) => {
                    app.download.set_transfer_filter(TransferFilter::All)
                }
                Some(keymap::transfers::Intent::Active) => {
                    app.download.set_transfer_filter(TransferFilter::Active)
                }
                Some(keymap::transfers::Intent::Failed) => {
                    app.download.set_transfer_filter(TransferFilter::Failed)
                }
                Some(keymap::transfers::Intent::TogglePause) => {
                    let Some(job_id) = app.download.job_id else {
                        app.message = "No Job is selected".to_owned();
                        continue;
                    };
                    let result = if app.download.state == Some(DownloadJobState::Paused) {
                        paths.client.resume_job(job_id).await
                    } else {
                        paths.client.pause(job_id).await
                    };
                    match result {
                        Ok(true) => {
                            app.message = if app.download.state == Some(DownloadJobState::Paused) {
                                format!("Resuming Job {}", job_id.0)
                            } else {
                                format!("Pausing Job {}", job_id.0)
                            };
                        }
                        Ok(false) => app.message = format!("Job {} is not active", job_id.0),
                        Err(error) => {
                            app.message =
                                format!("Could not change Job {} state: {error:#}", job_id.0)
                        }
                    }
                }
                Some(keymap::transfers::Intent::Stop) => {
                    let Some(job_id) = app.download.job_id else {
                        app.message = "No Job is selected".to_owned();
                        continue;
                    };
                    match paths.client.cancel(job_id).await {
                        Ok(true) => app.message = format!("Stopping Job {}", job_id.0),
                        Ok(false) => app.message = format!("Job {} is not active", job_id.0),
                        Err(error) => {
                            app.message = format!("Could not stop Job {}: {error:#}", job_id.0)
                        }
                    }
                }
                None => {}
            }
            continue;
        }
        if app.active_tab != AppTab::Files || app.mode != BrowserMode::Browsing {
            continue;
        }

        match keymap::files::intent(key.code) {
            Some(keymap::files::Intent::Previous) => app.select_previous(),
            Some(keymap::files::Intent::Next) => app.select_next(),
            Some(keymap::files::Intent::PageUp) => app.page_up(),
            Some(keymap::files::Intent::PageDown) => app.page_down(),
            Some(keymap::files::Intent::First) => app.select_first(),
            Some(keymap::files::Intent::Last) => app.select_last(),
            Some(keymap::files::Intent::Sources) => app.open_provider_selector(),
            Some(keymap::files::Intent::Reload) => {
                if providers[active_provider].available {
                    reload_current_page(terminal, paths.client.as_ref(), &mut app, logger).await?;
                } else {
                    app.message = "The selected Provider is not ready".to_owned();
                }
            }
            Some(keymap::files::Intent::Download) if app.selected_download_target().is_some() => {
                app.request_download();
            }
            Some(keymap::files::Intent::Download) => {}
            Some(keymap::files::Intent::Parent) => {
                if app.browser_layer() == BrowserLayer::DriveFolder {
                    app.go_back();
                } else {
                    app.navigation_focus = NavigationFocus::Navigation;
                }
            }
            Some(keymap::files::Intent::Open) => match app.browser_layer() {
                BrowserLayer::SharedLinks => {
                    let Some(item) = app.selected_item().cloned() else {
                        continue;
                    };
                    if app
                        .current_source
                        .as_ref()
                        .is_some_and(|key| key.source_id == item.reference.source_id)
                    {
                        open_selected_folder(terminal, paths.client.as_ref(), &mut app, logger)
                            .await?;
                        continue;
                    }
                    let Some(source_index) = app.source_index_for_item(&item) else {
                        app.message = "Selected Source is no longer available".to_owned();
                        continue;
                    };
                    let activation = app.activate_selection(active_provider, Some(source_index));
                    if let Some(root) = activation.root_to_load {
                        load_source_root(terminal, paths.client.as_ref(), &mut app, logger, root)
                            .await?;
                    }
                }
                BrowserLayer::DriveFolder => {
                    if app.selected_item().is_some_and(RemoteItem::is_folder) {
                        open_selected_folder(terminal, paths.client.as_ref(), &mut app, logger)
                            .await?;
                    }
                }
            },
            None => {}
        }
    }
}

#[cfg(unix)]
struct TerminationSignals {
    received: std::sync::Arc<std::sync::atomic::AtomicBool>,
    listener: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl TerminationSignals {
    fn new() -> Result<Self> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tokio::signal::unix::{SignalKind, signal};

        let mut hangup = signal(SignalKind::hangup()).context("failed to listen for SIGHUP")?;
        let mut interrupt =
            signal(SignalKind::interrupt()).context("failed to listen for SIGINT")?;
        let mut terminate =
            signal(SignalKind::terminate()).context("failed to listen for SIGTERM")?;
        let received = Arc::new(AtomicBool::new(false));
        let signal_received = Arc::clone(&received);
        let listener = tokio::spawn(async move {
            tokio::select! {
                _ = hangup.recv() => {}
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            signal_received.store(true, Ordering::Release);
        });
        Ok(Self { received, listener })
    }

    fn received(&self) -> bool {
        self.received.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(unix)]
impl Drop for TerminationSignals {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

fn record_runtime_log(
    app: &mut BrowserApp,
    logger: &mut SessionLogger,
    level: LogLevel,
    target: &'static str,
    message: impl Into<String>,
) -> Result<()> {
    let entry = logger.write(level, target, message)?;
    if app.log_offset_from_bottom > 0
        && app.log_filter.matches(entry.level)
        && app.log_target_filter.matches(&entry.target)
    {
        app.log_offset_from_bottom = app.log_offset_from_bottom.saturating_add(1);
    }
    app.logs.push(entry);
    Ok(())
}

fn record_provider_refresh(
    app: &mut BrowserApp,
    logger: &mut SessionLogger,
    provider: &BrowserProvider,
) -> Result<()> {
    let identity = &provider.identity;
    if let Some(error) = &provider.source_error {
        return record_runtime_log(
            app,
            logger,
            LogLevel::Error,
            "source",
            format!(
                "refresh failed provider={} profile={} reason={error}",
                identity.provider_id, identity.profile_id
            ),
        );
    }
    if provider.available {
        return record_runtime_log(
            app,
            logger,
            LogLevel::Info,
            "source",
            format!(
                "refreshed provider={} profile={} count={}",
                identity.provider_id,
                identity.profile_id,
                provider.sources.len()
            ),
        );
    }
    let level = if matches!(
        provider.status,
        ProviderStatus::ConfigError(_) | ProviderStatus::ConnectionError(_)
    ) {
        LogLevel::Error
    } else {
        LogLevel::Info
    };
    record_runtime_log(
        app,
        logger,
        level,
        "provider",
        format!(
            "unavailable provider={} profile={} status={}",
            identity.provider_id,
            identity.profile_id,
            provider.status_label()
        ),
    )
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
        DownloadEvent::Retrying {
            job_id,
            provider,
            transfer_id,
            path,
            offset,
            attempt,
            max_attempts,
            error,
        } => Some((
            LogLevel::Info,
            format!(
                "retrying provider={} profile={} job={} transfer={} path={} offset={offset} attempt={attempt}/{max_attempts} reason={error}",
                provider.provider_id,
                provider.profile_id,
                job_id.0,
                transfer_id.0,
                app.absolute_event_path(path).display()
            ),
        )),
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
        } => {
            let all = summary
                .downloaded
                .saturating_add(summary.skipped)
                .saturating_add(summary.failed);
            let skip_reasons = if app.download.skip_reasons.is_empty() {
                "none".to_owned()
            } else {
                app.download
                    .skip_reasons
                    .iter()
                    .map(|(reason, count)| format!("{reason}={count}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            Some((
                LogLevel::Info,
                format!(
                    "finished provider={} profile={} job={} all={all} downloaded={} skipped={} failed={} skip_reasons=[{skip_reasons}] completed_bytes={} partial_bytes={}",
                    provider.provider_id,
                    provider.profile_id,
                    job_id.0,
                    summary.downloaded,
                    summary.skipped,
                    summary.failed,
                    summary.bytes_written,
                    summary.partial_bytes
                ),
            ))
        }
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
        DownloadEvent::Paused { job_id, provider } => Some((
            LogLevel::Info,
            format!(
                "paused provider={} profile={} job={} partial_files=retained",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
        DownloadEvent::Resumed { job_id, provider } => Some((
            LogLevel::Info,
            format!(
                "resumed provider={} profile={} job={} mode=range_resume",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
        DownloadEvent::Cancelled { job_id, provider } => Some((
            LogLevel::Error,
            format!(
                "stopped provider={} profile={} job={} reason=stopped_by_user partial_files=retained",
                provider.provider_id, provider.profile_id, job_id.0
            ),
        )),
    };
    if let Some((level, message)) = message {
        record_runtime_log(app, logger, level, "download", message)?;
    }
    Ok(())
}

async fn reload_current_page(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
    logger: &mut SessionLogger,
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
            let context = app
                .current_source
                .as_ref()
                .map(|key| {
                    format!(
                        "provider={} profile={} source={}",
                        key.identity.provider_id, key.identity.profile_id, key.source_id
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "provider={} profile={}",
                        app.provider_name, app.profile_name
                    )
                });
            record_runtime_log(
                app,
                logger,
                LogLevel::Info,
                "source",
                format!("reload completed {context} count={count}"),
            )?;
        }
        Err(error) => {
            let detail = format!("{error:#}");
            app.message = format!("Reload failed: {detail}");
            record_runtime_log(
                app,
                logger,
                LogLevel::Error,
                "source",
                format!("reload failed reason={detail}"),
            )?;
        }
    }
    Ok(())
}

async fn open_selected_folder(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
    logger: &mut SessionLogger,
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
        Ok(children) => {
            let count = children.len();
            record_runtime_log(
                app,
                logger,
                LogLevel::Info,
                "source",
                format!(
                    "folder loaded provider={} profile={} source={} object={} count={count}",
                    item.reference.identity.provider_id,
                    item.reference.identity.profile_id,
                    item.reference.source_id,
                    item.reference.object_id
                ),
            )?;
            app.enter_loaded(item, children);
        }
        Err(error) => {
            let detail = format!("{error:#}");
            app.message = format!("Failed to open folder: {detail}");
            record_runtime_log(
                app,
                logger,
                LogLevel::Error,
                "source",
                format!(
                    "folder load failed provider={} profile={} source={} object={} reason={detail}",
                    item.reference.identity.provider_id,
                    item.reference.identity.profile_id,
                    item.reference.source_id,
                    item.reference.object_id
                ),
            )?;
        }
    }
    Ok(())
}

async fn load_source_root(
    terminal: &mut DefaultTerminal,
    client: &dyn PanClient,
    app: &mut BrowserApp,
    logger: &mut SessionLogger,
    root: RemoteItem,
) -> Result<()> {
    terminal.draw(|frame| render_ui(frame, app))?;
    match client.list_children(&root.reference).await {
        Ok(children) => {
            let count = children.len();
            record_runtime_log(
                app,
                logger,
                LogLevel::Info,
                "source",
                format!(
                    "root loaded provider={} profile={} source={} object={} count={count}",
                    root.reference.identity.provider_id,
                    root.reference.identity.profile_id,
                    root.reference.source_id,
                    root.reference.object_id
                ),
            )?;
            app.enter_loaded(root, children);
            app.message = format!("Loaded {count} items from {}", app.source_name);
        }
        Err(error) => {
            let detail = format!("{error:#}");
            app.message = format!("Source load failed: {detail}");
            record_runtime_log(
                app,
                logger,
                LogLevel::Error,
                "source",
                format!(
                    "root load failed provider={} profile={} source={} object={} reason={detail}",
                    root.reference.identity.provider_id,
                    root.reference.identity.profile_id,
                    root.reference.source_id,
                    root.reference.object_id
                ),
            )?;
        }
    }
    Ok(())
}

fn is_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
}

pub(super) fn finish_global_settings_save(
    app: &mut BrowserApp,
    kind: ToastKind,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Some(form) = app.settings_form.as_mut() {
        form.editing = false;
        match kind {
            ToastKind::Success => form.refresh_status(),
            ToastKind::Warning => form.status = message.clone(),
        }
    }
    app.message = message.clone();
    app.show_toast(kind, message);
}

pub(super) fn settings_save_message(
    message: impl Into<String>,
    active_job_keeps_destination: bool,
) -> String {
    let message = message.into();
    if active_job_keeps_destination {
        format!("{message}; active Job keeps its original destination")
    } else {
        message
    }
}

fn is_save(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL)
}
