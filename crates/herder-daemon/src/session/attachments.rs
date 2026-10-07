//! Images and files prompts carry: checked when the prompt arrives, kept on this host, read
//! back when the prompt's turn starts and when a client fetches one.
//!
//! Each image is `<dir>/<session>/<attachment>.<ext>`, `<ext>` naming its media type. Each file
//! is `<dir>/<session>/<attachment>/<name>`: under its own name, which the agent sees, in a
//! folder of its own, so names never clash. `<dir>` is the daemon's own, outside any worktree,
//! so a file never shows up in the session's repository. They stay as long as the session's
//! journal does, archive included, since its transcript shows them.

use std::path::{Path, PathBuf};

use herder_protocol::{
    Attachment, AttachmentId, Bytes, ErrorCode, ErrorInfo, FILE_MEDIA_TYPE, IMAGE_MEDIA_TYPES,
    IMAGE_NOT_BACKED_UP, Image, MAX_FILE_BYTES, MAX_FILE_NAME_BYTES, MAX_IMAGE_BYTES,
    MAX_PROMPT_ATTACHMENT_BYTES, PromptFile, SessionId,
};

use super::error;

/// Checks that `images` and `files` are within the size limits, that each image is an image of
/// its media type, by its leading bytes, and that each file has a plain name.
pub(super) fn validate(images: &[Image], files: &[PromptFile]) -> Result<(), ErrorInfo> {
    let mut total = 0;
    for image in images {
        let Some(extension) = extension(&image.media_type) else {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "images must be one of {}, not {}",
                    IMAGE_MEDIA_TYPES.join(", "),
                    image.media_type
                ),
            ));
        };
        let bytes = &image.data.0;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(error(
                ErrorCode::BadRequest,
                format!("an image may have at most {MAX_IMAGE_BYTES} bytes"),
            ));
        }
        total += bytes.len();
        if !looks_like(extension, bytes) {
            return Err(error(
                ErrorCode::BadRequest,
                format!("an image sent as {} is not one", image.media_type),
            ));
        }
    }
    for file in files {
        if !plain_name(&file.name) {
            return Err(error(
                ErrorCode::BadRequest,
                format!(
                    "a file's name must be a plain name of at most {MAX_FILE_NAME_BYTES} bytes, \
                     not {:?}",
                    file.name
                ),
            ));
        }
        if file.data.0.len() > MAX_FILE_BYTES {
            return Err(error(
                ErrorCode::BadRequest,
                format!("a file may have at most {MAX_FILE_BYTES} bytes"),
            ));
        }
        total += file.data.0.len();
    }
    if total > MAX_PROMPT_ATTACHMENT_BYTES {
        return Err(error(
            ErrorCode::BadRequest,
            format!(
                "a prompt's images and files may have at most {MAX_PROMPT_ATTACHMENT_BYTES} bytes \
                 together"
            ),
        ));
    }
    Ok(())
}

/// Keeps `images` and `files`, already validated, under `dir` for `session_id`; returns them
/// as the prompt's attachments: the images, then the files, each in order.
pub(super) async fn save(
    dir: &Path,
    session_id: &SessionId,
    images: Vec<Image>,
    files: Vec<PromptFile>,
) -> Result<Vec<Attachment>, ErrorInfo> {
    let new_id = || AttachmentId::new(ulid::Ulid::new().to_string());
    let images = images.into_iter().map(|image| {
        let attachment = Attachment {
            attachment_id: new_id(),
            media_type: image.media_type,
            size: image.data.0.len() as u64,
            name: None,
        };
        (attachment, image.data)
    });
    let files = files.into_iter().map(|file| {
        let attachment = Attachment {
            attachment_id: new_id(),
            media_type: FILE_MEDIA_TYPE.to_owned(),
            size: file.data.0.len() as u64,
            name: Some(file.name),
        };
        (attachment, file.data)
    });
    let kept: Vec<_> = images.chain(files).collect();
    let attachments = kept
        .iter()
        .map(|(attachment, _)| attachment.clone())
        .collect();
    keep(dir, session_id, kept).await?;
    Ok(attachments)
}

