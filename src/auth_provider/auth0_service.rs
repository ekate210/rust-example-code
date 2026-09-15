use crate::{
    config::{
        AUTH0_CLIENT_ID, AUTH0_ISSUER_BASE_URL, M2M_AUTH0_AUDIENCE, M2M_AUTH0_CLIENT_ID,
        M2M_AUTH0_CLIENT_SECRET,
    },
    services::auth_provider::auth_provider_trait::AuthProviderTrait,
    types::user_auth::auth0_user_data::AuthUserData,
    utils::authz_error::AuthzError,
};
use common_rust::prelude::ServiceError;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::collections::HashMap;
use tonic::async_trait;

pub struct Auth0Service;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct UserAuth0Data {
    pub user_id: String,
    pub user_metadata: Option<Value>,
}

impl Auth0Service {
    pub fn new() -> Self {
        Auth0Service {}
    }

    pub async fn get_auth0_token(&self) -> Result<String, ServiceError> {
        let client = reqwest::Client::builder().build().map_err(|err| {
            ServiceError::GeneralError(AuthzError::AuthProviderError(err.to_string()).to_string())
        })?;

        let token_url = format!("{}/oauth/token", *AUTH0_ISSUER_BASE_URL);
        let mut params = HashMap::new();
        params.insert("grant_type", "client_credentials");
        params.insert("client_id", M2M_AUTH0_CLIENT_ID.as_str());
        params.insert("client_secret", M2M_AUTH0_CLIENT_SECRET.as_str());
        params.insert("audience", M2M_AUTH0_AUDIENCE.as_str());

        let response = client
            .post(token_url)
            .form(&params)
            .send()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?;

        let token: String = match response.json::<serde_json::Value>().await {
            Ok(value) => value["access_token"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            Err(err) => {
                return Err(ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                ));
            }
        };

        Ok(token)
    }

    pub async fn parse_users_data(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        let body = request
            .send()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?
            .text()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?;

        let users: Vec<AuthUserData> = serde_json::from_str(&body).map_err(|err| {
            ServiceError::GeneralError(AuthzError::AuthProviderError(err.to_string()).to_string())
        })?;

        Ok(users)
    }

    pub async fn retrieve_auth0_user_id_by_email(
        &self,
        email: &str,
        client: &reqwest::Client,
        headers: &HeaderMap,
    ) -> Result<UserAuth0Data, ServiceError> {
        let query = format!("email:\"{email}\"");

        let request_url = format!(
            "{}/api/v2/users?q={}&search_engine=v3",
            *AUTH0_ISSUER_BASE_URL,
            urlencoding::encode(&query)
        );

        let res = client
            .get(&request_url)
            .headers(headers.clone())
            .send()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?;

        let users: Vec<Value> = res
            .json()
            .await
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        if let Some(u) = users.first() {
            Ok(UserAuth0Data {
                user_id: u
                    .get("user_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                user_metadata: u.get("user_metadata").cloned(),
            })
        } else {
            Err(ServiceError::NotFoundError(format!(
                "User with email [{email}] doesn't exist."
            )))
        }
    }
}

#[async_trait]
impl AuthProviderTrait for Auth0Service {
    async fn get_user_data_with_user_emails(
        &self,
        user_emails: &[String],
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        if user_emails.is_empty() {
            return Ok(vec![]);
        }

        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let token = self.get_auth0_token().await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .unwrap_or_else(|_| HeaderValue::from_str("").unwrap()),
        );

        let query = user_emails
            .iter()
            .map(|email| format!("email:\"{email}\""))
            .collect::<Vec<String>>()
            .join(" OR ");

        let request_url = format!(
            "{}/api/v2/users?q={}&per_page=20&search_engine=v3",
            *AUTH0_ISSUER_BASE_URL,
            urlencoding::encode(&query)
        );

        let request = client.get(&request_url).headers(headers);

        let users = self.parse_users_data(request).await?;

        Ok(users)
    }

    async fn delete_user_from_org(
        &self,
        email: &str,
        org_id: &str,
        _user_id: &Option<String>,
    ) -> Result<(), ServiceError> {
        let client = match reqwest::Client::builder().build() {
            Ok(client) => client,
            Err(err) => return Err(ServiceError::GeneralError(err.to_string())),
        };

        let token = self.get_auth0_token().await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .unwrap_or(HeaderValue::from_str("").unwrap()),
        );

