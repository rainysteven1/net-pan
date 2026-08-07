use ratatui::{Terminal, backend::TestBackend, style::Color};

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

fn editable_provider_form() -> ProviderSettingsForm {
    ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "enabled",
            label: "Enabled",
            description: "Enable this Provider.",
            kind: ProviderSettingKind::Boolean,
            required: true,
        }],
        &ProviderSettingsSnapshot::default(),
    )
}

fn rendered(app: &mut BrowserApp) -> String {
    rendered_at(app, 100, 28)
}

fn rendered_at(app: &mut BrowserApp, width: u16, height: u16) -> String {
    rendered_lines_at(app, width, height).concat()
}

fn rendered_lines_at(app: &mut BrowserApp, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render_ui(frame, app)).unwrap();
    let cells: Vec<_> = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    cells
        .chunks(width as usize)
        .map(|row| row.concat())
        .collect()
}

fn rendered_colors_at(app: &mut BrowserApp, width: u16, height: u16) -> Vec<(Color, Color)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render_ui(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| (cell.fg, cell.bg))
        .collect()
}

fn rendered_footer_at(app: &mut BrowserApp, width: u16, height: u16) -> String {
    let footer_height = super::render::footer_height(app, width) as usize;
    rendered_lines_at(app, width, height)
        .into_iter()
        .rev()
        .take(footer_height)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[test]
fn enters_and_leaves_one_level_at_a_time() {
    let child = item("child", true);
    let mut app = app_with(vec![child.clone()]);
    assert_eq!(app.browser_layer(), BrowserLayer::SharedLinks);
    let grandchild = item("grandchild", true);
    app.enter_loaded(child, vec![grandchild.clone()]);
    assert_eq!(app.browser_layer(), BrowserLayer::DriveFolder);
    assert_eq!(app.breadcrumbs(), "child");
    assert_eq!(app.current().items.len(), 1);

    app.enter_loaded(grandchild, vec![item("file.txt", false)]);
    assert_eq!(app.breadcrumbs(), "child / grandchild");
    let output = rendered_at(&mut app, 80, 24);
    assert!(output.contains("Backspace parent"));
    assert!(output.contains("Left nav"));

    app.go_back();
    assert_eq!(app.breadcrumbs(), "child");
    app.go_back();
    assert_eq!(app.breadcrumbs(), "");
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
    assert!(output.contains("Sources"));
    assert!(output.contains("2 sources"));
    assert!(output.contains("[LINK] child/"));
    assert!(output.contains("r reload"));
    assert!(output.contains("d download"));
    assert!(!output.contains("Backspace"));
    assert!(!output.contains("parent"));
    assert!(!output.contains("Right"));
    assert!(!output.contains("Confirm download"));
}

#[test]
fn files_and_directories_use_the_same_foreground_color() {
    let theme = super::theme::for_provider(Some(&ProviderId::new("google").unwrap()));
    let directory = item("folder", true);
    let file = item("file.bin", false);
    let (directory_prefix, directory_style) =
        super::render::file_item_style(&directory, false, &theme);
    let (file_prefix, file_style) = super::render::file_item_style(&file, false, &theme);

    assert_eq!(directory_prefix, "[DIR ]");
    assert_eq!(file_prefix, "[FILE]");
    assert_eq!(directory_style.fg, file_style.fg);
    assert_eq!(directory_style.fg, None);
    assert_eq!(file_style.fg, None);
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
    assert!(output.contains("Breadcrumb: cp_result"));
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
    assert!(output.contains("Breadcrumb: child / nested"));
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
    app.active_tab = AppTab::Transfers;
    let output = rendered(&mut app);
    assert!(output.contains("R2 P0 Q0 D0 F0"));
    assert!(output.contains("3.0 KB/s"));
    assert!(output.contains("STATE"));
    assert!(output.contains("PROGRESS"));
    assert!(output.contains("root/model.bin"));
    assert!(output.contains("root/data.bin"));
    assert!(output.contains("50%"));
    assert!(output.contains("25%"));
    assert!(output.contains("20B/40B"), "{output}");
    assert!(output.contains('['));
    assert!(output.contains('#'));
    let hints = super::keymap::page_hints(&app);
    assert!(
        hints
            .iter()
            .any(|hint| hint.key == "Space" && hint.action == "pause/resume")
    );
    assert!(
        hints
            .iter()
            .any(|hint| hint.key == "x" && hint.action == "stop")
    );
}

#[test]
fn daemon_snapshots_render_parallel_rows_and_do_not_regress_terminal_jobs() {
    use crate::job::TransferSnapshot;

    let mut app = app_with(vec![item("selected", false)]);
    app.destination = PathBuf::from("/mnt/new-download-root");
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
    assert_eq!(
        app.download_target_destination(),
        PathBuf::from("/home/tester/Downloads/selected")
    );
    app.active_tab = AppTab::Transfers;
    let output = rendered(&mut app);
    assert!(output.contains("R1 P0 Q1 D0 F1"));
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
    app.active_tab = AppTab::Transfers;
    let output = rendered(&mut app);
    assert!(output.contains("R0 P0 Q0 D1 F1"));
    assert!(output.contains("SKIP"));

    app.apply_download_event(DownloadEvent::Finished {
        job_id: JobId(2),
        provider: event_provider(),
        summary: DownloadSummary {
            downloaded: 1,
            skipped: 1,
            failed: 1,
            bytes_written: 10,
            partial_bytes: 5,
        },
    });
    let complete = rendered(&mut app);
    assert!(complete.contains("All: 3 | Downloaded: 1 | Skipped: 1 | Failed: 1"));
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
        (7, TransferState::Paused),
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
    assert_eq!(ids, vec![1, 7, 2, 3, 4, 6, 5]);

    app.download.set_transfer_filter(TransferFilter::Active);
    let active_ids: Vec<_> = app
        .download
        .sorted_transfers()
        .iter()
        .map(|transfer| transfer.id.0)
        .collect();
    assert_eq!(active_ids, vec![1, 7, 3]);
    app.active_tab = AppTab::Transfers;
    let output = rendered(&mut app);
    assert!(output.contains("Transfers [Active]"));
    assert!(output.contains("1.bin"));
    assert!(output.contains("3.bin"));
    assert!(output.contains("7.bin"));
    assert!(!output.contains("2.bin"));
}

#[test]
fn failed_transfer_keeps_progress_in_the_row_and_full_error_in_detail() {
    let mut app = app_with(Vec::new());
    app.mode = BrowserMode::Downloading;
    app.active_tab = AppTab::Transfers;
    app.download.job_id = Some(JobId(3));
    app.download.state = Some(DownloadJobState::Running);
    let mut transfer =
        TransferDisplay::queued(TransferId(1), PathBuf::from("LiDAR.ply"), Some(200_000_000));
    transfer.status = TransferState::Failed;
    transfer.written = 146_931_712;
    transfer.detail = Some(
        "provider download stream failed after 4/4 retries at byte offset 146931712: error decoding response body: unexpected EOF"
            .to_owned(),
    );
    app.download.transfers.insert(transfer.id, transfer);

    let output = rendered_at(&mut app, 100, 28);
    assert!(output.contains("FAIL"));
    assert!(output.contains('%'));
    assert!(output.contains("140.1MB/190.7MB"), "{output}");
    assert!(output.contains("DETAIL"));
    assert!(output.contains("after 4/4 retries"));
    assert!(output.contains("unexpected EOF"));
}

#[test]
fn paused_and_stopped_transfers_have_distinct_states_and_failure_filtering() {
    let mut app = app_with(Vec::new());
    app.active_tab = AppTab::Transfers;
    app.download.job_id = Some(JobId(9));
    app.download.state = Some(DownloadJobState::Paused);
    let mut running = TransferDisplay::queued(TransferId(1), PathBuf::from("large.bin"), Some(100));
    running.status = TransferState::Running;
    running.written = 40;
    app.download.transfers.insert(running.id, running);
    app.apply_download_event(DownloadEvent::Paused {
        job_id: JobId(9),
        provider: event_provider(),
    });
    assert_eq!(app.mode, BrowserMode::Paused);
    assert_eq!(
        app.download.transfers[&TransferId(1)].status,
        TransferState::Paused
    );
    let hints = super::keymap::page_hints(&app);
    assert!(hints.iter().any(|hint| hint.key == "Space"));
    assert!(hints.iter().any(|hint| hint.key == "x"));

    app.apply_download_event(DownloadEvent::Cancelled {
        job_id: JobId(9),
        provider: event_provider(),
    });
    assert_eq!(app.download.state, Some(DownloadJobState::Cancelled));
    app.download.set_transfer_filter(TransferFilter::Failed);
    let failed = app.download.sorted_transfers();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].status, TransferState::Interrupted);
}

