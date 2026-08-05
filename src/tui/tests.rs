use ratatui::{Terminal, backend::TestBackend};

use super::*;
use crate::job::DownloadJobRequest;
use crate::provider::{
    DownloadCapability, ProfileId, ProviderId, ProviderIdentity, ProviderSettingSpec,
    RemoteItemKind, RemoteObjectId, RemoteRef, SourceId,
};

fn event_provider() -> ProviderIdentity {
    ProviderIdentity::new(
        ProviderId::new("test").unwrap(),
        ProfileId::new("default").unwrap(),
    )
}

fn item(name: &str, folder: bool) -> RemoteItem {
    RemoteItem {
        reference: RemoteRef {
            identity: event_provider(),
            source_id: SourceId::new("source").unwrap(),
            object_id: RemoteObjectId::new(format!("id-{name}-123456789")).unwrap(),
        },
        name: name.to_owned(),
        kind: if folder {
            RemoteItemKind::Folder
        } else {
            RemoteItemKind::File
        },
        size: (!folder).then_some(1024),
        checksum: None,
        download: DownloadCapability::Supported { resume: true },
    }
}

fn app_with(shared_items: Vec<RemoteItem>) -> BrowserApp {
    BrowserApp::new(
        shared_items,
        PathBuf::from("/home/tester/Downloads"),
        NetworkSettings::default(),
    )
}

fn rendered(app: &mut BrowserApp) -> String {
    rendered_at(app, 100, 28)
}

fn rendered_at(app: &mut BrowserApp, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render_ui(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn enters_and_leaves_one_level_at_a_time() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    assert_eq!(app.browser_layer(), BrowserLayer::SharedLinks);
    app.enter_loaded(child, vec![item("file.txt", false)]);
    assert_eq!(app.browser_layer(), BrowserLayer::DriveFolder);
    assert_eq!(app.breadcrumbs(), "Shared links / child");
    assert_eq!(app.current().items.len(), 1);
    let output = rendered(&mut app);
    assert!(output.contains("r reload"));
    assert!(output.contains("d download"));
    app.go_back();
    assert_eq!(app.breadcrumbs(), "Shared links");
}

#[test]
fn moves_selection_and_opens_confirmation() {
    let mut app = app_with(vec![item("a", true), item("b", true)]);
    app.select_next();
    assert_eq!(app.selected_item().unwrap().name, "b");
    app.select_next();
    assert_eq!(app.selected_item().unwrap().name, "a");
    let folder = app.selected_item().unwrap().clone();
    app.enter_loaded(folder, vec![item("download-me", true)]);
    app.request_download();
    assert_eq!(app.mode, BrowserMode::Confirming);
    assert_eq!(app.download_target().unwrap().name, "download-me");
    app.cancel_download();
    assert_eq!(app.mode, BrowserMode::Browsing);
    assert!(app.download_target().is_none());
}

#[test]
fn shared_root_shows_its_own_key_context() {
    let mut app = app_with(vec![item("child", true), item("readme", false)]);
    let output = rendered(&mut app);
    assert_eq!(app.mode, BrowserMode::Browsing);
    assert_eq!(app.message, "Ready");
    assert!(output.contains("Shared links"));
    assert!(output.contains("2 saved links"));
    assert!(output.contains("Remote Files"));
    assert!(output.contains("[DIR ] child/"));
    assert!(output.contains("r reload"));
    assert!(output.contains("d download"));
    assert!(!output.contains("Backspace"));
    assert!(!output.contains("Right"));
    assert!(!output.contains("Confirm download"));
}

#[test]
fn shared_root_download_targets_highlighted_link() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.request_download();
    let output = rendered(&mut app);
    assert_eq!(app.mode, BrowserMode::Confirming);
    assert_eq!(app.download_target().unwrap().name, "cp_result");
    assert!(output.contains("Download form"));
    assert!(output.contains("Target type: recursive folder"));
    assert!(output.contains("File workers: [1]"));
    assert!(output.contains("Shared links / cp_result"));
    assert!(output.contains("/home/tester/Downloads/cp_result"));
}

#[test]
fn reload_replacement_preserves_selection_by_drive_id() {
    let a = item("a", true);
    let b = item("b", true);
    let mut app = app_with(vec![a, b.clone()]);
    app.select_next();
    assert_eq!(
        app.selected_item().unwrap().reference.object_id,
        b.reference.object_id
    );

    app.replace_current_items(vec![item("c", true), b.clone()]);
    assert_eq!(
        app.selected_item().unwrap().reference.object_id,
        b.reference.object_id
    );
    assert_eq!(app.current().selected, Some(1));

    app.replace_current_items(Vec::new());
    assert_eq!(app.current().selected, None);
}