/// Keeps `attachments` under `dir` for `session_id`, each under its id: a new prompt's, or
/// those of a session forked from another one.
pub(super) async fn keep(
    dir: &Path,
    session_id: &SessionId,
    attachments: Vec<(Attachment, Bytes)>,
) -> Result<(), ErrorInfo> {
    for (attachment, data) in attachments {
        let path = path(dir, session_id, &attachment).ok_or_else(|| {
            error(
                ErrorCode::BadRequest,
                format!(
                    "{} is neither an image nor a file with a plain name",
                    attachment.attachment_id
                ),
            )
        })?;
        let written = async {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, &data.0).await
        };
        written.await.map_err(|err| {
            error(
                ErrorCode::Internal,
                format!("cannot keep an attachment at {}: {err}", path.display()),
            )
        })?;
    }
    Ok(())
}

/// The bytes of `attachment` of `session_id`, as kept under `dir`.
pub(super) async fn load(
    dir: &Path,
    session_id: &SessionId,
    attachment: &Attachment,
) -> Result<Bytes, ErrorInfo> {
    let path = path(dir, session_id, attachment).ok_or_else(|| {
        error(
            ErrorCode::Internal,
            format!(
                "{} is neither an image nor a file with a plain name",
                attachment.attachment_id
            ),
        )
    })?;
    let data = tokio::fs::read(&path).await.map_err(|err| {
        error(
            ErrorCode::Internal,
            format!("cannot read the attachment {}: {err}", path.display()),
        )
    })?;
    Ok(Bytes(data))
}

/// The media type and bytes of the attachment `attachment_id` of `session_id`, as kept under
/// `dir`, whatever it is.
pub(super) async fn fetch(
    dir: &Path,
    session_id: &SessionId,
    attachment_id: &AttachmentId,
) -> Result<(String, Bytes), ErrorInfo> {
    let dir = dir.join(session_id.as_str());
    let read = |path: PathBuf| async move {
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(Bytes(data))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(error(
                ErrorCode::Internal,
                format!("cannot read the attachment {}: {err}", path.display()),
            )),
        }
    };
    if let Some(id) = plain_id(attachment_id) {
        for media_type in IMAGE_MEDIA_TYPES {
            let Some(extension) = extension(media_type) else {
                continue;
            };
            if let Some(data) = read(dir.join(format!("{id}.{extension}"))).await? {
                return Ok((media_type.to_owned(), data));
            }
        }
        // A file's folder holds just that file.
        if let Ok(mut entries) = tokio::fs::read_dir(dir.join(id)).await
            && let Ok(Some(entry)) = entries.next_entry().await
            && let Some(data) = read(entry.path()).await?
        {
            return Ok((FILE_MEDIA_TYPE.to_owned(), data));
        }
    }
    // Kept before its prompt is journaled, so one a prompt names is missing only from a
    // session recovered from a vault that never got it.
    Err(error(
        ErrorCode::NotFound,
        format!(
            "{IMAGE_NOT_BACKED_UP}: session {session_id} has no attachment {attachment_id} here"
        ),
    ))
}

/// Where `attachment` of `session_id` is kept under `dir`; `None` for one that is neither an
/// image nor a file with a plain name. Ids are the daemon's own ULIDs, so they name a file
/// safely; one a client sends is checked to be a plain name.
pub(super) fn path(dir: &Path, session_id: &SessionId, attachment: &Attachment) -> Option<PathBuf> {
    let id = plain_id(&attachment.attachment_id)?;
    let dir = dir.join(session_id.as_str());
    match &attachment.name {
        None => Some(dir.join(format!("{id}.{}", extension(&attachment.media_type)?))),
        Some(name) if attachment.media_type == FILE_MEDIA_TYPE && plain_name(name) => {
            Some(dir.join(id).join(name))
        }
        Some(_) => None,
    }
}

