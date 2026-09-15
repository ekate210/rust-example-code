use crate::{
    config::{
        AWS_REGION, PRE_SIGNED_URL_EXPIRATION_TIME, S3_BUCKET, UNKNOWN_MIME_TYPE,
        UPLOAD_CHUNK_SIZE,
    },
    services::{
        storage_service::storage_service_trait::{
            ContentRepresentationInfo, FromStageInfo, GenerateUploadUrlOptions,
            GenerateUploadUrlResult, StageItemResult, StorageServiceTrait,
        },
        storage_service_factory::STORAGE_SERVICE_FACTORY,
    },
    types::{
        adapter_config::{AdapterConfig, AwsS3Config},
        content_properties::ContentHeadInfo,
        retrieve::RetrieveProperties,
    },
    utils::{
        content_disposition::build_content_disposition, content_error::ContentError,
        content_properties_mapping::resolve_mime_type,
    },
};
use async_trait::async_trait;
use aws_config::Region;
use aws_sdk_s3::{
    client::Client, config::BehaviorVersion, error::SdkError, presigning::PresigningConfig,
};
use common_rust::{
    ctx_error,
    prelude::{CallContext, ServiceError},
};
use futures::{StreamExt, future::join_all};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::{collections::HashMap, time::Duration};
use tokio::sync::RwLock;

const AWS_S3_URI_SCHEME: &str = "s3://";
const AWS_S3_TAGGING_HEADER: &str = "x-amz-tagging";
const AWS_S3_MAX_OBJECT_TAGS: usize = 10;
const AWS_S3_MAX_TAG_KEY_CHARS: usize = 128;
const AWS_S3_MAX_TAG_VALUE_CHARS: usize = 256;

struct AwsS3Uri {
    bucket_name: String,
    object_key: String,
    filename: String,
}

impl AwsS3Uri {
    fn parse(uri: &str) -> Result<Self, ServiceError> {
        let uri_without_scheme = uri.strip_prefix(AWS_S3_URI_SCHEME).ok_or_else(|| {
            ServiceError::InvalidRequestParameters(format!("Invalid AWS S3 URI [{uri}] format"))
        })?;

        let idx = uri_without_scheme.find('/').ok_or_else(|| {
            ServiceError::InvalidRequestParameters(format!("Invalid AWS S3 URI [{uri}] format"))
        })?;
        let bucket_name = &uri_without_scheme[..idx];
        let object_key = &uri_without_scheme[(idx + 1)..];

        let idx = uri_without_scheme.rfind('/').ok_or_else(|| {
            ServiceError::InvalidRequestParameters(format!("Invalid AWS S3 URI [{uri}] format"))
        })?;
        let filename = &uri_without_scheme[(idx + 1)..];

        Ok(Self {
            bucket_name: bucket_name.to_string(),
            object_key: object_key.to_string(),
            filename: filename.to_string(),
        })
    }

    fn is_valid(uri: &str) -> bool {
        Self::parse(uri).is_ok()
    }
}

// S3 clients are cheap to clone (internally Arc'd) but expensive to build:
// every aws_config::load() constructs a fresh credential provider chain, so a
// per-request client re-resolves credentials over HTTP (ECS/IMDS/STS) with no
// caching or connection reuse. Under bulk ingest those lookups time out and
// surface as SdkError::DispatchFailure ("dispatch failure"). The client only
// depends on the adapter's region (configs carry no credentials), so cache one
// per region; the SDK then caches and auto-refreshes credentials itself.
static S3_CLIENTS: Lazy<RwLock<HashMap<String, Client>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

const S3_TRANSIENT_MAX_RETRIES: u32 = 2;

// Bounded retry for transient connection-level failures (same classification
// as retrieve_head_infos): DispatchFailure covers connect timeouts / DNS / TLS
// to AWS, which a moment later usually succeed.
async fn with_transient_retry<T, E, R, Fut>(
    operation: &str,
    mut op: impl FnMut() -> Fut,
) -> Result<T, ServiceError>
where
    Fut: std::future::Future<Output = Result<T, SdkError<E, R>>>,
    E: std::fmt::Display,
{
    let mut attempt: u32 = 0;

    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let is_transient =
                    matches!(&err, SdkError::DispatchFailure(_) | SdkError::TimeoutError(_));

                if is_transient && attempt < S3_TRANSIENT_MAX_RETRIES {
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    attempt += 1;
                    continue;
                }

                return Err(ServiceError::GeneralError(
                    ContentError::AwsS3ClientError(format!("S3 {operation} failed: {err}"))
                        .to_string(),
                ));
            }
        }
    }
}

