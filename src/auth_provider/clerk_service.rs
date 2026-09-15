use crate::{
    config::{CLERK_API_BASE_URL, CLERK_API_KEY},
    services::auth_provider::auth_provider_trait::AuthProviderTrait,
    types::user_auth::auth0_user_data::AuthUserData,
};

use common_rust::prelude::ServiceError;
use kv_log_macro::{error, info};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde_json::Value;
use tonic::async_trait;

const CLERK_FORM_IDENTIFIER_EXISTS_CODE: &str = "form_identifier_exists";

pub struct ClerkService {
    client: Client,
}

impl ClerkService {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let url = format!("{}{}", *CLERK_API_BASE_URL, path);
        self.client
            .request(method, url)
            .header(CONTENT_TYPE, "application/json")
            .bearer_auth(CLERK_API_KEY.as_str())
    }

    async fn send_request_optional(
        &self,
        rb: RequestBuilder,
    ) -> Result<Option<Value>, ServiceError> {
        let response = rb.send().await.map_err(ServiceError::ReqwestError)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ServiceError::GeneralError(format!(
                "Clerk API error ({}): {}",
                status, text
            )));
        }

        let json = response.json().await.map_err(ServiceError::ReqwestError)?;
        Ok(Some(json))
    }

    async fn send_request(&self, rb: RequestBuilder) -> Result<Value, ServiceError> {
        match self.send_request_optional(rb).await? {
            Some(json) => Ok(json),
            None => Err(ServiceError::NotFoundError(
                "The requested resource does not exist in Clerk".to_string(),
            )),
        }
    }

    fn map_auth_data_from_get_user_response(
        &self,
        user: &Value,
    ) -> Result<AuthUserData, ServiceError> {
        let email = user["email_addresses"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|e| e["email_address"].as_str())
            .unwrap_or_default()
            .to_string();

        Ok(AuthUserData {
            user_id: user["id"].as_str().unwrap_or_default().to_string(),
            email,
            department: user["public_metadata"]["department"]
                .as_str()
                .map(String::from),
            first_name: user["first_name"].as_str().map(String::from),
            last_name: user["last_name"].as_str().map(String::from),
            last_login: Some(user["last_active_at"].to_string()),
            picture: user["image_url"].as_str().map(String::from),
            username: user["username"].as_str().map(String::from),
        })
    }

    fn map_auth_data_from_get_invited_user_response(
        &self,
        user: &Value,
    ) -> Result<AuthUserData, ServiceError> {
        let email = user["email_address"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        Ok(AuthUserData {
            user_id: email.clone(),
            email,
            department: None,
            first_name: None,
            last_name: None,
            last_login: None,
            picture: None,
            username: None,
        })
    }

    async fn resolve_user_id(
        &self,
        email: &str,
        provided_id: &Option<String>,
    ) -> Result<String, ServiceError> {
        if let Some(id) = provided_id {
            return Ok(id.clone());
        }

        let users = self
            .get_user_data_with_user_emails(&[email.to_string()])
            .await?;
        users.first().map(|u| u.user_id.clone()).ok_or_else(|| {
            ServiceError::NotFoundError(format!("User with email {} not found", email))
        })
    }
}

