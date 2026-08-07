use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    client::PanClient,
    config::{AppConfig, validate_download_destination},
    download::{
        DownloadEvent, DownloadJobState, DownloadOptions, DownloadSummary, JobId, TransferId,
        TransferState, format_bytes,
    },
    job::DownloadJobSnapshot,
    job_client::JobUpdate,
    logger::{LogEntry, LogLevel, SessionLogger},
    plan::safe_name,
    provider::{
        ProfileId, ProviderCatalogEntry, ProviderId, ProviderIdentity, ProviderSetting,
        ProviderSettingKind, ProviderSettingsSnapshot, ProviderStatus, RemoteItem, RemoteSource,
        SourceId,
    },
    settings::NetworkSettings,
};
use anyhow::{Context, Result};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};

mod instance;
mod keymap;
mod render;
mod runtime;
#[cfg(test)]
mod tests;
mod theme;

pub use instance::TuiInstanceGuard;
use render::render_ui;
#[cfg(test)]
use runtime::record_download_event;
pub use runtime::run_browser;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserMode {
    Browsing,
    Confirming,
    Scanning,
    Downloading,
    Paused,
    Finished,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserLayer {
    SharedLinks,
    DriveFolder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppTab {
    Accounts,
    Files,
    Transfers,
    Logs,
    Settings,
}

impl AppTab {
    const ALL: [Self; 5] = [
        Self::Accounts,
        Self::Files,
        Self::Transfers,
        Self::Logs,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Files => "Files",
            Self::Transfers => "Transfers",
            Self::Logs => "Logs",
            Self::Settings => "Settings",
        }
    }

    fn adjacent(self, delta: isize) -> Self {
        let current = Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .expect("active navigation item is registered");
        let next = if delta < 0 {
            (current + Self::ALL.len() - 1) % Self::ALL.len()
        } else {
            (current + 1) % Self::ALL.len()
        };
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationFocus {
    Navigation,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelFocus {
    Upper,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalSettingField {
    Destination,
    Proxy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastKind {
    Success,
    Warning,
}

#[derive(Clone, Debug)]
struct Toast {
    kind: ToastKind,
    message: String,
    expires_at: Instant,
}

impl GlobalSettingField {
    fn row(self) -> usize {
        match self {
            Self::Destination => 0,
            Self::Proxy => 1,
        }
    }

    fn toggle(&mut self) {
        *self = match self {
            Self::Destination => Self::Proxy,
            Self::Proxy => Self::Destination,
        };
    }
}

#[derive(Clone, Debug)]
struct SettingsForm {
    destination: String,
    proxy: String,
    field: GlobalSettingField,
    cursor: usize,
    editing: bool,
    status: String,
}

impl SettingsForm {
    fn new(destination: &Path, settings: &NetworkSettings) -> Self {
        let destination = destination.display().to_string();
        let proxy = settings.proxy_input().to_owned();
        let status = match validate_download_destination(Path::new(&destination)) {
            Ok(()) => "Download path is ready.".to_owned(),
            Err(error) => format!("Download path needs attention: {error}"),
        };
        let cursor = destination.chars().count();
        Self {
            destination,
            proxy,
            field: GlobalSettingField::Destination,
            cursor,
            editing: false,
            status,
        }
    }

    fn selected_value(&self) -> &str {
        match self.field {
            GlobalSettingField::Destination => &self.destination,
            GlobalSettingField::Proxy => &self.proxy,
        }
    }

    fn selected_value_mut(&mut self) -> &mut String {
        match self.field {
            GlobalSettingField::Destination => &mut self.destination,
            GlobalSettingField::Proxy => &mut self.proxy,
        }
    }

    fn select_adjacent(&mut self) {
        self.field.toggle();
        self.cursor = self.selected_value().chars().count();
    }

    fn refresh_status(&mut self) {
        self.status = match validate_download_destination(Path::new(&self.destination)) {
            Ok(()) => "Download path is ready.".to_owned(),
            Err(error) => format!("Download path needs attention: {error}"),
        };
    }
}

#[derive(Clone, Debug)]
struct ProviderField {
    spec: ProviderSetting,
    value: String,
    original: String,
    secret_configured: bool,
    cursor: usize,
}

#[derive(Clone, Debug, Default)]
struct ProviderSettingsForm {
    fields: Vec<ProviderField>,
    selected: usize,
    editing: bool,
    status: String,
    read_only_reason: Option<String>,
}

impl ProviderSettingsForm {
    fn from_snapshot<S>(schema: Vec<S>, snapshot: &ProviderSettingsSnapshot) -> Self
    where
        S: Into<ProviderSetting>,
    {
        let fields = schema
            .into_iter()
            .map(Into::into)
            .map(|spec: ProviderSetting| {
                let value = snapshot.values.get(&spec.key).cloned().unwrap_or_default();
                let cursor = value.chars().count();
                ProviderField {
                    secret_configured: snapshot.configured_secrets.contains(&spec.key),
                    spec,
                    original: value.clone(),
                    value,
                    cursor,
                }
            })
            .collect();
        let mut form = Self {
            fields,
            status: snapshot
                .read_only_reason
                .clone()
                .unwrap_or_else(|| "Ready".to_owned()),
            read_only_reason: snapshot.read_only_reason.clone(),
            ..Self::default()
        };
        form.selected = form.editable_indices().into_iter().next().unwrap_or(0);
        form
    }

    fn selected_field(&self) -> Option<&ProviderField> {
        self.is_editable(self.selected)
            .then(|| self.fields.get(self.selected))
            .flatten()
    }

    fn selected_field_mut(&mut self) -> Option<&mut ProviderField> {
        if !self.is_editable(self.selected) {
            return None;
        }
        self.fields.get_mut(self.selected)
    }

    fn is_editable(&self, index: usize) -> bool {
        self.read_only_reason.is_none()
            && self
                .fields
                .get(index)
                .is_some_and(|field| field.spec.kind != ProviderSettingKind::Action)
    }

    fn editable_indices(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|index| self.is_editable(*index))
            .collect()
    }

    fn read_only_indices(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|index| !self.is_editable(*index))
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let editable = self.editable_indices();
        if editable.is_empty() {
            return;
        }
        let position = editable
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = if delta < 0 {
            (position + editable.len() - 1) % editable.len()
        } else {
            (position + 1) % editable.len()
        };
        self.selected = editable[next];
    }

    fn changes(&self) -> BTreeMap<String, String> {
        self.fields
            .iter()
            .enumerate()
            .filter(|(index, field)| {
                self.is_editable(*index)
                    && field.value != field.original
                    && !(field.spec.kind == ProviderSettingKind::Secret
                        && field.value.is_empty()
                        && field.secret_configured)
            })
            .map(|(_, field)| (field.spec.key.to_owned(), field.value.clone()))
            .collect()
    }

    fn display_value(field: &ProviderField, editing: bool) -> String {
        if field.spec.kind == ProviderSettingKind::Secret {
            if editing && !field.value.is_empty() {
                with_cursor(&"*".repeat(field.value.chars().count()), field.cursor)
            } else if field.secret_configured || !field.value.is_empty() {
                "********".to_owned()
            } else {
                "<not configured>".to_owned()
            }
        } else if editing {
            with_cursor(&field.value, field.cursor)
        } else if field.value.is_empty() {
            "<blank>".to_owned()
        } else {
            field.value.clone()
        }
    }

    fn handle_edit_key(&mut self, code: KeyCode) {
        let Some(field) = self.selected_field_mut() else {
            return;
        };
        if field.spec.kind == ProviderSettingKind::Action {
            return;
        }
        match code {
            KeyCode::Left => field.cursor = field.cursor.saturating_sub(1),
            KeyCode::Right => {
                field.cursor = field
                    .cursor
                    .saturating_add(1)
                    .min(field.value.chars().count())
            }
            KeyCode::Home => field.cursor = 0,
            KeyCode::End => field.cursor = field.value.chars().count(),
            KeyCode::Backspace if field.cursor > 0 => {
                remove_char(&mut field.value, field.cursor - 1);
                field.cursor -= 1;
            }
            KeyCode::Delete if field.cursor < field.value.chars().count() => {
                remove_char(&mut field.value, field.cursor);
            }
            KeyCode::Char(character) => {
                insert_char(&mut field.value, field.cursor, character);
                field.cursor += 1;
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogFilter {
    All,
    Info,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LogTargetFilter {
    #[default]
    All,
    Runtime,
    Provider,
    Source,
    Daemon,
    Settings,
    Download,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TransferFilter {
    #[default]
    All,
    Active,
    Failed,
}

impl TransferFilter {
    fn matches(self, state: TransferState) -> bool {
        match self {
            Self::All => true,
            Self::Active => matches!(
                state,
                TransferState::Queued | TransferState::Running | TransferState::Paused
            ),
            Self::Failed => matches!(state, TransferState::Failed | TransferState::Interrupted),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Debug)]
struct DownloadForm {
    workers: usize,
}

#[derive(Clone, Debug, Default)]
struct AccountNameForm {
    value: String,
    cursor: usize,
    status: String,
}

impl AccountNameForm {
    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cursor = self
                    .cursor
                    .saturating_add(1)
                    .min(self.value.chars().count())
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            KeyCode::Backspace if self.cursor > 0 => {
                remove_char(&mut self.value, self.cursor - 1);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < self.value.chars().count() => {
                remove_char(&mut self.value, self.cursor);
            }
            KeyCode::Char(character) => {
                insert_char(&mut self.value, self.cursor, character);
                self.cursor += 1;
            }
            _ => {}
        }
    }
}

impl DownloadForm {
    fn new() -> Self {
        Self { workers: 1 }
    }
}

impl LogFilter {
    fn matches(self, level: LogLevel) -> bool {
        match self {
            Self::All => true,
            Self::Info => level == LogLevel::Info,
            Self::Error => level == LogLevel::Error,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Info => "Info",
            Self::Error => "Error",
        }
    }
}

impl LogTargetFilter {
    const ORDER: [Self; 7] = [
        Self::All,
        Self::Runtime,
        Self::Provider,
        Self::Source,
        Self::Daemon,
        Self::Settings,
        Self::Download,
    ];

    fn matches(self, target: &str) -> bool {
        match self {
            Self::All => true,
            Self::Runtime => target == "runtime",
            Self::Provider => target == "provider",
            Self::Source => target == "source",
            Self::Daemon => target == "daemon",
            Self::Settings => target == "settings",
            Self::Download => target == "download",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All targets",
            Self::Runtime => "Runtime",
            Self::Provider => "Provider",
            Self::Source => "Source",
            Self::Daemon => "Daemon",
            Self::Settings => "Settings",
            Self::Download => "Download",
        }
    }

    fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }
}

#[derive(Clone, Debug)]
enum PageLocation {
    SharedLinks,
    Folder(Box<RemoteItem>),
}

#[derive(Clone, Debug)]
struct FolderPage {
    location: PageLocation,
    items: Vec<RemoteItem>,
    selected: Option<usize>,
    offset: usize,
}

impl FolderPage {
    fn shared(items: Vec<RemoteItem>) -> Self {
        let selected = (!items.is_empty()).then_some(0);
        Self {
            location: PageLocation::SharedLinks,
            items,
            selected,
            offset: 0,
        }
    }

    fn folder(folder: RemoteItem, items: Vec<RemoteItem>) -> Self {
        let selected = (!items.is_empty()).then_some(0);
        Self {
            location: PageLocation::Folder(Box::new(folder)),
            items,
            selected,
            offset: 0,
        }
    }

    fn name(&self) -> &str {
        match &self.location {
            PageLocation::SharedLinks => "Sources",
            PageLocation::Folder(folder) => &folder.name,
        }
    }

    fn drive_folder(&self) -> Option<&RemoteItem> {
        match &self.location {
            PageLocation::SharedLinks => None,
            PageLocation::Folder(folder) => Some(folder),
        }
    }

    fn is_shared_root(&self) -> bool {
        matches!(self.location, PageLocation::SharedLinks)
    }
}

impl TransferState {
    fn sort_rank(self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Paused => 1,
            Self::Failed => 2,
            Self::Interrupted => 3,
            Self::Queued => 4,
            Self::Skipped => 5,
            Self::Completed => 6,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Queued => "WAIT",
            Self::Running => "RUN ",
            Self::Paused => "PAUSE",
            Self::Completed => "DONE",
            Self::Skipped => "SKIP",
            Self::Failed => "FAIL",
            Self::Interrupted => "STOP",
        }
    }
}

#[derive(Clone, Debug)]
struct TransferDisplay {
    id: TransferId,
    path: PathBuf,
    status: TransferState,
    written: u64,
    total: Option<u64>,
    bytes_per_second: f64,
    detail: Option<String>,
}

impl TransferDisplay {
    fn queued(id: TransferId, path: PathBuf, total: Option<u64>) -> Self {
        Self {
            id,
            path,
            status: TransferState::Queued,
            written: 0,
            total,
            bytes_per_second: 0.0,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct DownloadDisplay {
    job_id: Option<JobId>,
    state: Option<DownloadJobState>,
    transfers: BTreeMap<TransferId, TransferDisplay>,
    transfer_filter: TransferFilter,
    transfer_selected: usize,
    transfer_offset: usize,
    files: usize,
    overall_written: u64,
    partial_bytes: u64,
    items_processed: usize,
    items_total: usize,
    workers: usize,
    downloaded: usize,
    skipped: usize,
    failed: usize,
    pre_skipped: usize,
    skip_reasons: BTreeMap<String, usize>,
    last_issue: Option<String>,
    fatal: Option<String>,
}

impl DownloadDisplay {
    fn sorted_transfers(&self) -> Vec<&TransferDisplay> {
        let mut transfers: Vec<_> = self
            .transfers
            .values()
            .filter(|transfer| self.transfer_filter.matches(transfer.status))
            .collect();
        transfers.sort_by(|left, right| {
            left.status
                .sort_rank()
                .cmp(&right.status.sort_rank())
                .then_with(|| {
                    if left.status == TransferState::Completed {
                        right.id.cmp(&left.id)
                    } else {
                        left.id.cmp(&right.id)
                    }
                })
        });
        transfers
    }

    fn aggregate_speed(&self) -> f64 {
        self.transfers
            .values()
            .filter(|transfer| transfer.status == TransferState::Running)
            .map(|transfer| transfer.bytes_per_second)
            .sum()
    }

    fn status_count(&self, status: TransferState) -> usize {
        self.transfers
            .values()
            .filter(|transfer| transfer.status == status)
            .count()
    }

    fn move_transfer_selection(&mut self, delta: isize) {
        let maximum = self.sorted_transfers().len().saturating_sub(1);
        self.transfer_selected = if delta < 0 {
            self.transfer_selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.transfer_selected
                .saturating_add(delta as usize)
                .min(maximum)
        };
        self.transfer_offset = self.transfer_selected;
    }

    fn set_transfer_filter(&mut self, filter: TransferFilter) {
        self.transfer_filter = filter;
        self.transfer_selected = 0;
        self.transfer_offset = 0;
    }
}

#[derive(Clone, Debug)]
pub struct BrowserApp {
    pages: Vec<FolderPage>,
    source_sessions: BTreeMap<SourceSessionKey, SourceSession>,
    current_source: Option<SourceSessionKey>,
    mode: BrowserMode,
    message: String,
    page_step: usize,
    destination: PathBuf,
    download_target: Option<RemoteItem>,
    download_destination: Option<PathBuf>,
    download_form: Option<DownloadForm>,
    download: DownloadDisplay,
    active_tab: AppTab,
    navigation_tab: AppTab,
    navigation_focus: NavigationFocus,
    account_selected: usize,
    account_form: Option<AccountNameForm>,
    active_accounts: BTreeMap<ProviderId, ProfileId>,
    logs_focus: PanelFocus,
    network_settings: NetworkSettings,
    settings_form: Option<SettingsForm>,
    provider_form: Option<ProviderSettingsForm>,
    account_detail_open: bool,
    global_settings_open: bool,
    settings_path: PathBuf,
    provider_name: String,
    profile_name: String,
    source_name: String,
    provider_status: String,
    provider_settings_count: usize,
    provider_choices: Vec<ProviderChoice>,
    active_provider: usize,
    provider_selector: Option<ProviderSelector>,
    logs: Vec<LogEntry>,
    log_filter: LogFilter,
    log_target_filter: LogTargetFilter,
    log_offset_from_bottom: usize,
    quit_confirming: bool,
    daemon_mode: bool,
    toast: Option<Toast>,
}

#[derive(Clone, Debug)]
pub enum BrowserOutcome {
    Quit,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceSessionKey {
    identity: ProviderIdentity,
    source_id: SourceId,
}

#[derive(Clone, Debug)]
struct SourceSession {
    pages: Vec<FolderPage>,
    message: String,
}

#[derive(Clone, Debug)]
struct SourceChoice {
    id: SourceId,
    name: String,
    root: RemoteItem,
}

#[derive(Clone, Debug)]
struct ProviderChoice {
    id: ProviderId,
    name: String,
    profile: ProfileId,
    profile_name: String,
    status: String,
    settings_count: usize,
    sources: Vec<SourceChoice>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProviderSelector {
    provider: usize,
    source: usize,
}

struct ProviderActivation {
    restored: bool,
    root_to_load: Option<RemoteItem>,
}

pub struct BrowserProvider {
    id: ProviderId,
    name: String,
    identity: ProviderIdentity,
    profile_name: String,
    status: ProviderStatus,
    available: bool,
    settings_schema: Vec<ProviderSetting>,
    sources: Vec<RemoteSource>,
    source_error: Option<String>,
    settings: ProviderSettingsSnapshot,
}

impl BrowserProvider {
    pub fn new(entry: ProviderCatalogEntry) -> Self {
        Self {
            id: entry.id,
            name: entry.display_name,
            identity: entry.identity,
            profile_name: entry.profile_name,
            status: entry.status,
            available: entry.available,
            settings_schema: entry.settings_schema,
            sources: Vec::new(),
            source_error: None,
            settings: entry.settings,
        }
    }

    fn update(&mut self, entry: ProviderCatalogEntry) {
        self.id = entry.id;
        self.name = entry.display_name;
        self.identity = entry.identity;
        self.profile_name = entry.profile_name;
        self.status = entry.status;
        self.available = entry.available;
        self.settings_schema = entry.settings_schema;
        self.settings = entry.settings;
    }

    pub fn id(&self) -> &ProviderId {
        &self.id
    }

    pub fn is_ready(&self) -> bool {
        self.available
    }

    pub fn identity(&self) -> ProviderIdentity {
        self.identity.clone()
    }

    pub fn status(&self) -> ProviderStatus {
        self.status.clone()
    }

    fn status_label(&self) -> String {
        match &self.status {
            ProviderStatus::Disabled => "Disabled".to_owned(),
            ProviderStatus::NeedsConfig => "NeedsConfig".to_owned(),
            ProviderStatus::Connecting => "Connecting".to_owned(),
            ProviderStatus::Ready if self.source_error.is_none() => "Ready".to_owned(),
            ProviderStatus::Ready => format!(
                "SourceError: {}",
                self.source_error.as_deref().unwrap_or("unknown")
            ),
            ProviderStatus::ConfigError(message) => format!("ConfigError: {message}"),
            ProviderStatus::ConnectionError(message) => format!("ConnectionError: {message}"),
        }
    }

    async fn refresh_sources(&mut self, client: &dyn PanClient) {
        if !self.available {
            self.sources.clear();
            self.source_error = None;
            return;
        }
        match client.sources(&self.identity).await {
            Ok(sources) => {
                self.sources = sources;
                self.source_error = None;
            }
            Err(error) => {
                self.sources.clear();
                self.source_error = Some(format!("{error:#}"));
            }
        }
    }
}

pub struct BrowserRuntime {
    settings: PathBuf,
    network: NetworkSettings,
    destination: PathBuf,
    options: DownloadOptions,
    client: Arc<dyn PanClient>,
}

impl BrowserRuntime {
    pub fn new(
        client: Arc<dyn PanClient>,
        settings: PathBuf,
        network: NetworkSettings,
        destination: PathBuf,
        options: DownloadOptions,
    ) -> Self {
        Self {
            settings,
            network,
            destination,
            options,
            client,
        }
    }
}

impl BrowserApp {
    pub fn new(
        shared_items: Vec<RemoteItem>,
        destination: PathBuf,
        network_settings: NetworkSettings,
    ) -> Self {
        Self {
            pages: vec![FolderPage::shared(shared_items)],
            source_sessions: BTreeMap::new(),
            current_source: None,
            mode: BrowserMode::Browsing,
            message: "Ready".to_owned(),
            page_step: 10,
            destination,
            download_target: None,
            download_destination: None,
            download_form: None,
            download: DownloadDisplay::default(),
            active_tab: AppTab::Files,
            navigation_tab: AppTab::Files,
            navigation_focus: NavigationFocus::Content,
            account_selected: 0,
            account_form: None,
            active_accounts: BTreeMap::new(),
            logs_focus: PanelFocus::Upper,
            network_settings,
            settings_form: None,
            provider_form: None,
            account_detail_open: false,
            global_settings_open: false,
            settings_path: PathBuf::from("~/.net-pan/settings.toml"),
            provider_name: "Cloud Provider".to_owned(),
            profile_name: "default".to_owned(),
            source_name: "-".to_owned(),
            provider_status: "NeedsConfig".to_owned(),
            provider_settings_count: 0,
            provider_choices: Vec::new(),
            active_provider: 0,
            provider_selector: None,
            logs: Vec::new(),
            log_filter: LogFilter::All,
            log_target_filter: LogTargetFilter::All,
            log_offset_from_bottom: 0,
            quit_confirming: false,
            daemon_mode: false,
            toast: None,
        }
    }

    fn show_toast(&mut self, kind: ToastKind, message: impl Into<String>) {
        self.toast = Some(Toast {
            kind,
            message: message.into(),
            expires_at: Instant::now() + Duration::from_secs(2),
        });
    }

    fn expire_toast(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| Instant::now() >= toast.expires_at)
        {
            self.toast = None;
        }
    }

    fn set_provider_catalog(&mut self, providers: &[BrowserProvider], active: usize) {
        self.active_provider = active;
        self.refresh_provider_catalog(providers);
        self.refresh_provider_form(&providers[active]);
        let choice = &self.provider_choices[active];
        self.source_name = "-".to_owned();
        self.pages = vec![FolderPage::shared(
            choice
                .sources
                .iter()
                .map(|source| source.root.clone())
                .collect(),
        )];
        self.remember_active_account();
        self.sync_account_selection();
    }

    fn refresh_provider_catalog(&mut self, providers: &[BrowserProvider]) {
        self.provider_choices = providers
            .iter()
            .map(|runtime| ProviderChoice {
                id: runtime.id.clone(),
                name: runtime.name.clone(),
                profile: runtime.identity().profile_id,
                profile_name: runtime.profile_name.clone(),
                status: runtime.status_label(),
                settings_count: runtime.settings_schema.len(),
                sources: runtime
                    .sources
                    .iter()
                    .map(|source| SourceChoice {
                        id: source.id.clone(),
                        name: source.name.clone(),
                        root: source.root.clone(),
                    })
                    .collect(),
            })
            .collect();
        let choice = &self.provider_choices[self.active_provider];
        self.provider_name = choice.name.clone();
        self.profile_name = choice.profile_name.clone();
        self.provider_status = choice.status.clone();
        self.provider_settings_count = choice.settings_count;
        if let Some(key) = &self.current_source
            && let Some(source) = choice
                .sources
                .iter()
                .find(|source| source.id == key.source_id)
        {
            self.source_name = source.name.clone();
        }
        self.sync_account_selection();
    }

    fn current_provider_id(&self) -> Option<&ProviderId> {
        self.provider_choices
            .get(self.active_provider)
            .map(|choice| &choice.id)
    }

    fn account_indices(&self) -> Vec<usize> {
        let Some(provider_id) = self.current_provider_id() else {
            return Vec::new();
        };
        self.provider_choices
            .iter()
            .enumerate()
            .filter_map(|(index, choice)| (&choice.id == provider_id).then_some(index))
            .collect()
    }

    fn sync_account_selection(&mut self) {
        let indices = self.account_indices();
        self.account_selected = indices
            .iter()
            .position(|index| *index == self.active_provider)
            .unwrap_or(0)
            .min(indices.len().saturating_sub(1));
    }

    fn move_account_selection(&mut self, delta: isize) {
        let count = self.account_indices().len();
        if count == 0 {
            self.account_selected = 0;
            return;
        }
        self.account_selected = if delta < 0 {
            (self.account_selected + count - 1) % count
        } else {
            (self.account_selected + 1) % count
        };
    }

    fn selected_account_provider_index(&self) -> Option<usize> {
        self.account_indices().get(self.account_selected).copied()
    }

    fn remember_active_account(&mut self) {
        if let Some(choice) = self.provider_choices.get(self.active_provider) {
            self.active_accounts
                .insert(choice.id.clone(), choice.profile.clone());
        }
    }

    fn switch_provider_type(&mut self, delta: isize) -> Option<usize> {
        let mut provider_ids = Vec::<ProviderId>::new();
        for choice in &self.provider_choices {
            if !provider_ids.contains(&choice.id) {
                provider_ids.push(choice.id.clone());
            }
        }
        let current = self.current_provider_id()?.clone();
        let current_index = provider_ids.iter().position(|id| id == &current)?;
        let next_index = if delta < 0 {
            (current_index + provider_ids.len() - 1) % provider_ids.len()
        } else {
            (current_index + 1) % provider_ids.len()
        };
        let next_id = &provider_ids[next_index];
        let remembered = self.active_accounts.get(next_id);
        self.provider_choices
            .iter()
            .position(|choice| {
                &choice.id == next_id
                    && remembered
                        .map(|profile| &choice.profile == profile)
                        .unwrap_or(true)
            })
            .or_else(|| {
                self.provider_choices
                    .iter()
                    .position(|choice| &choice.id == next_id)
            })
    }

    fn open_account_form(&mut self) {
        self.account_form = Some(AccountNameForm {
            value: String::new(),
            cursor: 0,
            status: "Enter an Account name (1-64 characters).".to_owned(),
        });
    }

    fn open_global_settings(&mut self) {
        self.global_settings_open = true;
        if self.settings_form.is_none() {
            self.settings_form = Some(SettingsForm::new(&self.destination, &self.network_settings));
        }
    }

    fn open_account_detail(&mut self) {
        self.account_detail_open = true;
    }

    fn open_provider_settings_page(&mut self) {
        self.active_tab = AppTab::Settings;
        self.navigation_tab = AppTab::Settings;
        self.navigation_focus = NavigationFocus::Content;
        self.account_detail_open = false;
    }

    fn provider_settings_visible(&self) -> bool {
        self.active_tab == AppTab::Settings
            || (self.active_tab == AppTab::Accounts && self.account_detail_open)
    }

    fn account_config_path(&self) -> PathBuf {
        let root = self
            .settings_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("~/.net-pan"));
        self.provider_choices
            .get(self.active_provider)
            .map(|choice| {
                root.join("providers")
                    .join(choice.id.as_str())
                    .join(choice.profile.as_str())
                    .join("config.toml")
            })
            .unwrap_or_else(|| root.join("providers"))
    }

    fn refresh_provider_form(&mut self, runtime: &BrowserProvider) {
        self.provider_form = Some(ProviderSettingsForm::from_snapshot(
            runtime.settings_schema.clone(),
            &runtime.settings,
        ));
    }

    fn open_provider_selector(&mut self) {
        let source = self
            .current_source
            .as_ref()
            .and_then(|key| {
                self.provider_choices[self.active_provider]
                    .sources
                    .iter()
                    .position(|source| source.id == key.source_id)
            })
            .unwrap_or(0);
        self.provider_selector = Some(ProviderSelector {
            provider: self.active_provider,
            source,
        });
    }

    fn move_source_selection(&mut self, delta: isize) {
        let Some(selector) = self.provider_selector.as_mut() else {
            return;
        };
        let count = self.provider_choices[selector.provider].sources.len();
        if count == 0 {
            selector.source = 0;
            return;
        }
        selector.source = if delta < 0 {
            (selector.source + count - 1) % count
        } else {
            (selector.source + 1) % count
        };
    }

    fn save_current_source_session(&mut self) {
        let Some(key) = self.current_source.clone() else {
            return;
        };
        self.source_sessions.insert(
            key,
            SourceSession {
                pages: std::mem::replace(&mut self.pages, vec![FolderPage::shared(Vec::new())]),
                message: self.message.clone(),
            },
        );
    }

    fn activate_selection(
        &mut self,
        provider_index: usize,
        source_index: Option<usize>,
    ) -> ProviderActivation {
        self.save_current_source_session();
        let choice = self.provider_choices[provider_index].clone();
        self.active_provider = provider_index;
        self.provider_name = choice.name;
        self.profile_name = choice.profile_name.clone();
        self.provider_status = choice.status;
        self.provider_settings_count = choice.settings_count;
        self.provider_selector = None;
        self.remember_active_account();
        self.sync_account_selection();
        let Some(source) = source_index
            .and_then(|index| choice.sources.get(index))
            .cloned()
        else {
            self.current_source = None;
            self.source_name = "-".to_owned();
            self.pages = vec![FolderPage::shared(
                choice
                    .sources
                    .iter()
                    .map(|source| source.root.clone())
                    .collect(),
            )];
            self.message = self.provider_status.clone();
            return ProviderActivation {
                restored: false,
                root_to_load: None,
            };
        };
        let key = SourceSessionKey {
            identity: ProviderIdentity::new(choice.id, choice.profile),
            source_id: source.id,
        };
        self.current_source = Some(key.clone());
        self.source_name = source.name;
        if let Some(session) = self.source_sessions.remove(&key) {
            self.pages = session.pages;
            self.message = session.message;
            ProviderActivation {
                restored: true,
                root_to_load: None,
            }
        } else {
            self.pages = vec![FolderPage::shared(vec![source.root.clone()])];
            self.message = format!("Loading {}...", self.source_name);
            ProviderActivation {
                restored: false,
                root_to_load: Some(source.root),
            }
        }
    }

    fn source_index_for_item(&self, item: &RemoteItem) -> Option<usize> {
        self.provider_choices[self.active_provider]
            .sources
            .iter()
            .position(|source| source.id == item.reference.source_id)
    }

    fn current(&self) -> &FolderPage {
        self.pages.last().expect("browser always has a root page")
    }

    fn current_mut(&mut self) -> &mut FolderPage {
        self.pages
            .last_mut()
            .expect("browser always has a root page")
    }

    fn selected_item(&self) -> Option<&RemoteItem> {
        let page = self.current();
        page.selected.and_then(|index| page.items.get(index))
    }

    fn selected_download_target(&self) -> Option<&RemoteItem> {
        match self.browser_layer() {
            BrowserLayer::SharedLinks => self.selected_item().filter(|item| item.is_folder()),
            BrowserLayer::DriveFolder => self
                .selected_item()
                .filter(|item| item.is_folder() || item.can_download()),
        }
    }

    fn download_target(&self) -> Option<&RemoteItem> {
        self.download_target.as_ref()
    }

    fn download_target_breadcrumbs(&self) -> String {
        self.download_target()
            .map(|target| {
                let breadcrumbs = self.breadcrumbs();
                if breadcrumbs.is_empty() {
                    target.name.clone()
                } else {
                    format!("{breadcrumbs} / {}", target.name)
                }
            })
            .unwrap_or_else(|| self.breadcrumbs())
    }

    fn download_target_destination(&self) -> PathBuf {
        self.download_destination
            .clone()
            .unwrap_or_else(|| self.destination.clone())
    }

    fn selected_target_destination(&self, root: &Path, target: &RemoteItem) -> PathBuf {
        let mut path = root.to_path_buf();
        for page in self.pages.iter().skip(1) {
            if let Some(folder) = page.drive_folder() {
                path.push(safe_name(&folder.name, folder.reference.object_id.as_str()));
            }
        }
        target_destination(&path, target)
    }

    fn download_parent_destination(&self) -> PathBuf {
        self.download_target_destination()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.destination.clone())
    }

    fn absolute_event_path(&self, relative: &Path) -> PathBuf {
        self.download_parent_destination().join(relative)
    }

    fn planning_message(&self) -> &'static str {
        match self.download_target() {
            Some(target) if target.is_folder() => "Scanning selected folder recursively...",
            Some(_) => "Preparing selected file...",
            None => "Preparing download...",
        }
    }

    fn current_drive_folder(&self) -> Option<&RemoteItem> {
        self.current().drive_folder()
    }

    fn level_count_label(&self) -> String {
        let count = self.current().items.len();
        if self.current().is_shared_root() {
            format!("{count} sources")
        } else {
            format!("{count} direct items")
        }
    }

    fn browser_layer(&self) -> BrowserLayer {
        if self.current().is_shared_root() {
            BrowserLayer::SharedLinks
        } else {
            BrowserLayer::DriveFolder
        }
    }

    fn breadcrumbs(&self) -> String {
        self.breadcrumb_components().join(" / ")
    }

    fn breadcrumb_components(&self) -> Vec<&str> {
        self.pages.iter().skip(1).map(FolderPage::name).collect()
    }

    fn select_previous(&mut self) {
        let page = self.current_mut();
        if page.items.is_empty() {
            page.selected = None;
            return;
        }
        page.selected = Some(match page.selected.unwrap_or(0) {
            0 => page.items.len() - 1,
            index => index - 1,
        });
    }

    fn select_next(&mut self) {
        let page = self.current_mut();
        if page.items.is_empty() {
            page.selected = None;
            return;
        }
        page.selected = Some((page.selected.unwrap_or(0) + 1) % page.items.len());
    }

    fn select_first(&mut self) {
        self.current_mut().selected = (!self.current().items.is_empty()).then_some(0);
    }

    fn select_last(&mut self) {
        let len = self.current().items.len();
        self.current_mut().selected = (len > 0).then_some(len - 1);
    }

    fn page_up(&mut self) {
        let step = self.page_step;
        let page = self.current_mut();
        if let Some(index) = page.selected {
            page.selected = Some(index.saturating_sub(step));
        }
    }

    fn page_down(&mut self) {
        let step = self.page_step;
        let page = self.current_mut();
        if let Some(index) = page.selected {
            page.selected = Some((index + step).min(page.items.len().saturating_sub(1)));
        }
    }

    fn replace_current_items(&mut self, items: Vec<RemoteItem>) {
        let selected_id = self
            .selected_item()
            .map(|item| item.reference.object_id.clone());
        let selected = selected_id
            .as_ref()
            .and_then(|id| {
                items
                    .iter()
                    .position(|item| &item.reference.object_id == id)
            })
            .or_else(|| (!items.is_empty()).then_some(0));
        let page = self.current_mut();
        page.items = items;
        page.selected = selected;
        page.offset = page.offset.min(page.items.len().saturating_sub(1));
    }

    fn enter_loaded(&mut self, folder: RemoteItem, children: Vec<RemoteItem>) {
        self.pages.push(FolderPage::folder(folder, children));
        self.message = "Folder loaded".to_owned();
    }

    fn go_back(&mut self) {
        if self.pages.len() > 1 {
            self.pages.pop();
            self.message = "Returned to parent".to_owned();
        }
    }

    fn request_download(&mut self) {
        let Some(target) = self.selected_download_target().cloned() else {
            return;
        };
        self.download_form = Some(DownloadForm::new());
        self.download_destination =
            Some(self.selected_target_destination(&self.destination, &target));
        self.download_target = Some(target);
        self.mode = BrowserMode::Confirming;
    }

    fn cancel_download(&mut self) {
        self.mode = BrowserMode::Browsing;
        self.message = "Download cancelled".to_owned();
        self.download_target = None;
        self.download_destination = None;
        self.download_form = None;
    }

    fn begin_download(&mut self) {
        self.mode = BrowserMode::Scanning;
        self.message = self.planning_message().to_owned();
        self.download = DownloadDisplay::default();
        self.download_form = None;
    }

    fn return_to_browser(&mut self) {
        self.mode = BrowserMode::Browsing;
        self.message = "Ready".to_owned();
        self.download_target = None;
        self.download_destination = None;
        self.download_form = None;
    }

    fn handle_form_edit_key(&mut self, code: KeyCode) {
        let Some(form) = self.download_form.as_mut() else {
            return;
        };
        match code {
            KeyCode::Left | KeyCode::Char('-') => {
                form.workers = form.workers.saturating_sub(1).max(1);
            }
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                form.workers = form.workers.saturating_add(1).min(5);
            }
            KeyCode::Char(value @ '1'..='5') => {
                form.workers = value.to_digit(10).unwrap_or(1) as usize;
            }
            _ => {}
        }
    }

    fn move_navigation(&mut self, forward: bool) {
        self.navigation_tab = self.navigation_tab.adjacent(if forward { 1 } else { -1 });
    }

    fn activate_navigation(&mut self) {
        self.active_tab = self.navigation_tab;
        self.navigation_focus = NavigationFocus::Content;
        self.account_detail_open = false;
    }

    fn handle_settings_edit_key(&mut self, code: KeyCode) {
        let Some(form) = self.settings_form.as_mut() else {
            return;
        };
        let value_len = form.selected_value().chars().count();
        match code {
            KeyCode::Left => form.cursor = form.cursor.saturating_sub(1),
            KeyCode::Right => {
                form.cursor = form.cursor.saturating_add(1).min(value_len);
            }
            KeyCode::Home => form.cursor = 0,
            KeyCode::End => form.cursor = value_len,
            KeyCode::Backspace if form.cursor > 0 => {
                let cursor = form.cursor;
                remove_char(form.selected_value_mut(), cursor - 1);
                form.cursor -= 1;
            }
            KeyCode::Delete if form.cursor < value_len => {
                let cursor = form.cursor;
                remove_char(form.selected_value_mut(), cursor);
            }
            KeyCode::Char(character) => {
                let cursor = form.cursor;
                insert_char(form.selected_value_mut(), cursor, character);
                form.cursor += 1;
            }
            _ => {}
        }
    }

    fn filtered_log_count(&self) -> usize {
        self.logs
            .iter()
            .filter(|entry| {
                self.log_filter.matches(entry.level)
                    && self.log_target_filter.matches(&entry.target)
            })
            .count()
    }

    #[cfg(test)]
    fn handle_log_key(&mut self, code: &KeyCode) {
        self.handle_log_intent(keymap::logs::intent(*code));
    }

    fn handle_log_intent(&mut self, intent: Option<keymap::logs::Intent>) {
        let max_offset = self.filtered_log_count().saturating_sub(1);
        match intent {
            Some(keymap::logs::Intent::Previous) => {
                self.log_offset_from_bottom = self
                    .log_offset_from_bottom
                    .saturating_add(1)
                    .min(max_offset);
            }
            Some(keymap::logs::Intent::Next) => {
                self.log_offset_from_bottom = self.log_offset_from_bottom.saturating_sub(1);
            }
            Some(keymap::logs::Intent::PageUp) => {
                self.log_offset_from_bottom = self
                    .log_offset_from_bottom
                    .saturating_add(self.page_step)
                    .min(max_offset);
            }
            Some(keymap::logs::Intent::PageDown) => {
                self.log_offset_from_bottom =
                    self.log_offset_from_bottom.saturating_sub(self.page_step);
            }
            Some(keymap::logs::Intent::First) => self.log_offset_from_bottom = max_offset,
            Some(keymap::logs::Intent::Last) => self.log_offset_from_bottom = 0,
            Some(keymap::logs::Intent::All) => self.set_log_filter(LogFilter::All),
            Some(keymap::logs::Intent::Info) => self.set_log_filter(LogFilter::Info),
            Some(keymap::logs::Intent::Error) => self.set_log_filter(LogFilter::Error),
            Some(keymap::logs::Intent::CycleTarget) => {
                self.log_target_filter = self.log_target_filter.next();
                self.log_offset_from_bottom = 0;
            }
            Some(keymap::logs::Intent::TogglePane) => {
                self.logs_focus = match self.logs_focus {
                    PanelFocus::Upper => PanelFocus::Lower,
                    PanelFocus::Lower => PanelFocus::Upper,
                };
            }
            None => {}
        }
    }

    fn set_log_filter(&mut self, filter: LogFilter) {
        self.log_filter = filter;
        self.log_offset_from_bottom = 0;
    }

    fn apply_download_event(&mut self, event: DownloadEvent) {
        match event {
            DownloadEvent::Scanning {
                job_id,
                provider: _,
            } => {
                self.mode = BrowserMode::Scanning;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Scanning);
                self.message = self.planning_message().to_owned();
            }
            DownloadEvent::PlanReady {
                job_id,
                provider: _,
                files,
                items,
                bytes: _,
                pre_skipped,
                workers,
            } => {
                self.mode = BrowserMode::Downloading;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Running);
                self.download.files = files;
                self.download.items_total = items;
                self.download.workers = workers;
                self.download.pre_skipped = pre_skipped;
                self.message = format!("Download plan ready: {items} items, {workers} workers");
            }
            DownloadEvent::TransferQueued {
                job_id,
                provider: _,
                transfer_id,
                path,
                size,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| TransferDisplay::queued(transfer_id, path.clone(), size));
                if !transfer.status.is_terminal() {
                    transfer.path = path;
                    transfer.total = size;
                }
            }
            DownloadEvent::FileStarted {
                job_id,
                provider: _,
                transfer_id,
                index: _,
                total: _,
                path,
                size,
            } => {
                self.mode = BrowserMode::Downloading;
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| TransferDisplay::queued(transfer_id, path.clone(), size));
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path;
                transfer.status = TransferState::Running;
                transfer.written = 0;
                transfer.total = size;
                transfer.bytes_per_second = 0.0;
                transfer.detail = None;
            }
            DownloadEvent::Progress {
                job_id,
                provider: _,
                transfer_id,
                index: _,
                total: _,
                path,
                file_written,
                file_total,
                bytes_per_second,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| {
                        TransferDisplay::queued(transfer_id, path.clone(), file_total)
                    });
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path;
                transfer.status = TransferState::Running;
                transfer.written = file_written;
                transfer.total = file_total;
                transfer.bytes_per_second = bytes_per_second;
            }
            DownloadEvent::Retrying {
                job_id,
                provider: _,
                transfer_id,
                path,
                offset,
                attempt,
                max_attempts,
                error,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| TransferDisplay::queued(transfer_id, path.clone(), None));
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path;
                transfer.status = TransferState::Running;
                transfer.written = offset;
                transfer.bytes_per_second = 0.0;
                transfer.detail = Some(format!(
                    "Stream retry {attempt}/{max_attempts} at byte {offset}: {error}"
                ));
                self.message = format!(
                    "Retrying transfer {} ({attempt}/{max_attempts})",
                    transfer_id.0
                );
            }
            DownloadEvent::FileSkipped {
                job_id,
                provider: _,
                transfer_id,
                path,
                reason,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| TransferDisplay::queued(transfer_id, path.clone(), None));
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path.clone();
                transfer.status = TransferState::Skipped;
                transfer.bytes_per_second = 0.0;
                transfer.detail = Some(reason.clone());
                self.download.skipped += 1;
                *self
                    .download
                    .skip_reasons
                    .entry(reason.clone())
                    .or_default() += 1;
                self.download.items_processed = self.download.items_processed.saturating_add(1);
                self.download.last_issue = Some(format!("Skipped {}: {reason}", path.display()));
            }
            DownloadEvent::FileCompleted {
                job_id,
                provider: _,
                transfer_id,
                path,
                bytes,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| {
                        TransferDisplay::queued(transfer_id, path.clone(), Some(bytes))
                    });
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path.clone();
                transfer.status = TransferState::Completed;
                transfer.written = bytes;
                transfer.total = transfer.total.or(Some(bytes));
                transfer.bytes_per_second = 0.0;
                self.download.downloaded += 1;
                self.download.items_processed = self.download.items_processed.saturating_add(1);
                self.download.overall_written = self.download.overall_written.saturating_add(bytes);
                self.download.last_issue = Some(format!(
                    "Completed {} ({})",
                    path.display(),
                    format_bytes(bytes)
                ));
            }
            DownloadEvent::FileFailed {
                job_id,
                provider: _,
                transfer_id,
                path,
                bytes,
                error,
            } => {
                self.download.job_id = Some(job_id);
                let transfer = self
                    .download
                    .transfers
                    .entry(transfer_id)
                    .or_insert_with(|| TransferDisplay::queued(transfer_id, path.clone(), None));
                if transfer.status.is_terminal() {
                    return;
                }
                transfer.path = path.clone();
                transfer.status = TransferState::Failed;
                transfer.written = bytes;
                transfer.bytes_per_second = 0.0;
                transfer.detail = Some(error.clone());
                self.download.failed += 1;
                self.download.items_processed = self.download.items_processed.saturating_add(1);
                self.download.overall_written = self.download.overall_written.saturating_add(bytes);
                self.download.partial_bytes = self.download.partial_bytes.saturating_add(bytes);
                self.download.last_issue = Some(format!(
                    "Failed {} after {}: {error}",
                    path.display(),
                    format_bytes(bytes)
                ));
            }
            DownloadEvent::Finished {
                job_id,
                provider: _,
                summary,
            } => {
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Finished);
                self.finish_download(summary);
            }
            DownloadEvent::Fatal {
                job_id,
                provider: _,
                error,
            } => {
                self.mode = BrowserMode::Failed;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Failed);
                self.download.fatal = Some(error);
                self.message = "Download failed".to_owned();
            }
            DownloadEvent::Paused {
                job_id,
                provider: _,
            } => {
                self.mode = BrowserMode::Paused;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Paused);
                for transfer in self.download.transfers.values_mut() {
                    if !transfer.status.is_terminal() {
                        transfer.status = TransferState::Paused;
                        transfer.bytes_per_second = 0.0;
                        transfer.detail = Some("Paused by user; partial file retained".to_owned());
                    }
                }
                self.message = format!("Job {} paused", job_id.0);
            }
            DownloadEvent::Resumed {
                job_id,
                provider: _,
            } => {
                self.mode = BrowserMode::Scanning;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Queued);
                for transfer in self.download.transfers.values_mut() {
                    if transfer.status == TransferState::Paused {
                        transfer.status = TransferState::Queued;
                        transfer.detail = Some("Resuming from retained partial file".to_owned());
                    }
                }
                self.message = format!("Job {} resumed", job_id.0);
            }
            DownloadEvent::Cancelled {
                job_id,
                provider: _,
            } => {
                self.mode = BrowserMode::Failed;
                self.download.job_id = Some(job_id);
                self.download.state = Some(DownloadJobState::Cancelled);
                for transfer in self.download.transfers.values_mut() {
                    if !transfer.status.is_terminal() {
                        transfer.status = TransferState::Interrupted;
                        transfer.bytes_per_second = 0.0;
                        transfer.detail = Some("Stopped by user; partial file retained".to_owned());
                    }
                }
                self.download.failed = self
                    .download
                    .status_count(TransferState::Failed)
                    .saturating_add(self.download.status_count(TransferState::Interrupted));
                self.message = "Download stopped by user".to_owned();
            }
        }
    }

    fn finish_download(&mut self, summary: DownloadSummary) {
        self.mode = BrowserMode::Finished;
        self.download.downloaded = summary.downloaded;
        self.download.skipped = summary.skipped;
        self.download.failed = summary.failed;
        self.download.overall_written = summary.bytes_written.saturating_add(summary.partial_bytes);
        self.download.partial_bytes = summary.partial_bytes;
        self.message = "Download finished".to_owned();
    }

    fn apply_download_snapshots(&mut self, snapshots: &[DownloadJobSnapshot]) {
        let selected = self
            .download
            .job_id
            .and_then(|job_id| snapshots.iter().find(|snapshot| snapshot.id == job_id))
            .or_else(|| snapshots.iter().max_by_key(|snapshot| snapshot.id));
        let Some(snapshot) = selected else {
            return;
        };
        if self.download.job_id == Some(snapshot.id)
            && self
                .download
                .state
                .is_some_and(DownloadJobState::is_terminal)
            && !snapshot.state.is_terminal()
        {
            return;
        }
        let mut display = DownloadDisplay {
            job_id: Some(snapshot.id),
            state: Some(snapshot.state),
            files: snapshot.transfers.len(),
            items_total: snapshot.transfers.len(),
            workers: snapshot.request.options.concurrency,
            fatal: snapshot.error.clone(),
            ..DownloadDisplay::default()
        };
        for transfer in snapshot.transfers.values() {
            display.transfers.insert(
                transfer.id,
                TransferDisplay {
                    id: transfer.id,
                    path: transfer.path.clone(),
                    status: transfer.state,
                    written: transfer.written,
                    total: transfer.size,
                    bytes_per_second: transfer.bytes_per_second,
                    detail: transfer.error.clone(),
                },
            );
        }
        if self.download.job_id == Some(snapshot.id) {
            for transfer in self.download.transfers.values() {
                if transfer.status.is_terminal()
                    && display
                        .transfers
                        .get(&transfer.id)
                        .is_some_and(|current| !current.status.is_terminal())
                {
                    display.transfers.insert(transfer.id, transfer.clone());
                }
            }
        }
        display.items_processed = display
            .transfers
            .values()
            .filter(|transfer| transfer.status.is_terminal())
            .count();
        display.downloaded = display.status_count(TransferState::Completed);
        display.skipped = display.status_count(TransferState::Skipped);
        display.failed = display
            .status_count(TransferState::Failed)
            .saturating_add(display.status_count(TransferState::Interrupted));
        display.overall_written = display
            .transfers
            .values()
            .filter(|transfer| {
                matches!(
                    transfer.status,
                    TransferState::Completed | TransferState::Failed
                )
            })
            .map(|transfer| transfer.written)
            .sum();
        display.partial_bytes = display
            .transfers
            .values()
            .filter(|transfer| transfer.status == TransferState::Failed)
            .map(|transfer| transfer.written)
            .sum();
        if let Some(summary) = &snapshot.summary {
            display.downloaded = summary.downloaded;
            display.skipped = summary.skipped;
            display.failed = summary.failed;
            display.overall_written = summary.bytes_written.saturating_add(summary.partial_bytes);
            display.partial_bytes = summary.partial_bytes;
        }
        self.download_target = Some(snapshot.request.target.clone());
        self.download_destination = Some(target_destination(
            &snapshot.request.destination,
            &snapshot.request.target,
        ));
        self.download = display;
        match snapshot.state {
            DownloadJobState::Queued | DownloadJobState::Scanning => {
                self.mode = BrowserMode::Scanning;
                self.message = format!("Job {} is planning", snapshot.id.0);
            }
            DownloadJobState::Running => {
                self.mode = BrowserMode::Downloading;
                self.message = format!("Job {} is downloading", snapshot.id.0);
            }
            DownloadJobState::Paused => {
                self.mode = BrowserMode::Paused;
                self.message = format!("Job {} is paused", snapshot.id.0);
            }
            DownloadJobState::Finished => {
                self.mode = BrowserMode::Finished;
                self.message = format!("Job {} finished", snapshot.id.0);
            }
            DownloadJobState::Failed => {
                self.mode = BrowserMode::Failed;
                self.message = format!("Job {} failed", snapshot.id.0);
            }
            DownloadJobState::Cancelled => {
                self.mode = BrowserMode::Failed;
                self.message = format!("Job {} was cancelled", snapshot.id.0);
            }
            DownloadJobState::Interrupted => {
                self.mode = BrowserMode::Failed;
                self.message = format!("Job {} is interrupted", snapshot.id.0);
            }
        }
    }
}

fn target_destination(root: &Path, target: &RemoteItem) -> PathBuf {
    root.join(safe_name(&target.name, target.reference.object_id.as_str()))
}

fn char_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn insert_char(value: &mut String, char_index: usize, character: char) {
    let byte_index = char_byte_index(value, char_index);
    value.insert(byte_index, character);
}

fn remove_char(value: &mut String, char_index: usize) {
    let start = char_byte_index(value, char_index);
    let end = char_byte_index(value, char_index.saturating_add(1));
    if start < end {
        value.replace_range(start..end, "");
    }
}

fn with_cursor(value: &str, char_index: usize) -> String {
    let mut displayed = value.to_owned();
    let byte_index = char_byte_index(&displayed, char_index);
    displayed.insert(byte_index, '▏');
    displayed
}

fn validate_account_name(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    let length = value.chars().count();
    if length == 0 {
        return Err("Account name cannot be blank.");
    }
    if length > 64 {
        return Err("Account name must be 64 characters or fewer.");
    }
    if value.chars().any(char::is_control) {
        return Err("Account name cannot contain control characters.");
    }
    Ok(value.to_owned())
}
