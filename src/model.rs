use serde::Deserialize;

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const GOOGLE_MIME_PREFIX: &str = "application/vnd.google-apps.";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub can_download: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub md5_checksum: Option<String>,
    #[serde(default)]
    pub resource_key: Option<String>,
    #[serde(default)]
    pub capabilities: Capabilities,
}

impl DriveItem {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }

    pub fn is_google_native(&self) -> bool {
        self.mime_type.starts_with(GOOGLE_MIME_PREFIX)
    }

    pub fn can_download(&self) -> bool {
        self.capabilities.can_download != Some(false)
    }

    pub fn size_bytes(&self) -> Option<u64> {
        self.size.as_deref()?.parse().ok()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileListResponse {
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub files: Vec<DriveItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_drive_list_response() {
        let response: FileListResponse = serde_json::from_str(
            r#"{
                "nextPageToken": "next",
                "files": [{
                    "id": "folder-123456789",
                    "name": "images",
                    "mimeType": "application/vnd.google-apps.folder",
                    "capabilities": {"canDownload": true}
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(response.next_page_token.as_deref(), Some("next"));
        assert_eq!(response.files.len(), 1);
        assert!(response.files[0].is_folder());
    }
}
