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
}
