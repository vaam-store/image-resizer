use anyhow::Result;
use derive_builder::Builder;
use gen_server::models::ResizeAnImageQueryParams;

#[derive(Clone, Builder)]
pub struct ResizeService {}

impl ResizeService {
    pub async fn resize(
        &self,
        ResizeAnImageQueryParams { url, .. }: &ResizeAnImageQueryParams,
    ) -> Result<String> {
        todo!()
    }
}