#[async_trait]
impl AuthProviderTrait for ClerkService {
    async fn get_org_user_data(
        &self,
        org_id: String,
        search_query: Option<String>,
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        let page_size: usize = 500;
        let invited_email_filter = search_query.as_ref().map(|query| query.to_lowercase());
        let users_base_path = if let Some(query) = search_query.as_ref() {
            format!(
                "/v1/users?organization_id={}&query={}",
                org_id,
                urlencoding::encode(query)
            )
        } else {
            format!("/v1/users?organization_id={}", org_id)
        };

        let invited_users_base_path =
            format!("/v1/organizations/{}/invitations?status=pending", org_id,);

        let (users_auth_data, invited_users_auth_data) = tokio::join!(
            async {
                let mut users_auth_data = Vec::new();
                let mut offset = 0usize;

                loop {
                    let user_path =
                        format!("{}&offset={offset}&limit={page_size}", users_base_path);
                    let users_query_json = self
                        .send_request(self.request(Method::GET, &user_path))
                        .await?;

                    let users_query_array = users_query_json
                        .as_array()
                        .or(users_query_json["data"].as_array())
                        .ok_or_else(|| {
                            ServiceError::GeneralError(
                                "Unexpected Clerk response format".to_string(),
                            )
                        })?;

                    users_auth_data.extend(
                        users_query_array
                            .iter()
                            .map(|u| self.map_auth_data_from_get_user_response(u))
                            .collect::<Result<Vec<_>, _>>()?,
                    );

                    if users_query_array.len() < page_size {
                        break;
                    }
                    offset += page_size;
                }

                Ok::<Vec<AuthUserData>, ServiceError>(users_auth_data)
            },
            async {
                let mut invited_users_auth_data = Vec::new();
                let mut offset = 0usize;

                loop {
                    let invited_users_path = format!(
                        "{}&offset={offset}&limit={page_size}",
                        invited_users_base_path
                    );
                    let invited_users_query_json = self
                        .send_request(self.request(Method::GET, &invited_users_path))
                        .await?;

                    let invited_users_query_array = invited_users_query_json
                        .as_array()
                        .or(invited_users_query_json["data"].as_array())
                        .ok_or_else(|| {
                            ServiceError::GeneralError(
                                "Unexpected Clerk response format".to_string(),
                            )
                        })?;

                    invited_users_auth_data.extend(
                        invited_users_query_array
                            .iter()
                            .filter(|invitation| {
                                if invitation["object"].as_str() != Some("organization_invitation")
                                {
                                    return false;
                                }

                                match invited_email_filter.as_ref() {
                                    Some(query) => invitation["email_address"]
                                        .as_str()
                                        .map(|email| email.to_lowercase().contains(query))
                                        .unwrap_or(false),
                                    None => true,
                                }
                            })
                            .map(|u| self.map_auth_data_from_get_invited_user_response(u))
                            .collect::<Result<Vec<_>, _>>()?,
                    );

                    if invited_users_query_array.len() < page_size {
                        break;
                    }
                    offset += page_size;
                }

                Ok::<Vec<AuthUserData>, ServiceError>(invited_users_auth_data)
            },
        );

        Ok(users_auth_data?
            .into_iter()
            .chain(invited_users_auth_data?)
            .collect())
    }

    async fn get_user_data_with_user_ids(
        &self,
        user_ids: &[String],
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        if user_ids.is_empty() {
            return Ok(vec![]);
        }

        let mut results = Vec::with_capacity(user_ids.len());
        for id in user_ids {
            let path = format!("/v1/users/{}", id);
            let response = self
                .send_request_optional(self.request(Method::GET, &path))
                .await?;

            if let Some(json) = response {
                results.push(self.map_auth_data_from_get_user_response(&json)?);
            }
        }
        Ok(results)
    }

    async fn get_user_data_with_user_emails(
        &self,
        user_emails: &[String],
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        if user_emails.is_empty() {
            return Ok(vec![]);
        }

        let query = user_emails
            .iter()
            .map(|email| format!("email_address[]={}", urlencoding::encode(email)))
            .collect::<Vec<_>>()
            .join("&");

        let path = format!("/v1/users?{}", query);
        let json = self.send_request(self.request(Method::GET, &path)).await?;

        let user_list = json["data"].as_array().or(json.as_array());

        match user_list {
            Some(arr) => arr
                .iter()
                .map(|u| self.map_auth_data_from_get_user_response(u))
                .collect(),
            None => Ok(vec![]),
        }
    }
    async fn delete_user_from_org(
        &self,
        email: &str,
        org_id: &str,
        user_id: &Option<String>,
    ) -> Result<(), ServiceError> {
        let uid = match self.resolve_user_id(email, user_id).await {
            Ok(id) => id,
            Err(ServiceError::NotFoundError(_)) => return Ok(()),
            Err(e) => return Err(e),
        };

        let path = format!("/v1/organizations/{}/memberships/{}", org_id, uid);
        self.send_request_optional(self.request(Method::DELETE, &path))
            .await?;
        Ok(())
    }

    async fn update_user(
        &self,
        email: &str,
        first_name: &str,
        last_name: &str,
        user_id: &Option<String>,
        username: &str,
    ) -> Result<(), ServiceError> {
        let uid = match self.resolve_user_id(email, user_id).await {
            Ok(id) => id,
            Err(ServiceError::NotFoundError(_)) => return Ok(()),
            Err(e) => return Err(e),
        };

        let mut body = serde_json::Map::new();
        if !first_name.is_empty() {
            body.insert("first_name".into(), first_name.into());
        }
        if !last_name.is_empty() {
            body.insert("last_name".into(), last_name.into());
        }
        if !username.is_empty() {
            body.insert("username".into(), username.into());
        }

        let path = format!("/v1/users/{}", uid);
        self.send_request_optional(
            self.request(Method::PATCH, &path)
                .json(&Value::Object(body)),
        )
        .await?;
        Ok(())
    }