#[test]
fn log_tab_filters_levels_targets_and_preserves_scroll_position() {
    let mut app = app_with(vec![item("child", true)]);
    app.logs.push(LogEntry {
        timestamp: chrono::Local::now(),
        level: LogLevel::Info,
        target: "runtime".to_owned(),
        message: "session started".to_owned(),
    });
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
        message: "failed path=two\nretry later".to_owned(),
    });
    app.request_download();
    app.begin_download();
    assert_eq!(app.logs.len(), 3);
    app.active_tab = AppTab::Logs;
    assert_eq!(app.active_tab, AppTab::Logs);
    assert_eq!(app.log_offset_from_bottom, 0);
    let all = rendered(&mut app);
    assert!(all.contains("Runtime logs [All | All targets] (3)"));
    assert!(all.contains("completed path=one"));
    assert!(all.contains("failed path=two"));
    assert!(all.contains("failed path=two\\nretry later"));
    assert!(all.contains("a/i/e filter"));
    assert!(!all.contains("d download"));

    app.handle_log_key(&KeyCode::Char('e'));
    let errors = rendered(&mut app);
    assert!(errors.contains("Runtime logs [Error | All targets] (1)"));
    assert!(!errors.contains("completed path=one"));
    assert!(errors.contains("failed path=two"));

    app.handle_log_key(&KeyCode::Char('a'));
    app.handle_log_key(&KeyCode::Char('f'));
    let runtime = rendered(&mut app);
    assert!(runtime.contains("Runtime logs [All | Runtime] (1)"));
    assert!(runtime.contains("session started"));
    assert!(!runtime.contains("completed path=one"));

    for expected in [
        LogTargetFilter::Provider,
        LogTargetFilter::Source,
        LogTargetFilter::Daemon,
        LogTargetFilter::Settings,
        LogTargetFilter::Download,
        LogTargetFilter::All,
    ] {
        app.handle_log_key(&KeyCode::Char('f'));
        assert_eq!(app.log_target_filter, expected);
    }

    app.log_offset_from_bottom = 1;
    app.active_tab = AppTab::Files;
    app.active_tab = AppTab::Logs;
    assert_eq!(app.log_offset_from_bottom, 1);

    app.log_offset_from_bottom = 0;
    app.active_tab = AppTab::Files;
    app.active_tab = AppTab::Logs;
    assert_eq!(app.log_offset_from_bottom, 0);
}

#[test]
fn navigation_selection_is_separate_from_the_active_route() {
    let mut app = app_with(vec![item("child", true)]);
    assert_eq!(app.active_tab, AppTab::Files);
    assert_eq!(app.navigation_tab, AppTab::Files);
    app.navigation_focus = NavigationFocus::Navigation;
    app.move_navigation(true);
    assert_eq!(app.navigation_tab, AppTab::Transfers);
    assert_eq!(app.active_tab, AppTab::Files);

    app.activate_navigation();
    assert_eq!(app.active_tab, AppTab::Transfers);
    assert_eq!(app.navigation_focus, NavigationFocus::Content);

    app.navigation_focus = NavigationFocus::Navigation;
    app.move_navigation(true);
    app.move_navigation(true);
    assert_eq!(app.navigation_tab, AppTab::Settings);
    assert_eq!(app.active_tab, AppTab::Transfers);
    app.activate_navigation();
    assert_eq!(app.active_tab, AppTab::Settings);
    assert!(app.provider_settings_visible());
    assert!(!app.global_settings_open);
    assert_eq!(
        super::keymap::navigation::intent(KeyCode::Enter),
        Some(super::keymap::navigation::Intent::Select)
    );
    assert_eq!(super::keymap::navigation::intent(KeyCode::Right), None);
    assert_eq!(super::keymap::navigation::intent(KeyCode::Tab), None);
}

