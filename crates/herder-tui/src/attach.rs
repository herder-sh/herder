//! Images for a prompt, made ready to send, and images of the transcript, kept or shown.
//!
//! An image comes from a file (`@path`, a dropped path) or from the clipboard (Ctrl-V). It
//! goes out as one of [`IMAGE_MEDIA_TYPES`], at most [`MAX_IMAGE_BYTES`]; one larger than
//! [`LONG_EDGE`] on its long edge, or in bytes, is scaled down first. No provider looks at
//! more pixels than that, so the upload is smaller and nothing is lost.
//!
//! Everything here does I/O; the event loop runs it on its own threads, never the reducer.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use herder_protocol::{Bytes, IMAGE_MEDIA_TYPES, Image, MAX_IMAGE_BYTES};
use image::{DynamicImage, ImageFormat, ImageReader};

/// The longest edge, in pixels, an image is sent at.
pub const LONG_EDGE: u32 = 2048;

/// Bytes past which a file is not read at all: no screenshot or photo comes close.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The file extensions an `@path` to an image ends in.
const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// Where an image to attach comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The system clipboard.
    Clipboard,
    /// A file, as typed: relative to the working directory, `~` for home.
    File(String),
}

/// An entry of a folder the `@` popup completes in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// Whether `path` names an image by its extension.
pub fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// `bytes` as people read a size: `340 KB`, `1.2 MB`.
pub fn size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    match bytes {
        bytes if bytes >= MB => {
            let mb = format!("{:.1}", bytes as f64 / MB as f64);
            format!("{} MB", mb.trim_end_matches(".0"))
        }
        bytes if bytes >= 1024 => format!("{} KB", bytes / 1024),
        bytes => format!("{bytes} B"),
    }
}

/// The file extension of `media_type`.
pub fn extension(media_type: &str) -> &str {
    match media_type {
        "image/jpeg" => "jpg",
        other => other.strip_prefix("image/").unwrap_or("bin"),
    }
}

/// The paths a paste names, as a file manager drops them: one or more, quoted or with
/// escaped spaces, or `file://` URLs. `None` unless every one is an image's.
pub fn dropped_paths(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut paths = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut chars = text.chars();
    let ended = |word: &mut String, paths: &mut Vec<String>| {
        if !word.is_empty() {
            paths.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', None) => word.extend(chars.next()),
            ('\'' | '"', None) => quote = Some(c),
            (c, Some(open)) if c == open => quote = None,
            (c, None) if c.is_whitespace() => ended(&mut word, &mut paths),
            (c, _) => word.push(c),
        }
    }
    ended(&mut word, &mut paths);
    let paths: Vec<String> = paths
        .into_iter()
        .map(|path| match path.strip_prefix("file://") {
            Some(path) => unescape_url(path),
            None => path,
        })
        .collect();
    let file = |path: &String| {
        (path.starts_with('/') || path.starts_with("~/") || path.starts_with("./"))
            && is_image_path(path)
    };
    (!paths.is_empty() && paths.iter().all(file)).then_some(paths)
}

/// A `file://` URL's path with its `%xx` escapes decoded.
fn unescape_url(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `path` with a leading `~` for the home directory.
fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// Reads the image `source` names and makes it ready to send.
pub fn load(source: &Source) -> Result<Image, String> {
    match source {
        Source::Clipboard => from_clipboard(),
        Source::File(path) => from_file(path),
    }
}

/// The image file `path`, ready to send.
fn from_file(path: &str) -> Result<Image, String> {
    let full = expand(path);
    // Errors name the file, not its folders: they show in one row.
    let path = full.file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let path = path.as_str();
    let meta = std::fs::metadata(&full).map_err(|err| format!("{path}: {}", reason(&err)))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{path}: {}, too large to attach", size(meta.len())));
    }
    let bytes = std::fs::read(&full).map_err(|err| format!("{path}: {}", reason(&err)))?;
    prepare(bytes).map_err(|err| format!("{path}: {err}"))
}

/// An I/O error as the end of a sentence: `no such file`.
fn reason(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "no such file".to_owned(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => err.to_string(),
    }
}

/// The clipboard's image, as a PNG ready to send.
fn from_clipboard() -> Result<Image, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|_| "no clipboard here; attach a file with @path".to_owned())?;
    let image = clipboard.get_image().map_err(|err| match err {
        arboard::Error::ContentNotAvailable => "no image on the clipboard".to_owned(),
        err => format!("clipboard: {err}"),
    })?;
    let width = u32::try_from(image.width).map_err(|_| "the clipboard's image is too wide")?;
    let height = u32::try_from(image.height).map_err(|_| "the clipboard's image is too tall")?;
    let pixels = image::RgbaImage::from_raw(width, height, image.bytes.into_owned())
        .ok_or("the clipboard's image is malformed")?;
    let image = scaled(DynamicImage::ImageRgba8(pixels));
    let data = encode(&image, ImageFormat::Png)?;
    checked(Image {
        media_type: "image/png".to_owned(),
        data: Bytes(data),
    })
}

