use std::borrow::Cow;

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use color_quant::NeuQuant;
use image::{
    DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    imageops::FilterType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PngMode {
    /// Leave PNG data untouched (unless resized)
    None,
    /// Lossless optimization with oxipng
    Lossless,
    /// Reduce to a color palette (lossy), then optimize losslessly
    Quantize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ConvertTo {
    /// Keep original image formats
    None,
    /// Convert opaque PNGs to JPEG (works on all readers)
    Jpeg,
    /// Convert PNGs and JPEGs to WebP (EPUB 3.3 readers only)
    Webp,
}

impl ConvertTo {
    /// Returns the new file extension and media type for images of `kind`.
    pub fn target_for(self, kind: ImageKind) -> Option<(&'static str, &'static str)> {
        match (self, kind) {
            (Self::Webp, _) => Some(("webp", "image/webp")),
            (Self::Jpeg, ImageKind::Png) => Some(("jpg", "image/jpeg")),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
}

impl ImageKind {
    pub fn from_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        if extension.eq_ignore_ascii_case("png") {
            Some(Self::Png)
        } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
            Some(Self::Jpeg)
        } else {
            None
        }
    }

    fn format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageSettings {
    pub level: u8,
    pub quality: u8,
    pub recompress_jpeg: bool,
    pub max_dimension: Option<u32>,
    pub png_mode: PngMode,
    pub png_colors: u16,
    pub convert_to: ConvertTo,
}

pub struct Optimized {
    pub data: Vec<u8>,
    pub converted: bool,
}

/// Returns the smallest encoding that beats `original`, or `None` to keep it.
pub fn optimize(
    original: &[u8],
    kind: ImageKind,
    can_convert: bool,
    settings: &ImageSettings,
) -> Result<Option<Optimized>> {
    let decoded = image::load_from_memory_with_format(original, kind.format())
        .context("could not decode image")?;
    let resized = downscale(&decoded, settings.max_dimension);
    let image = resized.as_ref().unwrap_or(&decoded);
    let mut best = None;

    if can_convert {
        match settings.convert_to {
            ConvertTo::Webp => keep_smaller(
                &mut best,
                original.len(),
                encode_webp(image, settings.quality)?,
                true,
            ),
            ConvertTo::Jpeg if is_opaque(image) => keep_smaller(
                &mut best,
                original.len(),
                encode_jpeg(image, settings.quality)?,
                true,
            ),
            _ => {}
        }
    }

    if best.is_none() {
        match kind {
            ImageKind::Jpeg if settings.recompress_jpeg || resized.is_some() => keep_smaller(
                &mut best,
                original.len(),
                encode_jpeg(image, settings.quality)?,
                false,
            ),
            ImageKind::Jpeg => {}
            ImageKind::Png => {
                // Resampling flat-color art adds colors that can outweigh the pixel savings.
                let sizes = [
                    Some((image, resized.is_some())),
                    resized.is_some().then_some((&decoded, false)),
                ];
                for (candidate, is_resized) in sizes.into_iter().flatten() {
                    if let Some(data) = optimize_png(original, candidate, is_resized, settings)? {
                        keep_smaller(&mut best, original.len(), data, false);
                    }
                }
            }
        }
    }

    Ok(best)
}

fn keep_smaller(best: &mut Option<Optimized>, original_len: usize, data: Vec<u8>, converted: bool) {
    let limit = best.as_ref().map_or(original_len, |current| current.data.len());
    if data.len() < limit {
        *best = Some(Optimized { data, converted });
    }
}

fn downscale(image: &DynamicImage, max_dimension: Option<u32>) -> Option<DynamicImage> {
    let max = max_dimension?;
    (image.width() > max || image.height() > max)
        .then(|| image.resize(max, max, FilterType::Lanczos3))
}

fn is_opaque(image: &DynamicImage) -> bool {
    !image.color().has_alpha() || image.to_rgba8().pixels().all(|pixel| pixel[3] == u8::MAX)
}

fn optimize_png(
    original: &[u8],
    image: &DynamicImage,
    resized: bool,
    settings: &ImageSettings,
) -> Result<Option<Vec<u8>>> {
    let source: Cow<[u8]> = match settings.png_mode {
        PngMode::Quantize => Cow::Owned(quantize(image, settings.png_colors)?),
        _ if resized => Cow::Owned(encode_png(image)?),
        PngMode::None => return Ok(None),
        PngMode::Lossless => Cow::Borrowed(original),
    };
    if settings.png_mode == PngMode::None {
        return Ok(Some(source.into_owned()));
    }

    let mut options = oxipng::Options::from_preset(settings.level * 4 / 9);
    options.strip = oxipng::StripChunks::Safe;
    options.optimize_alpha = true;
    Ok(Some(oxipng::optimize_from_memory(&source, &options)?))
}

fn quantize(image: &DynamicImage, colors: u16) -> Result<Vec<u8>> {
    let rgba = image.to_rgba8();
    let quantizer = NeuQuant::new(10, usize::from(colors), rgba.as_raw());
    let indices: Vec<u8> = rgba
        .as_raw()
        .chunks_exact(4)
        .map(|pixel| quantizer.index_of(pixel) as u8)
        .collect();
    let alpha = quantizer.color_map_alpha();

    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, rgba.width(), rgba.height());
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(quantizer.color_map_rgb());
    if alpha.iter().any(|&value| value != u8::MAX) {
        encoder.set_trns(alpha);
    }
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&indices)?;
    writer.finish()?;
    Ok(output)
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    image.write_with_encoder(PngEncoder::new(&mut output))?;
    Ok(output)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut output, quality);
    if image.color().has_color() {
        let rgb = image.to_rgb8();
        encoder.write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)?;
    } else {
        let gray = image.to_luma8();
        encoder.write_image(gray.as_raw(), gray.width(), gray.height(), ExtendedColorType::L8)?;
    }
    Ok(output)
}

fn encode_webp(image: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let quality = f32::from(quality);
    let encoded = if is_opaque(image) {
        let rgb = image.to_rgb8();
        webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height())
            .encode_simple(false, quality)
    } else {
        let rgba = image.to_rgba8();
        webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
            .encode_simple(false, quality)
    };
    let encoded = encoded.map_err(|error| anyhow!("could not encode WebP: {error:?}"))?;
    Ok(encoded.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn noisy_image(alpha: u8) -> DynamicImage {
        let mut seed = 12345u32;
        DynamicImage::ImageRgba8(RgbaImage::from_fn(64, 64, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let [r, g, b, _] = seed.to_le_bytes();
            Rgba([r, g, b, alpha])
        }))
    }

    #[test]
    fn quantize_produces_indexed_png() -> Result<()> {
        let bytes = quantize(&noisy_image(255), 256)?;
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info()?;
        assert_eq!(reader.info().color_type, png::ColorType::Indexed);
        Ok(())
    }

    #[test]
    fn jpeg_conversion_skips_transparent_images() {
        assert!(is_opaque(&noisy_image(255)));
        assert!(!is_opaque(&noisy_image(128)));
    }

    #[test]
    fn downscale_preserves_aspect_ratio() {
        let image = DynamicImage::new_rgb8(2000, 1000);
        let resized = downscale(&image, Some(1000)).expect("image should be downscaled");
        assert_eq!((resized.width(), resized.height()), (1000, 500));
        assert!(downscale(&image, Some(4000)).is_none());
    }
}