/// `text` with a note listing `paths`, the files a prompt carries, for the agent to read.
pub(super) fn with_files(text: &str, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return text.to_owned();
    }
    let mut text = text.to_owned();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("Attached files:");
    for path in paths {
        text.push_str("\n- ");
        text.push_str(&path.display().to_string());
    }
    text
}

/// `attachment_id`, when it is a plain ASCII alphanumeric name.
fn plain_id(attachment_id: &AttachmentId) -> Option<&str> {
    let id = attachment_id.as_str();
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())).then_some(id)
}

/// Whether `name` names a file in a folder, and nothing else: no folders, no `.` or `..`, no
/// control characters, within [`MAX_FILE_NAME_BYTES`].
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// The file extension of an image media type.
fn extension(media_type: &str) -> Option<&'static str> {
    Some(match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    })
}

/// Whether `bytes` start the way an image file with `extension` does.
fn looks_like(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(b"\xff\xd8\xff"),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(media_type: &str, data: &[u8]) -> Image {
        Image {
            media_type: media_type.into(),
            data: Bytes(data.to_vec()),
        }
    }

    fn file(name: &str, data: &[u8]) -> PromptFile {
        PromptFile {
            name: name.into(),
            data: Bytes(data.to_vec()),
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn images_must_be_of_their_type_and_within_the_limits() {
        assert!(validate(&[], &[]).is_ok());
        let ok = [
            image("image/png", PNG),
            image("image/jpeg", b"\xff\xd8\xff\xe0"),
            image("image/gif", b"GIF89a.."),
            image("image/webp", b"RIFF\0\0\0\0WEBPVP8 "),
        ];
        assert!(validate(&ok, &[]).is_ok());
        let refused = |images: &[Image]| validate(images, &[]).unwrap_err().code;
        assert_eq!(
            refused(&[image("image/svg+xml", b"<svg")]),
            ErrorCode::BadRequest
        );
        assert_eq!(
            refused(&[image("image/png", b"GIF89a")]),
            ErrorCode::BadRequest
        );
        assert_eq!(
            refused(&[image("image/webp", b"RIFF")]),
            ErrorCode::BadRequest
        );
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(refused(&[image("image/png", &big)]), ErrorCode::BadRequest);
        big.truncate(MAX_IMAGE_BYTES);
        let three = vec![image("image/png", &big); 3];
        assert_eq!(refused(&three), ErrorCode::BadRequest);
        assert!(validate(&three[..2], &[]).is_ok());
    }

    #[test]
    fn files_must_have_plain_names_and_fit_with_the_images() {
        let ok = [
            file("report.xlsx", b"PK\x03\x04"),
            file(".env.example", b""),
            file("Q3 – summary (final).pdf", b"%PDF"),
        ];
        assert!(validate(&[], &ok).is_ok());
        let refused = |files: &[PromptFile]| validate(&[], files).unwrap_err().code;
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "a\nb",
            &"x".repeat(256),
        ] {
            assert_eq!(
                refused(&[file(name, b"x")]),
                ErrorCode::BadRequest,
                "{name:?}"
            );
        }
        let big = vec![0; MAX_FILE_BYTES + 1];
        assert_eq!(refused(&[file("big.bin", &big)]), ErrorCode::BadRequest);
        let half = vec![0; MAX_PROMPT_ATTACHMENT_BYTES / 2];
        assert!(validate(&[], &[file("a", &half), file("b", &half)]).is_ok());
        assert_eq!(
            refused(&[file("a", &half), file("b", &half), file("c", b"x")]),
            ErrorCode::BadRequest
        );
        // Images and files share the prompt's limit.
        let mut png = PNG.to_vec();
        png.resize(MAX_IMAGE_BYTES, 0);
        let files = [file(
            "a",
            &vec![0; MAX_PROMPT_ATTACHMENT_BYTES - MAX_IMAGE_BYTES],
        )];
        assert!(validate(&[image("image/png", &png)], &files).is_ok());
        let error = validate(
            &[image("image/png", &png)],
            &[file("a", &half), file("b", &half)],
        );
        assert_eq!(error.unwrap_err().code, ErrorCode::BadRequest);
    }

    #[tokio::test]
    async fn kept_images_are_read_back_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionId::new("s1");
        let images = vec![
            image("image/png", PNG),
            image("image/jpeg", b"\xff\xd8\xff"),
        ];
        let kept = save(tmp.path(), &session, images.clone(), Vec::new())
            .await
            .unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(
            (kept[0].media_type.as_str(), kept[0].size, &kept[0].name),
            ("image/png", 12, &None)
        );
        for (attachment, image) in kept.iter().zip(&images) {
            assert_eq!(
                load(tmp.path(), &session, attachment).await.unwrap(),
                image.data
            );
            let fetched = fetch(tmp.path(), &session, &attachment.attachment_id).await;
            assert_eq!(
                fetched.unwrap(),
                (image.media_type.clone(), image.data.clone())
            );
        }
        let missing = async |session: &str, id: &str| {
            let session = SessionId::new(session);
            let fetched = fetch(tmp.path(), &session, &AttachmentId::new(id)).await;
            let error = fetched.unwrap_err();
            // As a session recovered without it shows it.
            assert!(error.message.starts_with(IMAGE_NOT_BACKED_UP), "{error:?}");
            error.code
        };
        assert_eq!(
            missing("s2", kept[0].attachment_id.as_str()).await,
            ErrorCode::NotFound
        );
        assert_eq!(missing("s1", "../../etc/passwd").await, ErrorCode::NotFound);
    }

    #[test]
    fn the_agent_gets_the_files_paths_after_the_text() {
        assert_eq!(with_files("hi", &[]), "hi");
        let paths = [PathBuf::from("/a/1/x.csv"), PathBuf::from("/a/2/y z.pdf")];
        assert_eq!(
            with_files("sum these", &paths),
            "sum these\n\nAttached files:\n- /a/1/x.csv\n- /a/2/y z.pdf"
        );
        assert_eq!(with_files("", &paths[..1]), "Attached files:\n- /a/1/x.csv");
    }

    #[tokio::test]
    async fn kept_files_keep_their_names_in_a_folder_each() {
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionId::new("s1");
        let files = vec![file("report.xlsx", b"one"), file("report.xlsx", b"two")];
        let kept = save(tmp.path(), &session, vec![image("image/png", PNG)], files)
            .await
            .unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].name, None);
        for (attachment, data) in kept[1..].iter().zip([b"one", b"two"]) {
            assert_eq!(attachment.name.as_deref(), Some("report.xlsx"));
            assert_eq!(attachment.media_type, FILE_MEDIA_TYPE);
            assert_eq!(attachment.size, 3);
            let path = path(tmp.path(), &session, attachment).unwrap();
            assert_eq!(
                path,
                tmp.path()
                    .join("s1")
                    .join(attachment.attachment_id.as_str())
                    .join("report.xlsx")
            );
            assert_eq!(std::fs::read(&path).unwrap(), data);
            assert_eq!(
                load(tmp.path(), &session, attachment).await.unwrap().0,
                data
            );
            let fetched = fetch(tmp.path(), &session, &attachment.attachment_id).await;
            assert_eq!(
                fetched.unwrap(),
                (FILE_MEDIA_TYPE.to_owned(), Bytes(data.to_vec()))
            );
        }
        // A forked session's file named anything but a plain name is refused.
        let mut sneaky = kept[1].clone();
        sneaky.name = Some("../../escape".into());
        let kept = keep(tmp.path(), &session, vec![(sneaky, Bytes(b"x".to_vec()))]).await;
        assert_eq!(kept.unwrap_err().code, ErrorCode::BadRequest);
    }
}
