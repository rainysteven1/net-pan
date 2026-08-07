use std::{future::Future, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use crate::{
    download::JobId,
    ipc::{self, DaemonRuntime, IpcCommand, IpcRequest, IpcResponse, IpcResult, PROTOCOL_VERSION},
    job::{DownloadJobRequest, DownloadManager},
    notifier::{NotificationWorker, Notifier},
    provider::{
        ProviderCatalogEntry, ProviderId, ProviderIdentity, ProviderRuntime,
        ProviderSettingsSaveResult, RemoteItem, RemoteRef, RemoteSource,
    },
    settings::NetworkSettings,
    state::StateStore,
};

#[derive(Clone)]
pub struct DaemonClient {
    socket: PathBuf,
    client_id: String,
}

impl DaemonClient {
    pub fn new(socket: PathBuf, client_id: String) -> Self {
        Self { socket, client_id }
    }

    pub fn local(socket: Option<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            socket.unwrap_or(ipc::default_socket_path()?),
            ipc::load_or_create_client_id()?,
        ))
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn new_request_id() -> Result<u64> {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow::anyhow!("failed to generate IPC request ID: {error}"))?;
        Ok(u64::from_le_bytes(bytes).max(1))
    }

    pub async fn status(&self) -> Result<IpcResult> {
        let result = self
            .request(Self::new_request_id()?, IpcCommand::Status)
            .await?;
        if matches!(result, IpcResult::Status { .. }) {
            Ok(result)
        } else {
            bail!("daemon returned an unexpected status response")
        }
    }

    pub async fn providers(&self) -> Result<Vec<ProviderCatalogEntry>> {
        match self
            .request(Self::new_request_id()?, IpcCommand::Providers)
            .await?
        {
            IpcResult::Providers { providers } => Ok(providers),
            _ => bail!("daemon returned an unexpected Providers response"),
        }
    }

    pub async fn sources(&self, identity: ProviderIdentity) -> Result<Vec<RemoteSource>> {
        match self
            .request(Self::new_request_id()?, IpcCommand::Sources { identity })
            .await?
        {
            IpcResult::Sources { sources } => Ok(sources),
            _ => bail!("daemon returned an unexpected Sources response"),
        }
    }

    pub async fn list_children(&self, parent: RemoteRef) -> Result<Vec<RemoteItem>> {
        match self
            .request(Self::new_request_id()?, IpcCommand::ListChildren { parent })
            .await?
        {
            IpcResult::Children { items } => Ok(items),
            _ => bail!("daemon returned an unexpected ListChildren response"),
        }
    }

    pub async fn refresh_item(&self, item: RemoteRef) -> Result<RemoteItem> {
        match self
            .request(Self::new_request_id()?, IpcCommand::RefreshItem { item })
            .await?
        {
            IpcResult::Item { item } => Ok(item),
            _ => bail!("daemon returned an unexpected RefreshItem response"),
        }
    }

    pub async fn save_provider_settings(
        &self,
        identity: ProviderIdentity,
        changes: std::collections::BTreeMap<String, String>,
    ) -> Result<ProviderSettingsSaveResult> {
        match self
            .request(
                Self::new_request_id()?,
                IpcCommand::SaveProviderSettings { identity, changes },
            )
            .await?
        {
            IpcResult::ProviderSettingsSaved { outcome } => Ok(outcome),
            _ => bail!("daemon returned an unexpected SaveProviderSettings response"),
        }
    }

    pub async fn create_profile(
        &self,
        provider_id: ProviderId,
        name: String,
    ) -> Result<ProviderCatalogEntry> {
        match self
            .request(
                Self::new_request_id()?,
                IpcCommand::CreateProfile { provider_id, name },
            )
            .await?
        {
            IpcResult::ProfileCreated { profile } => Ok(profile),
            _ => bail!("daemon returned an unexpected CreateProfile response"),
        }
    }

    pub async fn reconfigure_network(
        &self,
        network: NetworkSettings,
    ) -> Result<Vec<ProviderCatalogEntry>> {
        match self
            .request(
                Self::new_request_id()?,
                IpcCommand::ReconfigureNetwork { network },
            )
            .await?
        {
            IpcResult::NetworkReconfigured { providers } => Ok(providers),
            _ => bail!("daemon returned an unexpected ReconfigureNetwork response"),
        }
    }

    pub async fn submit(&self, request_id: u64, request: DownloadJobRequest) -> Result<IpcResult> {
        let result = self
            .request(
                request_id,
                IpcCommand::Submit {
                    request: Box::new(request),
                },
            )
            .await?;
        if matches!(result, IpcResult::Submitted { .. }) {
            Ok(result)
        } else {
            bail!("daemon returned an unexpected submit response")
        }
    }

    pub async fn cancel(&self, job_id: JobId) -> Result<IpcResult> {
        let result = self
            .request(Self::new_request_id()?, IpcCommand::Cancel { job_id })
            .await?;
        if matches!(result, IpcResult::Cancelled { .. }) {
            Ok(result)
        } else {
            bail!("daemon returned an unexpected cancel response")
        }
    }

    pub async fn pause(&self, job_id: JobId) -> Result<IpcResult> {
        let result = self
            .request(Self::new_request_id()?, IpcCommand::Pause { job_id })
            .await?;
        if matches!(result, IpcResult::Paused { .. }) {
            Ok(result)
        } else {
            bail!("daemon returned an unexpected pause response")
        }
    }

    pub async fn resume(&self, job_id: JobId) -> Result<IpcResult> {
        let result = self
            .request(Self::new_request_id()?, IpcCommand::Resume { job_id })
            .await?;
        if matches!(result, IpcResult::Resumed { .. }) {
            Ok(result)
        } else {
            bail!("daemon returned an unexpected resume response")
        }
    }

    pub async fn subscribe(
        &self,
        after_revision: u64,
    ) -> Result<mpsc::Receiver<std::result::Result<IpcResponse, String>>> {
        ipc::subscribe(&self.socket, self.client_id.clone(), after_revision).await
    }

    async fn request(&self, request_id: u64, command: IpcCommand) -> Result<IpcResult> {
        let response = ipc::request(
            &self.socket,
            &IpcRequest {
                version: PROTOCOL_VERSION,
                client_id: self.client_id.clone(),
                request_id,
                command,
            },
        )
        .await?;
        if let Some(error) = response.error {
            bail!("{}: {}", error.code, error.message);
        }
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("daemon response has no result"))
    }
}

