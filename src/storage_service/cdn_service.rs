use crate::{
    config::{
        BUNNY_PULL_ZONE_DOMAIN, BUNNY_PULL_ZONE_NAME, BUNNYNET_API_KEY,
        BUNNYNET_DEFAULT_PULLZONE_ID, BUNNYNET_DEFAULT_PULLZONE_TOKEN_AUTH_KEY,
        BUNNYNET_PULLZONE_NAME_PREFIX, CDN_ADAPTER_ID_PREFIX, CONTENT_ID_PREFIX,
        PRE_SIGNED_URL_EXPIRATION_TIME,
    },
    services::{
        storage_service::storage_service_trait::{
            ContentRepresentationInfo, FromStageInfo, StageItemResult, StorageServiceTrait,
        },
        storage_service_factory::STORAGE_SERVICE_FACTORY,
    },
    types::{
        adapter_config::{AdapterConfig, CdnConfig},
        adapter_type::AdapterType,
        content_properties::ContentHeadInfo,
        retrieve::RetrieveProperties,
    },
    utils::content_properties_mapping::resolve_mime_type,
    utils::util::get_adapter_type_from_id,
};
use async_trait::async_trait;
use common_rust::{
    config::SERVICES_BASE_URL,
    ctx_error,
    prelude::{CallContext, ServiceError},
};
use futures::future::join_all;
use itertools::Itertools;
use kv_log_macro::warn;
use once_cell::sync::Lazy;
use rand::RngExt;
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use std::time;
use url::Url;

const BUNNYNET_DEFAULT_TLD: &str = ".b-cdn.net";
const BUNNYNET_API_BASE_URL: &str = "https://api.bunny.net";
const BUNNYNET_URI_SCHEME: &str = "https";
const BUNNYNET_API_CONNECT_TIMEOUT: time::Duration = time::Duration::from_secs(10);
const BUNNYNET_API_REQUEST_TIMEOUT: time::Duration = time::Duration::from_secs(60);
const BUNNYNET_API_MAX_ATTEMPTS: u32 = 3;
const BUNNYNET_API_RETRY_BASE_DELAY_MS: u64 = 200;
const BUNNYNET_API_ERROR_BODY_MAX_CHARS: usize = 512;

/// Shared HTTP client for the Bunny.net management API. Building a client per
/// request discards the connection pool, so every call paid for a fresh DNS
/// lookup and TLS handshake against Bunny's edge.
static BUNNYNET_HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .connect_timeout(BUNNYNET_API_CONNECT_TIMEOUT)
        .timeout(BUNNYNET_API_REQUEST_TIMEOUT)
        .build()
        .expect("Failed to build Bunny.net API HTTP client")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BunnynetRetryPolicy {
    Idempotent,
    NonIdempotent,
}

struct BunnynetRequestFailure {
    error: ServiceError,
    retryable: bool,
}

impl BunnynetRequestFailure {
    fn fatal(error: ServiceError) -> Self {
        Self {
            error,
            retryable: false,
        }
    }
}

fn is_retryable_bunnynet_transport_error(
    err: &reqwest::Error,
    policy: BunnynetRetryPolicy,
) -> bool {
    if err.is_builder() {
        return false;
    }
    err.is_connect() || policy == BunnynetRetryPolicy::Idempotent
}

fn is_retryable_bunnynet_status(status: StatusCode, policy: BunnynetRetryPolicy) -> bool {
    policy == BunnynetRetryPolicy::Idempotent
        && (status.is_server_error()
            || status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS)
}

fn bunnynet_retry_delay(attempt: u32) -> time::Duration {
    let base_delay_ms = BUNNYNET_API_RETRY_BASE_DELAY_MS << attempt.saturating_sub(1);
    let jitter_ms = rand::rng().random_range(0..=base_delay_ms / 4);
    time::Duration::from_millis(base_delay_ms + jitter_ms)
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        message.push_str(": ");
        message.push_str(&err.to_string());
        source = err.source();
    }
    message
}

