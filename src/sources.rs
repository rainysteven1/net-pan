use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    drive::{DriveClient, DriveReference, parse_drive_reference},
    model::DriveItem,
};

#[derive(Debug, Deserialize)]
struct SourcesFile {
    sources: Vec<String>,
}

pub async fn load_source_folders(path: &Path, drive: &DriveClient) -> Result<Vec<DriveItem>> {
    let contents = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read sources file {}", path.display()))?;
    let references = parse_sources(&contents)
        .with_context(|| format!("invalid sources file {}", path.display()))?;

    let mut folders = Vec::with_capacity(references.len());
    for (index, reference) in references.iter().enumerate() {
        let item = drive
            .get_item(reference)
            .await
            .with_context(|| format!("failed to load configured source #{}", index + 1))?;
        if !item.is_folder() {
            bail!(
                "configured source #{} ({}) is not a Google Drive folder",
                index + 1,
                item.name
            );
        }
        folders.push(item);
    }
    folders.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(folders)
}

pub(crate) fn parse_sources(contents: &str) -> Result<Vec<DriveReference>> {
    let file: SourcesFile = serde_json::from_str(contents).context("expected a sources array")?;
    if file.sources.is_empty() {
        bail!("sources array is empty");
    }

    let mut seen = HashSet::new();
    let mut references = Vec::new();
    for (index, value) in file.sources.iter().enumerate() {
        let reference = parse_drive_reference(value)
            .with_context(|| format!("invalid Drive link or ID at sources[{index}]"))?;
        if seen.insert(reference.file_id.clone()) {
            references.push(reference);
        }
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_deduplicates_configured_sources() {
        let references = parse_sources(
            r#"{
                "sources": [
                    "1-7ZhLs-DjQJr4Rol1KbLsusF7i8qDsPB",
                    "https://drive.google.com/drive/folders/1-7ZhLs-DjQJr4Rol1KbLsusF7i8qDsPB",
                    "https://drive.google.com/drive/folders/another-folder-123456"
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].file_id, "1-7ZhLs-DjQJr4Rol1KbLsusF7i8qDsPB");
        assert_eq!(references[1].file_id, "another-folder-123456");
    }

    #[test]
    fn rejects_empty_or_invalid_sources() {
        assert!(parse_sources(r#"{"sources": []}"#).is_err());
        assert!(parse_sources(r#"{"sources": ["https://example.com/not-drive"]}"#).is_err());
    }
}
