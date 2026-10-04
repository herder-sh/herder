//! Images attached to prompts.
//!
//! A client sends each image's bytes once, in the `send_prompt` that carries it. The daemon
//! keeps them on the session's host and journals only an [`Attachment`] in the prompt's
//! `user_message` item; clients fetch the bytes with `get_attachment`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AttachmentId, Bytes};

/// Media types an image may have.
pub const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Most bytes one image may have.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// How a daemon's error for an image the vault never got, or evicted, begins, so a session
/// recovered from the vault lacks it; clients show it in place of the image.
pub const IMAGE_NOT_BACKED_UP: &str = "image not backed up";

/// Most bytes all images of one prompt may have together; keeps a `send_prompt` well within a
/// WebSocket frame.
pub const MAX_PROMPT_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// An image file a client sends: with a prompt, or as a project's icon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    /// The image's media type: one of [`IMAGE_MEDIA_TYPES`] with a prompt, of
    /// [`crate::PROJECT_ICON_MEDIA_TYPES`] as an icon; the bytes must be an image of that type.
    pub media_type: String,
    /// The image file's bytes: at most [`MAX_IMAGE_BYTES`] with a prompt, at most
    /// [`crate::MAX_PROJECT_ICON_BYTES`] as an icon.
    pub data: Bytes,
}

/// An image a prompt carried, kept on the session's host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    /// Fetches the bytes with `get_attachment`.
    pub attachment_id: AttachmentId,
    /// One of [`IMAGE_MEDIA_TYPES`].
    pub media_type: String,
    /// Size of the image in bytes.
    pub size: u64,
}
