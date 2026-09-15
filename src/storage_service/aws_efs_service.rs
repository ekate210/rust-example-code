use crate::{
    config::{AWS_EFS_BASE_PATH, UNKNOWN_MIME_TYPE},
    services::{
        storage_service::storage_service_trait::{
            ContentRepresentationInfo, FromStageInfo, StageItemResult, StorageServiceTrait,
        },
        storage_service_factory::STORAGE_SERVICE_FACTORY,
    },
    types::{
        adapter_config::{AdapterConfig, AwsEfsConfig},
        content_properties::ContentHeadInfo,
    },
};
use async_std::stream::StreamExt;
use async_trait::async_trait;
use common_rust::{
    ctx_debug,
    prelude::{CallContext, ServiceError},
};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

#[derive(Default)]
pub struct AwsEfsService {}

impl AwsEfsService {
    pub fn new() -> Self {
        AwsEfsService {}
    }
}

#[async_trait]
impl StorageServiceTrait for AwsEfsService {
    async fn retrieve_head_infos(
        &self,
        _ctx: &CallContext,
        content_uris: &[String],
        _adapter_configs: &[AdapterConfig],
    ) -> Result<Vec<ContentHeadInfo>, ServiceError> {
        let mut head_infos = vec![];

        for content_uri in content_uris {
            let file_path = content_uri;

            let metadata = fs::metadata(file_path).map_err(|err| {
                ServiceError::FileSystemError(format!("Failed to get file metadata: [{err}]"))
            })?;
            let size = metadata.len();

            let kind_option = infer::get_from_path(file_path).map_err(|err| {
                ServiceError::FileSystemError(format!("Failed to infer file kind: [{err}]"))
            })?;
            let mime_type = match kind_option {
                Some(kind) => kind.mime_type().to_string(),
                None => UNKNOWN_MIME_TYPE.to_string(),
            };

            head_infos.push(ContentHeadInfo {
                mime_type,
                size,
                filename: None,
            });
        }

        Ok(head_infos)
    }

    async fn purge(
        &self,
        ctx: &CallContext,
        content_uri: &str,
        _adapter_config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        let file_path = content_uri;

        // Remove file if exists
        if Path::new(file_path).exists() {
            fs::remove_file(file_path).map_err(|err| {
                ServiceError::FileSystemError(format!(
                    "Failed to remove file: [{file_path}]. Reason: [{err}]"
                ))
            })?;
        } else {
            ctx_debug!(ctx, "File \"{file_path}\" not exist, skip removing");
        }

        // Remove parent directory until base path if exists and empty
        let mut parent_path_option = Path::new(file_path).parent();

        while let Some(parent_path) = parent_path_option {
            if !parent_path.exists() {
                break;
            }

            if parent_path == Path::new(AWS_EFS_BASE_PATH.as_str()) {
                break;
            }

            if parent_path
                .read_dir()
                .map_err(|err| {
                    ServiceError::FileSystemError(format!(
                        "Failed to read directory: [{}]. Reason: [{}]",
                        parent_path.display(),
                        err
                    ))
                })?
                .next()
                .is_some()
            {
                break;
            }

            fs::remove_dir(parent_path).map_err(|err| {
                ServiceError::FileSystemError(format!(
                    "Failed to remove directory: [{}]. Reason: [{}]",
                    parent_path.display(),
                    err
                ))
            })?;

            parent_path_option = parent_path.parent();
        }

        Ok(())
    }

