use std::{cmp::Ordering, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Response, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    auth::AccessTokenProvider,
    model::{DriveItem, FileListResponse},
    settings::NetworkSettings,
};

const DEFAULT_API_BASE: &str = "https://www.googleapis.com/drive/v3/files";
const FILE_FIELDS: &str = "id,name,mimeType,size,md5Checksum,resourceKey,capabilities(canDownload)";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct DriveReference {
    pub file_id: String,
    pub resource_key: Option<String>,
}

#[derive(Clone)]
pub struct DriveClient {
    http: reqwest::Client,
    tokens: Arc<dyn AccessTokenProvider>,
    api_base: String,
}

impl DriveClient {
    pub fn new(tokens: Arc<dyn AccessTokenProvider>, settings: &NetworkSettings) -> Result<Self> {
        let http = settings
            .configure_client(reqwest::Client::builder())?
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(120))
            .user_agent(concat!("net-pan/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create HTTP client")?;
        Ok(Self {
            http,
            tokens,
            api_base: DEFAULT_API_BASE.to_owned(),
        })
    }

    pub async fn get_item(&self, reference: &DriveReference) -> Result<DriveItem> {
        let token = self.tokens.access_token().await?;
        let mut request = self
            .http
            .get(format!("{}/{}", self.api_base, reference.file_id))
            .bearer_auth(token)
            .query(&[("supportsAllDrives", "true"), ("fields", FILE_FIELDS)]);
        if let Some(key) = reference.resource_key.as_deref() {
            request = request.header(
                "X-Goog-Drive-Resource-Keys",
                format!("{}/{}", reference.file_id, key),
            );
        }
        let response = request
            .send()
            .await
            .context("failed to fetch configured Drive folder metadata")?;
        json_response(response, "fetch configured Drive folder metadata").await
    }

    pub async fn list_children(&self, folder: &DriveItem) -> Result<Vec<DriveItem>> {
        if !folder.is_folder() {
            bail!("{} is not a Google Drive folder", folder.name);
        }

        self.list_query(
            format!("'{}' in parents and trashed = false", folder.id),
            resource_key_header(folder),
            "list Drive folder",
        )
        .await
    }

    async fn list_query(
        &self,
        query_text: String,
        resource_key: Option<HeaderValue>,
        operation: &'static str,
    ) -> Result<Vec<DriveItem>> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.tokens.access_token().await?;
            let mut query = vec![
                ("q", query_text.clone()),
                ("pageSize", "1000".to_owned()),
                ("supportsAllDrives", "true".to_owned()),
                ("includeItemsFromAllDrives", "true".to_owned()),
                ("spaces", "drive".to_owned()),
                ("orderBy", "folder,name_natural".to_owned()),
                ("fields", format!("nextPageToken,files({FILE_FIELDS})")),
            ];
            if let Some(value) = page_token.as_ref() {
                query.push(("pageToken", value.clone()));
            }

            let mut request = self
                .http
                .get(&self.api_base)
                .bearer_auth(token)
                .query(&query);
            if let Some(value) = resource_key.as_ref() {
                request = request.header("X-Goog-Drive-Resource-Keys", value.clone());
            }
            let response = request
                .send()
                .await
                .with_context(|| format!("failed to {operation}"))?;
            let page: FileListResponse = json_response(response, operation).await?;
            items.extend(page.files);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        sort_items(&mut items);
        Ok(items)
    }

    pub async fn media_response(&self, item: &DriveItem, offset: u64) -> Result<Response> {
        self.media_response_ref(
            &DriveReference {
                file_id: item.id.clone(),
                resource_key: item.resource_key.clone(),
            },
            offset,
        )
        .await
    }

    pub async fn media_response_ref(
        &self,
        reference: &DriveReference,
        offset: u64,
    ) -> Result<Response> {
        let token = self.tokens.access_token().await?;
        let mut request = self
            .http
            .get(format!("{}/{}", self.api_base, reference.file_id))
            .bearer_auth(token)
            .query(&[("alt", "media"), ("supportsAllDrives", "true")]);
        if let Some(key) = reference.resource_key.as_deref()
            && let Ok(value) = HeaderValue::from_str(&format!("{}/{}", reference.file_id, key))
        {
            request = request.header("X-Goog-Drive-Resource-Keys", value);
        }
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        request
            .send()
            .await
            .context("failed to start Drive download request")
    }
}

