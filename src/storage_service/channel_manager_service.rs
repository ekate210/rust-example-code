use crate::{
    config::BUNNY_PULL_ZONE_DOMAIN,
    services::storage_service::storage_service_trait::{
        ContentRepresentationInfo, StorageServiceTrait,
    },
    types::{
        adapter_config::{AdapterConfig, ChannelManagerConfig},
        channel_api::{
            self,
            channel_manager_service_client::ChannelManagerServiceClient,
            retrieve_content::{ContentRetrievalQuery, content_retrieval_response::AuthTypesOneof},
        },
        channel_auth_type::{URLWithAuth, UrlAuth, format_url_with_auth},
        content_properties::ContentHeadInfo,
        retrieve::RetrieveProperties,
    },
    utils::cdn_url_encryption::encrypt_url,
};
use async_trait::async_trait;
use common_rust::{
    ctx_error,
    prelude::{CallContext, ServiceError, get_client, get_service_connection},
};
use tokio::sync::OnceCell;
use futures::future::join_all;
use reqwest::{Client, header};
use serde_json::Value;
use tonic::transport::Channel;

const CHANNEL_TYPE_URI_PREFIX: &str = "chnltyp";

static CHANNEL_MANAGER_CHANNEL: OnceCell<Channel> = OnceCell::const_new();

async fn get_channel_manager_connection() -> Result<Channel, ServiceError> {
    let channel = CHANNEL_MANAGER_CHANNEL
        .get_or_try_init(|| async { Ok::<_, ServiceError>(get_service_connection!()) })
        .await?;

    Ok(channel.clone())
}

#[derive(Default)]
pub struct ChannelManager {}

impl ChannelManager {
    pub fn new() -> Self {
        ChannelManager {}
    }

    fn get_channel_id_from_uri(&self, uri: &str) -> Result<String, ServiceError> {
        if !uri.starts_with(&format!("{CHANNEL_TYPE_URI_PREFIX}::")) {
            return Err(ServiceError::ParseError(
                "Invalid content URI format for channel".to_string(),
            ));
        }

        let uri_items: Vec<&str> = uri.split("::").collect();
        if uri_items.len() < 3 {
            return Err(ServiceError::ParseError(
                "Invalid content URI format for channel".to_string(),
            ));
        }

        let channel_id = uri_items
            .last()
            .ok_or_else(|| {
                ServiceError::ParseError("Invalid content URI format for channel".to_string())
            })?
            .split('/')
            .next()
            .ok_or_else(|| {
                ServiceError::ParseError("Invalid content URI format for channel".to_string())
            })?;

        Ok(channel_id.to_string())
    }

    async fn generate_encrypted_cdn_url(
        &self,
        content_url: &str,
        url_auth: UrlAuth,
        filename: &str,
        headers: Vec<(String, String)>,
    ) -> Result<String, ServiceError> {
        let url_with_auth = URLWithAuth::new(content_url, url_auth, headers);

        let serialized_url = format_url_with_auth(&url_with_auth, filename);
        let encrypted_url = encrypt_url(serialized_url.as_str()).await?;

        Ok(format!(
            "https://{}/channel/{encrypted_url}",
            *BUNNY_PULL_ZONE_DOMAIN,
        ))
    }

