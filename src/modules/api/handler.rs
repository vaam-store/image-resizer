use crate::services::resize::handler::{ResizeService, ResizeServiceBuilder};
use anyhow::Result;
use derive_builder::Builder;
use gen_server::apis::ErrorHandler;


#[derive(Clone, Builder)]
pub struct ApiService {
    pub resize_service: ResizeService,
}

impl ApiService {
    pub fn create() -> Result<Self> {
        let resize_service = ResizeServiceBuilder::default().build()?;

        let api_service = ApiServiceBuilder::default()
            .resize_service(resize_service)
            .build()?;

        Ok(api_service)
    }
}

impl ErrorHandler<()> for ApiService {}

impl AsRef<ApiService> for ApiService {
    fn as_ref(&self) -> &ApiService {
        self
    }
}
