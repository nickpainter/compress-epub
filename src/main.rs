mod images;
mod references;

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rayon::prelude::*;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use images::{ConvertTo, ImageKind, ImageSettings, PngMode};
use references::Rename;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// EPUB file to compress
    #[arg(short, long, value_name = "FILE")]
    input: PathBuf,

    /// Destination EPUB file
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,

    /// Compression level from 0 (fastest/largest) to 9 (slowest/smallest)
    #[arg(short, long, default_value_t = 6, value_parser = clap::value_parser!(u8).range(0..=9))]
    level: u8,

    /// Quality for lossy image encoding, 1-100 [default: derived from --level]
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: Option<u8>,

    /// Downscale images whose width or height exceeds this many pixels
    #[arg(long, value_name = "PX", value_parser = clap::value_parser!(u32).range(1..))]
    max_dimension: Option<u32>,

    /// How to compress images that remain PNG
    #[arg(long, value_enum, default_value_t = PngMode::Lossless)]
    png_mode: PngMode,

    /// Palette size for --png-mode quantize
    #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u16).range(64..=256))]
    png_colors: u16,

    /// Convert images to another format and update EPUB references.
    /// Use `none` or `jpeg` for readers without WebP support
    #[arg(long, value_enum, default_value_t = ConvertTo::None)]
    convert_to: ConvertTo,
}

impl Cli {
    fn image_settings(&self) -> ImageSettings {
        ImageSettings {
            level: self.level,
            quality: self.quality.unwrap_or_else(|| jpeg_quality(self.level)),
            recompress_jpeg: self.level > 0 || self.quality.is_some(),
            max_dimension: self.max_dimension,
            png_mode: self.png_mode,
            png_colors: self.png_colors,
            convert_to: self.convert_to,
        }
    }
}

struct Report {
    original_size: u64,
    compressed_size: u64,
    images_optimized: usize,
    images_converted: usize,
}

struct ImageResult {
    index: usize,
    name: String,
    target: Option<(String, &'static str)>,
    optimized: Option<images::Optimized>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let report = compress_epub(&cli.input, &cli.output, &cli.image_settings())?;
    let saved = report.original_size - report.compressed_size;
    println!(
        "{}: {} -> {} bytes ({:.1}% smaller); {} images optimized, {} converted",
        cli.output.display(),
        report.original_size,
        report.compressed_size,
        saved as f64 / report.original_size as f64 * 100.0,
        report.images_optimized,
        report.images_converted
    );
    Ok(())
}

fn compress_epub(input: &Path, output: &Path, settings: &ImageSettings) -> Result<Report> {
    if input == output {
        bail!("input and output must be different files");
    }
    if output.exists() {
        bail!("output file already exists: {}", output.display());
    }

    let result = compress_epub_inner(input, output, settings);
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}

fn compress_epub_inner(input: &Path, output: &Path, settings: &ImageSettings) -> Result<Report> {
    let input_file = File::open(input)
        .with_context(|| format!("could not open input file: {}", input.display()))?;
    let mut archive =
        ZipArchive::new(input_file).context("input is not a valid EPUB/ZIP archive")?;

    let mimetype = {
        let mut entry = archive
            .by_name("mimetype")
            .context("EPUB is missing its required mimetype entry")?;
        let mut value = Vec::new();
        entry.read_to_end(&mut value)?;
        if value != b"application/epub+zip" {
            bail!("archive does not contain a valid EPUB mimetype");
        }
        value
    };

    let (mut replacements, renames) = optimize_images(&mut archive, settings)?;
    let images_optimized = replacements.len();

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create output directory: {}", parent.display()))?;
    }

    let output_file = File::create(output)
        .with_context(|| format!("could not create output file: {}", output.display()))?;
    let mut writer = ZipWriter::new(output_file);

    writer.start_file("mimetype", file_options(CompressionMethod::Stored, None))?;
    writer.write_all(&mimetype)?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        if name == "mimetype" {
            continue;
        }

        let permissions = entry.unix_mode();
        if entry.is_dir() {
            writer.add_directory(name, file_options(CompressionMethod::Stored, permissions))?;
            continue;
        }

        let options = file_options(CompressionMethod::Deflated, permissions)
            .compression_level(Some(i64::from(settings.level)));

        if let Some((new_name, data)) = replacements.remove(&index) {
            writer.start_file(new_name, options)?;
            writer.write_all(&data)?;
        } else if !renames.is_empty() && references::is_text_document(&name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            writer.start_file(&name, options)?;
            match String::from_utf8(bytes) {
                Ok(text) => {
                    writer.write_all(references::rewrite(&name, &text, &renames).as_bytes())?
                }
                Err(error) => writer.write_all(error.as_bytes())?,
            }
        } else {
            writer.start_file(&name, options)?;
            io::copy(&mut entry, &mut writer)?;
        }
    }

    let output_file = writer.finish()?;
    output_file.sync_all()?;
    drop(output_file);

    let original_size = fs::metadata(input)?.len();
    let compressed_size = fs::metadata(output)?.len();
    if compressed_size > original_size {
        fs::copy(input, output).context("could not preserve the smaller original EPUB")?;
        return Ok(Report {
            original_size,
            compressed_size: original_size,
            images_optimized: 0,
            images_converted: 0,
        });
    }

    Ok(Report {
        original_size,
        compressed_size,
        images_optimized,
        images_converted: renames.len(),
    })
}

type Replacements = HashMap<usize, (String, Vec<u8>)>;