    async fn stage(
        &self,
        ctx: &CallContext,
        _adapter_id: &str,
        adapter_config: AdapterConfig,
        origin_stage_info: FromStageInfo,
        _alias: Option<String>,
        representation_type: Option<String>,
    ) -> Result<StageItemResult, ServiceError> {
        let representation_type = representation_type.unwrap_or(
            origin_stage_info
                .content_representation_info
                .representation_type,
        );

        let origin_adapter_service =
            STORAGE_SERVICE_FACTORY.get_by_id(&origin_stage_info.adapter_id)?;
        let origin_pre_signed_url = origin_adapter_service
            .generate_pre_signed_url(
                ctx,
                Some(origin_stage_info.adapter_id),
                &origin_stage_info.content_representation_info.uri,
                origin_stage_info.adapter_config.clone(),
                None,
                Some(ContentRepresentationInfo {
                    content_id: origin_stage_info
                        .content_representation_info
                        .content_id
                        .clone(),
                    representation_type: representation_type.clone(),
                    uri: origin_stage_info.content_representation_info.uri.clone(),
                    filename: origin_stage_info
                        .content_representation_info
                        .filename
                        .clone(),
                    mime_type: origin_stage_info.content_representation_info.mime_type,
                    metadata: origin_stage_info
                        .content_representation_info
                        .metadata
                        .clone(),
                }),
            )
            .await?;

        let file_path = format!(
            "{}/{}/{}/{}/{}",
            *AWS_EFS_BASE_PATH,
            ctx.org_id,
            origin_stage_info.content_representation_info.content_id,
            representation_type,
            origin_stage_info.content_representation_info.filename
        );
        let Some(parent_path) = Path::new(&file_path).parent() else {
            return Err(ServiceError::FileSystemError(format!(
                "Failed to get parent path of file: [{file_path}]"
            )));
        };
        if !parent_path.exists() {
            fs::create_dir_all(parent_path).map_err(|err| {
                ServiceError::FileSystemError(format!(
                    "Failed to create parent path of file: [{err}]"
                ))
            })?;
        }
        let content_file = File::create(&file_path).map_err(|err| {
            ServiceError::FileSystemError(format!("Failed to create file: [{err}]"))
        })?;

        let response = reqwest::Client::new()
            .get(&origin_pre_signed_url)
            .send()
            .await?
            .error_for_status()?;
        let mut bytes_stream = response.bytes_stream();
        let mut buf_writer = BufWriter::new(&content_file);

        while let Some(item) = bytes_stream.next().await {
            let bytes = item.map_err(ServiceError::ReqwestError)?;

            buf_writer.write_all(&bytes).map_err(|err| {
                ServiceError::FileSystemError(format!("Failed to write content to file: [{err}]"))
            })?;
        }

        buf_writer.flush().map_err(|err| {
            ServiceError::FileSystemError(format!("Failed to flush buffer: [{err}]"))
        })?;

        // file path is the content URI in the context of AWS EFS
        let uri = file_path;
        let stripped_uri = uri.clone();
        let head_info = self
            .retrieve_head_infos(
                ctx,
                std::slice::from_ref(&uri),
                std::slice::from_ref(&adapter_config),
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ServiceError::FileSystemError(format!("Failed to get head info for file: [{uri}]"))
            })?;

        Ok(StageItemResult {
            uri,
            stripped_uri,
            representation_type,
            head_info,
            filename: origin_stage_info.content_representation_info.filename,
        })
    }

    fn reconstruct_uri(
        &self,
        content_uri: &str,
        _adapter_config: &AdapterConfig,
        _content_id: Option<String>,
        _representation_type: Option<String>,
        _alias: Option<String>,
        _short_link_slug: Option<String>,
        _is_dynamic: bool,
    ) -> Result<String, ServiceError> {
        // No changes
        Ok(content_uri.to_string())
    }

    async fn update_adapter(
        &self,
        _ctx: &CallContext,
        _old_config: AdapterConfig,
        _new_config: AdapterConfig,
        _has_linked_alias: bool,
    ) -> Result<AdapterConfig, ServiceError> {
        // Content service connected to only one default instance of AWS EFS
        Err(ServiceError::InvalidRequestParameters(
            "AWS EFS adapter updating is not supported".to_string(),
        ))
    }

    async fn purge_by_url(&self, _url: &str) -> Result<Option<Value>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    fn is_valid_uri(&self, uri: &str) -> bool {
        uri.starts_with(AWS_EFS_BASE_PATH.as_str())
    }

    fn get_filename_from_uri(&self, uri: &str) -> Option<String> {
        let path = Path::new(uri);
        let filename = path.file_name()?.to_string_lossy().to_string();

        Some(filename)
    }

    fn get_default_config(&self) -> AdapterConfig {
        AdapterConfig::AwsEfs(AwsEfsConfig {})
    }
}