pub struct DaemonService {
    socket: PathBuf,
    runtime: DaemonRuntime,
    notifiers: Vec<Arc<dyn Notifier>>,
    recovered_jobs: usize,
}

impl DaemonService {
    pub async fn prepare(
        socket: PathBuf,
        state: StateStore,
        providers: ProviderRuntime,
        notifiers: Vec<Arc<dyn Notifier>>,
    ) -> Result<Self> {
        state.set_notification_targets(
            notifiers
                .iter()
                .map(|notifier| notifier.id().to_owned())
                .collect(),
        );
        let jobs = DownloadManager::persistent(state.clone())?;
        let runtime = DaemonRuntime::new(jobs, state, providers);
        let recovered_jobs = runtime.recover().await?;
        Ok(Self {
            socket,
            runtime,
            notifiers,
            recovered_jobs,
        })
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub fn recovered_jobs(&self) -> usize {
        self.recovered_jobs
    }

    pub async fn run<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()>,
    {
        let worker = NotificationWorker::new(self.runtime.state_store(), self.notifiers);
        let (stop_worker, worker_stopped) = tokio::sync::oneshot::channel();
        let worker_task = tokio::spawn(worker.run(async {
            let _ = worker_stopped.await;
        }));
        let result = ipc::serve(&self.socket, self.runtime, shutdown).await;
        let _ = stop_worker.send(());
        let _ = worker_task.await;
        result
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn library_service_and_client_share_the_daemon_lifecycle() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("daemon.sock");
        let state = StateStore::open(&temporary.path().join("state.db")).unwrap();
        let providers = ProviderRuntime::for_test(Vec::new());
        let service = DaemonService::prepare(socket.clone(), state, providers, Vec::new())
            .await
            .unwrap();
        assert_eq!(service.recovered_jobs(), 0);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            service
                .run(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let client = DaemonClient::new(socket.clone(), "library-test".to_owned());
        assert!(matches!(
            client.status().await.unwrap(),
            IpcResult::Status { ref jobs, .. } if jobs.is_empty()
        ));

        let _ = stop.send(());
        server.await.unwrap();
        assert!(!socket.exists());
    }
}
