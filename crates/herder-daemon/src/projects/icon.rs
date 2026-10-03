//! Project icons: an image file found in a project's clone, as T3 Code finds them.
//!
//! [`find`] takes the file a `[[project]]` entry's `icon` names, else the first of
//! [`CANDIDATES`] that is an icon, else the largest PNG of an Xcode `AppIcon.appiconset` at most
//! two folders down. A file is an icon when its extension gives one of
//! [`PROJECT_ICON_MEDIA_TYPES`] and it has at most [`MAX_PROJECT_ICON_BYTES`]; nothing outside
//! the clone is ever read, so a symlink leading out of it is no icon.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use herder_protocol::{MAX_PROJECT_ICON_BYTES, PROJECT_ICON_MEDIA_TYPES};
use sha2::{Digest, Sha256};

/// Where icons usually are, relative to the clone, in the order they are tried.
pub const CANDIDATES: &[&str] = &[
    "favicon.svg",
    "favicon.png",
    "favicon.ico",
    "public/favicon.svg",
    "public/favicon.png",
    "public/favicon.ico",
    "app/favicon.ico",
    "app/icon.svg",
    "app/icon.png",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "static/favicon.svg",
    "static/favicon.png",
    "static/favicon.ico",
    "assets/icon.svg",
    "assets/icon.png",
    "logo.svg",
    "logo.png",
    ".github/logo.svg",
    ".github/logo.png",
    "docs/logo.svg",
    "docs/logo.png",
];

/// Folders never searched for an `AppIcon.appiconset`: large and never holding one.
const SKIPPED: [&str; 2] = ["node_modules", "target"];

/// An icon file's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// SHA-256 of `data` as lowercase hex.
    pub hash: String,
    /// One of [`PROJECT_ICON_MEDIA_TYPES`].
    pub media_type: &'static str,
    /// The file's bytes.
    pub data: Vec<u8>,
}

/// The icon of the clone at `clone`: the file `explicit` names relative to it, else the first
/// candidate that is an icon. Blocks on the file system.
pub fn find(clone: &Path, explicit: Option<&Path>) -> Option<Icon> {
    let root = clone.canonicalize().ok()?;
    explicit
        .map(Path::to_path_buf)
        .into_iter()
        .chain(CANDIDATES.iter().map(PathBuf::from))
        .chain(std::iter::once_with(|| app_icon(&root)).flatten())
        .find_map(|relative| read(&root, &relative))
}

/// The file at `relative` in the canonical clone `root`, if it is an icon inside it.
fn read(root: &Path, relative: &Path) -> Option<Icon> {
    let media_type = media_type(relative)?;
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let file = File::open(&path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut data = Vec::new();
    // One byte more than allowed tells a file at the cap from a larger one.
    file.take(MAX_PROJECT_ICON_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.is_empty() || data.len() > MAX_PROJECT_ICON_BYTES {
        return None;
    }
    let hash = Sha256::digest(&data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(Icon {
        hash,
        media_type,
        data,
    })
}

/// The media type a file's extension gives, if an icon may have it.
fn media_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "jpg" | "jpeg" => "image/jpeg",
        _ => return None,
    };
    debug_assert!(PROJECT_ICON_MEDIA_TYPES.contains(&media_type));
    Some(media_type)
}

/// The largest PNG of at most [`MAX_PROJECT_ICON_BYTES`] in the first
/// `Assets.xcassets/AppIcon.appiconset` in `root` or a folder at most two down, relative to
/// `root`. Hidden folders and [`SKIPPED`] ones are not searched, nor are symlinks followed.
fn app_icon(root: &Path) -> Option<PathBuf> {
    let children = subfolders(root);
    let grandchildren = children.iter().flat_map(|child| subfolders(child));
    std::iter::once(root.to_path_buf())
        .chain(children.iter().cloned())
        .chain(grandchildren)
        .find_map(|folder| {
            let set = folder.join("Assets.xcassets/AppIcon.appiconset");
            let largest = fs::read_dir(&set)
                .ok()?
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|e| e == "png"))
                .filter_map(|entry| Some((entry.metadata().ok()?.len(), entry.path())))
                .filter(|(len, _)| *len <= MAX_PROJECT_ICON_BYTES as u64)
                .max_by_key(|(len, _)| *len)?;
            largest.1.strip_prefix(root).ok().map(Path::to_path_buf)
        })
}