#[test]
fn provider_type_switch_and_source_selector_restore_sessions() {
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
            profile_name: "Work Google".to_owned(),
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
            profile_name: "Personal Aliyun".to_owned(),
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
    let selector = rendered_at(&mut app, 100, 28);
    assert!(selector.contains("Google Drive"));
    assert!(selector.contains("Google Source"));
    assert!(selector.contains("Up/Down source"));
    assert!(selector.contains("Enter open"));
    assert!(selector.contains("Esc close"));
    let outer_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(outer_footer.contains("Backspace parent"));
    assert!(outer_footer.contains("Left nav"));
    assert!(outer_footer.contains("Enter open"));
    assert!(outer_footer.contains("[/] provider"));
    assert_eq!(app.active_tab, AppTab::Files);

    let aliyun_index = app.switch_provider_type(1).unwrap();
    assert_eq!(aliyun_index, 1);
    let aliyun_activation = app.activate_selection(aliyun_index, Some(0));
    assert!(!aliyun_activation.restored);
    app.enter_loaded(aliyun_root, vec![item("aliyun-folder", true)]);

    assert!(app.activate_selection(0, Some(0)).restored);
    assert!(app.breadcrumbs().contains("google-root"));
    assert_eq!(app.provider_name, "Google Drive");
    assert_eq!(app.active_tab, AppTab::Files);
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
        profile_name: "Test Account".to_owned(),
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
fn accounts_are_listed_by_provider_and_provider_switch_restores_the_active_account() {
    let google = ProviderId::new("google").unwrap();
    let aliyun = ProviderId::new("aliyun").unwrap();
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![
        ProviderChoice {
            id: google.clone(),
            name: "Google Drive".to_owned(),
            profile: ProfileId::new("p_abcdefgh").unwrap(),
            profile_name: "Work Google".to_owned(),
            status: "Ready".to_owned(),
            settings_count: 1,
            sources: Vec::new(),
        },
        ProviderChoice {
            id: google.clone(),
            name: "Google Drive".to_owned(),
            profile: ProfileId::new("p_bcdefghj").unwrap(),
            profile_name: "Archive Google".to_owned(),
            status: "Disabled".to_owned(),
            settings_count: 1,
            sources: Vec::new(),
        },
        ProviderChoice {
            id: aliyun,
            name: "Aliyun Drive".to_owned(),
            profile: ProfileId::new("p_cdefghjk").unwrap(),
            profile_name: "Personal Aliyun".to_owned(),
            status: "NeedsConfig".to_owned(),
            settings_count: 1,
            sources: Vec::new(),
        },
    ];
    app.activate_selection(0, None);
    app.active_tab = AppTab::Accounts;
    app.move_account_selection(1);
    let second_google = app.selected_account_provider_index().unwrap();
    assert_eq!(second_google, 1);
    app.activate_selection(second_google, None);

    let aliyun_index = app.switch_provider_type(1).unwrap();
    app.activate_selection(aliyun_index, None);
    assert_eq!(app.profile_name, "Personal Aliyun");
    let restored_google = app.switch_provider_type(1).unwrap();
    assert_eq!(restored_google, 1);
    app.activate_selection(restored_google, None);
    assert_eq!(app.profile_name, "Archive Google");

    app.active_tab = AppTab::Accounts;
    let output = rendered_at(&mut app, 100, 28);
    assert!(output.contains("Work Google"));
    assert!(output.contains("Archive Google"));
    assert!(!output.contains("Personal Aliyun"));
    assert!(output.contains("p_bcdefghj"));
}

#[test]
fn account_and_file_keymaps_keep_declared_aliases_aligned() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.active_tab = AppTab::Accounts;
    let account_hints = super::keymap::page_hints(&app);
    assert!(
        account_hints
            .iter()
            .any(|hint| hint.key == "Enter" && hint.action == "open")
    );
    assert!(
        account_hints
            .iter()
            .any(|hint| hint.key == "Space" && hint.action == "activate")
    );
    assert!(
        account_hints
            .iter()
            .any(|hint| hint.key == "a" && hint.action == "add")
    );
    assert_eq!(
        super::keymap::accounts::intent(KeyCode::Enter),
        Some(super::keymap::accounts::Intent::Open)
    );
    assert_eq!(
        super::keymap::accounts::intent(KeyCode::Char('e')),
        Some(super::keymap::accounts::Intent::Open)
    );
    assert_eq!(
        super::keymap::accounts::intent(KeyCode::Char(' ')),
        Some(super::keymap::accounts::Intent::Activate)
    );
    assert_eq!(
        super::keymap::accounts::intent(KeyCode::Char('n')),
        Some(super::keymap::accounts::Intent::Add)
    );

    app.active_tab = AppTab::Files;
    assert_eq!(
        super::keymap::files::intent(KeyCode::Char('o')),
        Some(super::keymap::files::Intent::Sources)
    );
    assert_eq!(
        super::keymap::files::intent(KeyCode::Char('p')),
        Some(super::keymap::files::Intent::Sources)
    );
    assert_eq!(super::keymap::files::intent(KeyCode::Char('l')), None);
    assert_eq!(super::keymap::files::intent(KeyCode::Char('j')), None);
    assert_eq!(super::keymap::files::intent(KeyCode::Char('k')), None);
    assert_eq!(
        super::keymap::files::intent(KeyCode::Backspace),
        Some(super::keymap::files::Intent::Parent)
    );
    assert_eq!(super::keymap::files::intent(KeyCode::Left), None);
    assert_eq!(super::keymap::files::intent(KeyCode::Char('h')), None);

    app.active_tab = AppTab::Logs;
    assert_eq!(app.logs_focus, PanelFocus::Upper);
    app.handle_log_key(&KeyCode::Tab);
    assert_eq!(app.logs_focus, PanelFocus::Lower);
}

