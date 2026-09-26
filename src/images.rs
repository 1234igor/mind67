//! The image store: files named by a hash of their own bytes.
//!
//! An image lives in `<data dir>/images/`, and a node holds its name. The map
//! JSON never holds pixels — which matters more here than it looks: undo is a
//! whole-graph clone, two hundred deep, so anything on a node is copied two
//! hundred times.
//!
//! Content addressing buys three things at once. Dropping the same photo twice
//! writes one file. A file is never rewritten, so a name always means the same
//! pixels. And nothing has to decide what an image is *called*, which is the
//! question every "pasted image 3.png" scheme eventually gets wrong.
//!
//! # Deletion
//!
//! Never automatic. Deleting a node drops the reference, not the file, because
//! undo would otherwise bring the node back pointing at nothing. The store only
//! grows, and reclaiming it is an explicit act.
//!
//! Duplicated, deliberately, from GravityNote's module of the same name: the
//! two apps are separate repositories that must each build from a fresh clone,
//! and a path dependency across them would break that.

use std::io;
use std::path::{Path, PathBuf};

/// Where the store sits, relative to the data directory.
pub const DIR_NAME: &str = "images";

/// Formats stored as they arrive. Everything else that macOS can read — HEIC
/// above all, which is what an iPhone photo is — is transcoded to PNG on the
/// way in, because the decoder cannot open it later.
const NATIVE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif", "qoi",
];

/// Formats accepted from a drop or the pasteboard.
const ACCEPTED: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif", "qoi", "heic",
    "heif", "pdf", "psd", "tga",
];

/// A newly stored image: its name in the store, and the pixel size it turned
/// out to have. The size is used once, to choose the box the note will record;
/// after that the note is the only thing that says how big to draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub name: String,
    pub w: u32,
    pub h: u32,
}

/// `<dir>/images`
pub fn store_dir(dir: &Path) -> PathBuf {
    dir.join(DIR_NAME)
}

/// The absolute path of a stored image. Absolute on purpose: a bundle launched
/// by `open` has `/` for a working directory, so the relative form in the note
/// text would resolve to nothing.
pub fn path(dir: &Path, name: &str) -> PathBuf {
    store_dir(dir).join(name)
}

/// Whether a dropped file is worth trying to import.
pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| ACCEPTED.contains(&e.as_str()))
}

/// Store `bytes`, returning the name to refer to them by.
///
/// Runs off the main thread — hashing and writing twenty megabytes is not
/// something a frame should wait for.
pub fn import_bytes(dir: &Path, bytes: &[u8]) -> io::Result<Import> {
    let (bytes, ext) = match sniff(bytes) {
        Some(ext) => (bytes.to_vec(), ext),
        // Not something the decoder can read — HEIC, PDF, PSD. macOS can read
        // it, so hand it to macOS once, here, rather than failing at every
        // future draw.
        None => (transcode_to_png(bytes)?, "png".to_string()),
    };
    let (w, h) = dimensions(&bytes)?;
    let name = format!("{}.{ext}", hash_hex(&bytes));
    let path = path(dir, &name);
    // Content addressed: if it is already there, it is already identical.
    if !path.exists() {
        std::fs::create_dir_all(store_dir(dir))?;
        write_atomically(&path, &bytes)?;
    }
    Ok(Import { name, w, h })
}

/// Store the contents of a file that was dropped on the window.
pub fn import_file(dir: &Path, src: &Path) -> io::Result<Import> {
    import_bytes(dir, &std::fs::read(src)?)
}

/// The extension to store under, or `None` when the decoder cannot read this
/// at all. Sniffed from the bytes, never from the name a file arrived with.
fn sniff(bytes: &[u8]) -> Option<String> {
    let format = image::guess_format(bytes).ok()?;
    let ext = format.extensions_str().first()?.to_string();
    NATIVE.contains(&ext.as_str()).then_some(ext)
}

/// Read the pixel size out of the header. Cheap: it does not decode the image.
fn dimensions(bytes: &[u8]) -> io::Result<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Convert anything macOS can read into a PNG, via `sips`, which ships with the
/// system. This is the only path an iPhone photo can take: `image` cannot
/// decode HEIC and neither can GPUI, so a heic left as it arrived would be a
/// reference that never draws.
fn transcode_to_png(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("jotmind-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let src = dir.join("in");
    let dst = dir.join("out.png");
    std::fs::write(&src, bytes)?;
    let status = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "png"])
        .arg(&src)
        .arg("--out")
        .arg(&dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    let out = if status.success() {
        std::fs::read(&dst)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an image this Mac can read",
        ))
    };
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Write via a temporary file in the same directory and a rename, so a crash
/// or a full disk never leaves a half-written image behind a name that claims
/// to be the hash of its contents.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// The first 8 bytes of a SHA-256, hex. Sixteen hex digits name every image a
/// person will ever paste with room to spare, and a short name keeps the note
/// readable — the reference is text someone has to look at.
fn hash_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jotmind-images-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A 2x1 PNG, encoded here so the tests need no fixture file.
    fn png_2x1() -> Vec<u8> {
        let mut out = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn importing_twice_writes_one_file() {
        let dir = scratch("dedup");
        let bytes = png_2x1();
        let a = import_bytes(&dir, &bytes).unwrap();
        let b = import_bytes(&dir, &bytes).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.w, 2);
        assert_eq!(a.h, 1);
        assert!(a.name.ends_with(".png"));
        let files: Vec<_> = std::fs::read_dir(store_dir(&dir)).unwrap().collect();
        assert_eq!(files.len(), 1, "the same bytes must be one file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_bytes_get_different_names() {
        let dir = scratch("distinct");
        let mut other = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([0, 0, 255, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut other),
            image::ImageFormat::Png,
        )
        .unwrap();
        let a = import_bytes(&dir, &png_2x1()).unwrap();
        let b = import_bytes(&dir, &other).unwrap();
        assert_ne!(a.name, b.name);
        assert_eq!((b.w, b.h), (3, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stored_bytes_are_the_bytes_that_arrived() {
        let dir = scratch("verbatim");
        let bytes = png_2x1();
        let import = import_bytes(&dir, &bytes).unwrap();
        assert_eq!(std::fs::read(path(&dir, &import.name)).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn junk_is_not_an_image() {
        let dir = scratch("junk");
        assert!(import_bytes(&dir, b"this is not an image at all").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_store_sits_inside_the_data_directory() {
        let dir = PathBuf::from("/tmp/x");
        assert_eq!(path(&dir, "a.png"), PathBuf::from("/tmp/x/images/a.png"));
        assert!(path(&dir, "a.png").is_absolute());
    }

    #[test]
    fn droppable_files_are_recognised_by_extension() {
        assert!(is_image_file(Path::new("/a/b.PNG")));
        assert!(is_image_file(Path::new("/a/b.heic")));
        assert!(!is_image_file(Path::new("/a/b.md")));
        assert!(!is_image_file(Path::new("/a/b")));
    }
}