fn optimize_images(
    archive: &mut ZipArchive<File>,
    settings: &ImageSettings,
) -> Result<(Replacements, Vec<Rename>)> {
    let mut claimed: HashSet<String> = archive.file_names().map(str::to_owned).collect();
    let mut jobs = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(kind) = ImageKind::from_name(entry.name()).filter(|_| !entry.is_dir()) else {
            continue;
        };
        let name = entry.name().to_owned();
        // Skip conversion when the renamed file would collide with an existing entry.
        let target = settings
            .convert_to
            .target_for(kind)
            .and_then(|(extension, media_type)| {
                let renamed = replace_extension(&name, extension);
                claimed
                    .insert(renamed.clone())
                    .then_some((renamed, media_type))
            });
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        jobs.push((index, name, kind, target, data));
    }

    let results: Vec<ImageResult> = jobs
        .into_par_iter()
        .map(|(index, name, kind, target, data)| {
            let optimized = images::optimize(&data, kind, target.is_some(), settings)
                .unwrap_or_else(|error| {
                    eprintln!("warning: keeping {name} unchanged: {error:#}");
                    None
                });
            ImageResult {
                index,
                name,
                target,
                optimized,
            }
        })
        .collect();

    let mut replacements = HashMap::new();
    let mut renames = Vec::new();
    for result in results {
        let Some(optimized) = result.optimized else {
            continue;
        };
        let name = match result.target {
            Some((to, media_type)) if optimized.converted => {
                renames.push(Rename {
                    from: result.name,
                    to: to.clone(),
                    media_type,
                });
                to
            }
            _ => result.name,
        };
        replacements.insert(result.index, (name, optimized.data));
    }
    Ok((replacements, renames))
}

fn replace_extension(name: &str, extension: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    format!("{stem}.{extension}")
}

fn file_options(method: CompressionMethod, permissions: Option<u32>) -> SimpleFileOptions {
    let options = SimpleFileOptions::default().compression_method(method);
    permissions.map_or(options, |mode| options.unix_permissions(mode))
}

fn jpeg_quality(level: u8) -> u8 {
    100 - level * 6
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbImage};
    use tempfile::tempdir;

    fn settings(convert_to: ConvertTo) -> ImageSettings {
        ImageSettings {
            level: 6,
            quality: jpeg_quality(6),
            recompress_jpeg: true,
            max_dimension: None,
            png_mode: PngMode::Lossless,
            png_colors: 256,
            convert_to,
        }
    }

    fn write_epub(path: &Path, files: &[(&str, &[u8])]) -> Result<()> {
        let mut writer = ZipWriter::new(File::create(path)?);
        writer.start_file("mimetype", file_options(CompressionMethod::Stored, None))?;
        writer.write_all(b"application/epub+zip")?;
        for (name, data) in files {
            writer.start_file(*name, SimpleFileOptions::default())?;
            writer.write_all(data)?;
        }
        writer.finish()?;
        Ok(())
    }

    fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
        let mut text = String::new();
        archive.by_name(name)?.read_to_string(&mut text)?;
        Ok(text)
    }

    fn noisy_png() -> Result<Vec<u8>> {
        let mut seed = 7u32;
        let image = RgbImage::from_fn(128, 128, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let [r, g, b, _] = seed.to_le_bytes();
            image::Rgb([r, g, b])
        });
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut io::Cursor::new(&mut bytes), ImageFormat::Png)?;
        Ok(bytes)
    }

    #[test]
    fn output_keeps_epub_mimetype_first_and_uncompressed() -> Result<()> {
        let directory = tempdir()?;
        let input = directory.path().join("input.epub");
        let output = directory.path().join("output.epub");
        write_epub(&input, &[("chapter.xhtml", b"<p>Hello, EPUB!</p>")])?;

        compress_epub(&input, &output, &settings(ConvertTo::None))?;

        let mut archive = ZipArchive::new(File::open(output)?)?;
        let first = archive.by_index(0)?;
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), CompressionMethod::Stored);
        Ok(())
    }

    #[test]
    fn converting_to_jpeg_renames_image_and_rewrites_references() -> Result<()> {
        let directory = tempdir()?;
        let input = directory.path().join("input.epub");
        let output = directory.path().join("output.epub");
        let png = noisy_png()?;
        write_epub(
            &input,
            &[
                (
                    "OEBPS/content.opf",
                    br#"<item id="a" href="image/a.png" media-type="image/png"/>"#,
                ),
                ("OEBPS/text/c.xhtml", br#"<img src="../image/a.png"/>"#),
                ("OEBPS/image/a.png", &png),
            ],
        )?;

        let report = compress_epub(&input, &output, &settings(ConvertTo::Jpeg))?;
        assert_eq!(report.images_converted, 1);

        let mut archive = ZipArchive::new(File::open(output)?)?;
        assert!(archive.by_name("OEBPS/image/a.png").is_err());
        assert!(archive.by_name("OEBPS/image/a.jpg").is_ok());
        assert_eq!(
            read_entry(&mut archive, "OEBPS/content.opf")?,
            r#"<item id="a" href="image/a.jpg" media-type="image/jpeg"/>"#
        );
        assert_eq!(
            read_entry(&mut archive, "OEBPS/text/c.xhtml")?,
            r#"<img src="../image/a.jpg"/>"#
        );
        Ok(())
    }

    #[test]
    fn quality_decreases_as_compression_increases() {
        assert!(jpeg_quality(1) > jpeg_quality(9));
    }
}