        let request_url = format!(
            "{}/api/v2/organizations/{}/members",
            *AUTH0_ISSUER_BASE_URL, org_id
        );

        let auth_user_id = self
            .retrieve_auth0_user_id_by_email(email, &client, &headers)
            .await?
            .user_id;

        let request = client.delete(request_url).headers(headers).json(&json!({
            "members": [auth_user_id]
        }));

        request.send().await.map_or_else(
            |err| {
                Err(ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                ))
            },
            |_| Ok(()),
        )
    }

    async fn update_user(
        &self,
        email: &str,
        first_name: &str,
        last_name: &str,
        _user_id: &Option<String>,
        username: &str,
    ) -> Result<(), ServiceError> {
        let client = match reqwest::Client::builder().build() {
            Ok(client) => client,
            Err(err) => return Err(ServiceError::GeneralError(err.to_string())),
        };

        let token = self.get_auth0_token().await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .unwrap_or(HeaderValue::from_str("").unwrap()),
        );

        let auth_user_id = self
            .retrieve_auth0_user_id_by_email(email, &client, &headers)
            .await?
            .user_id;

        let request_url = format!("{}/api/v2/users/{}", *AUTH0_ISSUER_BASE_URL, auth_user_id);

        let request = client.patch(request_url).headers(headers).json(&json!({
            "first_name": first_name,
            "last_name": last_name,
            "username": username,
        }));

        request.send().await.map_or_else(
            |err| {
                Err(ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                ))
            },
            |_| Ok(()),
        )
    }

    async fn invite_user(
        &self,
        org_id: &str,
        user_id: &str,
        email: &str,
        default_site_domain: &str,
        _is_admin_role: &bool,
    ) -> Result<String, ServiceError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let token = self.get_auth0_token().await?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, "application/json".parse().unwrap());
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .unwrap_or(HeaderValue::from_str("").unwrap()),
        );
        headers.insert(
            CONTENT_TYPE,
            "application/json"
                .parse()
                .unwrap_or(HeaderValue::from_str("").unwrap()),
        );

        let existing_user = self
            .retrieve_auth0_user_id_by_email(email, &client, &headers)
            .await;

        let mut domains = match existing_user {
            Ok(user) => user
                .user_metadata
                .and_then(|m| m.get("default_site_domains").cloned())
                .unwrap_or_else(|| json!({})),
            Err(ServiceError::NotFoundError(_)) => json!({}),
            Err(e) => return Err(e),
        };

        if let Some(obj) = domains.as_object_mut() {
            obj.insert(org_id.to_string(), json!(default_site_domain));
        }

        let invite_data = json!({
            "inviter": { "name": "App" },
            "invitee": { "email": email },
            "client_id": AUTH0_CLIENT_ID.to_owned(),
            "ttl_sec": 259200,
            "send_invitation_email": true,
            "app_metadata": {
                "app_user_id": user_id,
            },
            "user_metadata": {
                "default_site_domains": domains
            }
        });

        let invite_request_url = format!(
            "{}/api/v2/organizations/{}/invitations",
            *AUTH0_ISSUER_BASE_URL, org_id
        );

        let response = client
            .post(&invite_request_url)
            .headers(headers)
            .json(&invite_data)
            .send()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?;

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let invitation_id = json["id"]
            .as_str()
            .ok_or_else(|| ServiceError::GeneralError("Missing invitation ID".into()))?
            .to_string();

        Ok(invitation_id)
    }

    async fn cancel_invitation(
        &self,
        org_id: &str,
        invitation_id: &str,
    ) -> Result<(), ServiceError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| ServiceError::GeneralError(err.to_string()))?;

        let token = self.get_auth0_token().await?;

        let cancel_invite_request_url = format!(
            "{}/api/v2/organizations/{}/invitations/{}",
            *AUTH0_ISSUER_BASE_URL, org_id, invitation_id
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .unwrap_or(HeaderValue::from_str("").unwrap()),
        );

        let _ = client
            .delete(&cancel_invite_request_url)
            .headers(headers)
            .send()
            .await
            .map_err(|err| {
                ServiceError::GeneralError(
                    AuthzError::AuthProviderError(err.to_string()).to_string(),
                )
            })?;

        Ok(())
    }
}

impl Default for Auth0Service {
    fn default() -> Self {
        Self::new()
    }
}