#[test]
fn renders_confirmation_modal_for_highlighted_folder() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![item("nested", true)]);
    app.request_download();
    let output = rendered(&mut app);
    assert!(output.contains("Download form"));
    assert!(output.contains("Target type: recursive folder"));
    assert!(output.contains("Shared links / child / nested"));
    assert!(output.contains("/home/tester/Downloads/child/nested"));
}

#[test]
fn regular_file_can_be_selected_for_download() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![item("model.bin", false)]);
    let browsing = rendered(&mut app);
    assert!(browsing.contains("d download"));
    assert!(!browsing.contains("Enter open"));

    app.request_download();
    let confirmation = rendered(&mut app);
    assert_eq!(app.download_target().unwrap().name, "model.bin");
    assert!(confirmation.contains("Target type: single file"));
    assert!(confirmation.contains("/home/tester/Downloads/child/model.bin"));
}

#[test]
fn unsupported_item_and_empty_level_hide_download_key() {
    let child = item("child", true);
    let mut native = item("notes", false);
    native.download = DownloadCapability::Unsupported {
        reason: "export required".to_owned(),
    };
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![native]);
    assert!(!rendered(&mut app).contains("d download"));
    app.request_download();
    assert_eq!(app.mode, BrowserMode::Browsing);

    app.replace_current_items(Vec::new());
    assert!(!rendered(&mut app).contains("d download"));
}

#[test]
fn renders_multiple_concurrent_transfers_without_overwriting_rows() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![item("selected", true)]);
    app.request_download();
    app.begin_download();
    app.apply_download_event(DownloadEvent::PlanReady {
        job_id: JobId(1),
        provider: event_provider(),
        files: 2,
        items: 2,
        bytes: 100,
        pre_skipped: 0,
        workers: 2,
    });
    for (id, path, size) in [(1, "root/model.bin", 40), (2, "root/data.bin", 60)] {
        app.apply_download_event(DownloadEvent::TransferQueued {
            job_id: JobId(1),
            provider: event_provider(),
            transfer_id: TransferId(id),
            path: PathBuf::from(path),
            size: Some(size),
        });
    }
    app.apply_download_event(DownloadEvent::FileStarted {
        job_id: JobId(1),
        provider: event_provider(),
        transfer_id: TransferId(1),
        index: 1,
        total: 2,
        path: PathBuf::from("root/model.bin"),
        size: Some(40),
    });
    app.apply_download_event(DownloadEvent::FileStarted {
        job_id: JobId(1),
        provider: event_provider(),
        transfer_id: TransferId(2),
        index: 2,
        total: 2,
        path: PathBuf::from("root/data.bin"),
        size: Some(60),
    });
    app.apply_download_event(DownloadEvent::Progress {
        job_id: JobId(1),
        provider: event_provider(),
        transfer_id: TransferId(1),
        index: 1,
        total: 2,
        path: PathBuf::from("root/model.bin"),
        file_written: 20,
        file_total: Some(40),
        bytes_per_second: 2048.0,
    });
    app.apply_download_event(DownloadEvent::Progress {
        job_id: JobId(1),
        provider: event_provider(),
        transfer_id: TransferId(2),
        index: 2,
        total: 2,
        path: PathBuf::from("root/data.bin"),
        file_written: 15,
        file_total: Some(60),
        bytes_per_second: 1024.0,
    });
    let output = rendered(&mut app);
    assert!(output.contains("2 running / 0 queued"));
    assert!(output.contains("Total 3.0 KiB/s"));
    assert!(output.contains("root/model.bin"));
    assert!(output.contains("root/data.bin"));
    assert!(output.contains("50%"));
    assert!(output.contains("25%"));
}

