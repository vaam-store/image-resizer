use anyhow::{Context, Result};
use derive_builder::Builder;
use image::ImageFormat;
use std::io::Cursor;
use tracing::debug;

#[derive(Clone, Builder)]
pub struct ImageService {}

impl ImageService {
    /// Download an image from a URL
    pub async fn download_image(&self, url: &str) -> Result<Vec<u8>> {
        let response = reqwest::get(url)
            .await
            .context("Failed to initiate image download")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to download image from {}: status {}",
                url,
                response.status()
            ));
        }

        let image_bytes = response
            .bytes()
            .await
            .context("Failed to read image bytes")?;

        Ok(image_bytes.to_vec())
    }

    /// Process an image (decode, resize, encode)
    pub fn process_image(
        &self,
        image_bytes: &[u8],
        width: u32,
        height: u32,
        format: &gen_server::models::ImageFormat,
    ) -> Result<(Vec<u8>, String)> {
        // Decode image
        let img = image::load_from_memory(image_bytes).context("Failed to decode image")?;

        debug!(
            "Image decoded. Original dimensions: {}x{}",
            img.width(),
            img.height()
        );

        // Resize image
        let resized_img = img.thumbnail(width, height);
        debug!(
            "Image resized to: {}x{}",
            resized_img.width(),
            resized_img.height()
        );

        // Determine output format and content type
        let (output_format, content_type) = match format {
            gen_server::models::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            gen_server::models::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            gen_server::models::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
            // This pattern is unreachable since we've covered all variants,
            // but we'll keep it with an annotation for future-proofing
            #[allow(unreachable_patterns)]
            _ => return Err(anyhow::anyhow!("Unsupported image format: {}", format)),
        };

        // Encode image
        let mut output_bytes = Cursor::new(Vec::new());
        resized_img
            .write_to(&mut output_bytes, output_format)
            .context(format!("Failed to encode image to {:?}", output_format))?;

        Ok((output_bytes.into_inner(), content_type.to_string()))
    }
}