#[test]
fn provider_switch_changes_the_rendered_accent() {
    let google = ProviderId::new("google").unwrap();
    let aliyun = ProviderId::new("aliyun").unwrap();
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![
        ProviderChoice {
            id: google.clone(),
            name: "Google Drive".to_owned(),
            profile: ProfileId::new("p_abcdefgh").unwrap(),
            profile_name: "Work Google".to_owned(),
            status: "Ready".to_owned(),
            settings_count: 1,
            sources: Vec::new(),
        },
        ProviderChoice {
            id: aliyun.clone(),
            name: "Aliyun Drive".to_owned(),
            profile: ProfileId::new("p_bcdefghj").unwrap(),
            profile_name: "Personal Aliyun".to_owned(),
            status: "NeedsConfig".to_owned(),
            settings_count: 1,
            sources: Vec::new(),
        },
    ];
    app.active_tab = AppTab::Accounts;
    app.activate_selection(0, None);
    let google_accent = super::theme::for_provider(Some(&google)).accent;
    let google_colors = rendered_colors_at(&mut app, 80, 24);
    assert!(google_colors.iter().any(|(_, bg)| *bg == google_accent));

    app.activate_selection(1, None);
    let aliyun_accent = super::theme::for_provider(Some(&aliyun)).accent;
    let aliyun_colors = rendered_colors_at(&mut app, 80, 24);
    assert_ne!(google_accent, aliyun_accent);
    assert!(aliyun_colors.iter().any(|(_, bg)| *bg == aliyun_accent));
}

#[test]
fn account_settings_are_scoped_to_the_active_identity_and_config_file() {
    let mut app = app_with(Vec::new());
    app.settings_path = PathBuf::from("/home/tester/.net-pan/settings.toml");
    app.provider_choices = vec![ProviderChoice {
        id: ProviderId::new("google").unwrap(),
        name: "Google Drive".to_owned(),
        profile: ProfileId::new("p_abcdefgh").unwrap(),
        profile_name: "Work Google".to_owned(),
        status: "Ready".to_owned(),
        settings_count: 1,
        sources: Vec::new(),
    }];
    app.activate_selection(0, None);
    app.active_tab = AppTab::Accounts;
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "enabled",
            label: "Enabled",
            description: "Enable this Provider without deleting its configuration.",
            kind: ProviderSettingKind::Boolean,
            required: true,
        }],
        &ProviderSettingsSnapshot::default(),
    ));
    app.open_account_detail();

    let output = rendered_at(&mut app, 120, 30);
    assert!(output.contains("Accounts / Work Google"));
    assert!(output.contains("PROVIDER"));
    assert!(output.contains("Google Drive"));
    assert!(output.contains("Work Google"));
    assert!(output.contains("p_abcdefgh"));
    assert!(output.contains("providers/google/p_abcdefgh/config.toml"));
    assert!(output.contains("Navigation"));
    assert!(!output.contains("GLOBAL SETTINGS"));
}

#[test]
fn settings_route_is_provider_scoped_and_global_settings_are_a_separate_layer() {
    let mut app = app_with(Vec::new());
    app.provider_name = "Google Drive".to_owned();
    app.profile_name = "Work Google".to_owned();
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "enabled",
            label: "Enabled",
            description: "Enable this Provider.",
            kind: ProviderSettingKind::Boolean,
            required: true,
        }],
        &ProviderSettingsSnapshot::default(),
    ));

    app.open_provider_settings_page();
    let provider = rendered_at(&mut app, 80, 24);
    assert!(provider.contains("Settings / Work Google"));
    assert!(provider.contains("PROVIDER"));
    assert!(!provider.contains("GLOBAL SETTINGS"));

    app.open_global_settings();
    let global = rendered_at(&mut app, 80, 24);
    assert!(global.contains("GLOBAL SETTINGS"));
    assert!(global.contains("APPLICATION"));
    assert!(global.contains("settings.toml"));
    assert!(!global.contains("Navigation"));
    assert!(!global.contains("Settings / Work Google"));
    let global_colors = rendered_colors_at(&mut app, 80, 24);
    assert!(
        global_colors
            .iter()
            .any(|(_, background)| *background == Color::Black)
    );
    assert!(
        !global_colors
            .iter()
            .any(|(_, background)| { *background == Color::Rgb(80, 250, 123) })
    );
}

#[test]
fn account_name_form_accepts_unicode_but_rejects_invalid_names() {
    assert_eq!(
        validate_account_name("  公司 Google 云盘  ").unwrap(),
        "公司 Google 云盘"
    );
    assert!(validate_account_name("\n").is_err());
    assert!(validate_account_name("bad\u{0007}name").is_err());
    assert!(validate_account_name(&"x".repeat(65)).is_err());
}

#[test]
fn needs_config_provider_opens_settings_without_a_ready_instance() {
    let mut app = app_with(Vec::new());
    app.provider_choices = vec![ProviderChoice {
        id: ProviderId::new("aliyun").unwrap(),
        name: "Aliyun Drive".to_owned(),
        profile: ProfileId::new("default").unwrap(),
        profile_name: "Aliyun Account".to_owned(),
        status: "NeedsConfig".to_owned(),
        settings_count: 9,
        sources: Vec::new(),
    }];
    app.activate_selection(0, None);
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "enabled",
            label: "Enabled",
            description: "Enable this Provider.",
            kind: ProviderSettingKind::Boolean,
            required: true,
        }],
        &ProviderSettingsSnapshot::default(),
    ));
    app.open_provider_settings_page();
    assert_eq!(app.active_tab, AppTab::Settings);
    let output = rendered(&mut app);
    assert!(output.contains("Aliyun Drive"));
    assert!(output.contains("NeedsConfig"));
    assert!(output.contains("Settings / Aliyun Account"));
    assert!(output.contains("PROVIDER"));
    assert!(!output.contains("GLOBAL SETTINGS"));
}

