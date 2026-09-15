#![allow(dead_code)]
use crate::services::auth_provider::{
    auth_provider_trait::{AuthProviderTrait, MockAuthProviderTrait},
    auth0_service::Auth0Service,
    clerk_service::ClerkService,
};

pub enum AuthProviderType {
    Auth0,
    Clerk,
}

pub struct AuthProviderFactory {
    auth0_service: Box<dyn AuthProviderTrait>,
    clerk_service: Box<dyn AuthProviderTrait>,
}

impl AuthProviderFactory {
    pub fn new() -> Self {
        AuthProviderFactory {
            auth0_service: Box::new(Auth0Service::new()),
            clerk_service: Box::new(ClerkService::new()),
        }
    }

    pub fn get_by_enum(&self, auth_provider_type: &AuthProviderType) -> &dyn AuthProviderTrait {
        match auth_provider_type {
            AuthProviderType::Auth0 => self.auth0_service.as_ref(),
            AuthProviderType::Clerk => self.clerk_service.as_ref(),
        }
    }
}

impl AuthProviderFactory {
    pub fn new_from_mocks(
        auth0_service: Option<MockAuthProviderTrait>,
        clerk_service: Option<MockAuthProviderTrait>,
    ) -> Self {
        AuthProviderFactory {
            auth0_service: match auth0_service {
                Some(auth0_service) => Box::new(auth0_service),
                None => Box::new(Auth0Service::new()),
            },
            clerk_service: match clerk_service {
                Some(clerk_service) => Box::new(clerk_service),
                None => Box::new(ClerkService::new()),
            },
        }
    }
}

impl Default for AuthProviderFactory {
    fn default() -> Self {
        Self::new_from_mocks(None, None)
    }
}