    async fn retrieve_content(
        &self,
        ctx: &CallContext,
        content_uri: &str,
        filename: &str,
    ) -> Result<String, ServiceError> {
        let channel = get_channel_manager_connection().await?;
        let mut client = get_client!(ctx, channel, ChannelManagerServiceClient);

        let channel_id = self.get_channel_id_from_uri(content_uri)?;
        let request = tonic::Request::new(channel_api::retrieve_content::Request {
            query: vec![ContentRetrievalQuery {
                channel_id: channel_id.clone(),
                content_uri: content_uri.to_string(),
            }],
        });

        match client.retrieve_content(request).await {
            Ok(res) => {
                let response = res.into_inner();
                let content_item = response.urls.into_iter().next().ok_or_else(|| {
                    ctx_error!(
                        ctx,
                        "No items found on ChannelManagerService/RetrieveContent request. Channel ID = [{}], Content URI = [{}]",
                        channel_id,
                        content_uri
                    );
                    ServiceError::NotFoundError(format!(
                        "Content not found in channel manager. Channel ID = [{channel_id}], Content URI = [{content_uri}]",
                    ))
                })?;

                let url_auth = (&content_item).into();
                let headers = content_item
                    .auth_headers
                    .into_iter()
                    .map(|h| (h.key, h.value))
                    .collect::<Vec<_>>();
                self.generate_encrypted_cdn_url(&content_item.url, url_auth, filename, headers)
                    .await
            }
            Err(err) => {
                ctx_error!(
                    ctx,
                    "ChannelManagerService/RetrieveContent call failed. Channel ID = [{}], Content URI = [{}]. Error: [{}]",
                    channel_id,
                    content_uri,
                    err
                );
                Err(ServiceError::ClientCallError(Box::new(err)))
            }
        }
    }
}

