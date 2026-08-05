use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use async_recursion::async_recursion;
use async_trait::async_trait;

use crate::provider::{CloudProvider, DownloadCapability, RemoteItem};

#[async_trait]
pub trait FolderLister: Sync {
    async fn list_folder_children(&self, folder: &RemoteItem) -> Result<Vec<RemoteItem>>;
}

#[async_trait]
impl<T> FolderLister for T
where
    T: CloudProvider + Sync + ?Sized,
{
    async fn list_folder_children(&self, folder: &RemoteItem) -> Result<Vec<RemoteItem>> {
        Ok(self.list_children(&folder.reference).await?)
    }
}

#[derive(Clone, Debug)]
pub struct PlannedFile {
    pub item: RemoteItem,
    pub relative_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SkippedItem {
    pub relative_path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct DownloadPlan {
    pub root_name: String,
    pub directories: Vec<PathBuf>,
    pub files: Vec<PlannedFile>,
    pub skipped: Vec<SkippedItem>,
    pub total_bytes: u64,
}

impl DownloadPlan {
    pub async fn build<C: FolderLister + ?Sized>(client: &C, root: &RemoteItem) -> Result<Self> {
        let root_name = safe_name(&root.name, root.reference.object_id.as_str());
        let root_path = PathBuf::from(&root_name);
        let mut plan = Self {
            root_name,
            directories: Vec::new(),
            files: Vec::new(),
            skipped: Vec::new(),
            total_bytes: 0,
        };
        if !root.is_folder() {
            plan.add_file(root.clone(), root_path);
            return Ok(plan);
        }

        plan.directories.push(root_path.clone());
        let mut visited = HashSet::new();
        scan_folder(client, root, &root_path, &mut plan, &mut visited).await?;
        Ok(plan)
    }

    fn add_file(&mut self, item: RemoteItem, relative_path: PathBuf) {
        match &item.download {
            DownloadCapability::Unsupported { reason } => self.skipped.push(SkippedItem {
                relative_path,
                reason: reason.clone(),
            }),
            DownloadCapability::Supported { .. } => {
                self.total_bytes = self.total_bytes.saturating_add(item.size.unwrap_or(0));
                self.files.push(PlannedFile {
                    item,
                    relative_path,
                });
            }
        }
    }
}

#[async_recursion]
async fn scan_folder(
    client: &(impl FolderLister + ?Sized),
    folder: &RemoteItem,
    relative_dir: &Path,
    plan: &mut DownloadPlan,
    visited: &mut HashSet<crate::provider::RemoteRef>,
) -> Result<()> {
    if !visited.insert(folder.reference.clone()) {
        return Ok(());
    }

    let children = client.list_folder_children(folder).await?;
    let mut used_names = HashSet::new();
    for item in children {
        let stable_id = item.reference.object_id.as_str();
        let base = safe_name(&item.name, stable_id);
        let local_name = unique_name(&base, stable_id, &mut used_names);
        let relative_path = relative_dir.join(local_name);
        if item.is_folder() {
            plan.directories.push(relative_path.clone());
            scan_folder(client, &item, &relative_path, plan, visited).await?;
        } else {
            plan.add_file(item, relative_path);
        }
    }
    Ok(())
}

pub fn safe_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    if cleaned.is_empty() || matches!(cleaned.as_str(), "." | "..") {
        fallback
            .chars()
            .map(|ch| match ch {
                '/' | '\\' | '\0' => '_',
                ch if ch.is_control() => '_',
                ch => ch,
            })
            .collect()
    } else {
        cleaned
    }
}

fn unique_name(base: &str, id: &str, used: &mut HashSet<String>) -> String {
    if used.insert(name_key(base)) {
        return base.to_owned();
    }

    let short_id: String = id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let short_id = if short_id.is_empty() {
        "remote"
    } else {
        &short_id
    };
    let (stem, extension) = match base.rfind('.') {
        Some(index) if index > 0 => (&base[..index], &base[index..]),
        _ => (base, ""),
    };
    let candidate = format!("{stem}__{short_id}{extension}");
    if used.insert(name_key(&candidate)) {
        return candidate;
    }
    let mut number = 2;
    loop {
        let candidate = format!("{stem}__{short_id}_{number}{extension}");
        if used.insert(name_key(&candidate)) {
            return candidate;
        }
        number += 1;
    }
}

fn name_key(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        DownloadCapability, ProfileId, ProviderId, ProviderIdentity, RemoteItemKind,
        RemoteObjectId, RemoteRef, SourceId,
    };

