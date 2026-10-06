# compress-epub

A Rust command-line tool that makes EPUB files smaller. It:

- rewrites the archive with DEFLATE compression
- recompresses JPEGs and optimizes or palette-reduces PNGs
- can downscale large images
- can convert images to JPEG or WebP, updating every reference inside the book

The EPUB `mimetype` entry is kept first and uncompressed, as the EPUB specification requires.

## Build

```powershell
cargo build --release
```

## Usage

```powershell
compress-epub --input book.epub --output book-compressed.epub [OPTIONS]
```

When it finishes, the tool prints the original and new sizes and how many images were optimized and converted. Run `compress-epub --help` to see every option.

### Options

| Option | Values | Default | Description |
|---|---|---|---|
| `-i, --input <FILE>` | path | required | EPUB to compress. Never modified. |
| `-o, --output <FILE>` | path | required | Where to write the result. Must be different from the input and must not already exist. |
| `-l, --level <N>` | `0`–`9` | `6` | Overall compression effort. See [Compression level](#compression-level). |
| `-q, --quality <N>` | `1`–`100` | based on `--level` | Quality for lossy JPEG and WebP encoding. Lower means smaller files with more visible artifacts. |
| `--max-dimension <PX>` | pixels | off | Downscale any image whose width or height is larger than this, keeping its aspect ratio. |
| `--png-mode <MODE>` | `none`, `lossless`, `quantize` | `lossless` | How to compress images that stay PNG. See [PNG modes](#png-modes). |
| `--png-colors <N>` | `64`–`256` | `256` | Palette size for `--png-mode quantize`. Fewer colors give smaller files but more banding. |
| `--convert-to <FORMAT>` | `none`, `jpeg`, `webp` | `none` | Convert images to another format. See [Format conversion](#format-conversion). |

### Compression level

`--level` controls three things at once:

| Level | ZIP (DEFLATE) level | PNG optimization effort | Default `--quality` |
|---|---|---|---|
| `0` | 0 (fastest) | lowest | 100; JPEGs are not recompressed |
| `3` | 3 | 1 of 4 | 82 |
| `6` | 6 | 2 of 4 | 64 |
| `9` | 9 (smallest) | 4 of 4 (slowest) | 46 |

The default quality is `100 - 6 × level`. Pass `--quality` to set it directly; this also turns on JPEG recompression at level `0`.

### PNG modes

- `none`: leave PNG data untouched. A PNG is only re-encoded if `--max-dimension` resizes it.
- `lossless` (default): optimize with [oxipng](https://github.com/shssoichiro/oxipng). This removes metadata and unused alpha channels and picks the best PNG filters. Pixels are unchanged. Savings are usually modest.
- `quantize`: reduce the image to at most `--png-colors` colors, then optimize losslessly. This is lossy, but often cuts illustrations and diagrams by 60–80%. Photographs may show banding.

### Format conversion

Converted images get a new file extension. The tool updates their entries and media types in the package (`.opf`) file and every reference in XHTML and CSS files.

- `none` (default): keep every image in its original format. This gives the widest reader compatibility.
- `jpeg`: convert PNGs to JPEG at `--quality`. PNGs that use transparency stay PNG, because JPEG can't store it. JPEG works on every EPUB reader.
- `webp`: convert both PNGs and JPEGs to WebP at `--quality`, keeping transparency. WebP gives the smallest files, but it is only part of the EPUB 3.3 standard. Many readers, including older Kindles and many e-ink devices, can't display it.

**For backwards compatibility, don't use `--convert-to webp`.** Without it, the output never contains WebP images.

### Safety guarantees

- Each image is replaced only if the new version is smaller. Otherwise the original bytes are kept.
- If the tool can't decode an image, it prints a warning and keeps the original image.
- A conversion is skipped if the new file name would clash with an existing file in the book.
- If the rewritten book would be larger than the original, the original is copied to the output unchanged.
- Images are processed in parallel on all CPU cores.

### Examples

``` powershell
# Typical performance-oriented compression for modern EPUB 3.3 readers
compress-epub -i book.epub -o book-small.epub --convert-to webp --quality 75

```powershell
# Safe defaults: ZIP + JPEG recompression + lossless PNG optimization
compress-epub -i book.epub -o book-small.epub

# Maximum effort while staying lossless for PNGs
compress-epub -i book.epub -o book-small.epub --level 9

# Illustrated book for e-readers: palette PNGs, cap images at 1400 px
compress-epub -i book.epub -o book-small.epub --png-mode quantize --max-dimension 1400

# Photo-heavy book, compatible with all readers: convert opaque PNGs to JPEG
compress-epub -i book.epub -o book-small.epub --convert-to jpeg --quality 80 --max-dimension 1400

# Smallest output for modern EPUB 3.3 readers only - max-dimension flag may affect the layout of the resulting file
compress-epub -i book.epub -o book-small.epub --convert-to webp --quality 75 --max-dimension 1400
```

## Testing

Run the full test suite:

```powershell
cargo test
```

The suite has two parts:

- **Unit tests** (in `src/`): EPUB packaging, image quantization, JPEG conversion, downscaling, and rewriting of references after images are renamed. They are fast and need no input files.
- **Fixture tests** ([tests/fixture_epubs.rs](tests/fixture_epubs.rs)): run the compiled CLI on every `.epub` file in the `epubs/` folder with four configurations (default, `quantize+1400px`, `jpeg`, `webp`). For each output they check that:
  - the `mimetype` entry is first and uncompressed
  - every entry passes its CRC check
  - the output is no larger than the input
  - no image references are broken
  - no WebP images appear unless `--convert-to webp` was used

  Outputs go to a temporary directory, and the originals are never modified. The tests fail if `epubs/` has no `.epub` files.

Run only one part:

```powershell
cargo test --bins                    # unit tests only
cargo test --test fixture_epubs      # fixture tests only
cargo test --test fixture_epubs convert_to_webp   # a single configuration
```

To see each book's size before and after, add `--nocapture`. Image processing is much faster in an optimized build, so use `--release` for the fixture tests on large books:

```powershell
cargo test --release --test fixture_epubs -- --nocapture
```

Each line of output is prefixed with its configuration, in the form `[jpeg] book.epub: <original> -> <compressed> bytes (<n>% smaller)`. To keep the lines from different configurations from interleaving, add `--test-threads=1` after `--`.