fn sort_items(files: &mut [DriveItem]) {
    files.sort_by(|left, right| match (left.is_folder(), right.is_folder()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
    });
}

fn resource_key_header(item: &DriveItem) -> Option<HeaderValue> {
    let key = item.resource_key.as_deref()?;
    HeaderValue::from_str(&format!("{}/{}", item.id, key)).ok()
}

pub async fn response_error(response: Response, operation: &str) -> anyhow::Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(300).collect());
    anyhow!("failed to {operation} (HTTP {status}): {message}")
}

async fn json_response<T: serde::de::DeserializeOwned>(
    response: Response,
    operation: &str,
) -> Result<T> {
    if !response.status().is_success() {
        return Err(response_error(response, operation).await);
    }
    response
        .json()
        .await
        .with_context(|| format!("Google returned invalid JSON while attempting to {operation}"))
}

pub fn parse_drive_reference(value: &str) -> Result<DriveReference> {
    let value = value.trim();
    if valid_file_id(value) {
        return Ok(DriveReference {
            file_id: value.to_owned(),
            resource_key: None,
        });
    }

    let url = Url::parse(value).context("source must be a Drive ID or Google Drive share URL")?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "google.com" && !host.ends_with(".google.com") {
        bail!("share URL is not hosted on a Google domain");
    }

    let query_id = url
        .query_pairs()
        .find_map(|(key, value)| (key == "id").then(|| value.into_owned()));
    let resource_key = url.query_pairs().find_map(|(key, value)| {
        (key.eq_ignore_ascii_case("resourcekey")).then(|| value.into_owned())
    });
    let segments: Vec<_> = url
        .path_segments()
        .map(|items| items.collect())
        .unwrap_or_default();
    let path_id = segments
        .windows(2)
        .find_map(|pair| matches!(pair[0], "d" | "folders").then(|| pair[1].to_owned()));
    let file_id = query_id
        .or(path_id)
        .ok_or_else(|| anyhow!("could not parse a Drive ID from the URL"))?;
    if !valid_file_id(&file_id) {
        bail!("invalid Drive ID format");
    }
    Ok(DriveReference {
        file_id,
        resource_key,
    })
}

fn valid_file_id(value: &str) -> bool {
    value.len() >= 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Capabilities, FOLDER_MIME};

    #[test]
    fn sorts_folders_before_files_then_by_name() {
        let item = |name: &str, mime_type: &str| DriveItem {
            id: format!("id-{name}-123456789"),
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size: None,
            md5_checksum: None,
            resource_key: None,
            capabilities: Capabilities::default(),
        };
        let mut items = vec![
            item("z.txt", "text/plain"),
            item("Beta", FOLDER_MIME),
            item("alpha", FOLDER_MIME),
            item("A.txt", "text/plain"),
        ];
        sort_items(&mut items);
        let names: Vec<_> = items.iter().map(|value| value.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta", "A.txt", "z.txt"]);
    }

    const ID: &str = "1-7ZhLs-DjQJr4Rol1KbLsusF7i8qDsPB";

    #[test]
    fn parses_folder_link_and_id() {
        assert_eq!(parse_drive_reference(ID).unwrap().file_id, ID);
        let parsed = parse_drive_reference(&format!(
            "https://drive.google.com/drive/folders/{ID}?resourcekey=secret"
        ))
        .unwrap();
        assert_eq!(parsed.file_id, ID);
        assert_eq!(parsed.resource_key.as_deref(), Some("secret"));
    }

    #[test]
    fn rejects_non_google_link() {
        assert!(parse_drive_reference(&format!("https://example.com/folders/{ID}")).is_err());
    }
}