#[test]
fn provider_secret_fields_are_masked_and_blank_keeps_existing_value() {
    let schema = vec![ProviderSettingSpec {
        key: "refresh_token",
        label: "Refresh token",
        description: "OAuth refresh token.",
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
fn provider_catalog_starts_on_saved_links_until_a_source_is_opened() {
    let identity = event_provider();
    let mut root = item("cp_result", true);
    root.reference.identity = identity.clone();
    let provider = BrowserProvider {
        id: identity.provider_id.clone(),
        name: "Google Drive".to_owned(),
        identity: identity.clone(),
        profile_name: "Work Google".to_owned(),
        status: ProviderStatus::Ready,
        available: true,
        settings_schema: Vec::new(),
        sources: vec![RemoteSource {
            id: root.reference.source_id.clone(),
            name: "cp_result".to_owned(),
            root,
        }],
        source_error: None,
        settings: ProviderSettingsSnapshot::default(),
    };
    let mut app = app_with(Vec::new());
    app.set_provider_catalog(&[provider], 0);

    assert_eq!(app.browser_layer(), BrowserLayer::SharedLinks);
    assert!(app.current_source.is_none());
    assert_eq!(app.source_name, "-");
    let output = rendered_at(&mut app, 80, 24);
    assert!(output.contains("Provider: Google Drive"));
    assert!(output.contains("Account: Work Google"));
    assert!(output.contains("Source: -"));
    assert!(output.contains("Status: Ready"));
    assert!(output.contains("Sources"));
    assert!(output.contains("[LINK] cp_result/"));
}

#[test]
fn provider_settings_show_help_for_the_selected_field_and_proxy_precedence() {
    let mut app = app_with(Vec::new());
    app.open_provider_settings_page();
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![
            ProviderSettingSpec {
                key: "enabled",
                label: "Enabled",
                description: "Enable this Provider without deleting its configuration.",
                kind: ProviderSettingKind::Boolean,
                required: true,
            },
            ProviderSettingSpec {
                key: "proxy",
                label: "Provider proxy",
                description: "Overrides the global application proxy; blank inherits it.",
                kind: ProviderSettingKind::Text,
                required: false,
            },
        ],
        &ProviderSettingsSnapshot::default(),
    ));

    let enabled = rendered_at(&mut app, 100, 28);
    assert!(enabled.contains("HELP    Enable this Provider"));
    assert!(enabled.contains("STATUS  Ready"));
    assert!(enabled.contains("Provider proxy > Global proxy > Direct"));
    app.provider_form.as_mut().unwrap().move_selection(1);
    let proxy = rendered_at(&mut app, 100, 28);
    assert!(proxy.contains("HELP    Overrides the global application proxy"));
}

#[test]
fn provider_settings_separate_editable_and_read_only_fields() {
    let snapshot = ProviderSettingsSnapshot {
        values: BTreeMap::from([
            ("enabled".to_owned(), "true".to_owned()),
            ("sources".to_owned(), "2 saved folders".to_owned()),
            ("proxy".to_owned(), String::new()),
        ]),
        ..ProviderSettingsSnapshot::default()
    };
    let mut form = ProviderSettingsForm::from_snapshot(
        vec![
            ProviderSettingSpec {
                key: "enabled",
                label: "Enabled",
                description: "Enable this Provider.",
                kind: ProviderSettingKind::Boolean,
                required: true,
            },
            ProviderSettingSpec {
                key: "sources",
                label: "Saved folders",
                description: "Managed by the Source selector.",
                kind: ProviderSettingKind::Action,
                required: false,
            },
            ProviderSettingSpec {
                key: "proxy",
                label: "Provider proxy",
                description: "Overrides the global proxy.",
                kind: ProviderSettingKind::Text,
                required: false,
            },
        ],
        &snapshot,
    );

    assert_eq!(form.selected, 0);
    form.move_selection(1);
    assert_eq!(form.selected, 2, "selection must skip read-only fields");
    form.move_selection(1);
    assert_eq!(form.selected, 0);

    let mut app = app_with(Vec::new());
    app.provider_form = Some(form);
    app.open_provider_settings_page();
    let output = rendered_at(&mut app, 100, 28);
    assert!(output.contains("EDITABLE SETTINGS"));
    assert!(output.contains("READ ONLY"));
    assert!(output.contains("Saved folders"));
    assert!(output.contains("2 saved folders"));
}

#[test]
fn read_only_provider_settings_expose_no_edit_actions() {
    let snapshot = ProviderSettingsSnapshot {
        values: BTreeMap::from([("endpoint".to_owned(), "https://example.test".to_owned())]),
        read_only_reason: Some("Configuration could not be loaded".to_owned()),
        ..ProviderSettingsSnapshot::default()
    };
    let form = ProviderSettingsForm::from_snapshot(
        vec![ProviderSettingSpec {
            key: "endpoint",
            label: "Endpoint",
            description: "Provider endpoint.",
            kind: ProviderSettingKind::Text,
            required: true,
        }],
        &snapshot,
    );
    assert!(form.editable_indices().is_empty());
    assert_eq!(form.read_only_indices(), vec![0]);

    let mut app = app_with(Vec::new());
    app.provider_form = Some(form);
    app.open_provider_settings_page();
    let hints = super::keymap::page_hints(&app);
    assert!(!hints.iter().any(|hint| hint.key == "Enter"));
    assert!(!hints.iter().any(|hint| hint.key == "Space"));
    let output = rendered_at(&mut app, 80, 24);
    assert!(output.contains("READ ONLY"));
    assert!(output.contains("Endpoint"));
}

#[test]
fn narrow_terminal_renders_files_selector_and_settings() {
    let mut app = app_with(vec![item(
        "a-very-long-source-folder-name-that-must-be-clipped",
        true,
    )]);
    app.provider_name = "A Provider Name That Is Intentionally Too Long".to_owned();
    app.source_name = "A Source Name That Is Intentionally Too Long".to_owned();
    app.provider_status = "ConnectionError: intentionally long test error".to_owned();
    let files = rendered_at(&mut app, 60, 20);
    assert!(files.contains("net-pan"));
    assert!(files.contains("ConnectionError"));
    assert!(files.contains("Sources"));
    assert!(files.contains("TYPE"));
    assert!(files.contains("Navigation"));

    let root = item("root", true);
    app.provider_choices = vec![ProviderChoice {
        id: ProviderId::new("test").unwrap(),
        name: "Test Drive With A Long Display Name".to_owned(),
        profile: ProfileId::new("default").unwrap(),
        profile_name: "Long Test Account".to_owned(),
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
    assert!(selector.contains("Source"));
    assert!(selector.contains("A Long Source Name"));

    app.provider_selector = None;
    app.active_tab = AppTab::Settings;
    app.provider_name = "Test Drive".to_owned();
    app.settings_form = Some(SettingsForm::new(
        Path::new("/home/tester/Downloads"),
        &NetworkSettings::default(),
    ));
    app.provider_form = Some(ProviderSettingsForm::from_snapshot(
        vec![
            ProviderSettingSpec {
                key: "endpoint",
                label: "Endpoint",
                description: "Remote API endpoint.",
                kind: ProviderSettingKind::Text,
                required: true,
            },
            ProviderSettingSpec {
                key: "proxy",
                label: "Provider proxy",
                description: "Provider-specific proxy.",
                kind: ProviderSettingKind::Text,
                required: false,
            },
        ],
        &ProviderSettingsSnapshot {
            values: BTreeMap::from([(
                "endpoint".to_owned(),
                "https://example.invalid/a/long/path".to_owned(),
            )]),
            ..ProviderSettingsSnapshot::default()
        },
    ));
    let settings = rendered_at(&mut app, 60, 20);
    assert!(settings.contains("Settings /"));
    assert!(settings.contains("PROVIDER"));
    assert!(settings.contains("Test Drive"));
    assert!(settings.contains("Endpoint"));
    assert!(settings.contains("Provider proxy"));

    app.open_global_settings();
    let global = rendered_at(&mut app, 60, 20);
    assert!(global.contains("GLOBAL SETTINGS"));
    assert!(global.contains("APPLICATION"));
    assert!(global.contains("EDITABLE SETTINGS"));
    assert!(!global.contains("Navigation"));
}

#[test]
fn standard_terminal_keeps_shell_tables_and_panel_focus_visible() {
    let mut app = app_with(vec![item("folder", true), item("file.bin", false)]);
    let files = rendered_at(&mut app, 80, 24);
    assert!(files.contains("net-pan"));
    assert!(files.contains("EMBEDDED"));
    assert!(files.contains("> Sources"));
    assert!(files.contains("TYPE"));

    app.active_tab = AppTab::Transfers;
    let transfers = rendered_at(&mut app, 80, 24);
    assert!(transfers.contains("> Transfers [All]"));
    assert!(transfers.contains("STATE"));

    app.active_tab = AppTab::Logs;
    let logs = rendered_at(&mut app, 80, 24);
    assert!(logs.contains("TIME"));
    assert!(logs.contains("LEVEL"));
    assert!(logs.contains("q quit"));

    app.open_global_settings();
    let settings = rendered_at(&mut app, 80, 24);
    assert!(settings.contains("EDITABLE SETTINGS"));
    assert!(settings.contains("READ ONLY"));
    assert!(settings.contains("APPLICATION"));
    assert!(settings.contains("settings.toml"));
    assert!(!settings.contains("PROVIDER SETTINGS"));
}

#[test]
fn download_form_uses_global_root_and_clamps_workers() {
    let child = item("cp_result", true);
    let mut app = app_with(vec![child]);
    app.request_download();
    for _ in 0..10 {
        app.handle_form_edit_key(KeyCode::Right);
    }
    assert_eq!(app.download_form.as_ref().unwrap().workers, 5);
    assert_eq!(
        app.download_target_destination(),
        PathBuf::from("/home/tester/Downloads/cp_result")
    );
    for _ in 0..10 {
        app.handle_form_edit_key(KeyCode::Left);
    }
    assert_eq!(app.download_form.as_ref().unwrap().workers, 1);
    let confirmation = rendered(&mut app);
    assert!(confirmation.contains("Global download root: /home/tester/Downloads"));
    assert!(confirmation.contains("Target: /home/tester/Downloads/cp_result"));
    assert!(confirmation.contains("Left/Right workers"));
    assert!(!confirmation.contains("Up/Down field"));
}

#[test]
fn settings_tab_edits_proxy_and_returns_to_previous_tab() {
    let mut app = app_with(vec![item("child", true)]);
    app.open_global_settings();
    let form = app.settings_form.as_mut().unwrap();
    form.select_adjacent();
    form.editing = true;
    for character in "http://127.0.0.1:7890?q=1".chars() {
        app.handle_settings_edit_key(KeyCode::Char(character));
    }
    let output = rendered(&mut app);
    assert!(output.contains("GLOBAL SETTINGS  EDITING"));
    assert!(output.contains("http://127.0.0.1:7890?q=1"));
    assert!(output.contains("System proxy variables are ignored"));
    assert!(output.contains("Ctrl-S save"));
    assert!(!output.contains("Enter save"));
    assert!(!output.contains("PROVIDER SETTINGS"));
    assert!(!output.contains("d download"));

    app.active_tab = AppTab::Logs;
    assert_eq!(app.active_tab, AppTab::Logs);
}

#[test]
fn global_settings_select_download_root_and_proxy_independently() {
    let temporary = tempfile::tempdir().unwrap();
    let mut app = BrowserApp::new(
        vec![item("cp_result", true)],
        temporary.path().to_path_buf(),
        NetworkSettings::default(),
    );
    app.open_global_settings();
    let form = app.settings_form.as_mut().unwrap();
    assert_eq!(form.field, GlobalSettingField::Destination);
    assert_eq!(form.destination, temporary.path().display().to_string());
    form.select_adjacent();
    assert_eq!(form.field, GlobalSettingField::Proxy);
    form.select_adjacent();
    assert_eq!(form.field, GlobalSettingField::Destination);

    let output = rendered(&mut app);
    assert!(output.contains("Download path"));
    assert!(output.contains("EDITABLE SETTINGS"));
    assert!(output.contains("READ ONLY"));
    assert!(output.contains("Connection"));
    assert!(output.contains("Precedence"));
    assert!(output.contains("Environment"));
    assert!(output.contains(&temporary.path().display().to_string()));
    assert!(output.contains("Download path is ready"));
}

#[test]
fn settings_save_stops_editing_and_shows_a_centered_expiring_toast() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.open_global_settings();
    app.settings_form.as_mut().unwrap().editing = true;
    let validation_status = app.settings_form.as_ref().unwrap().status.clone();

    super::runtime::finish_global_settings_save(
        &mut app,
        ToastKind::Success,
        "Global settings saved",
    );

    assert!(app.global_settings_open);
    assert!(!app.settings_form.as_ref().unwrap().editing);
    assert_eq!(
        app.settings_form.as_ref().unwrap().status,
        validation_status
    );
    assert!(!app.settings_form.as_ref().unwrap().status.contains("saved"));
    assert!(app.toast.is_some());
    let lines = rendered_lines_at(&mut app, 80, 24);
    let toast_row = lines
        .iter()
        .position(|line| line.contains("Global settings saved"))
        .unwrap();
    assert!((9..=14).contains(&toast_row), "toast row was {toast_row}");

    app.toast.as_mut().unwrap().expires_at = Instant::now() - Duration::from_millis(1);
    app.expire_toast();
    assert!(app.toast.is_none());

    assert_eq!(
        super::runtime::settings_save_message("Global settings saved", true),
        "Global settings saved; active Job keeps its original destination"
    );
}

#[test]
fn valid_global_download_root_is_persisted_without_changing_other_download_options() {
    let temporary = tempfile::tempdir().unwrap();
    let settings_path = temporary.path().join("settings.toml");
    let download_root = temporary.path().join("downloads");
    std::fs::create_dir(&download_root).unwrap();
    let mut original = AppConfig::default();
    original.download.concurrency = 4;
    original.download.overwrite = true;
    original.save(&settings_path).unwrap();
    let network = NetworkSettings::from_proxy_input("http://127.0.0.1:7890");

    super::runtime::persist_global_settings(&settings_path, &download_root, &network).unwrap();

    let saved = AppConfig::load(&settings_path).unwrap();
    assert_eq!(
        saved.download.destination.as_deref(),
        Some(download_root.as_path())
    );
    assert_eq!(saved.download.concurrency, 4);
    assert!(saved.download.overwrite);
    assert_eq!(saved.network, network);
}

#[test]
fn file_status_is_inside_files_panel_and_footer_is_a_shortcut_bar() {
    let mut app = app_with(vec![item("cp_result", true)]);
    let lines = rendered_lines_at(&mut app, 80, 24);
    let files = lines
        .iter()
        .position(|line| line.contains("Sources"))
        .unwrap();
    let status = lines
        .iter()
        .position(|line| line.contains("STATUS  Ready  |  1 sources"))
        .unwrap();
    assert!(files < status);
    assert!(!lines.iter().any(|line| line.contains("Transfers [All]")));
    let footer = rendered_footer_at(&mut app, 80, 24);
    assert!(footer.contains("Enter open"));
    assert!(footer.contains("d download"));
    assert!(!footer.contains("Actions"));
    assert!(!footer.contains("STATUS"));
}

#[test]
fn source_error_stays_in_the_shell_above_the_shortcut_bar() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.provider_status = "SourceError: could not list configured sources".to_owned();

    let lines = rendered_lines_at(&mut app, 80, 24);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Status: SourceError"))
    );
    let footer = rendered_footer_at(&mut app, 80, 24);
    assert!(footer.contains("Enter open"));
    assert!(!footer.contains("SourceError"));
    assert!(!footer.contains("Actions"));
}