#[test]
fn daemon_snapshots_render_parallel_rows_and_do_not_regress_terminal_jobs() {
    use crate::job::TransferSnapshot;

    let mut app = app_with(vec![item("selected", false)]);
    let request = DownloadJobRequest::new(
        item("selected", false),
        PathBuf::from("/home/tester/Downloads"),
        DownloadOptions {
            overwrite: false,
            resume: true,
            concurrency: 3,
        },
    );
    let running = DownloadJobSnapshot {
        id: JobId(40),
        request: request.clone(),
        state: DownloadJobState::Running,
        transfers: BTreeMap::from([
            (
                TransferId(1),
                TransferSnapshot {
                    id: TransferId(1),
                    path: PathBuf::from("root/running.bin"),
                    state: TransferState::Running,
                    size: Some(10),
                    written: 4,
                    bytes_per_second: 2048.0,
                    error: None,
                },
            ),
            (
                TransferId(2),
                TransferSnapshot {
                    id: TransferId(2),
                    path: PathBuf::from("root/queued.bin"),
                    state: TransferState::Queued,
                    size: Some(20),
                    written: 0,
                    bytes_per_second: 0.0,
                    error: None,
                },
            ),
            (
                TransferId(3),
                TransferSnapshot {
                    id: TransferId(3),
                    path: PathBuf::from("root/failed.bin"),
                    state: TransferState::Failed,
                    size: Some(30),
                    written: 6,
                    bytes_per_second: 0.0,
                    error: Some("network".to_owned()),
                },
            ),
        ]),
        summary: None,
        error: None,
    };
    app.apply_download_snapshots(std::slice::from_ref(&running));
    assert_eq!(app.mode, BrowserMode::Downloading);
    assert_eq!(app.download.transfers.len(), 3);
    let output = rendered(&mut app);
    assert!(output.contains("1 running / 1 queued / 0 done / 1 failed"));
    assert!(output.contains("root/running.bin"));
    assert!(output.contains("root/queued.bin"));
    assert!(output.contains("root/failed.bin"));

    let mut finished = running.clone();
    finished.state = DownloadJobState::Finished;
    finished.transfers.get_mut(&TransferId(1)).unwrap().state = TransferState::Completed;
    finished.transfers.get_mut(&TransferId(1)).unwrap().written = 10;
    finished.summary = Some(DownloadSummary {
        downloaded: 1,
        skipped: 0,
        failed: 1,
        bytes_written: 10,
        partial_bytes: 6,
    });
    app.apply_download_snapshots(&[finished]);
    assert_eq!(app.mode, BrowserMode::Finished);
    assert_eq!(
        app.download.transfers[&TransferId(1)].status,
        TransferState::Completed
    );

    app.apply_download_snapshots(&[running]);
    assert_eq!(app.mode, BrowserMode::Finished);
    assert_eq!(app.download.state, Some(DownloadJobState::Finished));
    assert_eq!(
        app.download.transfers[&TransferId(1)].status,
        TransferState::Completed
    );
}

#[test]
fn every_terminal_file_outcome_advances_overall_count() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![item("selected", true)]);
    app.request_download();
    app.begin_download();
    app.apply_download_event(DownloadEvent::PlanReady {
        job_id: JobId(2),
        provider: event_provider(),
        files: 3,
        items: 3,
        bytes: 30,
        pre_skipped: 0,
        workers: 3,
    });
    app.apply_download_event(DownloadEvent::FileSkipped {
        job_id: JobId(2),
        provider: event_provider(),
        transfer_id: TransferId(1),
        path: PathBuf::from("one"),
        reason: "target exists".to_owned(),
    });
    app.apply_download_event(DownloadEvent::FileCompleted {
        job_id: JobId(2),
        provider: event_provider(),
        transfer_id: TransferId(2),
        path: PathBuf::from("two"),
        bytes: 10,
    });
    app.apply_download_event(DownloadEvent::FileFailed {
        job_id: JobId(2),
        provider: event_provider(),
        transfer_id: TransferId(3),
        path: PathBuf::from("three"),
        bytes: 5,
        error: "network".to_owned(),
    });
    assert_eq!(app.download.items_processed, 3);
    assert_eq!(app.download.items_total, 3);
    assert_eq!(app.download.overall_written, 15);
    assert_eq!(app.download.partial_bytes, 5);
    let output = rendered(&mut app);
    assert!(output.contains("0 running / 0 queued / 1 done / 1 failed"));
    assert!(output.contains("SKIP"));
}

