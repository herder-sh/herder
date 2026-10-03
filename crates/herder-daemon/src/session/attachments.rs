//! Images prompts carry: checked when the prompt arrives, kept as files on this host, read
//! back when the prompt's turn starts and when a client fetches one.
//!
//! Each image is `<dir>/<session>/<attachment>.<ext>`, `<ext>` naming its media type. They
//! stay as long as the session's journal does, archive included, since its transcript shows
//! them.

use std::path::{Path, PathBuf};

use herder_protocol::{
    Attachment, AttachmentId, Bytes, ErrorCode, ErrorInfo, IMAGE_MEDIA_TYPES, Image,
    MAX_IMAGE_BYTES, MAX_PROMPT_IMAGE_BYTES, SessionId,
};

use super::error;

/// Checks that `images` are within the size limits and that each is an image of its media
/// type, by its leading bytes.
pub(super) fn validate(images: &[Image]) -> Result<(), ErrorInfo> {
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
    if total > MAX_PROMPT_IMAGE_BYTES {
        return Err(error(
            ErrorCode::BadRequest,
            format!("a prompt's images may have at most {MAX_PROMPT_IMAGE_BYTES} bytes together"),
        ));
    }
    Ok(())
}

/// Keeps `images`, already validated, under `dir` for `session_id`; returns them as the
/// prompt's attachments, in order.
pub(super) async fn save(
    dir: &Path,
    session_id: &SessionId,
    images: Vec<Image>,
) -> Result<Vec<Attachment>, ErrorInfo> {
    let images: Vec<_> = images
        .into_iter()
        .map(|image| (AttachmentId::new(ulid::Ulid::new().to_string()), image))
        .collect();
    let attachments = images
        .iter()
        .map(|(attachment_id, image)| Attachment {
            attachment_id: attachment_id.clone(),
            media_type: image.media_type.clone(),
            size: image.data.0.len() as u64,
        })
        .collect();
    keep(dir, session_id, images).await?;
    Ok(attachments)
}

/// Keeps `images` under `dir` for `session_id`, each under its id: a new prompt's, or those
/// of a session recovered from another host.
pub(super) async fn keep(
    dir: &Path,
    session_id: &SessionId,
    images: Vec<(AttachmentId, Image)>,
) -> Result<(), ErrorInfo> {
    let dir = dir.join(session_id.as_str());
    for (attachment_id, image) in images {
        let path = file(&dir, &attachment_id, &image.media_type).ok_or_else(|| {
            error(
                ErrorCode::BadRequest,
                format!("{} is not an image type", image.media_type),
            )
        })?;
        let written = async {
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(&path, &image.data.0).await
        };
        written.await.map_err(|err| {
            error(
                ErrorCode::Internal,
                format!("cannot keep an image at {}: {err}", path.display()),
            )
        })?;
    }
    Ok(())
}

/// The image `attachment` of `session_id`, as kept under `dir`.
pub(super) async fn load(
    dir: &Path,
    session_id: &SessionId,
    attachment: &Attachment,
) -> Result<Image, ErrorInfo> {
    let dir = dir.join(session_id.as_str());
    let path = file(&dir, &attachment.attachment_id, &attachment.media_type).ok_or_else(|| {
        error(
            ErrorCode::Internal,
            format!("{} is not an image type", attachment.media_type),
        )
    })?;
    let data = tokio::fs::read(&path).await.map_err(|err| {
        error(
            ErrorCode::Internal,
            format!("cannot read the image {}: {err}", path.display()),
        )
    })?;
    Ok(Image {
        media_type: attachment.media_type.clone(),
        data: Bytes(data),
    })
}

/// The image `attachment_id` of `session_id`, as kept under `dir`, whatever its type.
pub(super) async fn fetch(
    dir: &Path,
    session_id: &SessionId,
    attachment_id: &AttachmentId,
) -> Result<Image, ErrorInfo> {
    let dir = dir.join(session_id.as_str());
    for media_type in IMAGE_MEDIA_TYPES {
        let Some(path) = file(&dir, attachment_id, media_type) else {
            continue;
        };
        match tokio::fs::read(&path).await {
            Ok(data) => {
                return Ok(Image {
                    media_type: media_type.to_owned(),
                    data: Bytes(data),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(error(
                    ErrorCode::Internal,
                    format!("cannot read the image {}: {err}", path.display()),
                ));
            }
        }
    }
    Err(error(
        ErrorCode::NotFound,
        format!("session {session_id} has no image {attachment_id}"),
    ))
}

/// Where the image `attachment_id` of type `media_type` is kept in a session's `dir`; `None`
/// for a type that is no image. Ids are the daemon's own ULIDs, so they name a file safely;
/// one a client sends is checked to be a plain name.
fn file(dir: &Path, attachment_id: &AttachmentId, media_type: &str) -> Option<PathBuf> {
    let id = attachment_id.as_str();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(dir.join(format!("{id}.{}", extension(media_type)?)))
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

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn images_must_be_of_their_type_and_within_the_limits() {
        assert!(validate(&[]).is_ok());
        let ok = [
            image("image/png", PNG),
            image("image/jpeg", b"\xff\xd8\xff\xe0"),
            image("image/gif", b"GIF89a.."),
            image("image/webp", b"RIFF\0\0\0\0WEBPVP8 "),
        ];
        assert!(validate(&ok).is_ok());
        let refused = |images: &[Image]| validate(images).unwrap_err().code;
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
        assert!(validate(&three[..2]).is_ok());
    }

    #[tokio::test]
    async fn kept_images_are_read_back_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionId::new("s1");
        let images = vec![
            image("image/png", PNG),
            image("image/jpeg", b"\xff\xd8\xff"),
        ];
        let kept = save(tmp.path(), &session, images.clone()).await.unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(
            (kept[0].media_type.as_str(), kept[0].size),
            ("image/png", 12)
        );
        for (attachment, image) in kept.iter().zip(&images) {
            assert_eq!(
                &load(tmp.path(), &session, attachment).await.unwrap(),
                image
            );
            let fetched = fetch(tmp.path(), &session, &attachment.attachment_id).await;
            assert_eq!(&fetched.unwrap(), image);
        }
        let missing = async |session: &str, id: &str| {
            let session = SessionId::new(session);
            let fetched = fetch(tmp.path(), &session, &AttachmentId::new(id)).await;
            fetched.unwrap_err().code
        };
        assert_eq!(
            missing("s2", kept[0].attachment_id.as_str()).await,
            ErrorCode::NotFound
        );
        assert_eq!(missing("s1", "../../etc/passwd").await, ErrorCode::NotFound);
    }
}
