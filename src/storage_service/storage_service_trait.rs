use crate::types::{
    adapter_config::AdapterConfig, content_properties::ContentHeadInfo,
    retrieve::RetrieveProperties,
};
use async_trait::async_trait;
use common_rust::prelude::{CallContext, ServiceError};
use mockall::automock;
use std::collections::HashMap;

#[derive(Clone)]
pub struct ContentRepresentationInfo {
    pub content_id: String,
    pub representation_type: String,
    pub uri: String,
    pub filename: String,
    pub mime_type: String,
    pub metadata: Option<HashMap<String, String>>,
}

#[derive(Clone)]
pub struct GenerateUploadUrlOptions {
    pub content_id: String,
    pub representation_type: String,
    pub filename: String,
    pub mime_type: String,
    pub metadata: Option<HashMap<String, String>>,
}

pub struct GenerateUploadUrlResult {
    pub upload_url: String,
    pub content_uri: String,
    pub headers: Option<String>,
}

pub struct StageItemResult {
    pub uri: String,
    pub stripped_uri: String,
    pub representation_type: String,
    pub filename: String,
    pub head_info: ContentHeadInfo,
}

pub struct FromStageInfo {
    pub adapter_id: String,
    pub adapter_config: AdapterConfig,
    pub content_representation_info: ContentRepresentationInfo,
}

/// Trait representing a storage service of specific adapter.
#[automock]
#[async_trait]
pub trait StorageServiceTrait: Send + Sync {
    /// Retrieves the headers information of the content.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `content_uri` - URI of the content.
    /// * `adapter_config` - adapter configuration.
    ///
    /// # Returns
    ///
    /// * `Result` containing the `content_head_info` or the `err`.
    async fn retrieve_head_infos(
        &self,
        _ctx: &CallContext,
        _content_uris: &[String],
        _adapter_configs: &[AdapterConfig],
    ) -> Result<Vec<ContentHeadInfo>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }
    /// Purges the content from the adapter's storage.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `content_uri` - URI of the content.
    /// * `adapter_config` - adapter configuration.
    ///
    /// # Returns
    ///
    /// * `Result` containing `ok` or the `err`.
    async fn purge(
        &self,
        _ctx: &CallContext,
        _content_uri: &str,
        _adapter_config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Generates a pre-signed URL for the content.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `adapter_id` - optional adapter ID.
    /// * `content_uris` - array of content URIs.
    /// * `adapter_config` - adapter configuration.
    ///
    /// # Returns
    ///
    /// * `Result` containing the `pre_signed_url` or the `err`.
    async fn generate_pre_signed_url(
        &self,
        _ctx: &CallContext,
        _adapter_id: Option<String>,
        _content_uri: &str,
        _adapter_config: AdapterConfig,
        _retrieve_properties: Option<RetrieveProperties>,
        _content_repr_properties: Option<ContentRepresentationInfo>,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Generates an pre-signed upload URL for the content.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `adapter_config` - adapter configuration.
    /// * `options` - options for generating the upload URL.
    ///
    /// # Returns
    ///
    /// * `Result` containing the `upload_url`, `content_uri`, and required upload headers or the `err`.
    async fn generate_upload_url(
        &self,
        _ctx: &CallContext,
        _adapter_config: AdapterConfig,
        _options: GenerateUploadUrlOptions,
    ) -> Result<GenerateUploadUrlResult, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Uploads the content from the origin adapter.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `adapter_config` - adapter configuration.
    /// * `from_adapter_id` - ID of the origin adapter.
    /// * `from_adapter_config` - configuration of the origin adapter.
    /// * `from_content_representation_info` - information about the origin content representation.
    ///
    /// # Returns
    ///
    /// * `Result` containing the `content_uri` or the `err`.
    async fn upload_content(
        &self,
        _ctx: &CallContext,
        _adapter_config: AdapterConfig,
        _from_adapter_id: &str,
        _from_adapter_config: AdapterConfig,
        _from_content_representation_info: ContentRepresentationInfo,
        _to_representation_type: &str,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Stages the content from the origin adapter.
    ///
    /// # Arguments
    ///
    /// * `ctx` - call context.
    /// * `adapter_id` - ID of the adapter.
    /// * `adapter_config` - adapter configuration.
    /// * `from_adapter_id` - ID of the origin adapter.
    /// * `from_adapter_config` - configuration of the origin adapter.
    /// * `from_content_representation_info` - information about the origin content representation.
    /// * `alias` - use for CDN only, help to define custom URL to make it shorter
    ///
    /// # Returns
    ///
    /// * `Result` containing the `stage_item_result` or the `err`.
    async fn stage(
        &self,
        _ctx: &CallContext,
        _adapter_id: &str,
        _adapter_config: AdapterConfig,
        _from_stage_info: FromStageInfo,
        _to_alias: Option<String>,
        _to_representation_type: Option<String>,
    ) -> Result<StageItemResult, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Strips Content URI to remove the data which can be calculated.
    /// Allows to avoid data duplication during the content representation storage.
    /// Each adapter can cut out different parts of the Content URI depending on the storage specifics.
    /// For some adapters it can be full Content URI, for some adapters Content URI cannot be stripped at all.
    fn strip_uri(&self, _content_uri: &str) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Reconstructs the full Content URI from the stripped Content URI and additional data.
    #[allow(clippy::too_many_arguments)]
    fn reconstruct_uri(
        &self,
        _stripped_content_uri: &str,
        _adapter_config: &AdapterConfig,
        _content_id: Option<String>,
        _representation_type: Option<String>,
        _alias: Option<String>,
        _short_link_slug: Option<String>,
        _is_dynamic: bool,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Creates new adapter in the provider API, links to existing one, or forbids it depending on logic.
    async fn create_adapter(
        &self,
        _ctx: &CallContext,
        _adapter_id: &str,
        _new_config: AdapterConfig,
    ) -> Result<AdapterConfig, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Updates adapter.
    async fn update_adapter(
        &self,
        _ctx: &CallContext,
        _old_config: AdapterConfig,
        _new_config: AdapterConfig,
        _has_linked_alias: bool,
    ) -> Result<AdapterConfig, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Deletes adapter.
    async fn delete_adapter(
        &self,
        _ctx: &CallContext,
        _config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn purge_by_url(&self, _url: &str) -> Result<Option<serde_json::Value>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    /// Checks if the URI is valid for this adapter.
    fn is_valid_uri(&self, _uri: &str) -> bool {
        false
    }

    /// Gets the filename from the URI if it is possible.
    fn get_filename_from_uri(&self, _uri: &str) -> Option<String> {
        None
    }

    /// Gets the default configuration for the adapter.
    fn get_default_config(&self) -> AdapterConfig;
}