/// `bytes` as an image to send: its type taken from its leading bytes, scaled down where
/// it is larger than it needs to be. A GIF is sent as it is, which keeps its animation.
pub fn prepare(bytes: Vec<u8>) -> Result<Image, String> {
    let format = image::guess_format(&bytes).map_err(|_| not_an_image())?;
    let media_type = format.to_mime_type();
    if !IMAGE_MEDIA_TYPES.contains(&media_type) {
        return Err(not_an_image());
    }
    let reader = || {
        ImageReader::with_format(Cursor::new(&bytes), format)
            .into_dimensions()
            .map_err(|err| format!("not a readable image: {err}"))
    };
    let (width, height) = reader()?;
    let large = width.max(height) > LONG_EDGE || bytes.len() > MAX_IMAGE_BYTES;
    if !large || format == ImageFormat::Gif {
        return checked(Image {
            media_type: media_type.to_owned(),
            data: Bytes(bytes),
        });
    }
    let image = image::load_from_memory_with_format(&bytes, format)
        .map_err(|err| format!("not a readable image: {err}"))?;
    let image = scaled(image);
    // A PNG stays one, so a screenshot's text stays sharp, unless it is still too large.
    let mut out = if format == ImageFormat::Png {
        Image {
            media_type: "image/png".to_owned(),
            data: Bytes(encode(&image, ImageFormat::Png)?),
        }
    } else {
        jpeg(&image)?
    };
    if out.data.0.len() > MAX_IMAGE_BYTES && out.media_type != "image/jpeg" {
        out = jpeg(&image)?;
    }
    checked(out)
}

fn not_an_image() -> String {
    "not a PNG, JPEG, GIF or WebP image".to_owned()
}

/// `image` within [`LONG_EDGE`].
fn scaled(image: DynamicImage) -> DynamicImage {
    if image.width().max(image.height()) <= LONG_EDGE {
        return image;
    }
    image.resize(LONG_EDGE, LONG_EDGE, image::imageops::FilterType::Triangle)
}

fn jpeg(image: &DynamicImage) -> Result<Image, String> {
    // JPEG has no alpha.
    let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
    Ok(Image {
        media_type: "image/jpeg".to_owned(),
        data: Bytes(encode(&rgb, ImageFormat::Jpeg)?),
    })
}

fn encode(image: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    image
        .write_to(&mut out, format)
        .map_err(|err| format!("encoding the image: {err}"))?;
    Ok(out.into_inner())
}

/// `image`, unless it is over [`MAX_IMAGE_BYTES`].
fn checked(image: Image) -> Result<Image, String> {
    let bytes = image.data.0.len();
    if bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "{}, over the {} limit",
            size(bytes as u64),
            size(MAX_IMAGE_BYTES as u64)
        ));
    }
    Ok(image)
}

/// The folders and images in the folder `dir`, as typed, sorted by name; hidden ones too, for
/// the popup to filter.
pub fn list(dir: &str) -> Vec<Entry> {
    let path = if dir.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir)
    };
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // A link to a folder is a folder.
            let is_dir = std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
            (is_dir || is_image_path(&name)).then_some(Entry { name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Where a saved image goes: `~/Downloads` where there is one, else the working directory.
pub fn save_dir() -> PathBuf {
    let downloads = expand("~/Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        PathBuf::from(".")
    }
}

/// Writes `data` as `name` in `dir`, keeping any file already there; the path written.
pub fn save(dir: &Path, name: &str, data: &[u8]) -> Result<PathBuf, String> {
    let path = dir.join(name);
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, data).map_err(|err| format!("saving {}: {err}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::new(width, height));
        encode(&image, ImageFormat::Png).unwrap()
    }

    #[test]
    fn a_small_image_goes_as_it_is_and_a_large_one_is_scaled_down() {
        let small = png(40, 30);
        let image = prepare(small.clone()).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.data.0, small);

        let image = prepare(png(4096, 1024)).unwrap();
        assert_eq!(image.media_type, "image/png");
        let scaled = image::load_from_memory(&image.data.0).unwrap();
        assert_eq!((scaled.width(), scaled.height()), (LONG_EDGE, 512));
    }

    #[test]
    fn what_is_not_an_image_is_refused() {
        assert_eq!(
            prepare(b"%PDF-1.7 not an image".to_vec()).unwrap_err(),
            not_an_image()
        );
        let too_big = Image {
            media_type: "image/png".into(),
            data: Bytes(vec![0; MAX_IMAGE_BYTES + 1]),
        };
        assert_eq!(checked(too_big).unwrap_err(), "5 MB, over the 5 MB limit");
    }

    #[test]
    fn files_load_with_a_clear_reason_when_they_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, png(8, 8)).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let path = path.to_str().unwrap().to_owned();
        assert_eq!(load(&Source::File(path)).unwrap().media_type, "image/png");
        let missing = format!("{}/gone.png", dir.path().display());
        assert_eq!(
            load(&Source::File(missing)).unwrap_err(),
            "gone.png: no such file"
        );
        let names: Vec<_> = list(dir.path().to_str().unwrap())
            .into_iter()
            .map(|entry| (entry.name, entry.is_dir))
            .collect();
        assert_eq!(names, [("shot.png".into(), false), ("sub".into(), true)]);
    }

    #[test]
    fn dropped_paths_are_unquoted_and_only_images_count() {
        assert_eq!(
            dropped_paths("'/home/me/Screen Shot.png' /tmp/a\\ b.JPG "),
            Some(vec![
                "/home/me/Screen Shot.png".into(),
                "/tmp/a b.JPG".into()
            ])
        );
        assert_eq!(
            dropped_paths("file:///tmp/Screen%20Shot.png"),
            Some(vec!["/tmp/Screen Shot.png".into()])
        );
        assert_eq!(dropped_paths("/tmp/notes.txt"), None);
        assert_eq!(dropped_paths("look at shot.png"), None);
        assert_eq!(dropped_paths(""), None);
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(340 * 1024 + 5), "340 KB");
        assert_eq!(size(1_258_291), "1.2 MB");
        assert_eq!(size(5 * 1024 * 1024), "5 MB");
    }
}