#[derive(Default)]
pub struct CdnService;

impl CdnService {
    pub fn new() -> Self {
        CdnService {}
    }

    fn sign_authenticated_url(
        &self,
        url: &str,
        pullzone_security_key: &str,
        expiration_time: u64,
    ) -> Result<String, ServiceError> {
        let url_parsed = Url::parse(url)
            .map_err(|_| ServiceError::GeneralError("Invalid content URI format".to_string()))?;
        let signature_path = url_parsed.path();

        let expires = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs()
            + expiration_time;

        let query_params = url_parsed.query().unwrap_or_default();
        let sorted_query_params = query_params.split('&').sorted().join("&");

        let hashable_base =
            format!("{pullzone_security_key}{signature_path}{expires}{sorted_query_params}");

        let mut hasher = Sha256::new();
        hasher.update(hashable_base.as_bytes());
        let sha256_hash = hasher.finalize();

        let base64_string = base64::encode_config(sha256_hash, base64::STANDARD);

        let token = base64_string
            .replace('\n', "")
            .replace('+', "-")
            .replace('/', "_")
            .replace('=', "");

        let mut url_authenticated = url.to_string();
        if query_params.is_empty() {
            url_authenticated.push('?')
        } else {
            url_authenticated.push('&');
        }
        url_authenticated.push_str(&format!("token={token}&expires={expires}"));

        Ok(url_authenticated)
    }

    fn generate_pullzone_name_from_adapter_id(
        &self,
        adapter_id: &str,
    ) -> Result<String, ServiceError> {
        let id_suffix = adapter_id
            .strip_prefix(&format!("{CDN_ADAPTER_ID_PREFIX}_"))
            .ok_or_else(|| {
                ServiceError::GeneralError("Failed to get CND adapter ID suffix".to_string())
            })?;

        let pullzone_name = format!("{}-{}", *BUNNYNET_PULLZONE_NAME_PREFIX, id_suffix);

        if !pullzone_name
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() || c == '-')
        {
            return Err(ServiceError::GeneralError(
                "Pullzone name must consist of numbers, letters, and hyphens only".to_string(),
            ));
        }