#[test]
fn shortcut_bar_tracks_navigation_and_active_page() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.provider_form = Some(editable_provider_form());

    let files_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(files_footer.contains("Enter open"));
    assert!(files_footer.contains("g global"));
    assert!(!files_footer.contains("Tab nav"));
    assert!(!files_footer.contains("a/v/f filter"));
    let download = super::keymap::page_hints(&app)
        .into_iter()
        .find(|hint| hint.key == "d")
        .expect("download hint is visible for a folder");
    assert!(download.primary);

    app.active_tab = AppTab::Transfers;
    let transfers_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(transfers_footer.contains("Up/Down transfer"));
    assert!(transfers_footer.contains("a/r/f filter"));
    assert!(transfers_footer.contains("Left nav"));
    assert!(!transfers_footer.contains("Tab nav"));
    assert!(!transfers_footer.contains("Enter open"));

    app.active_tab = AppTab::Logs;
    let logs_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(logs_footer.contains("Up/Down select"));
    assert!(logs_footer.contains("a/i/e filter"));
    assert!(logs_footer.contains("Tab pane"));

    app.open_global_settings();
    let settings_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(settings_footer.contains("Up/Down global setting"));
    assert!(settings_footer.contains("Backspace back"));
    assert!(!settings_footer.contains("Esc/g close"));
    app.global_settings_open = false;

    app.active_tab = AppTab::Accounts;
    app.account_detail_open = true;
    let provider_footer = rendered_footer_at(&mut app, 100, 28);
    assert!(provider_footer.contains("Up/Down field"));
    assert!(provider_footer.contains("Space toggle"));
    assert!(provider_footer.contains("Backspace accounts"));
    assert!(provider_footer.contains("Left nav"));

    app.account_detail_open = false;
    app.navigation_focus = NavigationFocus::Navigation;
    let navigation = rendered_footer_at(&mut app, 100, 28);
    assert!(navigation.contains("Up/Down navigate"));
    assert!(navigation.contains("Enter select"));
    assert!(!navigation.contains("Tab nav"));
}