/// The folders directly in `folder` that are not symlinks, hidden or [`SKIPPED`], by name.
fn subfolders(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !SKIPPED.contains(&name.as_ref())
        })
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, data: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn found(root: &Path) -> Option<Vec<u8>> {
        find(root, None).map(|icon| icon.data)
    }

    #[test]
    fn a_repository_without_an_icon_has_none() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "README.md", b"# app");
        write(repo.path(), "public/robots.txt", b"");
        assert_eq!(find(repo.path(), None), None);
    }

    #[test]
    fn candidates_are_tried_in_order() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        // Written from last to first: each new one must win over all the others.
        for (index, candidate) in CANDIDATES.iter().enumerate().rev() {
            write(root, candidate, candidate.as_bytes());
            assert_eq!(
                found(root),
                Some(candidate.as_bytes().to_vec()),
                "candidate {index}"
            );
        }
    }

    #[test]
    fn an_icon_gives_its_media_type_and_hash() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "public/favicon.ico", b"ico");
        let icon = find(repo.path(), None).unwrap();
        assert_eq!(icon.media_type, "image/x-icon");
        assert_eq!(
            icon.hash,
            "c51052ef06b65a956d9edade6297b03388457a6521544a6d11e1c0507173b2f3"
        );
    }

    #[test]
    fn the_explicit_icon_comes_first_and_falls_back_to_the_candidates() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(root, "favicon.svg", b"<svg/>");
        write(root, "brand/mark.JPG", b"jpeg");
        let icon = find(root, Some(Path::new("brand/mark.JPG"))).unwrap();
        assert_eq!(
            (icon.media_type, icon.data),
            ("image/jpeg", b"jpeg".to_vec())
        );
        let icon = find(root, Some(Path::new("brand/missing.png"))).unwrap();
        assert_eq!(icon.data, b"<svg/>");
        let icon = find(root, Some(Path::new("brand/notes.txt"))).unwrap();
        assert_eq!(icon.data, b"<svg/>");
    }

    #[test]
    fn files_over_the_cap_or_empty_are_skipped() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        write(root, "favicon.svg", &vec![b' '; MAX_PROJECT_ICON_BYTES + 1]);
        write(root, "favicon.png", b"");
        write(root, "favicon.ico", &vec![0; MAX_PROJECT_ICON_BYTES]);
        let icon = find(root, None).unwrap();
        assert_eq!(
            (icon.media_type, icon.data.len()),
            ("image/x-icon", MAX_PROJECT_ICON_BYTES)
        );
    }

    #[test]
    fn nothing_outside_the_clone_is_read() {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.png", b"secret");
        write(outside.path(), "dir/favicon.png", b"secret");
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::os::unix::fs::symlink(outside.path().join("secret.png"), root.join("favicon.svg"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path().join("dir"), root.join("public")).unwrap();
        assert_eq!(find(root, None), None);
        let escape = format!(
            "../{}/secret.png",
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(find(root, Some(Path::new(&escape))), None);
        assert_eq!(find(root, Some(&outside.path().join("secret.png"))), None);

        // A symlink that stays inside the clone is followed.
        write(root, "art/logo.png", b"inside");
        std::os::unix::fs::symlink(root.join("art/logo.png"), root.join("logo.png")).unwrap();
        assert_eq!(found(root), Some(b"inside".to_vec()));
    }

    #[test]
    fn a_folder_named_like_a_candidate_is_no_icon() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("favicon.svg")).unwrap();
        write(repo.path(), "logo.png", b"png");
        assert_eq!(found(repo.path()), Some(b"png".to_vec()));
    }

    #[test]
    fn the_largest_png_of_an_app_icon_set_comes_last() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let set = "apple/App/Assets.xcassets/AppIcon.appiconset";
        write(root, &format!("{set}/Contents.json"), &[b'{'; 4096]);
        write(root, &format!("{set}/icon-64.png"), &[1; 64]);
        write(root, &format!("{set}/icon-1024.png"), &[2; 1024]);
        write(
            root,
            &format!("{set}/icon-huge.png"),
            &vec![3; MAX_PROJECT_ICON_BYTES + 1],
        );
        let icon = find(root, None).unwrap();
        assert_eq!((icon.media_type, icon.data), ("image/png", vec![2; 1024]));

        write(root, "docs/logo.png", b"docs");
        assert_eq!(found(root), Some(b"docs".to_vec()));
    }

    #[test]
    fn app_icon_sets_deeper_or_in_skipped_folders_are_not_searched() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        let set = "Assets.xcassets/AppIcon.appiconset/icon.png";
        write(root, &format!("a/b/c/{set}"), b"deep");
        write(root, &format!("node_modules/pkg/{set}"), b"dep");
        write(root, &format!(".build/{set}"), b"hidden");
        assert_eq!(found(root), None);
        write(root, &format!("ios/{set}"), b"ios");
        assert_eq!(found(root), Some(b"ios".to_vec()));
    }
}
