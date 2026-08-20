use gen_server::models::{ImageFormat, ResizeQueryParams};
use o2o::o2o;

#[derive(o2o, Clone, PartialEq, Debug)]
#[from_owned(ResizeQueryParams)]
pub struct ResizeQuery {
    pub url: String,

    #[from(~.map(|x| x as u32))]
    pub width: Option<u32>,

    #[from(~.map(|x| x as u32))]
    pub height: Option<u32>,

    #[from(~.unwrap_or_else(|| ImageFormat::Jpg))]
    pub format: ImageFormat,

    pub blur_sigma: Option<f32>,

    pub grayscale: Option<bool>,

    /// Opt-in permission to upscale past the source image's resolution
    /// (imgproxy's `enlarge` processing option). Defaults to `false` when
    /// converted from the generated `ResizeQueryParams` - which has no
    /// `enlarge` field yet, hence the `#[ghost]` default below - so that
    /// upscaling stays refused unless a caller explicitly opts in once the
    /// API surface grows the corresponding query parameter (#36). See
    /// `ImageService::process_image_blocking_with_limits`
    /// (`src/services/image/handler.rs`) for the guard this drives.
    #[ghost(false)]
    pub enlarge: bool,
}