    async fn invite_user(
        &self,
        org_id: &str,
        _user_id: &str,
        email: &str,
        default_site_domain: &str,
        is_admin_role: &bool,
    ) -> Result<String, ServiceError> {
        let role = if *is_admin_role {
            "org:admin"
        } else {
            "org:member"
        };
        let body = serde_json::json!({
            "email_address": email,
            "role": role,
            "redirect_url": format!("https://{}", default_site_domain)
        });

        let path = format!("/v1/organizations/{}/invitations", org_id);
        let json = self
            .send_request(self.request(Method::POST, &path).json(&body))
            .await?;

        json["id"].as_str().map(String::from).ok_or_else(|| {
            ServiceError::GeneralError(format!("Missing invite ID: {:?}", json["errors"]))
        })
    }

    async fn cancel_invitation(
        &self,
        org_id: &str,
        invitation_id: &str,
    ) -> Result<(), ServiceError> {
        let path = format!(
            "/v1/organizations/{}/invitations/{}/revoke",
            org_id, invitation_id
        );
        self.send_request(self.request(Method::POST, &path)).await?;
        Ok(())
    }

    async fn create_user(
        &self,
        email: &str,
        first_name: &str,
        last_name: &str,
        metadata: &Option<serde_json::Value>,
    ) -> Result<String, ServiceError> {
        let body = serde_json::json!({
            "email_address": [email],
            "first_name": first_name,
            "last_name": last_name,
            "skip_password_requirement": true,
            "public_metadata": metadata,
        });

        let response = self
            .request(Method::POST, "/v1/users")
            .json(&body)
            .send()
            .await
            .map_err(ServiceError::ReqwestError)?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(clerk_error("createUser", status, &text));
        }

        let json: Value = response.json().await.map_err(ServiceError::ReqwestError)?;

        json["id"].as_str().map(String::from).ok_or_else(|| {
            ServiceError::GeneralError(format!("Missing user ID: {:?}", json["errors"]))
        })
    }

    async fn create_sign_in_token(
        &self,
        user_id: &str,
        expires_in_seconds: u32,
    ) -> Result<String, ServiceError> {
        let body = serde_json::json!({
            "user_id": user_id,
            "expires_in_seconds": expires_in_seconds,
        });

        let json = self
            .send_request(self.request(Method::POST, "/v1/sign_in_tokens").json(&body))
            .await?;

        json["token"].as_str().map(String::from).ok_or_else(|| {
            ServiceError::GeneralError(format!("Missing sign-in token: {:?}", json["errors"]))
        })
    }

    async fn get_organization_public_metadata(
        &self,
        org_id: &str,
    ) -> Result<Option<Value>, ServiceError> {
        let path = format!("/v1/organizations/{org_id}");
        let response = self
            .request(Method::GET, &path)
            .send()
            .await
            .map_err(ServiceError::ReqwestError)?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(clerk_error("getOrganization", status, &text));
        }

        let json: Value = response.json().await.map_err(ServiceError::ReqwestError)?;
        Ok(Some(
            json.get("public_metadata").cloned().unwrap_or(Value::Null),
        ))
    }

    async fn merge_organization_public_metadata(
        &self,
        org_id: &str,
        public_metadata: Value,
    ) -> Result<(), ServiceError> {
        let path = format!("/v1/organizations/{org_id}/metadata");
        let body = serde_json::json!({ "public_metadata": public_metadata });

        let response = self
            .request(Method::PATCH, &path)
            .json(&body)
            .send()
            .await
            .map_err(ServiceError::ReqwestError)?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(clerk_error("mergeOrganizationMetadata", status, &text));
        }

        Ok(())
    }

    async fn create_organization(
        &self,
        organization_name: &str,
        admin_user_id: Option<String>,
        metadata: Option<serde_json::Value>,
        private_metadata: Option<serde_json::Value>,
    ) -> Result<String, ServiceError> {
        let body = serde_json::json!({
            "name": organization_name,
            "slug": organization_name.to_lowercase().replace(' ', "-"),
            "public_metadata": metadata.clone().unwrap_or(serde_json::Value::Null),
            "private_metadata": private_metadata.clone().unwrap_or(serde_json::Value::Null),
            "created_by": admin_user_id.unwrap_or_default(),
        });

        let response = self
            .request(Method::POST, "/v1/organizations")
            .json(&body)
            .send()
            .await
            .map_err(ServiceError::ReqwestError)?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            if is_duplicate_slug_error(&text) {
                info!(
                    "Clerk createOrganization rejected duplicate slug for organization_name={organization_name}"
                );
                return Err(ServiceError::AlreadyExistsError(format!(
                    "An organization named '{organization_name}' already exists. Please choose a different name."
                )));
            }

            return Err(clerk_error("createOrganization", status, &text));
        }

        let json: Value = response.json().await.map_err(ServiceError::ReqwestError)?;

        json["id"].as_str().map(String::from).ok_or_else(|| {
            ServiceError::GeneralError(format!(
                "Missing organization ID in response: {:?}",
                json["errors"]
            ))
        })
    }
}