#[test]
fn shortcut_bar_wraps_to_two_rows_when_the_terminal_is_narrow() {
    let mut app = app_with(vec![item("cp_result", true)]);

    assert_eq!(super::render::footer_height(&app, 120), 1);
    assert_eq!(super::render::footer_height(&app, 60), 2);

    let footer = rendered_footer_at(&mut app, 60, 20);
    for hint in [
        "Enter open",
        "d download",
        "r reload",
        "o sources",
        "l logs",
        "g global",
        "q quit",
    ] {
        assert!(footer.contains(hint), "missing shortcut hint: {hint}");
    }

    app.enter_loaded(item("cp_result", true), vec![item("child", true)]);
    assert_eq!(super::render::footer_height(&app, 60), 2);
    let nested = rendered_footer_at(&mut app, 60, 20);
    for hint in [
        "Up/Down item",
        "Backspace parent",
        "Enter open",
        "d download",
        "r reload",
        "o sources",
        "l logs",
        "g global",
        "q quit",
    ] {
        assert!(nested.contains(hint), "missing nested shortcut: {hint}");
    }
    assert!(!nested.contains("Left nav"));
    assert!(!nested.contains("[/] provider"));
}

#[test]
fn j_and_k_are_not_hidden_navigation_aliases() {
    for key in [KeyCode::Char('j'), KeyCode::Char('k')] {
        assert_eq!(super::keymap::navigation::intent(key), None);
        assert_eq!(super::keymap::accounts::intent(key), None);
        assert_eq!(super::keymap::global_settings::intent(key), None);
        assert_eq!(super::keymap::provider_settings::intent(key), None);
        assert_eq!(super::keymap::files::intent(key), None);
        assert_eq!(super::keymap::transfers::intent(key), None);
        assert_eq!(super::keymap::logs::intent(key), None);
    }
}

