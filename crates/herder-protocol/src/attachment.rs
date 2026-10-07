//! Images and files attached to prompts.
//!
//! A client sends each attachment's bytes once, in the `send_prompt` that carries it. The
//! daemon keeps them on the session's host and journals only an [`Attachment`] in the prompt's
//! `user_message` item; clients fetch the bytes with `get_attachment`. The agent sees an image
//! with the prompt's text; it gets a file as a path on the session's host, named in a note
//! the daemon appends to the text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AttachmentId, Bytes};

/// Media types an image may have.
pub const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Most bytes one image may have.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Most bytes one file may have: as many as a whole prompt's attachments.
pub const MAX_FILE_BYTES: usize = MAX_PROMPT_ATTACHMENT_BYTES;

/// The media type of every file attached to a prompt: herder keeps a file's bytes as they
/// are, and its name tells its type.
pub const FILE_MEDIA_TYPE: &str = "application/octet-stream";

/// Most bytes a file's name may have.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// How a daemon's error for an attachment the vault never got, or evicted, begins, so a
/// session recovered from the vault lacks it; clients show it in place of the attachment.
pub const IMAGE_NOT_BACKED_UP: &str = "image not backed up";

/// Most bytes all images and files of one prompt may have together; keeps a `send_prompt`
/// well within a WebSocket frame.
pub const MAX_PROMPT_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

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

/// A file of any type a client sends with a prompt, for the agent to read from the session's
/// host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PromptFile {
    /// The file's name, without any folder: at most [`MAX_FILE_NAME_BYTES`] bytes, neither
    /// `.` nor `..`, and without `/`, `\` or control characters.
    pub name: String,
    /// The file's bytes: at most [`MAX_FILE_BYTES`].
    pub data: Bytes,
}

/// An image or file a prompt carried, kept on the session's host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    /// Fetches the bytes with `get_attachment`.
    pub attachment_id: AttachmentId,
    /// One of [`IMAGE_MEDIA_TYPES`] for an image; [`FILE_MEDIA_TYPE`] for a file.
    pub media_type: String,
    /// Size of the image or file in bytes.
    pub size: u64,
    /// The file's name, as sent; absent for an image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