#[test]
fn terminal_transfer_state_ignores_late_progress_and_duplicate_outcomes() {
    let mut app = app_with(vec![item("selected", false)]);
    app.apply_download_event(DownloadEvent::PlanReady {
        job_id: JobId(3),
        provider: event_provider(),
        files: 1,
        items: 1,
        bytes: 10,
        pre_skipped: 0,
        workers: 1,
    });
    app.apply_download_event(DownloadEvent::FileCompleted {
        job_id: JobId(3),
        provider: event_provider(),
        transfer_id: TransferId(1),
        path: PathBuf::from("done.bin"),
        bytes: 10,
    });
    app.apply_download_event(DownloadEvent::Progress {
        job_id: JobId(3),
        provider: event_provider(),
        transfer_id: TransferId(1),
        index: 1,
        total: 1,
        path: PathBuf::from("done.bin"),
        file_written: 5,
        file_total: Some(10),
        bytes_per_second: 999.0,
    });
    app.apply_download_event(DownloadEvent::FileFailed {
        job_id: JobId(3),
        provider: event_provider(),
        transfer_id: TransferId(1),
        path: PathBuf::from("done.bin"),
        bytes: 5,
        error: "late failure".to_owned(),
    });

    let transfer = app.download.transfers.get(&TransferId(1)).unwrap();
    assert_eq!(transfer.status, TransferState::Completed);
    assert_eq!(transfer.written, 10);
    assert_eq!(app.download.downloaded, 1);
    assert_eq!(app.download.failed, 0);
    assert_eq!(app.download.items_processed, 1);
}

#[test]
fn transfer_sorting_and_filters_are_stable() {
    let mut app = app_with(Vec::new());
    app.mode = BrowserMode::Downloading;
    for (id, state) in [
        (5, TransferState::Completed),
        (3, TransferState::Queued),
        (1, TransferState::Running),
        (4, TransferState::Skipped),
        (2, TransferState::Failed),
        (6, TransferState::Completed),
    ] {
        let mut transfer =
            TransferDisplay::queued(TransferId(id), PathBuf::from(format!("{id}.bin")), None);
        transfer.status = state;
        app.download.transfers.insert(transfer.id, transfer);
    }
    let ids: Vec<_> = app
        .download
        .sorted_transfers()
        .iter()
        .map(|transfer| transfer.id.0)
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 6, 5]);

    app.download.set_transfer_filter(TransferFilter::Active);
    let active_ids: Vec<_> = app
        .download
        .sorted_transfers()
        .iter()
        .map(|transfer| transfer.id.0)
        .collect();
    assert_eq!(active_ids, vec![1, 3]);
    let output = rendered(&mut app);
    assert!(output.contains("Transfers [Active]"));
    assert!(output.contains("1.bin"));
    assert!(output.contains("3.bin"));
    assert!(!output.contains("2.bin"));
}

#[test]
fn log_tab_filters_levels_and_preserves_scroll_position() {
    let mut app = app_with(vec![item("child", true)]);
    app.logs.push(LogEntry {
        timestamp: chrono::Local::now(),
        level: LogLevel::Info,
        target: "download".to_owned(),
        message: "completed path=one".to_owned(),
    });
    app.logs.push(LogEntry {
        timestamp: chrono::Local::now(),
        level: LogLevel::Error,
        target: "download".to_owned(),
        message: "failed path=two".to_owned(),
    });
    app.switch_tab(true);
    assert_eq!(app.active_tab, AppTab::Logs);
    assert_eq!(app.log_offset_from_bottom, 0);
    let all = rendered(&mut app);
    assert!(all.contains("Download logs [All] (2)"));
    assert!(all.contains("completed path=one"));
    assert!(all.contains("failed path=two"));
    assert!(all.contains("a/i/e filter"));
    assert!(!all.contains("d download"));

    app.handle_log_key(&KeyCode::Char('e'));
    let errors = rendered(&mut app);
    assert!(errors.contains("Download logs [Error] (1)"));
    assert!(!errors.contains("completed path=one"));
    assert!(errors.contains("failed path=two"));

    app.log_offset_from_bottom = 1;
    app.switch_tab(false);
    app.switch_tab(true);
    assert_eq!(app.log_offset_from_bottom, 1);

    app.log_offset_from_bottom = 0;
    app.switch_tab(false);
    app.switch_tab(true);
    assert_eq!(app.log_offset_from_bottom, 0);
}

