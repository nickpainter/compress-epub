use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::tempdir;
use zip::{CompressionMethod, ZipArchive};

const IMAGE_EXTENSIONS: [&str; 4] = [".png", ".jpg", ".jpeg", ".webp"];
const TEXT_EXTENSIONS: [&str; 6] = [".opf", ".xhtml", ".html", ".htm", ".css", ".svg"];

#[test]
fn default_settings() {
    compress_fixtures("default", &[]);
}

#[test]
fn quantized_and_downscaled_png() {
    compress_fixtures(
        "quantize+1400px",
        &["--png-mode", "quantize", "--max-dimension", "1400"],
    );
}

#[test]
fn convert_to_jpeg() {
    compress_fixtures("jpeg", &["--convert-to", "jpeg", "--max-dimension", "1400"]);
}

#[test]
fn convert_to_webp() {
    compress_fixtures("webp", &["--convert-to", "webp", "--max-dimension", "1400"]);
}

fn compress_fixtures(label: &str, extra_args: &[&str]) {
    let output_directory = tempdir().expect("could not create temporary output directory");
    let fixtures = fixture_paths();
    assert!(!fixtures.is_empty(), "no EPUB fixtures found");

    for input in fixtures {
        let file_name = input.file_name().unwrap().to_string_lossy().into_owned();
        let output = output_directory.path().join(&file_name);
        let status = Command::new(env!("CARGO_BIN_EXE_compress-epub"))
            .args(["--input", input.to_str().unwrap()])
            .args(["--output", output.to_str().unwrap()])
            .args(extra_args)
            .status()
            .expect("could not run compress-epub");
        assert!(status.success(), "[{label}] CLI failed for {file_name}");

        let original_size = fs::metadata(&input).unwrap().len();
        let compressed_size = fs::metadata(&output).unwrap().len();
        assert!(
            compressed_size <= original_size,
            "[{label}] {file_name} got larger: {original_size} -> {compressed_size} bytes"
        );

        let (names, broken) = inspect(&output);
        let (_, originally_broken) = inspect(&input);
        assert!(
            broken.len() <= originally_broken.len(),
            "[{label}] {file_name} has new broken image references: {broken:?}"
        );
        if !extra_args.contains(&"webp") {
            assert!(
                !names.iter().any(|name| name.to_ascii_lowercase().ends_with(".webp")),
                "[{label}] {file_name} contains WebP images without --convert-to webp"
            );
        }

        println!(
            "[{label}] {file_name}: {original_size} -> {compressed_size} bytes ({:.1}% smaller)",
            (original_size - compressed_size) as f64 / original_size as f64 * 100.0
        );
    }
}

fn fixture_paths() -> Vec<PathBuf> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("epubs");
    fs::read_dir(fixtures)
        .expect("could not read epubs directory")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("epub"))
        .collect()
}

/// Verifies EPUB packaging and CRCs; returns entry names and unresolvable image references.
fn inspect(path: &Path) -> (HashSet<String>, Vec<String>) {
    let mut archive = ZipArchive::new(File::open(path).unwrap()).expect("not a valid ZIP archive");
    let first = archive.by_index(0).expect("archive should not be empty");
    assert_eq!(first.name(), "mimetype");
    assert_eq!(first.compression(), CompressionMethod::Stored);
    drop(first);

    let names: HashSet<String> = archive.file_names().map(str::to_owned).collect();
    let mut broken = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .unwrap_or_else(|error| panic!("CRC/read failure in {name}: {error}"));

        let lower = name.to_ascii_lowercase();
        if !TEXT_EXTENSIONS.iter().any(|extension| lower.ends_with(extension)) {
            continue;
        }
        let directory = name.rsplit_once('/').map_or("", |(directory, _)| directory);
        let text = String::from_utf8_lossy(&contents);
        for reference in text.split(['"', '\'', '(', ')']) {
            let lower = reference.to_ascii_lowercase();
            let is_image = IMAGE_EXTENSIONS.iter().any(|extension| lower.ends_with(extension));
            if !is_image || reference.contains([':', '<', '>', '\n']) {
                continue;
            }
            let resolved = resolve(directory, &reference.replace("%20", " "));
            if !names.contains(&resolved) {
                broken.push(format!("{name} -> {reference}"));
            }
        }
    }
    (names, broken)
}

fn resolve(directory: &str, reference: &str) -> String {
    let mut parts: Vec<&str> = directory.split('/').filter(|part| !part.is_empty()).collect();
    for part in reference.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