#[derive(Default)]
pub struct AwsS3Service {}

impl AwsS3Service {
    pub fn new() -> Self {
        AwsS3Service {}
    }

    fn build_tagging_from_metadata(
        metadata: &HashMap<String, String>,
    ) -> Result<Option<String>, ServiceError> {
        if metadata.is_empty() {
            return Ok(None);
        }

        if metadata.len() > AWS_S3_MAX_OBJECT_TAGS {
            return Err(ServiceError::InvalidRequestParameters(format!(
                "AWS S3 object tagging supports at most {AWS_S3_MAX_OBJECT_TAGS} metadata items"
            )));
        }

        let mut pairs: Vec<_> = metadata.iter().collect();
        pairs.sort_unstable_by_key(|(key, _)| *key);

        let mut encoded_pairs = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if key.is_empty() {
                return Err(ServiceError::InvalidRequestParameters(
                    "AWS S3 object tag keys cannot be empty".to_string(),
                ));
            }

            if key.chars().count() > AWS_S3_MAX_TAG_KEY_CHARS {
                return Err(ServiceError::InvalidRequestParameters(format!(
                    "AWS S3 object tag key [{key}] exceeds {AWS_S3_MAX_TAG_KEY_CHARS} characters"
                )));
            }

            if value.chars().count() > AWS_S3_MAX_TAG_VALUE_CHARS {
                return Err(ServiceError::InvalidRequestParameters(format!(
                    "AWS S3 object tag value for key [{key}] exceeds {AWS_S3_MAX_TAG_VALUE_CHARS} characters"
                )));
            }

            encoded_pairs.push(format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            ));
        }

        Ok(Some(encoded_pairs.join("&")))
    }

    fn add_tagging_header(
        request: reqwest::RequestBuilder,
        tagging: Option<&str>,
    ) -> reqwest::RequestBuilder {
        match tagging {
            Some(tagging) => request.header(AWS_S3_TAGGING_HEADER, tagging),
            None => request,
        }
    }

    fn build_upload_headers<'a, 'b>(
        headers: impl Iterator<Item = (&'a str, &'b str)>,
    ) -> Result<Option<String>, ServiceError> {
        let headers: HashMap<String, String> = headers
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();

        if headers.is_empty() {
            return Ok(None);
        }

        serde_json::to_string(&headers)
            .map(Some)
            .map_err(|err| ServiceError::GeneralError(err.to_string()))
    }

    async fn new_client_from_config(
        &self,
        aws_s3_config: &AwsS3Config,
    ) -> Result<Client, ServiceError> {
        {
            let clients = S3_CLIENTS.read().await;

            if let Some(client) = clients.get(&aws_s3_config.region) {
                return Ok(client.clone());
            }
        }

        let aws_endpoint_url = format!("https://s3.{}.amazonaws.com", aws_s3_config.region);
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .endpoint_url(aws_endpoint_url)
            .region(Region::new(aws_s3_config.region.to_owned()))
            .behavior_version(BehaviorVersion::latest())
            .load()
            .await;

        let mut clients = S3_CLIENTS.write().await;
        let client = clients
            .entry(aws_s3_config.region.clone())
            .or_insert_with(|| Client::new(&sdk_config))
            .clone();

        Ok(client)
    }
}