impl Default for ClerkService {
    fn default() -> Self {
        Self::new()
    }
}

fn clerk_error(context: &str, status: StatusCode, body: &str) -> ServiceError {
    error!("Clerk {context} failed ({status}): {body}");

    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) && let Some(message) = clerk_user_facing_message(body)
    {
        return ServiceError::InvalidRequestParameters(message);
    }

    let message = format!("Clerk API error ({status}): {body}");
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        return ServiceError::NotReadyError(message);
    }

    ServiceError::GeneralError(message)
}

fn clerk_user_facing_message(body: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(body).ok()?;
    let first_error = json.get("errors")?.as_array()?.first()?;
    let message = first_error
        .get("long_message")
        .and_then(Value::as_str)
        .or_else(|| first_error.get("message").and_then(Value::as_str))?
        .trim();

    (!message.is_empty()).then(|| message.to_string())
}

fn is_duplicate_slug_error(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| {
            json.get("errors").and_then(Value::as_array).map(|errors| {
                errors.iter().any(|error| {
                    error.get("code").and_then(Value::as_str)
                        == Some(CLERK_FORM_IDENTIFIER_EXISTS_CODE)
                        && error.pointer("/meta/param_name").and_then(Value::as_str) == Some("slug")
                })
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_facing_message_prefers_long_message() {
        let body = r#"{"errors":[{"message":"is invalid","long_message":"Name must not exceed 256 characters.","code":"form_param_invalid"}]}"#;
        assert_eq!(
            clerk_user_facing_message(body),
            Some("Name must not exceed 256 characters.".to_string())
        );
    }

    #[test]
    fn user_facing_message_falls_back_to_message() {
        let body = r#"{"errors":[{"message":"is invalid","code":"form_param_invalid"}]}"#;
        assert_eq!(
            clerk_user_facing_message(body),
            Some("is invalid".to_string())
        );
    }

    #[test]
    fn validation_failure_becomes_invalid_request_parameters() {
        let body = r#"{"errors":[{"long_message":"Name must not exceed 256 characters."}]}"#;
        let err = clerk_error("createOrganization", StatusCode::UNPROCESSABLE_ENTITY, body);
        assert!(matches!(
            err,
            ServiceError::InvalidRequestParameters(message)
                if message == "Name must not exceed 256 characters."
        ));
    }

    #[test]
    fn transient_failures_are_retryable() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let err = clerk_error("createOrganization", status, "upstream exploded");
            assert!(
                common_rust::prelude::retry::is_retryable(&err),
                "{status} should be retryable"
            );
        }
    }

    #[test]
    fn non_transient_failures_are_not_retryable() {
        // 401/403 must never surface Clerk's message (it describes our API key,
        // not anything the end user can act on) and must not be retried.
        let unauthorized = clerk_error(
            "createUser",
            StatusCode::UNAUTHORIZED,
            r#"{"errors":[{"long_message":"Invalid API key"}]}"#,
        );
        assert!(matches!(unauthorized, ServiceError::GeneralError(_)));
        assert!(!common_rust::prelude::retry::is_retryable(&unauthorized));
    }

    #[test]
    fn duplicate_slug_detection() {
        let body = r#"{"errors":[{"code":"form_identifier_exists","meta":{"param_name":"slug"}}]}"#;
        assert!(is_duplicate_slug_error(body));

        let other_param = r#"{"errors":[{"code":"form_identifier_exists","meta":{"param_name":"email_address"}}]}"#;
        assert!(!is_duplicate_slug_error(other_param));
        assert!(!is_duplicate_slug_error("not json"));
    }
}
