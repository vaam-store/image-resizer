use anyhow::Result;
use derive_builder::Builder;

#[derive(Clone, Builder)]
pub struct StorageService {}

impl StorageService {
    pub async fn find_activities(&self, _q: Option<String>) -> Result<String> {
        todo!()
    }
}