#[test]
fn tabs_and_panels_keep_independent_focus() {
    let mut app = app_with(vec![item("child", true)]);
    assert_eq!(app.active_tab, AppTab::Files);
    assert_eq!(app.focused_panel(), PanelFocus::Upper);
    app.switch_panel();
    assert_eq!(app.files_focus, PanelFocus::Lower);
    app.switch_tab(true);
    assert_eq!(app.active_tab, AppTab::Logs);
    assert_eq!(app.focused_panel(), PanelFocus::Upper);
    app.switch_panel();
    app.switch_tab(false);
    assert_eq!(app.active_tab, AppTab::Files);
    assert_eq!(app.focused_panel(), PanelFocus::Lower);
    app.switch_tab(true);
    assert_eq!(app.focused_panel(), PanelFocus::Lower);
}

#[test]
fn provider_selector_restores_provider_session_without_changing_tabs() {
    let google = ProviderIdentity::new(
        ProviderId::new("google").unwrap(),
        ProfileId::new("default").unwrap(),
    );
    let aliyun = ProviderIdentity::new(
        ProviderId::new("aliyun").unwrap(),
        ProfileId::new("default").unwrap(),
    );
    let mut google_root = item("google-root", true);
    google_root.reference.identity = google.clone();
    google_root.reference.source_id = SourceId::new("google-source").unwrap();
    let mut aliyun_root = item("aliyun-root", true);
    aliyun_root.reference.identity = aliyun.clone();
    aliyun_root.reference.source_id = SourceId::new("aliyun-source").unwrap();
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![
        ProviderChoice {
            id: google.provider_id.clone(),
            name: "Google Drive".to_owned(),
            profile: google.profile_id.clone(),
            status: "Ready".to_owned(),
            settings_count: 3,
            sources: vec![SourceChoice {
                id: google_root.reference.source_id.clone(),
                name: "Google Source".to_owned(),
                root: google_root.clone(),
            }],
        },
        ProviderChoice {
            id: aliyun.provider_id.clone(),
            name: "Aliyun Drive".to_owned(),
            profile: aliyun.profile_id.clone(),
            status: "Ready".to_owned(),
            settings_count: 5,
            sources: vec![SourceChoice {
                id: aliyun_root.reference.source_id.clone(),
                name: "Aliyun Source".to_owned(),
                root: aliyun_root.clone(),
            }],
        },
    ];
    let google_activation = app.activate_selection(0, Some(0));
    assert!(google_activation.root_to_load.is_some());
    app.enter_loaded(google_root, vec![item("google-folder", true)]);
    app.open_provider_selector();
    app.move_provider_selection(1);
    let selector = rendered(&mut app);
    assert!(selector.contains("Aliyun Drive"));
    assert!(selector.contains("Aliyun Source"));
    assert_eq!(app.active_tab, AppTab::Files);

    let aliyun_activation = app.activate_selection(1, Some(0));
    assert!(!aliyun_activation.restored);
    app.enter_loaded(aliyun_root, vec![item("aliyun-folder", true)]);

    assert!(app.activate_selection(0, Some(0)).restored);
    assert!(app.breadcrumbs().contains("google-root"));
    assert_eq!(app.provider_name, "Google Drive");
    assert_eq!(app.active_tab, AppTab::Files);
    assert_eq!(app.files_focus, PanelFocus::Upper);
}

#[test]
fn source_sessions_preserve_selection_and_error_state_independently() {
    let identity = ProviderIdentity::new(
        ProviderId::new("test").unwrap(),
        ProfileId::new("default").unwrap(),
    );
    let mut root_a = item("source-a", true);
    root_a.reference.identity = identity.clone();
    root_a.reference.source_id = SourceId::new("a").unwrap();
    let mut root_b = item("source-b", true);
    root_b.reference.identity = identity.clone();
    root_b.reference.source_id = SourceId::new("b").unwrap();
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![ProviderChoice {
        id: identity.provider_id.clone(),
        name: "Test Drive".to_owned(),
        profile: identity.profile_id.clone(),
        status: "Ready".to_owned(),
        settings_count: 0,
        sources: vec![
            SourceChoice {
                id: root_a.reference.source_id.clone(),
                name: "Source A".to_owned(),
                root: root_a.clone(),
            },
            SourceChoice {
                id: root_b.reference.source_id.clone(),
                name: "Source B".to_owned(),
                root: root_b.clone(),
            },
        ],
    }];
    app.activate_selection(0, Some(0));
    app.enter_loaded(root_a, vec![item("one", false), item("two", false)]);
    app.select_next();
    app.current_mut().offset = 1;
    app.message = "Source A failed at marker next-2".to_owned();

    app.activate_selection(0, Some(1));
    app.enter_loaded(root_b, vec![item("other", false)]);
    app.message = "Source B ready".to_owned();

    let restored = app.activate_selection(0, Some(0));
    assert!(restored.restored);
    assert_eq!(app.source_name, "Source A");
    assert_eq!(app.selected_item().unwrap().name, "two");
    assert_eq!(app.current().offset, 1);
    assert_eq!(app.message, "Source A failed at marker next-2");
}