#[async_trait]
impl StorageServiceTrait for AwsS3Service {
    async fn retrieve_head_infos(
        &self,
        ctx: &CallContext,
        content_uris: &[String],
        adapter_configs: &[AdapterConfig],
    ) -> Result<Vec<ContentHeadInfo>, ServiceError> {
        let futures = content_uris
            .iter()
            .zip(adapter_configs.iter().cloned())
            .map(|(content_uri, adapter_config)| async move {
                let AdapterConfig::AwsS3(aws_s3_config) = adapter_config else {
                    return Err(ServiceError::InvalidRequestParameters(
                        "Invalid AWS S3 Adapter config".to_string(),
                    ));
                };

                let client = self.new_client_from_config(&aws_s3_config).await?;
                let pared_uri = AwsS3Uri::parse(content_uri)?;

                if pared_uri.bucket_name != aws_s3_config.bucket_name {
                    return Err(ServiceError::InvalidRequestParameters(format!(
                        "Content URI = [{}] and Bucket Name = [{}] are mismatched",
                        content_uri, aws_s3_config.bucket_name
                    )));
                }

            let max_retries = 2;

            for attempt in 0..=max_retries {
                if attempt > 0 {
                    let delay = std::time::Duration::from_secs(2u64.pow(attempt as u32 - 1));
                    tokio::time::sleep(delay).await;
                }

                let response_result = client
                    .head_object()
                    .bucket(pared_uri.bucket_name.clone())
                    .key(pared_uri.object_key.clone())
                    .send()
                    .await;

                match response_result {
                    Ok(response) => {
                        return Ok(ContentHeadInfo {
                            mime_type: response.content_type.unwrap_or_default(),
                            size: response.content_length.unwrap_or_default() as u64,
                            filename: None,
                        });
                    }
                    Err(err) => {
                        let is_transient = matches!(&err, SdkError::DispatchFailure(_) | SdkError::TimeoutError(_));

                        if is_transient && attempt < max_retries {
                            continue;
                        }

                        ctx_error!(
                            ctx,
                            "Failed to retrieve head info after {} attempts: content URI [{}]. Debug error [{:?}]",
                            attempt + 1,
                            content_uri,
                            err
                        );

                        return Err(ServiceError::GeneralError(
                            ContentError::AwsS3ClientError(format!(
                                "S3 HeadObject failed for [{content_uri}]: {err}"
                            ))
                            .to_string(),
                        ));
                    }
                }
            }

            Err(ServiceError::GeneralError(
                ContentError::AwsS3ClientError(format!(
                    "Failed to retrieve head info after {} retries: content URI [{content_uri}]",
                    max_retries
                ))
                .to_string(),
            ))
        });

        let results = join_all(futures).await;
        let mut head_infos = vec![];

        for result in results {
            match result {
                Ok(head_info) => {
                    head_infos.push(head_info);
                }
                Err(err) => {
                    return Err(err);
                }
            }
        }

        Ok(head_infos)
    }