        Ok(pullzone_name)
    }

    async fn send_bunnynet_api_request(
        &self,
        method: http::Method,
        path: &str,
        request_body_json: Option<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ServiceError> {
        self.send_bunnynet_api_request_with_policy(
            method,
            path,
            request_body_json,
            BunnynetRetryPolicy::Idempotent,
        )
        .await
    }

    async fn send_bunnynet_api_request_with_policy(
        &self,
        method: http::Method,
        path: &str,
        request_body_json: Option<serde_json::Value>,
        policy: BunnynetRetryPolicy,
    ) -> Result<Option<serde_json::Value>, ServiceError> {
        let mut attempt = 1;
        loop {
            match self
                .send_bunnynet_api_request_once(&method, path, request_body_json.as_ref(), policy)
                .await
            {
                Ok(response) => return Ok(response),
                Err(failure) if failure.retryable && attempt < BUNNYNET_API_MAX_ATTEMPTS => {
                    let delay = bunnynet_retry_delay(attempt);
                    warn!(
                        "Bunny.net API request {} /{} failed on attempt {}/{}, retrying in {}ms: {}",
                        method,
                        path,
                        attempt,
                        BUNNYNET_API_MAX_ATTEMPTS,
                        delay.as_millis(),
                        failure.error
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(failure) => return Err(failure.error),
            }
        }
    }

    async fn send_bunnynet_api_request_once(
        &self,
        method: &http::Method,
        path: &str,
        request_body_json: Option<&serde_json::Value>,
        policy: BunnynetRetryPolicy,
    ) -> Result<Option<serde_json::Value>, BunnynetRequestFailure> {
        let request_url = format!("{BUNNYNET_API_BASE_URL}/{path}");
        let mut request = BUNNYNET_HTTP_CLIENT
            .request(method.clone(), &request_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("AccessKey", BUNNYNET_API_KEY.as_str());

        if let Some(request_body_json) = request_body_json {
            request = request.json(request_body_json);
        }

        let response = request.send().await.map_err(|err| BunnynetRequestFailure {
            retryable: is_retryable_bunnynet_transport_error(&err, policy),
            error: ServiceError::GeneralError(format!(
                "Bunny.net API request {method} /{path} failed: {}",
                error_chain(&err)
            )),
        })?;

        let status = response.status();
        if !status.is_success() {
            let body: String = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(BUNNYNET_API_ERROR_BODY_MAX_CHARS)
                .collect();
            return Err(BunnynetRequestFailure {
                retryable: is_retryable_bunnynet_status(status, policy),
                error: ServiceError::GeneralError(format!(
                    "Bunny.net API request {method} /{path} responded with HTTP {status}: {body}"
                )),
            });
        }

        // Bunny.net has processed the request at this point; nothing below may be retried.
        let response_content_length = response.content_length().ok_or_else(|| {
            BunnynetRequestFailure::fatal(ServiceError::GeneralError(
                "Failed to get response content length".to_string(),
            ))
        })?;
        if response_content_length == 0 {
            return Ok(None);
        }

        let response_json: serde_json::Value = response.json().await.map_err(|err| {
            BunnynetRequestFailure::fatal(ServiceError::GeneralError(format!(
                "Bunny.net API request {method} /{path} returned an unreadable body: {}",
                error_chain(&err)
            )))
        })?;

        Ok(Some(response_json))
    }
}

#[async_trait]
impl StorageServiceTrait for CdnService {
    async fn retrieve_head_infos(
        &self,
        ctx: &CallContext,
        content_uris: &[String],
        adapter_configs: &[AdapterConfig],
    ) -> Result<Vec<ContentHeadInfo>, ServiceError> {
        let futures = content_uris
            .iter()
            .cloned()
            .zip(adapter_configs.iter().cloned())
            .map(|(content_uri, adapter_config)| async move {
                let pre_signed_url = self
                    .generate_pre_signed_url(ctx, None, &content_uri, adapter_config, None, None)
                    .await?;

                let response = Client::new()
                    .head(pre_signed_url)
                    .send()
                    .await?
                    .error_for_status()?;

                let mime_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|val| val.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let size = response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|val| val.to_str().ok())
                    .and_then(|val| val.parse::<u64>().ok())
                    .unwrap_or(0);

                Ok(ContentHeadInfo {
                    mime_type,
                    size,
                    filename: None,
                })
            });

        let head_infos = join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<ContentHeadInfo>, ServiceError>>()?;

        Ok(head_infos)
    }

    async fn purge(
        &self,
        ctx: &CallContext,
        _content_uri: &str,
        adapter_config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        let AdapterConfig::Cdn(cdn_config) = adapter_config else {
            return Err(ServiceError::GeneralError(
                "Invalid CDN adapter config".to_string(),
            ));
        };

        let request_path = format!("pullzone/{}/purgeCache", cdn_config.pull_zone_id);
        self.send_bunnynet_api_request(http::Method::POST, &request_path, None)
            .await
            .map_err(|err| {
                ctx_error!(
                    ctx,
                    "Failed to purge cache for CDN pullzone: [{}]. Reason: [{}]",
                    cdn_config.pull_zone_name,
                    err
                );
                ServiceError::GeneralError(format!(
                    "Failed to purge cache for CDN pullzone: [{}]",
                    cdn_config.pull_zone_name
                ))
            })?;

        Ok(())
    }

    async fn generate_pre_signed_url(
        &self,
        _ctx: &CallContext,
        _adapter_id: Option<String>,
        content_uri: &str,
        adapter_config: AdapterConfig,
        retrieve_properties: Option<RetrieveProperties>,
        _content_repr_properties: Option<ContentRepresentationInfo>,
    ) -> Result<String, ServiceError> {
        let AdapterConfig::Cdn(cdn_config) = adapter_config else {
            return Err(ServiceError::GeneralError(
                "Invalid CDN adapter config".to_string(),
            ));
        };

        // Unmodified URL is the content URI in the context of CDN
        let mut url = content_uri.to_string();

        if let Some(retrieve_properties) = retrieve_properties
            && (retrieve_properties.width.is_some()
                || retrieve_properties.height.is_some()
                || retrieve_properties.aspect_ratio.is_some())
        {
            url.push_str("?optimizer=image");

            if let Some(image_width) = retrieve_properties.width {
                url.push_str(&format!("&width={image_width}"));
            }

            if let Some(image_height) = retrieve_properties.height {
                url.push_str(&format!("&height={image_height}"));
            }

            if let Some(aspect_ratio) = retrieve_properties.aspect_ratio {
                url.push_str(&format!("&aspect_ratio={aspect_ratio}"));
            }
        }

        if !cdn_config.use_authentication {
            return Ok(url);
        }

        let Some(pullzone_security_key) = cdn_config.api_key else {
            return Err(ServiceError::GeneralError(
                "Pullzone security key is required in CDN config when using token authentication"
                    .to_string(),
            ));
        };
        let authenticated_url = self.sign_authenticated_url(
            &url,
            &pullzone_security_key,
            *PRE_SIGNED_URL_EXPIRATION_TIME,
        )?;

        Ok(authenticated_url)
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
        let AdapterConfig::Cdn(cdn_config) = to_adapter_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid CDN config".to_string(),
            ));
        };

        if to_alias.is_none()
            && get_adapter_type_from_id(&from_stage_info.adapter_id) == AdapterType::Cdn
        {
            return Err(ServiceError::InvalidRequestParameters(
                "Stage from CDN representation allowed only when update alias".to_string(),
            ));
        }

        if to_alias.is_some() && to_representation_type.is_some() {
            return Err(ServiceError::InvalidRequestParameters(
                "Cannot stage to alias when stage to another representation type".to_string(),
            ));
        }

        if to_alias.is_some() && !cdn_config.alias_enabled {
            return Err(ServiceError::InvalidRequestParameters(
                "Cannot stage to alias when destination CDN adapter alias disabled".to_string(),
            ));
        }

        if to_alias.is_some() && cdn_config.use_authentication {
            return Err(ServiceError::InvalidRequestParameters(
                "Cannot stage to alias when destination CDN adapter use authentication".to_string(),
            ));
        }

        // Omit ID prefix to keep URL smaller and not reveal the prefix
        let content_id_no_prefix = from_stage_info
            .content_representation_info
            .content_id
            .strip_prefix(&format!("{CONTENT_ID_PREFIX}_"))
            .ok_or_else(|| {
                ServiceError::InvalidRequestParameters("Invalid content ID format".to_string())
            })?;

        let domain = match cdn_config.domain {
            Some(domain) => domain,
            None => format!("{}{}", cdn_config.pull_zone_name, BUNNYNET_DEFAULT_TLD),
        };

        let to_representation_type = to_representation_type.unwrap_or(
            from_stage_info
                .content_representation_info
                .representation_type
                .clone(),
        );

        let to_uri = format!(
            "https://{}/{}",
            domain,
            to_alias
                .as_deref()
                .unwrap_or(&format!("{content_id_no_prefix}/{to_representation_type}",))
        );

        // INFO: If CDN representation points to another representation type we put this origin representation type into stripped URI
        let stripped_uri = if to_representation_type
            != from_stage_info
                .content_representation_info
                .representation_type
        {
            from_stage_info
                .content_representation_info
                .representation_type
        } else {
            "".to_string()
        };

        let from_adapter_service =
            STORAGE_SERVICE_FACTORY.get_by_id(&from_stage_info.adapter_id)?;
        let from_head_info = from_adapter_service
            .retrieve_head_infos(
                ctx,
                std::slice::from_ref(&from_stage_info.content_representation_info.uri),
                &[from_stage_info.adapter_config],
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ServiceError::GeneralError(format!(
                    "Failed to get head info for origin URI [{}]",
                    from_stage_info.content_representation_info.uri
                ))
            })?;
        let resolved_mime_type = resolve_mime_type(
            Some(&from_stage_info.content_representation_info.mime_type),
            Some(&from_head_info.mime_type),
            &from_stage_info.content_representation_info.filename,
            Some(&from_stage_info.content_representation_info.uri),
        );

        Ok(StageItemResult {
            uri: to_uri,
            stripped_uri,
            representation_type: to_representation_type,
            head_info: ContentHeadInfo {
                mime_type: resolved_mime_type,
                size: from_head_info.size,
                filename: from_head_info.filename,
            },
            filename: from_stage_info.content_representation_info.filename,
        })
    }

    fn strip_uri(&self, _content_uri: &str) -> Result<String, ServiceError> {
        // CDN Content URI will be fully calculated when retrieved
        Ok("".to_string())
    }
    fn reconstruct_uri(
        &self,
        _stripped_content_uri: &str,
        adapter_config: &AdapterConfig,
        content_id: Option<String>,
        representation_type: Option<String>,
        alias: Option<String>,
        short_link_slug: Option<String>,
        is_dynamic: bool,
    ) -> Result<String, ServiceError> {
        if alias.is_some() && short_link_slug.is_some() {
            return Err(ServiceError::GeneralError(
                "Cannot use \"alias\" and \"short_link\" together".to_string(),
            ));
        }

        let AdapterConfig::Cdn(cdn_config) = adapter_config else {
            return Err(ServiceError::GeneralError(
                "Invalid CDN adapter config".to_string(),
            ));
        };

        let domain = match cdn_config.domain.clone() {
            Some(domain) => domain,
            None => format!("{}{}", cdn_config.pull_zone_name, BUNNYNET_DEFAULT_TLD),
        };

        if let Some(alias) = alias {
            let content_uri = if is_dynamic {
                format!("https://{domain}/dynamic/{alias}")
            } else {
                format!("https://{domain}/{alias}")
            };

            return Ok(content_uri);
        }

        if let Some(short_link_slug) = short_link_slug {
            let content_uri = if is_dynamic {
                format!("https://{domain}/dynamic/{short_link_slug}")
            } else {
                format!("https://{domain}/{short_link_slug}")
            };

            return Ok(content_uri);
        }

        if content_id.is_none() || representation_type.is_none() {
            return Err(ServiceError::GeneralError(
                "Content ID with representation type or Alias should be provided.".to_string(),
            ));
        }

        let content_id_no_prefix = match content_id {
            Some(content_id) => {
                let owned_content_id = content_id.to_owned();
                owned_content_id
                    .strip_prefix(&format!("{CONTENT_ID_PREFIX}_"))
                    .ok_or_else(|| {
                        ServiceError::GeneralError("Invalid content ID format".to_string())
                    })?
                    .to_string()
            }
            None => {
                return Err(ServiceError::GeneralError("Missing content ID".to_string()));
            }
        };

        let representation_type = if let Some(representation_type) = representation_type {
            representation_type
        } else {
            return Err(ServiceError::GeneralError(
                "Missing representation type".to_string(),
            ));
        };

        let content_uri = if is_dynamic {
            format!("https://{domain}/dynamic/{content_id_no_prefix}/{representation_type}")
        } else {
            // No backward compatibility is needed as Content URI is fully calculated when retrieved
            format!("https://{domain}/{content_id_no_prefix}/{representation_type}")
        };

        Ok(content_uri)
    }

    async fn create_adapter(
        &self,
        _ctx: &CallContext,
        adapter_id: &str,
        new_config: AdapterConfig,
    ) -> Result<AdapterConfig, ServiceError> {
        let AdapterConfig::Cdn(new_cdn_config) = new_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid CDN config".to_string(),
            ));
        };

        if new_cdn_config.alias_enabled && new_cdn_config.use_authentication {
            return Err(ServiceError::InvalidRequestParameters(
                "Alias and authentication could not be enabled at the same time.".to_string(),
            ));
        }

        let pullzone_name = self.generate_pullzone_name_from_adapter_id(adapter_id)?;
        let pullzone_origin_url = format!("{}/content/redirect/{}", *SERVICES_BASE_URL, adapter_id);
        let request_body_json = serde_json::json!({
            "ZoneSecurityEnabled": new_cdn_config.use_authentication,
            "FollowRedirects": true,
            "OriginUrl": pullzone_origin_url,
            "Name": pullzone_name,
            "OptimizerEnabled": true,
        });
        let response_json = self
            .send_bunnynet_api_request_with_policy(
                http::Method::POST,
                "pullzone",
                Some(request_body_json),
                BunnynetRetryPolicy::NonIdempotent,
            )
            .await
            .map_err(|err| {
                ServiceError::GeneralError(format!(
                    "Failed to send create pullzone request: [{err}]"
                ))
            })?
            .ok_or_else(|| {
                ServiceError::GeneralError(
                    "Expected response body from create pullzone request".to_string(),
                )
            })?;

        let pullzone_id = response_json
            .get("Id")
            .and_then(|id| id.as_number())
            .ok_or_else(|| {
                ServiceError::GeneralError("Failed to get pullzone ID from response".to_string())
            })?;

        let pullzone_security_key = if new_cdn_config.use_authentication {
            let pullzone_security_key = response_json
                .get("ZoneSecurityKey")
                .and_then(|val| val.as_str())
                .ok_or_else(|| {
                    ServiceError::GeneralError(
                        "Failed to get pullzone security key from response".to_string(),
                    )
                })?;

            Some(pullzone_security_key.to_string())
        } else {
            None
        };

        if let Some(domain) = &new_cdn_config.domain {
            let request_path = format!("pullzone/{pullzone_id}/addHostname");
            let request_body_json = serde_json::json!({
                "Hostname": domain,
            });
            self.send_bunnynet_api_request(
                http::Method::POST,
                &request_path,
                Some(request_body_json),
            )
            .await
            .map_err(|err| {
                ServiceError::GeneralError(format!("Failed to send add hostname request: [{err}]"))
            })?;

            let request_path = format!("pullzone/loadFreeCertificate?hostname={domain}");
            self.send_bunnynet_api_request(http::Method::GET, &request_path, None)
                .await
                .map_err(|err| {
                    ServiceError::GeneralError(format!(
                        "Failed to send load free certificate request: [{err}]"
                    ))
                })?;

            let request_path = format!("pullzone/{pullzone_id}/setForceSSL");
            let request_body_json = serde_json::json!({
                "ForceSSL": true,
                "Hostname": domain,
            });
            self.send_bunnynet_api_request(
                http::Method::POST,
                &request_path,
                Some(request_body_json),
            )
            .await
            .map_err(|err| {
                ServiceError::GeneralError(format!("Failed to send set force SSL request: [{err}]"))
            })?;
        }

        let config = AdapterConfig::Cdn(CdnConfig {
            api_key: pullzone_security_key,
            pull_zone_id: pullzone_id.to_string(),
            pull_zone_name: pullzone_name,
            ..new_cdn_config
        });

        Ok(config)
    }

    async fn update_adapter(
        &self,
        _ctx: &CallContext,
        old_config: AdapterConfig,
        new_config: AdapterConfig,
        has_linked_alias: bool,
    ) -> Result<AdapterConfig, ServiceError> {
        let AdapterConfig::Cdn(old_cdn_config) = old_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid CDN config".to_string(),
            ));
        };
        let AdapterConfig::Cdn(new_cdn_config) = new_config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid CDN config".to_string(),
            ));
        };

        if new_cdn_config.alias_enabled && new_cdn_config.use_authentication {
            return Err(ServiceError::InvalidRequestParameters(
                "Cannot enable alias and use authentication at the same time".to_string(),
            ));
        }

        if !new_cdn_config.alias_enabled && has_linked_alias {
            return Err(ServiceError::InvalidRequestParameters(
                "Cannot disable alias when CDN has linked alias".to_string(),
            ));
        }

        let pullzone_security_key =
            if new_cdn_config.use_authentication != old_cdn_config.use_authentication {
                let request_path = format!("pullzone/{}", old_cdn_config.pull_zone_id);
                let request_body_json = serde_json::json!({
                    "ZoneSecurityEnabled": new_cdn_config.use_authentication,
                });
                let response_json = self
                    .send_bunnynet_api_request(
                        http::Method::POST,
                        &request_path,
                        Some(request_body_json),
                    )
                    .await
                    .map_err(|err| {
                        ServiceError::GeneralError(format!(
                            "Failed to send update pullzone request: [{err}]"
                        ))
                    })?
                    .ok_or_else(|| {
                        ServiceError::GeneralError(
                            "Expected response body from update pullzone request".to_string(),
                        )
                    })?;

                if new_cdn_config.use_authentication {
                    let pullzone_security_key = response_json
                        .get("ZoneSecurityKey")
                        .and_then(|val| val.as_str())
                        .ok_or_else(|| {
                            ServiceError::GeneralError(
                                "Failed to get pullzone security key from response".to_string(),
                            )
                        })?;

                    Some(pullzone_security_key.to_string())
                } else {
                    None
                }
            } else {
                old_cdn_config.api_key
            };

        if new_cdn_config.domain != old_cdn_config.domain {
            if let Some(domain) = &old_cdn_config.domain {
                let request_path =
                    format!("pullzone/{}/removeHostname", old_cdn_config.pull_zone_id);
                let request_body_json = serde_json::json!({
                    "Hostname": domain,
                });
                self.send_bunnynet_api_request(
                    http::Method::DELETE,
                    &request_path,
                    Some(request_body_json),
                )
                .await
                .map_err(|err| {
                    ServiceError::GeneralError(format!(
                        "Failed to send remove hostname request: [{err}]"
                    ))
                })?;
            }

            if let Some(domain) = &new_cdn_config.domain {
                let request_path = format!("pullzone/{}/addHostname", old_cdn_config.pull_zone_id);
                let request_body_json = serde_json::json!({
                    "Hostname": domain,
                });
                self.send_bunnynet_api_request(
                    http::Method::POST,
                    &request_path,
                    Some(request_body_json),
                )
                .await
                .map_err(|err| {
                    ServiceError::GeneralError(format!(
                        "Failed to send add hostname request: [{err}]"
                    ))
                })?;

                let request_path = format!("pullzone/loadFreeCertificate?hostname={domain}");
                self.send_bunnynet_api_request(http::Method::GET, &request_path, None)
                    .await
                    .map_err(|err| {
                        ServiceError::GeneralError(format!(
                            "Failed to send load free certificate request: [{err}]"
                        ))
                    })?;

                let request_path = format!("pullzone/{}/setForceSSL", old_cdn_config.pull_zone_id);
                let request_body_json = serde_json::json!({
                    "ForceSSL": true,
                    "Hostname": domain,
                });
                self.send_bunnynet_api_request(
                    http::Method::POST,
                    &request_path,
                    Some(request_body_json),
                )
                .await
                .map_err(|err| {
                    ServiceError::GeneralError(format!(
                        "Failed to send set force SSL request: [{err}]"
                    ))
                })?;
            }
        }

        // Pullzone security key calculated automatically
        // Pullzone ID cannot be changed
        // Pullzone name cannot be changed
        let config = AdapterConfig::Cdn(CdnConfig {
            api_key: pullzone_security_key,
            pull_zone_id: old_cdn_config.pull_zone_id,
            pull_zone_name: old_cdn_config.pull_zone_name,
            ..new_cdn_config
        });

        Ok(config)
    }

    async fn delete_adapter(
        &self,
        _ctx: &CallContext,
        config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        let AdapterConfig::Cdn(cdn_config) = config else {
            return Err(ServiceError::InvalidRequestParameters(
                "Invalid CDN config".to_string(),
            ));
        };

        let request_path = format!("pullzone/{}", cdn_config.pull_zone_id);
        self.send_bunnynet_api_request(http::Method::DELETE, &request_path, None)
            .await
            .map_err(|err| {
                ServiceError::GeneralError(format!(
                    "Failed to send delete pullzone request: [{err}]"
                ))
            })?;

        Ok(())
    }

    async fn purge_by_url(&self, url: &str) -> Result<Option<serde_json::Value>, ServiceError> {
        let path = format!("purge?url={url}&async=true");
        self.send_bunnynet_api_request(http::Method::POST, &path, None)
            .await
    }

    fn is_valid_uri(&self, uri: &str) -> bool {
        url::Url::parse(uri).is_ok_and(|parsed_uri| parsed_uri.scheme() == BUNNYNET_URI_SCHEME)
    }

    fn get_filename_from_uri(&self, _uri: &str) -> Option<String> {
        None
    }

    fn get_default_config(&self) -> AdapterConfig {
        AdapterConfig::Cdn(CdnConfig {
            api_key: Some(BUNNYNET_DEFAULT_PULLZONE_TOKEN_AUTH_KEY.to_owned()),
            use_authentication: true,
            pull_zone_name: BUNNY_PULL_ZONE_NAME.to_owned(),
            domain: Some(BUNNY_PULL_ZONE_DOMAIN.to_owned()),
            pull_zone_id: BUNNYNET_DEFAULT_PULLZONE_ID.to_owned(),
            alias_enabled: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_statuses_retry_only_for_idempotent_requests() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
        ] {
            assert!(
                is_retryable_bunnynet_status(status, BunnynetRetryPolicy::Idempotent),
                "{status} should retry for idempotent requests"
            );
            assert!(
                !is_retryable_bunnynet_status(status, BunnynetRetryPolicy::NonIdempotent),
                "{status} must not retry for non-idempotent requests"
            );
        }
    }

    #[test]
    fn client_errors_never_retry() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
        ] {
            for policy in [
                BunnynetRetryPolicy::Idempotent,
                BunnynetRetryPolicy::NonIdempotent,
            ] {
                assert!(
                    !is_retryable_bunnynet_status(status, policy),
                    "{status} must not retry under {policy:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn connection_failures_retry_under_both_policies() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let err = Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap_err();

        assert!(err.is_connect(), "expected a connect error, got: {err:?}");
        assert!(is_retryable_bunnynet_transport_error(
            &err,
            BunnynetRetryPolicy::Idempotent
        ));
        assert!(is_retryable_bunnynet_transport_error(
            &err,
            BunnynetRetryPolicy::NonIdempotent
        ));
    }

    #[tokio::test]
    async fn builder_errors_never_retry() {
        let err = Client::new().get("not a url").send().await.unwrap_err();

        assert!(err.is_builder(), "expected a builder error, got: {err:?}");
        assert!(!is_retryable_bunnynet_transport_error(
            &err,
            BunnynetRetryPolicy::Idempotent
        ));
    }

    #[test]
    fn retry_delay_backs_off_with_bounded_jitter() {
        for _ in 0..100 {
            let first = bunnynet_retry_delay(1).as_millis();
            let second = bunnynet_retry_delay(2).as_millis();
            assert!((200..=250).contains(&first), "first delay {first}ms");
            assert!((400..=500).contains(&second), "second delay {second}ms");
        }
    }

    #[test]
    fn error_chain_includes_sources() {
        #[derive(Debug)]
        struct Outer(std::io::Error);

        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("error sending request")
            }
        }

        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = Outer(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "failed to verify TLS certificate",
        ));

        assert_eq!(
            error_chain(&err),
            "error sending request: failed to verify TLS certificate"
        );
    }
}
