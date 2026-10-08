//! Terminal image previews.
//!
//! Decoding and protocol encoding both happen off the render thread, in the
//! same background task that loads every other preview kind. The rendered
//! [`Protocol`] is then a plain value the draw path can blit, so no image work
//! ever runs inside `terminal.draw`.

use image::ImageDecoder;
use std::io::{Cursor, Read};
use std::path::Path;

use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;

use super::content::PreviewError;

/// Maximum image file size to attempt to decode (32 MB).
///
/// Decoding is bounded separately from [`super::content`]'s generic preview cap
/// because an image's decoded footprint is far larger than its file size.
const MAX_IMAGE_FILE_SIZE: u64 = 32 * 1024 * 1024;

/// Maximum decoded pixel count (~40 megapixels).
///
/// Guards against decompression-bomb images: a small file can declare enormous
/// dimensions, and allocating that buffer would exhaust memory.
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// File extensions rendered as images in [`super::PreviewMode::Auto`].
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "tiff", "tif", "webp", "avif", "qoi", "tga", "ff",
    "pnm", "pbm", "pgm", "ppm",
];

/// Whether a path looks like an image this build can render.
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| IMAGE_EXTENSIONS.contains(&extension.as_str()))
}

/// A rendered image protocol.
///
/// Wraps [`Protocol`] purely so [`super::PreviewContent`] can keep deriving
/// `Debug`: the protocol holds encoded terminal payloads that are neither
/// meaningful nor small when formatted.
#[derive(Clone)]
pub struct ImageProtocol(pub Protocol);

impl std::fmt::Debug for ImageProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImageProtocol(..)")
    }
}

/// Decode `path` and encode it for `picker`'s terminal protocol, fitted to
/// `area` character cells.
pub fn load(
    path: &Path,
    picker: &Picker,
    area: Size,
) -> Result<(ImageProtocol, u32, u32, String), PreviewError> {
    let file = super::content::PreviewLoader::open_preview_file(path)?;
    let metadata = file
        .metadata()
        .map_err(|e| PreviewError::IoError(e.to_string()))?;
    if metadata.len() > MAX_IMAGE_FILE_SIZE {
        return Err(PreviewError::TooLarge(metadata.len()));
    }

    let bytes = read_image_bytes(file, MAX_IMAGE_FILE_SIZE)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes));
    if let Ok(format) = image::ImageFormat::from_path(path) {
        reader.set_format(format);
    }
    let reader = reader
        .with_guessed_format()
        .map_err(|e| PreviewError::IoError(e.to_string()))?;

    let format = reader
        .format()
        .map(|format| format!("{format:?}").to_uppercase())
        .unwrap_or_else(|| "IMAGE".to_string());

    // Inspect and decode the same bounded input with the same decoder. A path
    // replacement cannot change the bytes after the dimensions have been checked.
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| PreviewError::IoError(format!("Could not inspect image: {e}")))?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_IMAGE_PIXELS {
        return Err(PreviewError::TooLarge(pixels));
    }
    // Preserve ImageReader::decode's output-buffer accounting when using the
    // decoder directly. Decoder-specific temporary allocation limits are best effort.
    let mut limits = image::Limits::default();
    limits
        .reserve(decoder.total_bytes())
        .and_then(|()| decoder.set_limits(limits))
        .map_err(|e| PreviewError::IoError(format!("Image allocation limit: {e}")))?;
    let decoded = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| PreviewError::IoError(format!("Could not decode image: {e}")))?;

    // An empty area would ask the encoder for a zero-sized image.
    let area = Size::new(area.width.max(1), area.height.max(1));

    let protocol = picker
        .new_protocol(decoded, area, Resize::Fit(None))
        .map_err(|e| PreviewError::IoError(format!("Could not render image: {e}")))?;

    Ok((ImageProtocol(protocol), width, height, format))
}

fn read_image_bytes(reader: impl Read, limit: u64) -> Result<Vec<u8>, PreviewError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| PreviewError::IoError(e.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(PreviewError::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn image_extensions_are_matched_case_insensitively() {
        assert!(is_image_path(&PathBuf::from("photo.PNG")));
        assert!(is_image_path(&PathBuf::from("photo.jpeg")));
        assert!(is_image_path(&PathBuf::from("/tmp/a.WebP")));
    }

    #[test]
    fn non_image_extensions_are_rejected() {
        assert!(!is_image_path(&PathBuf::from("notes.txt")));
        assert!(!is_image_path(&PathBuf::from("archive.tar.gz")));
        assert!(!is_image_path(&PathBuf::from("no_extension")));
    }

    #[test]
    fn a_small_image_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bomb.png");

        let mut png = image::RgbaImage::new(1, 1);
        png.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        png.save(&path).unwrap();

        // Sanity: a normal small image is accepted by the size guard.
        let picker = Picker::halfblocks();
        let result = load(&path, &picker, Size::new(10, 10));
        assert!(result.is_ok(), "small image should load: {result:?}");
    }

    #[test]
    fn declared_pixel_bomb_is_rejected_without_pixel_payload() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bomb.ppm");
        std::fs::write(&path, b"P6\n60000 60000\n255\n").unwrap();
        let result = load(&path, &Picker::halfblocks(), Size::new(10, 10));
        assert!(matches!(result, Err(PreviewError::TooLarge(3_600_000_000))));
    }

    #[test]
    fn encoded_input_limit_is_enforced_during_reading() {
        assert_eq!(read_image_bytes(Cursor::new(b"1234"), 4).unwrap(), b"1234");
        assert!(matches!(
            read_image_bytes(Cursor::new(b"12345"), 4),
            Err(PreviewError::TooLarge(5))
        ));
    }

    #[test]
    fn directories_are_rejected_before_image_inspection() {
        let temp = tempfile::tempdir().unwrap();
        assert!(load(temp.path(), &Picker::halfblocks(), Size::new(10, 10)).is_err());
    }

    #[test]
    fn a_decoded_image_renders_visible_cells() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui_image::Image;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("swatch.png");

        // A solid red block, large enough to cover several character cells.
        let mut buffer = image::RgbaImage::new(64, 64);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([220, 30, 30, 255]);
        }
        buffer.save(&path).unwrap();

        // Halfblocks needs no terminal support, so this exercises decode ->
        // encode -> render without depending on the host terminal.
        let picker = Picker::halfblocks();
        let (protocol, width, height, format) =
            load(&path, &picker, Size::new(16, 8)).expect("image should load");
        assert_eq!((width, height), (64, 64));
        assert_eq!(format, "PNG");

        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(Image::new(&protocol.0), frame.area()))
            .unwrap();

        // The encoded image must actually paint cells, not leave the buffer blank.
        let painted = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.bg != ratatui::style::Color::Reset)
            .count();
        assert!(painted > 0, "rendering the image painted nothing");
    }

    #[test]
    fn a_missing_file_reports_an_io_error() {
        let picker = Picker::halfblocks();
        let result = load(
            &PathBuf::from("/nonexistent/image.png"),
            &picker,
            Size::new(10, 10),
        );
        assert!(matches!(result, Err(PreviewError::IoError(_))));
    }
}
