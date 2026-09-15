use crate::{
    config::{
        AWS_EFS_ADAPTER_ID_PREFIX, AWS_S3_ADAPTER_ID_PREFIX, CDN_ADAPTER_ID_PREFIX,
        CHANNEL_MANAGER_ADAPTER_ID_PREFIX,
    },
    services::storage_service::{
        aws_efs_service::AwsEfsService,
        aws_s3_service::AwsS3Service,
        cdn_service::CdnService,
        channel_manager_service::ChannelManager,
        storage_service_trait::{MockStorageServiceTrait, StorageServiceTrait},
    },
    types::adapter_type::AdapterType,
};
use common_rust::prelude::{ServiceError, get_id_prefix};
use once_cell::sync::Lazy;
use std::sync::Arc;
use strong_id::StrongId;

pub struct StorageServiceFactory {
    aws_s3_service: Box<dyn StorageServiceTrait>,
    channel_manager_service: Box<dyn StorageServiceTrait>,
    cdn_service: Box<dyn StorageServiceTrait>,
    aws_efs_service: Box<dyn StorageServiceTrait>,
}

impl StorageServiceFactory {
    pub fn new() -> Self {
        StorageServiceFactory {
            aws_s3_service: Box::new(AwsS3Service::new()),
            channel_manager_service: Box::new(ChannelManager::new()),
            cdn_service: Box::new(CdnService::new()),
            aws_efs_service: Box::new(AwsEfsService::new()),
        }
    }

    pub fn get_by_id(&self, adapter_id: &str) -> Result<&dyn StorageServiceTrait, ServiceError> {
        let prefix = get_id_prefix!(adapter_id).unwrap_or_default();

        match prefix.as_str() {
            AWS_S3_ADAPTER_ID_PREFIX => Ok(self.aws_s3_service.as_ref()),
            CHANNEL_MANAGER_ADAPTER_ID_PREFIX => Ok(self.channel_manager_service.as_ref()),
            CDN_ADAPTER_ID_PREFIX => Ok(self.cdn_service.as_ref()),
            AWS_EFS_ADAPTER_ID_PREFIX => Ok(self.aws_efs_service.as_ref()),
            _ => Err(ServiceError::GeneralError("Invalid adapter ID".to_string())),
        }
    }

    pub fn get_by_enum(&self, adapter_type: &AdapterType) -> &dyn StorageServiceTrait {
        match adapter_type {
            AdapterType::AwsS3 => self.aws_s3_service.as_ref(),
            AdapterType::ChannelManager => self.channel_manager_service.as_ref(),
            AdapterType::Cdn => self.cdn_service.as_ref(),
            AdapterType::AwsEfs => self.aws_efs_service.as_ref(),
        }
    }
}

pub static STORAGE_SERVICE_FACTORY: Lazy<Arc<StorageServiceFactory>> =
    Lazy::new(|| Arc::new(StorageServiceFactory::new()));

impl Default for StorageServiceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageServiceFactory {
    pub fn new_from_mocks(
        aws_s3_service_option: Option<MockStorageServiceTrait>,
        channel_manager_service_option: Option<MockStorageServiceTrait>,
        cdn_service_option: Option<MockStorageServiceTrait>,
        aws_efs_service_option: Option<MockStorageServiceTrait>,
    ) -> Self {
        StorageServiceFactory {
            aws_s3_service: match aws_s3_service_option {
                Some(aws_s3_service) => Box::new(aws_s3_service),
                None => Box::new(AwsS3Service::new()),
            },
            channel_manager_service: match channel_manager_service_option {
                Some(channel_manager_service) => Box::new(channel_manager_service),
                None => Box::new(ChannelManager::new()),
            },
            cdn_service: match cdn_service_option {
                Some(cdn_service) => Box::new(cdn_service),
                None => Box::new(CdnService::new()),
            },
            aws_efs_service: match aws_efs_service_option {
                Some(aws_efs_service) => Box::new(aws_efs_service),
                None => Box::new(AwsEfsService::new()),
            },
        }
    }
}