#[test]
fn needs_config_provider_opens_settings_without_a_ready_instance() {
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![ProviderChoice {
        id: ProviderId::new("aliyun").unwrap(),
        name: "Aliyun Drive".to_owned(),
        profile: ProfileId::new("default").unwrap(),
        status: "NeedsConfig".to_owned(),
        settings_count: 9,
        sources: Vec::new(),
    }];
    app.activate_selection(0, None);
    assert_eq!(app.active_tab, AppTab::Settings);
    let output = rendered(&mut app);
    assert!(output.contains("Aliyun Drive"));
    assert!(output.contains("NeedsConfig"));
    assert!(output.contains("Provider settings are unavailable"));
}

#[test]
fn provider_secret_fields_are_masked_and_blank_keeps_existing_value() {
    let schema = vec![ProviderSettingSpec {
        key: "refresh_token",
        label: "Refresh token",
        kind: ProviderSettingKind::Secret,
        required: true,
    }];
    let snapshot = ProviderSettingsSnapshot {
        values: BTreeMap::from([("refresh_token".to_owned(), String::new())]),
        configured_secrets: std::collections::BTreeSet::from(["refresh_token".to_owned()]),
        read_only_reason: None,
    };
    let mut form = ProviderSettingsForm::from_snapshot(schema, &snapshot);
    assert_eq!(
        ProviderSettingsForm::display_value(form.selected_field().unwrap(), false),
        "********"
    );
    assert!(form.changes().is_empty());
    form.editing = true;
    for character in "new-secret".chars() {
        form.handle_edit_key(KeyCode::Char(character));
    }
    let displayed = ProviderSettingsForm::display_value(form.selected_field().unwrap(), true);
    assert!(!displayed.contains("new-secret"));
    assert_eq!(
        form.changes().get("refresh_token").map(String::as_str),
        Some("new-secret")
    );
}

#[test]
fn narrow_terminal_renders_files_selector_and_settings() {
    let mut app = app_with(vec![item(
        "a-very-long-source-folder-name-that-must-be-clipped",
        true,
    )]);
    let files = rendered_at(&mut app, 60, 20);
    assert!(files.contains("Pan TUI"));
    assert!(files.contains("Remote Files"));

    let root = item("root", true);
    app.provider_choices = vec![ProviderChoice {
        id: ProviderId::new("test").unwrap(),
        name: "Test Drive With A Long Display Name".to_owned(),
        profile: ProfileId::new("default").unwrap(),
        status: "ConnectionError: intentionally long test error".to_owned(),
        settings_count: 1,
        sources: vec![SourceChoice {
            id: SourceId::new("source").unwrap(),
            name: "A Long Source Name".to_owned(),
            root,
        }],
    }];
    app.open_provider_selector();
    let selector = rendered_at(&mut app, 60, 20);
    assert!(selector.contains("Provider / Profile / Source"));
    assert!(selector.contains("A Long Source Name"));

    app.provider_selector = None;
    app.active_tab = AppTab::Settings;
    app.provider_name = "Test Drive".to_owned();
    app.settings_form = Some(SettingsForm::new(&NetworkSettings::default()));
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "endpoint",
            label: "Endpoint",
            kind: ProviderSettingKind::Text,
            required: true,
        }],
        &ProviderSettingsSnapshot {
            values: BTreeMap::from([(
                "endpoint".to_owned(),
                "https://example.invalid/a/long/path".to_owned(),
            )]),
            ..ProviderSettingsSnapshot::default()
        },
    ));
    let settings = rendered_at(&mut app, 60, 20);
    assert!(settings.contains("Global settings"));
    assert!(settings.contains("Test Drive"));
}

