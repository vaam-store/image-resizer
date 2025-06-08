use crate::models::params::ResizeQuery;
use anyhow::{Context, Result};
use derive_builder::Builder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use std::io::Cursor;

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
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        // Decode image
        let img = image::load_from_memory(image_bytes).context("Failed to decode image")?;

        // Resize image
        let img = match (params.width, params.height) {
            (Some(w), None) => {
                // Only width specified: resize to width, maintain aspect ratio
                // image.resize uses nwidth and nheight as maximums.
                // Passing u32::MAX for height means no height constraint beyond aspect ratio.
                img.resize(w, u32::MAX, FilterType::Lanczos3)
            }
            (None, Some(h)) => {
                // Only height specified: resize to height, maintain aspect ratio
                img.resize(u32::MAX, h, FilterType::Lanczos3)
            }
            (Some(w), Some(h)) => {
                // Both width and height specified:
                // 1. Resize to fill the dimensions while maintaining aspect ratio (cover).
                //    This makes the image as small as possible while still covering w x h.
                let img = img.resize_to_fill(w, h, FilterType::Lanczos3);

                // 2. Crop the resized image from the center to the exact w x h.
                let (current_width, current_height) = img.dimensions();
                let crop_x = if current_width > w {
                    (current_width - w) / 2
                } else {
                    0
                };
                let crop_y = if current_height > h {
                    (current_height - h) / 2
                } else {
                    0
                };

                // Ensure crop dimensions are not larger than the image itself
                let crop_width = w.min(current_width - crop_x);
                let crop_height = h.min(current_height - crop_y);

                img.crop_imm(crop_x, crop_y, crop_width, crop_height)
            }
            (None, None) => {
                // No width or height specified, do nothing regarding resize/crop.
                img
            }
        };

        // GrayScale
        let img = if let Some(true) = params.grayscale {
            img.grayscale()
        } else {
            img
        };

        let img = if let Some(sigma) = params.blur_sigma {
            img.blur(sigma)
        } else {
            img
        };

        // Determine output format and content type
        let (output_format, content_type) = match params.format {
            gen_server::models::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            gen_server::models::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            gen_server::models::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
        };

        // Encode image
        let mut output_bytes = Cursor::new(Vec::new());
        img.write_to(&mut output_bytes, output_format)
            .context(format!("Failed to encode image to {:?}", output_format))?;

        Ok((output_bytes.into_inner(), content_type.to_string()))
    }
}