    async fn purge(
        &self,
        ctx: &CallContext,
        content_uri: &str,
        adapter_config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        let AdapterConfig::AwsS3(aws_s3_config) = adapter_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid AWS S3 Adapter config".to_string(),
            ));
        };

        let client = self.new_client_from_config(&aws_s3_config).await?;
        let pared_uri = AwsS3Uri::parse(content_uri)?;

        if pared_uri.bucket_name != aws_s3_config.bucket_name {
            return Err(ServiceError::InvalidRequestParameters(format!(
                "Content URI = [{}] and Bucket Name = [{}] are mismatched",
                content_uri, aws_s3_config.bucket_name
            )));
        }

        let response_result = client
            .delete_object()
            .bucket(pared_uri.bucket_name)
            .key(pared_uri.object_key)
            .send()
            .await;

        return match response_result {
            Ok(_) => Ok(()),
            Err(err) => {
                ctx_error!(
                    ctx,
                    "Failed to purge content from AWS S3: uri - [{}]",
                    content_uri
                );
                Err(ServiceError::GeneralError(
                    ContentError::AwsS3ClientError(err.to_string()).to_string(),
                ))
            }
        };
    }

    async fn generate_pre_signed_url(
        &self,
        _ctx: &CallContext,
        _adapter_id: Option<String>,
        content_uri: &str,
        adapter_config: AdapterConfig,
        _retrieve_properties: Option<RetrieveProperties>,
        content_repr_properties: Option<ContentRepresentationInfo>,
    ) -> Result<String, ServiceError> {
        let AdapterConfig::AwsS3(aws_s3_config) = adapter_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid AWS S3 Adapter config".to_string(),
            ));
        };

        let client = self.new_client_from_config(&aws_s3_config).await?;
        let pared_uri = AwsS3Uri::parse(content_uri)?;

        if pared_uri.bucket_name != aws_s3_config.bucket_name {
            return Err(ServiceError::InvalidRequestParameters(format!(
                "Content URI = [{}] and Bucket Name = [{}] are mismatched",
                content_uri, aws_s3_config.bucket_name
            )));
        }

        // Override the response Content-Disposition so browsers render
        // displayable types inline and save everything else as an attachment
        // under the representation's real filename. Without this, downloads
        // served through a CDN path like /{content_id}/original arrive as an
        // extensionless file named "original".
        let response_overrides = content_repr_properties.as_ref().map(|props| {
            let filename = if props.filename.trim().is_empty() {
                pared_uri.filename.clone()
            } else {
                props.filename.clone()
            };
            let resolved_mime_type = resolve_mime_type(Some(&props.mime_type), None, &filename, None);
            let content_disposition = build_content_disposition(&resolved_mime_type, &filename);

            // Only override Content-Type when we resolved a real type; an
            // octet-stream override could downgrade a correct type stored on
            // the S3 object itself.
            let content_type =
                (resolved_mime_type != UNKNOWN_MIME_TYPE).then_some(resolved_mime_type);

            (content_disposition, content_type)
        });

        let expires_in = Duration::from_secs(PRE_SIGNED_URL_EXPIRATION_TIME.to_owned());
        let presigning_config = PresigningConfig::expires_in(expires_in)
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let presigned_request = with_transient_retry("presign GetObject", || {
            let mut request = client
                .get_object()
                .bucket(pared_uri.bucket_name.clone())
                .key(pared_uri.object_key.clone());

            if let Some((content_disposition, content_type)) = &response_overrides {
                request = request.response_content_disposition(content_disposition);

                if let Some(content_type) = content_type {
                    request = request.response_content_type(content_type);
                }
            }

            request.presigned(presigning_config.clone())
        })
        .await?;

        Ok(presigned_request.uri().to_owned())
    }

    async fn generate_upload_url(
        &self,
        ctx: &CallContext,
        adapter_config: AdapterConfig,
        options: GenerateUploadUrlOptions,
    ) -> Result<GenerateUploadUrlResult, ServiceError> {
        let AdapterConfig::AwsS3(aws_s3_config) = adapter_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid AWS S3 Adapter config".to_string(),
            ));
        };

        let client = self.new_client_from_config(&aws_s3_config).await?;
        let object_key = format!(
            "{}/{}/{}/{}",
            ctx.org_id, options.content_id, options.representation_type, options.filename
        );
        let content_uri = format!("s3://{}/{}", aws_s3_config.bucket_name, object_key);
        let pared_uri = AwsS3Uri::parse(&content_uri)?;

        if pared_uri.bucket_name != aws_s3_config.bucket_name {
            return Err(ServiceError::InvalidRequestParameters(format!(
                "Content URI = [{}] and Bucket Name = [{}] are mismatched",
                content_uri, aws_s3_config.bucket_name
            )));
        }

        let expires_in = Duration::from_secs(*PRE_SIGNED_URL_EXPIRATION_TIME);
        let tagging = options
            .metadata
            .as_ref()
            .map(Self::build_tagging_from_metadata)
            .transpose()?
            .flatten();

        let presigning_config = PresigningConfig::expires_in(expires_in)
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let presigned_request = with_transient_retry("presign PutObject", || {
            client
                .put_object()
                .bucket(pared_uri.bucket_name.clone())
                .key(pared_uri.object_key.clone())
                .content_type(options.mime_type.clone())
                .set_tagging(tagging.clone())
                .presigned(presigning_config.clone())
        })
        .await?;

        Ok(GenerateUploadUrlResult {
            upload_url: presigned_request.uri().to_owned(),
            content_uri,
            headers: Self::build_upload_headers(presigned_request.headers())?,
        })
    }

    async fn upload_content(
        &self,
        ctx: &CallContext,
        to_adapter_config: AdapterConfig,
        from_adapter_id: &str,
        from_adapter_config: AdapterConfig,
        from_content_representation_info: ContentRepresentationInfo,
        to_representation_type: &str,
    ) -> Result<String, ServiceError> {
        let from_adapter_instance = STORAGE_SERVICE_FACTORY.get_by_id(from_adapter_id)?;
        let from_pre_signed_url = from_adapter_instance
            .generate_pre_signed_url(
                ctx,
                Some(from_adapter_id.to_string()),
                &from_content_representation_info.uri,
                from_adapter_config,
                None,
                Some(ContentRepresentationInfo {
                    content_id: from_content_representation_info.content_id.clone(),
                    representation_type: from_content_representation_info.representation_type,
                    uri: from_content_representation_info.uri.clone(),
                    filename: from_content_representation_info.filename.clone(),
                    mime_type: from_content_representation_info.mime_type.clone(),
                    metadata: from_content_representation_info.metadata.clone(),
                }),
            )
            .await?;

        let upload_result = self
            .generate_upload_url(
                ctx,
                to_adapter_config,
                GenerateUploadUrlOptions {
                    content_id: from_content_representation_info.content_id,
                    representation_type: to_representation_type.to_string(),
                    filename: from_content_representation_info.filename,
                    mime_type: from_content_representation_info.mime_type.clone(),
                    metadata: from_content_representation_info.metadata.clone(),
                },
            )
            .await?;

        let tagging = from_content_representation_info
            .metadata
            .as_ref()
            .map(Self::build_tagging_from_metadata)
            .transpose()?
            .flatten();

        let client = reqwest::Client::new();
        let response = client
            .get(&from_pre_signed_url)
            .send()
            .await
            .map_err(ServiceError::ReqwestError)?;
        let mut bytes_stream = response.bytes_stream();
        let mut buffer = Vec::new();

        while let Some(item) = bytes_stream.next().await {
            let item = item.map_err(|err| ServiceError::GeneralError(err.to_string()))?;
            buffer.extend_from_slice(&item);

            if buffer.len() >= UPLOAD_CHUNK_SIZE {
                let request = client
                    .put(&upload_result.upload_url)
                    .body(buffer.to_vec())
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        &from_content_representation_info.mime_type,
                    );

                Self::add_tagging_header(request, tagging.as_deref())
                    .send()
                    .await?
                    .error_for_status()?;
                buffer.clear();
            }
        }

        if !buffer.is_empty() {
            let request = client
                .put(&upload_result.upload_url)
                .body(buffer.to_vec())
                .header(
                    reqwest::header::CONTENT_TYPE,
                    &from_content_representation_info.mime_type,
                );

            Self::add_tagging_header(request, tagging.as_deref())
                .send()
                .await?
                .error_for_status()?;
            buffer.clear();
        }

        Ok(upload_result.content_uri)
    }

    async fn stage(
        &self,
        ctx: &CallContext,
        _to_adapter_id: &str,
        to_adapter_config: AdapterConfig,
        from_stage_info: FromStageInfo,
        to_alias: Option<String>,
        to_representation_type: Option<String>,
    ) -> Result<StageItemResult, ServiceError> {
        if to_alias.is_some() {
            return Err(ServiceError::InvalidRequestParameters(
                "Stage to alias is not supported for AWS S3".to_string(),
            ));
        }

        let to_representation_type = to_representation_type.unwrap_or(
            from_stage_info
                .content_representation_info
                .representation_type
                .clone(),
        );

        let content_uri = self
            .upload_content(
                ctx,
                to_adapter_config,
                &from_stage_info.adapter_id,
                from_stage_info.adapter_config.clone(),
                from_stage_info.content_representation_info.clone(),
                &to_representation_type,
            )
            .await?;
        let stripped_uri = self.strip_uri(&content_uri)?;

        let head_info = self
            .retrieve_head_infos(
                ctx,
                std::slice::from_ref(&content_uri),
                &[from_stage_info.adapter_config],
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ServiceError::GeneralError(format!(
                    "Failed to get head info for Content URI: [{content_uri}]"
                ))
            })?;

        Ok(StageItemResult {
            uri: content_uri,
            stripped_uri,
            representation_type: to_representation_type,
            head_info,
            filename: from_stage_info.content_representation_info.filename,
        })
    }

    fn strip_uri(&self, content_uri: &str) -> Result<String, ServiceError> {
        let parsed_uri = AwsS3Uri::parse(content_uri)?;

        Ok(parsed_uri.object_key)
    }

    fn reconstruct_uri(
        &self,
        stripped_content_uri: &str,
        adapter_config: &AdapterConfig,
        _content_id: Option<String>,
        _representation_type: Option<String>,
        _alias: Option<String>,
        _short_link_slug: Option<String>,
        _is_dynamic: bool,
    ) -> Result<String, ServiceError> {
        // Return Content URI if already built (backwards compatibility)
        if stripped_content_uri.starts_with("s3://") {
            return Ok(stripped_content_uri.to_string());
        }

        let AdapterConfig::AwsS3(aws_s3_config) = adapter_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid AWS S3 Adapter config".to_string(),
            ));
        };

        let content_uri = format!(
            "s3://{}/{}",
            aws_s3_config.bucket_name, stripped_content_uri
        );

        Ok(content_uri)
    }

    // TODO: Investigate S3 Bucket API creation/updating/deleting logic
    async fn create_adapter(
        &self,
        _ctx: &CallContext,
        _adapter_id: &str,
        new_config: AdapterConfig,
    ) -> Result<AdapterConfig, ServiceError> {
        Ok(new_config)
    }

    async fn update_adapter(
        &self,
        _ctx: &CallContext,
        _old_config: AdapterConfig,
        new_config: AdapterConfig,
        _has_linked_alias: bool,
    ) -> Result<AdapterConfig, ServiceError> {
        Ok(new_config)
    }

    async fn delete_adapter(
        &self,
        _ctx: &CallContext,
        _config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn purge_by_url(&self, _url: &str) -> Result<Option<Value>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    fn is_valid_uri(&self, uri: &str) -> bool {
        AwsS3Uri::is_valid(uri)
    }

    fn get_filename_from_uri(&self, uri: &str) -> Option<String> {
        AwsS3Uri::parse(uri)
            .ok()
            .map(|parsed_uri| parsed_uri.filename)
    }

    fn get_default_config(&self) -> AdapterConfig {
        AdapterConfig::AwsS3(AwsS3Config {
            app_name: "app".to_string(),
            region: AWS_REGION.to_owned(),
            bucket_name: S3_BUCKET.to_owned(),
            verify_pending_on_read: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_invalid_request_parameters(result: Result<Option<String>, ServiceError>) {
        assert!(matches!(
            result,
            Err(ServiceError::InvalidRequestParameters(_))
        ));
    }

    #[test]
    fn build_tagging_from_metadata_returns_none_for_empty_metadata() {
        let metadata = HashMap::new();

        let tagging = AwsS3Service::build_tagging_from_metadata(&metadata).unwrap();

        assert_eq!(tagging, None);
    }

    #[test]
    fn build_tagging_from_metadata_encodes_and_sorts_metadata() {
        let metadata = HashMap::from([
            ("source".to_string(), "cms".to_string()),
            ("campaign name".to_string(), "summer & fall".to_string()),
            ("empty".to_string(), "".to_string()),
        ]);

        let tagging = AwsS3Service::build_tagging_from_metadata(&metadata)
            .unwrap()
            .unwrap();

        assert_eq!(
            tagging,
            "campaign%20name=summer%20%26%20fall&empty=&source=cms"
        );
    }

    #[test]
    fn build_tagging_from_metadata_rejects_empty_tag_key() {
        let metadata = HashMap::from([("".to_string(), "value".to_string())]);

        assert_invalid_request_parameters(AwsS3Service::build_tagging_from_metadata(&metadata));
    }

    #[test]
    fn build_tagging_from_metadata_rejects_more_than_ten_metadata_items() {
        let metadata: HashMap<_, _> = (0..=AWS_S3_MAX_OBJECT_TAGS)
            .map(|idx| (format!("key-{idx}"), "value".to_string()))
            .collect();

        assert_invalid_request_parameters(AwsS3Service::build_tagging_from_metadata(&metadata));
    }

    #[test]
    fn build_tagging_from_metadata_rejects_overlong_key() {
        let metadata = HashMap::from([(
            "k".repeat(AWS_S3_MAX_TAG_KEY_CHARS + 1),
            "value".to_string(),
        )]);

        assert_invalid_request_parameters(AwsS3Service::build_tagging_from_metadata(&metadata));
    }

    #[test]
    fn build_tagging_from_metadata_rejects_overlong_value() {
        let metadata = HashMap::from([(
            "key".to_string(),
            "v".repeat(AWS_S3_MAX_TAG_VALUE_CHARS + 1),
        )]);

        assert_invalid_request_parameters(AwsS3Service::build_tagging_from_metadata(&metadata));
    }

    #[test]
    fn add_tagging_header_adds_x_amz_tagging_header() {
        let client = reqwest::Client::new();

        let request = AwsS3Service::add_tagging_header(
            client.put("https://example.com/upload"),
            Some("campaign=summer"),
        )
        .build()
        .unwrap();

        assert_eq!(
            request
                .headers()
                .get(AWS_S3_TAGGING_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("campaign=summer")
        );
    }

    #[test]
    fn build_upload_headers_serializes_non_empty_headers() {
        let headers = AwsS3Service::build_upload_headers(
            [
                ("content-type", "image/png"),
                (AWS_S3_TAGGING_HEADER, "contentId=content-123"),
                ("x-empty", ""),
            ]
            .into_iter(),
        )
        .unwrap()
        .unwrap();

        let parsed: HashMap<String, String> = serde_json::from_str(&headers).unwrap();

        assert_eq!(
            parsed.get("content-type").map(String::as_str),
            Some("image/png")
        );
        assert_eq!(
            parsed.get(AWS_S3_TAGGING_HEADER).map(String::as_str),
            Some("contentId=content-123")
        );
        assert!(!parsed.contains_key("x-empty"));
    }

    #[test]
    fn build_upload_headers_returns_none_for_empty_headers() {
        let headers = AwsS3Service::build_upload_headers([("x-empty", "")].into_iter()).unwrap();

        assert_eq!(headers, None);
    }
}
