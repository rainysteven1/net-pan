use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

use crate::settings::NetworkSettings;

pub const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
const JWT_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

#[async_trait]
pub trait AccessTokenProvider: Send + Sync {
    async fn access_token(&self) -> Result<String>;
}

#[derive(Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    private_key_id: Option<String>,
    token_uri: String,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: u64,
    iat: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

fn default_expires_in() -> u64 {
    3600
}

struct CachedToken {
    value: String,
    refresh_after: Instant,
}

pub struct ServiceAccountTokenProvider {
    key: ServiceAccountKey,
    http: reqwest::Client,
    cache: Mutex<Option<CachedToken>>,
}

impl ServiceAccountTokenProvider {
    pub async fn from_file(
        path: &Path,
        settings: &NetworkSettings,
    ) -> Result<Arc<dyn AccessTokenProvider>> {
        let contents = tokio::fs::read(path).await.with_context(|| {
            format!(
                "failed to read service-account credentials: {}",
                path.display()
            )
        })?;
        let key: ServiceAccountKey = serde_json::from_slice(&contents)
            .with_context(|| format!("invalid service-account JSON: {}", path.display()))?;
        if key.client_email.is_empty() || key.private_key.is_empty() || key.token_uri.is_empty() {
            bail!("service-account JSON is missing required fields");
        }
        let http = settings
            .configure_client(reqwest::Client::builder())?
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .user_agent(concat!("net-pan/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create OAuth HTTP client")?;
        Ok(Arc::new(Self {
            key,
            http,
            cache: Mutex::new(None),
        }))
    }

    fn signed_assertion(&self) -> Result<String> {
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs();
        let claims = Claims {
            iss: &self.key.client_email,
            scope: DRIVE_READONLY_SCOPE,
            aud: &self.key.token_uri,
            iat: issued_at,
            exp: issued_at + 3595,
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid.clone_from(&self.key.private_key_id);
        let encoding_key = EncodingKey::from_rsa_pem(self.key.private_key.as_bytes())
            .context("service-account private key is not valid RSA PEM")?;
        encode(&header, &claims, &encoding_key).context("failed to sign service-account JWT")
    }
}

#[async_trait]
impl AccessTokenProvider for ServiceAccountTokenProvider {
    async fn access_token(&self) -> Result<String> {
        {
            let cache = self.cache.lock().await;
            if let Some(token) = cache.as_ref()
                && Instant::now() < token.refresh_after
            {
                return Ok(token.value.clone());
            }
        }

        let assertion = self.signed_assertion()?;
        let response = self
            .http
            .post(&self.key.token_uri)
            .form(&[
                ("grant_type", JWT_GRANT_TYPE),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .context("failed to request Google OAuth access token")?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Google OAuth token request failed (HTTP {status})"));
        }
        let token: TokenResponse = response
            .json()
            .await
            .context("Google OAuth returned an invalid token response")?;
        if token.access_token.is_empty() {
            bail!("Google OAuth response did not contain an access token");
        }
        let refresh_in = token.expires_in.saturating_sub(5 * 60).max(60);
        *self.cache.lock().await = Some(CachedToken {
            value: token.access_token.clone(),
            refresh_after: Instant::now() + Duration::from_secs(refresh_in),
        });
        Ok(token.access_token)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct InstalledClientFile {
    installed: InstalledClient,
}

#[derive(Clone, Debug, Deserialize)]
struct InstalledClient {
    client_id: String,
    #[serde(default)]
    client_secret: String,
    auth_uri: String,
    token_uri: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct InstalledTokenState {
    refresh_token: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    expires_at_unix: i64,
}

#[derive(Debug, Deserialize)]
struct InstalledTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

#[derive(Clone)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    fn load<T: for<'de> Deserialize<'de>>(&self) -> Result<T> {
        let contents = fs::read(&self.path)
            .with_context(|| format!("failed to read credential state: {}", self.path.display()))?;
        serde_json::from_slice(&contents).context("credential state is invalid")
    }

    fn save<T: Serialize>(&self, value: &T) -> Result<()> {
        let contents = serde_json::to_vec_pretty(value)?;
        let parent = self
            .path
            .parent()
            .context("credential path has no parent")?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let temporary = parent.join(format!(
            ".token.tmp-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("failed to atomically persist credential state");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        let directory = fs::File::open(parent)?;
        directory.sync_all()?;
        Ok(())
    }

    pub fn remove(&self) -> Result<bool> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("failed to remove credential state"),
        }
    }
}

pub struct GoogleInstalledTokenProvider {
    client: InstalledClient,
    http: reqwest::Client,
    store: CredentialStore,
    state: Mutex<InstalledTokenState>,
}

impl GoogleInstalledTokenProvider {
    pub async fn from_files(
        client_path: &Path,
        token_path: PathBuf,
        settings: &NetworkSettings,
    ) -> Result<Arc<dyn AccessTokenProvider>> {
        let client = load_installed_client(client_path)?;
        let store = CredentialStore::new(token_path);
        let state = store.load::<InstalledTokenState>()?;
        if state.refresh_token.is_empty() {
            bail!("Google OAuth token state has no refresh token; run auth login again");
        }
        let http = oauth_http_client(settings)?;
        Ok(Arc::new(Self {
            client,
            http,
            store,
            state: Mutex::new(state),
        }))
    }

    async fn refresh(&self, current: &InstalledTokenState) -> Result<InstalledTokenState> {
        let response = self
            .http
            .post(&self.client.token_uri)
            .form(&[
                ("client_id", self.client.client_id.as_str()),
                ("client_secret", self.client.client_secret.as_str()),
                ("refresh_token", current.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .context("failed to refresh Google OAuth token")?;
        if !response.status().is_success() {
            bail!("Google OAuth refresh was rejected; run auth login again");
        }
        let token: InstalledTokenResponse = response
            .json()
            .await
            .context("Google OAuth refresh returned an invalid response")?;
        if token.access_token.is_empty() {
            bail!("Google OAuth refresh returned no access token");
        }
        let state = InstalledTokenState {
            refresh_token: if token.refresh_token.is_empty() {
                current.refresh_token.clone()
            } else {
                token.refresh_token
            },
            access_token: token.access_token,
            expires_at_unix: Utc::now()
                .timestamp()
                .saturating_add(i64::try_from(token.expires_in).unwrap_or(i64::MAX)),
        };
        self.store.save(&state)?;
        Ok(state)
    }
}

#[async_trait]
impl AccessTokenProvider for GoogleInstalledTokenProvider {
    async fn access_token(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        if !state.access_token.is_empty()
            && state.expires_at_unix > Utc::now().timestamp().saturating_add(300)
        {
            return Ok(state.access_token.clone());
        }
        *state = self.refresh(&state).await?;
        Ok(state.access_token.clone())
    }
}

pub struct GoogleAuthorizationSession {
    pub authorization_url: Url,
    listener: TcpListener,
    state: String,
    code_verifier: String,
    redirect_uri: String,
    client: InstalledClient,
    http: reqwest::Client,
    store: CredentialStore,
}

impl GoogleAuthorizationSession {
    pub async fn begin(
        client_path: &Path,
        token_path: PathBuf,
        settings: &NetworkSettings,
    ) -> Result<Self> {
        let client = load_installed_client(client_path)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind Google OAuth loopback callback")?;
        let address = listener.local_addr()?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", address.port());
        let state = random_urlsafe(24)?;
        let code_verifier = random_urlsafe(48)?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut authorization_url = Url::parse(&client.auth_uri)?;
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", DRIVE_READONLY_SCOPE)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(Self {
            authorization_url,
            listener,
            state,
            code_verifier,
            redirect_uri,
            client,
            http: oauth_http_client(settings)?,
            store: CredentialStore::new(token_path),
        })
    }

    pub async fn complete(self) -> Result<i64> {
        let (code, mut stream) = tokio::time::timeout(OAUTH_CALLBACK_TIMEOUT, async {
            let (mut stream, _) = self.listener.accept().await?;
            let mut buffer = vec![0_u8; 16 * 1024];
            let bytes = stream.read(&mut buffer).await?;
            let request =
                std::str::from_utf8(&buffer[..bytes]).context("invalid OAuth callback")?;
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .context("OAuth callback request target is missing")?;
            let callback = Url::parse(&format!("http://localhost{target}"))?;
            let values: std::collections::BTreeMap<_, _> =
                callback.query_pairs().into_owned().collect();
            if let Some(error) = values.get("error") {
                bail!("Google OAuth authorization was rejected: {error}");
            }
            if values.get("state") != Some(&self.state) {
                bail!("Google OAuth callback state did not match");
            }
            let code = values
                .get("code")
                .filter(|code| !code.is_empty())
                .cloned()
                .context("Google OAuth callback did not contain a code")?;
            Ok::<_, anyhow::Error>((code, stream))
        })
        .await
        .context("Google OAuth callback timed out")??;
        const CALLBACK_BODY: &str = "net-pan authorization completed. You may close this tab.";
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CALLBACK_BODY}",
                    CALLBACK_BODY.len()
                )
                .as_bytes(),
            )
            .await?;
        let response = self
            .http
            .post(&self.client.token_uri)
            .form(&[
                ("client_id", self.client.client_id.as_str()),
                ("client_secret", self.client.client_secret.as_str()),
                ("code", code.as_str()),
                ("code_verifier", self.code_verifier.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .context("failed to exchange Google OAuth authorization code")?;
        if !response.status().is_success() {
            bail!("Google OAuth authorization code exchange was rejected");
        }
        let token: InstalledTokenResponse = response
            .json()
            .await
            .context("Google OAuth token endpoint returned an invalid response")?;
        if token.access_token.is_empty() || token.refresh_token.is_empty() {
            bail!("Google OAuth did not return the required access and refresh tokens");
        }
        let expires_at_unix = Utc::now()
            .timestamp()
            .saturating_add(i64::try_from(token.expires_in).unwrap_or(i64::MAX));
        self.store.save(&InstalledTokenState {
            refresh_token: token.refresh_token,
            access_token: token.access_token,
            expires_at_unix,
        })?;
        Ok(expires_at_unix)
    }
}

fn load_installed_client(path: &Path) -> Result<InstalledClient> {
    let contents = fs::read(path).with_context(|| {
        format!(
            "failed to read Google OAuth client JSON: {}",
            path.display()
        )
    })?;
    let file: InstalledClientFile = serde_json::from_slice(&contents)
        .context("Google OAuth client JSON must contain an installed application")?;
    if file.installed.client_id.is_empty()
        || file.installed.auth_uri.is_empty()
        || file.installed.token_uri.is_empty()
    {
        bail!("Google OAuth client JSON is missing required installed-app fields");
    }
    Ok(file.installed)
}

fn oauth_http_client(settings: &NetworkSettings) -> Result<reqwest::Client> {
    settings
        .configure_client(reqwest::Client::builder())?
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .user_agent(concat!("net-pan/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to create Google OAuth HTTP client")
}

fn random_urlsafe(bytes: usize) -> Result<String> {
    let mut value = vec![0_u8; bytes];
    getrandom::fill(&mut value)
        .map_err(|error| anyhow!("failed to obtain secure OAuth randomness: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use tokio::net::TcpStream;

    use super::*;

    async fn read_http_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let bytes = stream.read(&mut buffer).await.unwrap();
            if bytes == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..bytes]);
            let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&data[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if data.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(data).unwrap()
    }

    #[tokio::test]
    async fn installed_oauth_validates_pkce_and_rotates_private_token_state() {
        let token_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_address = token_listener.local_addr().unwrap();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let server_requests = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut stream, _) = token_listener.accept().await.unwrap();
                let request = read_http_request(&mut stream).await;
                server_requests.lock().unwrap().push(request);
                let body = if index == 0 {
                    r#"{"access_token":"access-1","refresh_token":"refresh-1","expires_in":1}"#
                } else {
                    r#"{"access_token":"access-2","refresh_token":"refresh-2","expires_in":3600}"#
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let temporary = tempfile::tempdir().unwrap();
        let client_path = temporary.path().join("client.json");
        let token_path = temporary.path().join("private/token.json");
        fs::write(
            &client_path,
            format!(
                r#"{{"installed":{{"client_id":"client-id","client_secret":"client-secret","auth_uri":"https://accounts.example/authorize","token_uri":"http://{token_address}/token"}}}}"#
            ),
        )
        .unwrap();
        let session = GoogleAuthorizationSession::begin(
            &client_path,
            token_path.clone(),
            &NetworkSettings::default(),
        )
        .await
        .unwrap();
        let query: std::collections::BTreeMap<_, _> = session
            .authorization_url
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("access_type").unwrap(), "offline");
        assert!(!query.get("code_challenge").unwrap().is_empty());
        let callback = Url::parse(query.get("redirect_uri").unwrap()).unwrap();
        let state = query.get("state").unwrap().clone();
        let completion = tokio::spawn(session.complete());
        let mut callback_stream =
            TcpStream::connect((callback.host_str().unwrap(), callback.port().unwrap()))
                .await
                .unwrap();
        callback_stream
            .write_all(
                format!(
                    "GET /callback?code=authorization-code&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut callback_response = Vec::new();
        callback_stream
            .read_to_end(&mut callback_response)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&callback_response).contains("authorization completed"));
        completion.await.unwrap().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let tokens = GoogleInstalledTokenProvider::from_files(
            &client_path,
            token_path.clone(),
            &NetworkSettings::default(),
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token().await.unwrap(), "access-2");
        let state: InstalledTokenState = CredentialStore::new(token_path).load().unwrap();
        assert_eq!(state.refresh_token, "refresh-2");
        {
            let requests = requests.lock().unwrap();
            assert!(requests[0].contains("grant_type=authorization_code"));
            assert!(requests[0].contains("code_verifier="));
            assert!(requests[1].contains("grant_type=refresh_token"));
            assert!(requests[1].contains("refresh_token=refresh-1"));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn installed_oauth_rejects_callback_state_mismatch() {
        let temporary = tempfile::tempdir().unwrap();
        let client_path = temporary.path().join("client.json");
        fs::write(
            &client_path,
            r#"{"installed":{"client_id":"client-id","auth_uri":"https://accounts.example/authorize","token_uri":"http://127.0.0.1:9/token"}}"#,
        )
        .unwrap();
        let session = GoogleAuthorizationSession::begin(
            &client_path,
            temporary.path().join("token.json"),
            &NetworkSettings::default(),
        )
        .await
        .unwrap();
        let redirect = session
            .authorization_url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        let callback = Url::parse(&redirect).unwrap();
        let completion = tokio::spawn(session.complete());
        let mut stream =
            TcpStream::connect((callback.host_str().unwrap(), callback.port().unwrap()))
                .await
                .unwrap();
        stream
            .write_all(b"GET /callback?code=code&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let error = completion.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("state did not match"));
    }
}