#[test]
fn every_page_keeps_its_narrow_footer_complete_and_within_two_rows() {
    let mut app = app_with(vec![item("cp_result", true)]);
    app.provider_form = Some(editable_provider_form());

    for tab in [
        AppTab::Accounts,
        AppTab::Files,
        AppTab::Transfers,
        AppTab::Logs,
        AppTab::Settings,
    ] {
        app.active_tab = tab;
        app.navigation_focus = NavigationFocus::Content;
        let height = super::render::footer_height(&app, 60);
        assert!(height <= 2, "{tab:?} footer uses {height} rows");

        let footer = rendered_footer_at(&mut app, 60, 20);
        for hint in ["[/] provider", "l logs", "g global", "q quit"] {
            assert!(
                footer.contains(hint),
                "{tab:?} missing shortcut hint: {hint}"
            );
        }
        assert!(!footer.contains("Tab nav"));
    }

    app.active_tab = AppTab::Accounts;
    app.account_detail_open = true;
    let height = super::render::footer_height(&app, 60);
    assert!(height <= 2, "account settings footer uses {height} rows");
    let footer = rendered_footer_at(&mut app, 60, 20);
    for hint in [
        "Up/Down field",
        "Enter edit",
        "Space toggle",
        "Backspace accounts",
        "Left nav",
    ] {
        assert!(
            footer.contains(hint),
            "account settings missing shortcut hint: {hint}"
        );
    }
}

#[test]
fn destination_is_only_shown_in_the_shell_and_idle_transfers_have_no_selection_summary() {
    let destination = PathBuf::from("/home/tester/Downloads/archive-folder");
    let mut app = BrowserApp::new(
        vec![item("cp_result", true)],
        destination.clone(),
        NetworkSettings::default(),
    );
    app.provider_status = "Ready".to_owned();
    let lines = rendered_lines_at(&mut app, 80, 24);
    assert!(
        lines
            .iter()
            .any(|line| { line.contains(&format!("Destination: {}", destination.display())) })
    );
    assert!(!lines.iter().any(|line| line.contains("SELECTED")));
    assert!(!lines.iter().any(|line| line.contains("DESTINATION")));
}

#[test]
fn deep_breadcrumb_keeps_ends_and_collapses_the_middle() {
    let mut app = app_with(vec![item("cp_result", true)]);
    for (folder, child) in [
        ("cp_result", "CUHK_LOWER"),
        ("CUHK_LOWER", "colmap_metrics"),
        ("colmap_metrics", "dense-reconstruction-results"),
        ("dense-reconstruction-results", "dens"),
    ] {
        app.enter_loaded(item(folder, true), vec![item(child, true)]);
    }
    app.enter_loaded(item("dens", true), vec![item("result.bin", false)]);

    let output = rendered_at(&mut app, 60, 20);
    assert!(output.contains("cp_result / ... / dens"));
    assert!(!output.contains("Remote Files"));
    assert!(!output.contains("Shared links"));
}

#[test]
fn exit_confirmation_overlays_and_preserves_underlying_state() {
    let mut app = app_with(vec![item("child", true)]);
    app.active_tab = AppTab::Logs;
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
    app.apply_download_event(DownloadEvent::FileSkipped {
        job_id: JobId(7),
        provider: event_provider(),
        transfer_id: TransferId(2),
        path: PathBuf::from("CUHK_LOWER/existing.bin"),
        reason: "target exists".to_owned(),
    });
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
    record_download_event(
        &mut app,
        &DownloadEvent::Retrying {
            job_id: JobId(7),
            provider: event_provider(),
            transfer_id: TransferId(3),
            path: PathBuf::from("CUHK_LOWER/broken.bin"),
            offset: 5,
            attempt: 1,
            max_attempts: 4,
            error: "response body ended early: unexpected EOF".to_owned(),
        },
        &mut logger,
    )
    .unwrap();
    record_download_event(
        &mut app,
        &DownloadEvent::Paused {
            job_id: JobId(7),
            provider: event_provider(),
        },
        &mut logger,
    )
    .unwrap();
    record_download_event(
        &mut app,
        &DownloadEvent::Resumed {
            job_id: JobId(7),
            provider: event_provider(),
        },
        &mut logger,
    )
    .unwrap();
    record_download_event(
        &mut app,
        &DownloadEvent::Cancelled {
            job_id: JobId(7),
            provider: event_provider(),
        },
        &mut logger,
    )
    .unwrap();
    record_download_event(
        &mut app,
        &DownloadEvent::Finished {
            job_id: JobId(7),
            provider: event_provider(),
            summary: DownloadSummary {
                downloaded: 1,
                skipped: 1,
                failed: 1,
                bytes_written: 10,
                partial_bytes: 5,
            },
        },
        &mut logger,
    )
    .unwrap();

    assert_eq!(app.logs[0].level, LogLevel::Info);
    assert_eq!(app.logs[1].level, LogLevel::Info);
    assert_eq!(app.logs[2].level, LogLevel::Error);
    assert_eq!(app.logs[3].level, LogLevel::Info);
    assert_eq!(app.logs[4].level, LogLevel::Info);
    assert_eq!(app.logs[5].level, LogLevel::Info);
    assert_eq!(app.logs[6].level, LogLevel::Error);
    assert_eq!(app.logs[7].level, LogLevel::Info);
    assert_eq!(app.log_offset_from_bottom, 7);
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
    assert!(content.contains("offset=5 attempt=1/4 reason=response body ended early"));
    assert!(content.contains("paused provider=test profile=default job=7"));
    assert!(content.contains("resumed provider=test profile=default job=7"));
    assert!(content.contains(" | ERROR    | download - stopped provider=test"));
    assert!(content.contains(
        "finished provider=test profile=default job=7 all=3 downloaded=1 skipped=1 failed=1 skip_reasons=[target exists=1]"
    ));
}