    struct FakeLister;

    #[async_trait]
    impl FolderLister for FakeLister {
        async fn list_folder_children(&self, folder: &RemoteItem) -> Result<Vec<RemoteItem>> {
            Ok(match folder.reference.object_id.as_str() {
                "root-123456789" => vec![
                    item("folder-123456789", "child", RemoteItemKind::Folder, None),
                    item(
                        "file-a-123456789",
                        "same.bin",
                        RemoteItemKind::File,
                        Some(3),
                    ),
                    item("file-b-abcdefgh", "same.bin", RemoteItemKind::File, Some(5)),
                    unsupported("doc-123456789", "notes", "export required"),
                ],
                "folder-123456789" => vec![item(
                    "nested-123456789",
                    "nested.txt",
                    RemoteItemKind::File,
                    Some(7),
                )],
                _ => Vec::new(),
            })
        }
    }

    fn identity() -> ProviderIdentity {
        ProviderIdentity::new(
            ProviderId::new("fake").unwrap(),
            ProfileId::new("default").unwrap(),
        )
    }

    fn item(id: &str, name: &str, kind: RemoteItemKind, size: Option<u64>) -> RemoteItem {
        RemoteItem {
            reference: RemoteRef {
                identity: identity(),
                source_id: SourceId::new("source").unwrap(),
                object_id: RemoteObjectId::new(id).unwrap(),
            },
            name: name.to_owned(),
            kind,
            size,
            checksum: None,
            download: DownloadCapability::Supported { resume: true },
        }
    }

    fn unsupported(id: &str, name: &str, reason: &str) -> RemoteItem {
        let mut item = item(id, name, RemoteItemKind::File, None);
        item.download = DownloadCapability::Unsupported {
            reason: reason.to_owned(),
        };
        item
    }

    fn root_item() -> RemoteItem {
        item("root-123456789", "root", RemoteItemKind::Folder, None)
    }

    #[test]
    fn sanitizes_remote_names() {
        assert_eq!(safe_name("../../model.bin", "id"), ".._.._model.bin");
        assert_eq!(safe_name("..", "fallback"), "fallback");
        assert_eq!(safe_name("a\\b/c", "id"), "a_b_c");
    }

    #[test]
    fn disambiguates_duplicate_names() {
        let mut used = HashSet::new();
        assert_eq!(unique_name("same.bin", "first-id", &mut used), "same.bin");
        assert_eq!(
            unique_name("same.bin", "second-id", &mut used),
            "same__secondid.bin"
        );
    }

    #[test]
    fn disambiguates_names_that_differ_only_by_case() {
        let mut used = HashSet::new();
        assert_eq!(unique_name("Foo.bin", "first-id", &mut used), "Foo.bin");
        assert_eq!(
            unique_name("foo.bin", "second-id", &mut used),
            "foo__secondid.bin"
        );
    }

    #[tokio::test]
    async fn builds_recursive_plan_with_safe_unique_paths() {
        let plan = DownloadPlan::build(&FakeLister, &root_item())
            .await
            .unwrap();
        assert_eq!(plan.directories.len(), 2);
        assert_eq!(plan.files.len(), 3);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.total_bytes, 15);
        assert!(
            plan.files
                .iter()
                .any(|file| file.relative_path == Path::new("root/child/nested.txt"))
        );
    }

    #[tokio::test]
    async fn builds_single_file_plan_without_listing_children() {
        let file = item(
            "file-root-123456789",
            "selected.bin",
            RemoteItemKind::File,
            Some(11),
        );
        let plan = DownloadPlan::build(&FakeLister, &file).await.unwrap();
        assert_eq!(plan.directories.len(), 0);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.total_bytes, 11);
    }

    #[tokio::test]
    async fn records_unsupported_selected_file_as_skipped() {
        let file = unsupported("doc-root-123456789", "notes", "export required");
        let plan = DownloadPlan::build(&FakeLister, &file).await.unwrap();
        assert!(plan.files.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, "export required");
    }
}