#[test]
fn download_form_edits_root_and_clamps_workers() {
    let child = item("cp_result", true);
    let mut app = app_with(vec![child]);
    app.request_download();
    {
        let form = app.download_form.as_mut().unwrap();
        form.destination_root = "/data/archive".to_owned();
        form.cursor = form.destination_root.chars().count();
        form.field = FormField::Workers;
    }
    for _ in 0..10 {
        app.handle_form_edit_key(KeyCode::Right);
    }
    assert_eq!(app.download_form.as_ref().unwrap().workers, 5);
    assert_eq!(
        app.download_target_destination(),
        PathBuf::from("/data/archive/cp_result")
    );
    for _ in 0..10 {
        app.handle_form_edit_key(KeyCode::Left);
    }
    assert_eq!(app.download_form.as_ref().unwrap().workers, 1);
}

#[test]
fn settings_tab_edits_proxy_and_returns_to_previous_tab() {
    let mut app = app_with(vec![item("child", true)]);
    app.active_tab = AppTab::Logs;
    app.switch_tab(true);
    app.settings_form.as_mut().unwrap().editing = true;
    for character in "http://127.0.0.1:7890".chars() {
        app.handle_settings_edit_key(KeyCode::Char(character));
    }
    let output = rendered(&mut app);
    assert!(output.contains("Global settings [editing]"));
    assert!(output.contains("http://127.0.0.1:7890"));
    assert!(output.contains("system proxy variables are ignored"));
    assert!(output.contains("Provider Settings"));
    assert!(!output.contains("d download"));

    app.switch_tab(false);
    assert_eq!(app.active_tab, AppTab::Logs);
}

#[test]
fn exit_confirmation_overlays_and_preserves_underlying_state() {
    let mut app = app_with(vec![item("child", true)]);
    app.switch_tab(true);
    app.log_offset_from_bottom = 3;
    app.quit_confirming = true;
    let output = rendered(&mut app);
    assert!(output.contains("Confirm exit"));
    assert!(output.contains("Downloads stop; .part files are kept."));
    assert!(output.contains("y/Enter exit | n/Esc return"));
    app.daemon_mode = true;
    let daemon_output = rendered(&mut app);
    assert!(daemon_output.contains("Downloads continue in the daemon."));
    app.quit_confirming = false;
    assert_eq!(app.active_tab, AppTab::Logs);
    assert_eq!(app.log_offset_from_bottom, 3);
}

#[test]
fn persists_success_skip_and_failure_with_required_levels() {
    let child = item("cp_result", true);
    let mut app = app_with(vec![child.clone()]);
    app.enter_loaded(child, vec![item("CUHK_LOWER", true)]);
    app.request_download();
    let temporary = tempfile::tempdir().unwrap();
    let mut logger = SessionLogger::new(temporary.path().to_path_buf(), 1024 * 1024).unwrap();

    record_download_event(
        &mut app,
        &DownloadEvent::FileCompleted {
            job_id: JobId(7),
            provider: event_provider(),
            transfer_id: TransferId(1),
            path: PathBuf::from("CUHK_LOWER/ok.bin"),
            bytes: 10,
        },
        &mut logger,
    )
    .unwrap();
    record_download_event(
        &mut app,
        &DownloadEvent::FileSkipped {
            job_id: JobId(7),
            provider: event_provider(),
            transfer_id: TransferId(2),
            path: PathBuf::from("CUHK_LOWER/existing.bin"),
            reason: "target exists".to_owned(),
        },
        &mut logger,
    )
    .unwrap();
    app.log_offset_from_bottom = 1;
    record_download_event(
        &mut app,
        &DownloadEvent::FileFailed {
            job_id: JobId(7),
            provider: event_provider(),
            transfer_id: TransferId(3),
            path: PathBuf::from("CUHK_LOWER/broken.bin"),
            bytes: 5,
            error: "network".to_owned(),
        },
        &mut logger,
    )
    .unwrap();

    assert_eq!(app.logs[0].level, LogLevel::Info);
    assert_eq!(app.logs[1].level, LogLevel::Info);
    assert_eq!(app.logs[2].level, LogLevel::Error);
    assert_eq!(app.log_offset_from_bottom, 2);
    let content = std::fs::read_to_string(logger.current_path()).unwrap();
    assert!(content.contains(
            "completed provider=test profile=default job=7 transfer=1 path=/home/tester/Downloads/cp_result/CUHK_LOWER/ok.bin"
        ));
    assert!(content.contains(
            "skipped provider=test profile=default job=7 transfer=2 path=/home/tester/Downloads/cp_result/CUHK_LOWER/existing.bin"
        ));
    assert!(content.contains(
        " | ERROR    | download - failed provider=test profile=default job=7 transfer=3 path="
    ));
    assert!(content.contains("partial_bytes=5 reason=network"));
}
