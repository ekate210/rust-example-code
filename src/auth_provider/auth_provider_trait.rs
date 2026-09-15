use crate::types::user_auth::auth0_user_data::AuthUserData;
use common_rust::prelude::ServiceError;
use mockall::automock;
use tonic::async_trait;

#[automock]
#[async_trait]
pub trait AuthProviderTrait: Send + Sync {
    async fn get_org_user_data(
        &self,
        _org_id: String,
        _search_query: Option<String>,
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn get_user_data_with_user_emails(
        &self,
        _user_emails: &[String],
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn get_user_data_with_user_ids(
        &self,
        _user_ids: &[String],
    ) -> Result<Vec<AuthUserData>, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn delete_user_from_org(
        &self,
        _email: &str,
        _org_id: &str,
        _user_id: &Option<String>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn update_user(
        &self,
        _email: &str,
        _first_name: &str,
        _last_name: &str,
        _user_id: &Option<String>,
        _username: &str,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn invite_user(
        &self,
        _org_id: &str,
        _user_id: &str,
        _email: &str,
        _default_site_domain: &str,
        _is_admin_role: &bool,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn cancel_invitation(
        &self,
        _org_id: &str,
        _invitation_id: &str,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn create_user(
        &self,
        _email: &str,
        _first_name: &str,
        _last_name: &str,
        _metadata: &Option<serde_json::Value>,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn create_sign_in_token(
        &self,
        _user_id: &str,
        _expires_in_seconds: u32,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn create_organization(
        &self,
        _organization_name: &str,
        _admin_user_id: Option<String>,
        _metadata: Option<serde_json::Value>,
        _private_metadata: Option<serde_json::Value>,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::ForbiddenError)
    }

    async fn get_organization_public_metadata(
        &self,
        org_id: &str,
    ) -> Result<Option<serde_json::Value>, ServiceError> {
        let _ = org_id;
        Err(ServiceError::ForbiddenError)
    }

    /// Deep-merge `public_metadata` into the organization's existing public metadata.
    async fn merge_organization_public_metadata(
        &self,
        org_id: &str,
        public_metadata: serde_json::Value,
    ) -> Result<(), ServiceError> {
        let _ = (org_id, public_metadata);
        Err(ServiceError::ForbiddenError)
    }
}