#[async_trait]
impl StorageServiceTrait for ChannelManager {
    async fn retrieve_head_infos(
        &self,
        ctx: &CallContext,
        content_uris: &[String],
        _adapter_configs: &[AdapterConfig],
    ) -> Result<Vec<ContentHeadInfo>, ServiceError> {
        let channel = get_channel_manager_connection().await?;
        let mut client = get_client!(ctx, channel, ChannelManagerServiceClient);

        let request = tonic::Request::new(channel_api::retrieve_content::Request {
            query: content_uris
                .iter()
                .map(|uri| {
                    let channel_id = self.get_channel_id_from_uri(uri)?;

                    Ok(ContentRetrievalQuery {
                        channel_id,
                        content_uri: uri.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ServiceError>>()?,
        });
        let response = client
            .retrieve_content(request)
            .await
            .map_err(|err| ServiceError::ClientCallError(Box::new(err)))?
            .into_inner();

        if response.urls.len() != content_uris.len() {
            ctx_error!(
                ctx,
                "Not all items found on ChannelManagerService/RetrieveContent request. Expected items: [{}], Found items: [{}]",
                content_uris.len(),
                response.urls.len()
            );
            return Err(ServiceError::NotFoundError(format!(
                "Content not found in channel manager. Expected items: [{}], Found items: [{}]",
                content_uris.len(),
                response.urls.len()
            )));
        }

        fn apply_auth(req: reqwest::RequestBuilder, auth: &UrlAuth) -> reqwest::RequestBuilder {
            match auth {
                UrlAuth::None => req,
                UrlAuth::Bearer(token) => req.bearer_auth(token),
                UrlAuth::Basic(username, password) => req.basic_auth(username, Some(password)),
            }
        }

        fn apply_headers(
            mut req: reqwest::RequestBuilder,
            headers: &Vec<(String, String)>,
        ) -> reqwest::RequestBuilder {
            for (k, v) in headers {
                req = req.header(k, v);
            }
            req
        }

        let futures = response.urls.into_iter().map(|item| async move {
            if item.url.is_empty() {
                ctx_error!(
                    ctx,
                    "Retrieved content from ChannelManagerService/RetrieveContent has empty URL. Channel ID = [{}], Content URI = [{}]. Saving empty metadata...",
                    item.channel_id,
                    item.content_uri
                );

                Ok(ContentHeadInfo {
                    mime_type: "".to_string(),
                    size: 0,
                    filename: None,
                })
            } else {
                let headers = item
                    .auth_headers
                    .into_iter()
                    .map(|h| (h.key, h.value))
                    .collect::<Vec<_>>();
                let auth = match &item.auth_types_oneof {
                    Some(AuthTypesOneof::Bearer(b)) => UrlAuth::Bearer(b.clone()),
                    Some(AuthTypesOneof::BasicAuth(b)) => {
                        UrlAuth::Basic(b.username.clone(), b.password.clone())
                    }
                    _ => UrlAuth::None,
                };

                let url_with_auth = URLWithAuth::new(
                    item.url.clone(),
                    auth,
                    headers,
                );

                let client = Client::new();

                let mut head_request = client.head(&url_with_auth.url);
                head_request = apply_auth(head_request, &url_with_auth.auth);
                head_request = apply_headers(head_request, &url_with_auth.headers);

                let mut response = head_request
                    .send()
                    .await
                    .map_err(ServiceError::ReqwestError)?;

                let mut status_code = response.status().as_u16();
                if status_code == 403 || status_code == 401 || status_code == 501 {
                    //attempt range get
                    let mut get_request = client
                        .get(&url_with_auth.url)
                        .header("Range", "bytes=0-");

                    get_request = apply_auth(get_request, &url_with_auth.auth);
                    get_request = apply_headers(get_request, &url_with_auth.headers);

                    response = get_request
                        .send()
                        .await
                        .map_err(ServiceError::ReqwestError)?;
                    status_code = response.status().as_u16();
                }

                if !(200..=399).contains(&status_code) {
                    return Err(ServiceError::GeneralError(format!(
                        "Head request to content failed with status [{status_code}]"
                    )));
                }

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
            }
        });
        let results: Vec<Result<_, ServiceError>> = join_all(futures).await;
        let head_infos = results.into_iter().collect::<Result<Vec<_>, _>>()?;

        Ok(head_infos)
    }

    async fn generate_pre_signed_url(
        &self,
        ctx: &CallContext,
        _adapter_id: Option<String>,
        content_uri: &str,
        _adapter_config: AdapterConfig,
        _retrieve_properties: Option<RetrieveProperties>,
        content_repr_properties: Option<ContentRepresentationInfo>,
    ) -> Result<String, ServiceError> {
        let filename = match content_repr_properties {
            Some(props) => props.filename,
            None => {
                return Err(ServiceError::GeneralError(
                    "Representation properties should be provided to generate pre-signed url."
                        .to_string(),
                ));
            }
        };
        let content_item_url = self.retrieve_content(ctx, content_uri, &filename).await?;

        Ok(content_item_url)
    }

    fn strip_uri(&self, content_uri: &str) -> Result<String, ServiceError> {
        // No changes
        Ok(content_uri.to_string())
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

    async fn create_adapter(
        &self,
        _ctx: &CallContext,
        _adapter_id: &str,
        _new_config: AdapterConfig,
    ) -> Result<AdapterConfig, ServiceError> {
        // Content service connected to only one default instance of channel manager
        Err(ServiceError::InvalidRequestParameters(
            "Channel manager adapter creation is not supported".to_string(),
        ))
    }

    async fn update_adapter(
        &self,
        _ctx: &CallContext,
        _old_config: AdapterConfig,
        _new_config: AdapterConfig,
        _has_linked_alias: bool,
    ) -> Result<AdapterConfig, ServiceError> {
        // Content service connected to only one default instance of channel manager
        Err(ServiceError::InvalidRequestParameters(
            "Channel manager adapter updating is not supported".to_string(),
        ))
    }

    async fn delete_adapter(
        &self,
        _ctx: &CallContext,
        _config: AdapterConfig,
    ) -> Result<(), ServiceError> {
        // Content service connected to only one default instance of channel manager
        Err(ServiceError::InvalidRequestParameters(
            "Channel manager adapter deletion is not supported".to_string(),
        ))
    }

    async fn purge_by_url(&self, _url: &str) -> Result<Option<Value>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    fn is_valid_uri(&self, uri: &str) -> bool {
        uri.starts_with(&format!("{CHANNEL_TYPE_URI_PREFIX}::"))
    }

    fn get_filename_from_uri(&self, _uri: &str) -> Option<String> {
        None
    }

    fn get_default_config(&self) -> AdapterConfig {
        AdapterConfig::ChannelManager(ChannelManagerConfig {})
    }
}
