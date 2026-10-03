//! Images: attached to a prompt from the clipboard, a drop or a file, shown as thumbnails in
//! the composer and in the transcript's prompts, and full size in a dialog.
//!
//! An image goes out as one of [`IMAGE_MEDIA_TYPES`], at most [`MAX_IMAGE_BYTES`]; one larger
//! than [`LONG_EDGE`] on its long edge, or in bytes, is scaled down first, as the TUI does. No
//! provider looks at more pixels than that. Decoding runs on a blocking thread, never the main
//! loop.
//!
//! A prompt's images are fetched from its machine with `get_attachment` once each, while the
//! session is open; one the machine no longer has shows as a quiet placeholder.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk_pixbuf::{InterpType, PixbufLoader};
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk};
use herder_protocol::{Attachment, AttachmentId, Bytes, IMAGE_MEDIA_TYPES, Image, MAX_IMAGE_BYTES};

use crate::transcript::{hbox, vbox};

/// The longest edge, in pixels, an image is sent at.
pub const LONG_EDGE: i32 = 2048;
/// Bytes past which a file is not read at all: no screenshot or photo comes close.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// A thumbnail's corner radius.
const RADIUS: f32 = 8.0;
/// The height of a prompt's thumbnails in the transcript, and their widest.
const MESSAGE_HEIGHT: i32 = 120;
const MESSAGE_WIDTH: i32 = 240;
/// The side of a composer's square thumbnails.
pub const PENDING_SIDE: i32 = 64;

/// An image ready to send, and its picture.
#[derive(Clone, Debug)]
pub struct Attached {
    pub image: Image,
    pub texture: gdk::Texture,
}

/// Makes `data` an image to send, scaled down and converted when it must be.
pub async fn load(data: Vec<u8>) -> Result<Attached, String> {
    gio::spawn_blocking(move || {
        let image = prepare(&data)?;
        let texture = gdk::Texture::from_bytes(&glib::Bytes::from(&image.data.0))
            .map_err(|_| "this image cannot be shown".to_owned())?;
        Ok(Attached { image, texture })
    })
    .await
    .unwrap_or_else(|_| Err("loading the image failed".to_owned()))
}

/// Reads the image file `file` and makes it an image to send.
pub async fn load_file(file: gio::File) -> Result<Attached, String> {
    let name = file
        .basename()
        .map(|name| name.display().to_string())
        .unwrap_or_else(|| "the file".to_owned());
    let info = file
        .query_info_future(
            gio::FILE_ATTRIBUTE_STANDARD_SIZE,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .map_err(|err| format!("cannot read {name}: {err}"))?;
    if u64::try_from(info.size()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(format!("{name} is too large to attach"));
    }
    let (data, _) = file
        .load_bytes_future()
        .await
        .map_err(|err| format!("cannot read {name}: {err}"))?;
    load(data.to_vec())
        .await
        .map_err(|err| format!("{name}: {err}"))
}

/// Makes a picture an image to send, as the clipboard or a drop gives it.
pub async fn load_texture(texture: &gdk::Texture) -> Result<Attached, String> {
    load(texture.save_to_png_bytes().to_vec()).await
}

/// `data` as an image the daemon takes: as it is when it is one already and small enough,
/// else scaled to fit [`LONG_EDGE`] and [`MAX_IMAGE_BYTES`] as PNG, or as JPEG when opaque.
fn prepare(data: &[u8]) -> Result<Image, String> {
    let not_image = || "this is not an image herder can read".to_owned();
    let loader = PixbufLoader::new();
    loader
        .write(data)
        .and_then(|()| loader.close())
        .map_err(|_| not_image())?;
    let pixbuf = loader.pixbuf().ok_or_else(not_image)?;
    let media_type = loader.format().and_then(|format| {
        format
            .mime_types()
            .into_iter()
            .find(|mime| IMAGE_MEDIA_TYPES.contains(&mime.as_str()))
    });
    let long = pixbuf.width().max(pixbuf.height());
    if let Some(media_type) = media_type
        && data.len() <= MAX_IMAGE_BYTES
        && long <= LONG_EDGE
    {
        return Ok(Image {
            media_type: media_type.into(),
            data: Bytes(data.to_vec()),
        });
    }
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let (kind, media_type, options): (_, _, &[(&str, &str)]) = if pixbuf.has_alpha() {
        ("png", "image/png", &[])
    } else {
        ("jpeg", "image/jpeg", &[("quality", "85")])
    };
    let mut edge = long.min(LONG_EDGE);
    while edge >= 64 {
        let scale = f64::from(edge) / f64::from(long);
        let side = |pixels: i32| ((f64::from(pixels) * scale).round() as i32).max(1);
        let scaled = pixbuf
            .scale_simple(
                side(pixbuf.width()),
                side(pixbuf.height()),
                InterpType::Hyper,
            )
            .ok_or_else(not_image)?;
        let out = scaled
            .save_to_bufferv(kind, options)
            .map_err(|err| format!("cannot convert the image: {err}"))?;
        if out.len() <= MAX_IMAGE_BYTES {
            return Ok(Image {
                media_type: media_type.to_owned(),
                data: Bytes(out),
            });
        }
        edge /= 2;
    }
    Err("this image is too large to send".to_owned())
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

mod imp {
    use super::*;

    /// A picture drawn at a fixed size, cropped to fill it, with rounded corners.
    #[derive(Default)]
    pub struct Thumb {
        pub texture: RefCell<Option<gdk::Texture>>,
        pub size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Thumb {
        const NAME: &'static str = "HerderThumb";
        type Type = super::Thumb;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("thumb");
        }
    }

    impl ObjectImpl for Thumb {}

    impl WidgetImpl for Thumb {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (width, height) = self.size.get();
            let side = if orientation == gtk::Orientation::Horizontal {
                width
            } else {
                height
            };
            (side, side, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width() as f32, widget.height() as f32);
            let bounds = graphene::Rect::new(0.0, 0.0, width, height);
            let clip = gsk::RoundedRect::from_rect(bounds, RADIUS);
            if let Some(texture) = self.texture.borrow().as_ref() {
                let (w, h) = (texture.width() as f32, texture.height() as f32);
                let scale = (width / w).max(height / h);
                let (w, h) = (w * scale, h * scale);
                snapshot.push_rounded_clip(&clip);
                snapshot.append_scaled_texture(
                    texture,
                    gsk::ScalingFilter::Trilinear,
                    &graphene::Rect::new((width - w) / 2.0, (height - h) / 2.0, w, h),
                );
                snapshot.pop();
            }
            // A hairline keeps a picture as light as the page apart from it.
            let mut line = widget.color();
            line.set_alpha(0.12);
            snapshot.append_border(&clip, &[1.0; 4], &[line; 4]);
        }
    }
}

glib::wrapper! {
    /// A picture drawn at a fixed size, cropped to fill it; empty, it is a card while it loads.
    pub struct Thumb(ObjectSubclass<imp::Thumb>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Thumb {
    fn new(texture: Option<&gdk::Texture>, width: i32, height: i32) -> Self {
        let thumb: Self = glib::Object::new();
        thumb.imp().size.set((width, height));
        thumb.imp().texture.replace(texture.cloned());
        thumb
    }

    /// A square thumbnail, as the composer shows what is attached.
    pub fn square(texture: &gdk::Texture, side: i32) -> Self {
        Self::new(Some(texture), side, side)
    }

    /// A prompt's thumbnail: [`MESSAGE_HEIGHT`] tall, as wide as the picture is, within bounds.
    pub fn message(texture: &gdk::Texture) -> Self {
        let aspect = f64::from(texture.width()) / f64::from(texture.height().max(1));
        let width = (f64::from(MESSAGE_HEIGHT) * aspect).round() as i32;
        Self::new(
            Some(texture),
            width.clamp(MESSAGE_HEIGHT * 3 / 4, MESSAGE_WIDTH),
            MESSAGE_HEIGHT,
        )
    }

    /// Whether it shows a picture.
    #[cfg(test)]
    pub fn has_picture(&self) -> bool {
        self.imp().texture.borrow().is_some()
    }
}

/// `thumb` as a button that opens the picture full size over `parent`'s window.
pub fn opener(thumb: &Thumb, texture: &gdk::Texture, bytes: u64) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(thumb)
        .tooltip_text("Open full size")
        .css_classes(["flat", "thumb-button"])
        .build();
    let texture = texture.clone();
    button.connect_clicked(move |button| view(button, &texture, bytes));
    button
}

/// Shows `texture` full size in a dialog that fits the window, scaling it down if it must.
pub fn view(over: &impl IsA<gtk::Widget>, texture: &gdk::Texture, bytes: u64) {
    let (width, height) = (texture.width(), texture.height());
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(
            "Image",
            &format!("{width} × {height} · {}", size(bytes)),
        ))
        .build();
    let picture = gtk::Picture::builder()
        .paintable(texture)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .css_classes(["viewer"])
        .build();
    let view = adw::ToolbarView::new();
    view.add_css_class("viewer-page");
    view.add_top_bar(&header);
    view.set_content(Some(&picture));
    let dialog = adw::Dialog::builder().child(&view).title("Image").build();
    // As large as the picture, within nine tenths of the window; the header's height is
    // libadwaita's.
    if let Some(root) = over.as_ref().root() {
        let room_w = (f64::from(root.width()) * 0.9).max(160.0);
        let room_h = (f64::from(root.height()) * 0.9 - 47.0).max(120.0);
        let scale = (room_w / f64::from(width))
            .min(room_h / f64::from(height))
            .min(1.0);
        dialog.set_content_width((f64::from(width) * scale).round() as i32);
        dialog.set_content_height((f64::from(height) * scale).round() as i32 + 47);
    }
    dialog.present(Some(over.as_ref()));
}

/// Fetches an image of the open session: its bytes, or why not.
pub type Fetch = Rc<dyn Fn(AttachmentId) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>>>;

/// What is known of one of the open session's images.
enum Fetched {
    /// Being fetched; the slots to fill once it is in.
    Loading(Vec<glib::WeakRef<gtk::Box>>),
    Ready(gdk::Texture, u64),
    /// The machine could not give it, and why.
    Missing(String),
}

/// The open session's images, fetched once each and kept while it is open. Cheap to clone.
#[derive(Clone)]
pub struct Gallery {
    fetch: Fetch,
    fetched: Rc<RefCell<HashMap<AttachmentId, Fetched>>>,
}

impl Gallery {
    pub fn new(fetch: Fetch) -> Self {
        Self {
            fetch,
            fetched: Rc::default(),
        }
    }

    /// Forgets every image, for another session.
    pub fn clear(&self) {
        self.fetched.borrow_mut().clear();
    }

    /// A prompt's images, wrapping: each a thumbnail once fetched.
    pub fn row(&self, attachments: &[Attachment]) -> gtk::FlowBox {
        let row = images_row();
        for attachment in attachments {
            row.append(&self.slot(attachment));
        }
        row
    }

    /// A slot for `attachment`: its thumbnail, a card while it loads, or a placeholder.
    fn slot(&self, attachment: &Attachment) -> gtk::Box {
        let slot = hbox(0);
        let mut fetched = self.fetched.borrow_mut();
        match fetched.get_mut(&attachment.attachment_id) {
            Some(Fetched::Ready(texture, bytes)) => fill(&slot, &Ok((texture.clone(), *bytes))),
            Some(Fetched::Missing(why)) => fill(&slot, &Err(why.clone())),
            Some(Fetched::Loading(slots)) => {
                slot.append(&loading());
                slots.push(slot.downgrade());
            }
            None => {
                slot.append(&loading());
                fetched.insert(
                    attachment.attachment_id.clone(),
                    Fetched::Loading(vec![slot.downgrade()]),
                );
                let id = attachment.attachment_id.clone();
                let reply = (self.fetch)(id.clone());
                let gallery = self.clone();
                glib::spawn_future_local(async move {
                    let result = match reply.await {
                        Ok(data) => {
                            let bytes = data.len() as u64;
                            gio::spawn_blocking(move || {
                                gdk::Texture::from_bytes(&glib::Bytes::from_owned(data))
                                    .map(|texture| (texture, bytes))
                                    .map_err(|_| "This image cannot be shown.".to_owned())
                            })
                            .await
                            .unwrap_or_else(|_| Err("This image cannot be shown.".to_owned()))
                        }
                        Err(why) => Err(why),
                    };
                    gallery.arrived(id, result);
                });
            }
        }
        slot
    }

    fn arrived(&self, id: AttachmentId, result: Result<(gdk::Texture, u64), String>) {
        let known = match &result {
            Ok((texture, bytes)) => Fetched::Ready(texture.clone(), *bytes),
            Err(why) => Fetched::Missing(why.clone()),
        };
        let mut fetched = self.fetched.borrow_mut();
        // Gone with a session switched away from.
        let Some(Fetched::Loading(slots)) = fetched.insert(id.clone(), known) else {
            fetched.remove(&id);
            return;
        };
        drop(fetched);
        for slot in slots.iter().filter_map(glib::WeakRef::upgrade) {
            fill(&slot, &result);
        }
    }
}

/// A prompt's thumbnails, side by side, wrapping onto more lines when narrow.
pub fn images_row() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .column_spacing(6)
        .row_spacing(6)
        .max_children_per_line(16)
        .halign(gtk::Align::Start)
        .css_classes(["images"])
        .build()
}

/// Fills `slot` with a fetched image's thumbnail, or the placeholder of one not there.
fn fill(slot: &gtk::Box, result: &Result<(gdk::Texture, u64), String>) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    match result {
        Ok((texture, bytes)) => slot.append(&opener(&Thumb::message(texture), texture, *bytes)),
        Err(why) => slot.append(&missing(why)),
    }
}

/// A card the size of a thumbnail, while the image loads.
fn loading() -> gtk::Widget {
    let thumb = Thumb::new(None, MESSAGE_HEIGHT * 4 / 3, MESSAGE_HEIGHT);
    thumb.add_css_class("loading");
    thumb.upcast()
}

/// What shows for an image the machine does not have: quiet, the reason in its tooltip.
fn missing(why: &str) -> gtk::Widget {
    let card = vbox(6);
    card.add_css_class("image-missing");
    card.set_size_request(MESSAGE_HEIGHT * 4 / 3, MESSAGE_HEIGHT);
    card.set_valign(gtk::Align::Start);
    card.set_tooltip_text(Some(why));
    let icon = gtk::Image::from_icon_name("image-missing-symbolic");
    icon.set_vexpand(true);
    icon.set_valign(gtk::Align::End);
    card.append(&icon);
    let label = gtk::Label::builder()
        .label("Image not available")
        .vexpand(true)
        .valign(gtk::Align::Start)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["caption"])
        .build();
    card.append(&label);
    card.upcast()
}

#[cfg(test)]
pub mod tests {
    use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

    use super::*;

    /// A `width` by `height` PNG of one colour, `0xRRGGBBAA`.
    pub fn png(width: i32, height: i32, colour: u32) -> Vec<u8> {
        let pixbuf = Pixbuf::new(Colorspace::Rgb, false, 8, width, height).expect("a pixbuf");
        pixbuf.fill(colour);
        pixbuf.save_to_bufferv("png", &[]).expect("a PNG")
    }

    #[test]
    fn an_image_small_enough_goes_as_it_is() {
        let data = png(32, 24, 0x7aa2_f7ff);
        let image = prepare(&data).expect("an image");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.data.0, data);
    }

    #[test]
    fn a_large_image_is_scaled_to_the_long_edge() {
        let data = png(LONG_EDGE * 2, 100, 0x2020_20ff);
        let image = prepare(&data).expect("an image");
        // Opaque, so JPEG.
        assert_eq!(image.media_type, "image/jpeg");
        let loader = PixbufLoader::new();
        loader.write(&image.data.0).expect("it decodes");
        loader.close().expect("it decodes");
        let pixbuf = loader.pixbuf().expect("a picture");
        assert_eq!((pixbuf.width(), pixbuf.height()), (LONG_EDGE, 50));
    }

    #[test]
    fn what_is_not_an_image_is_refused() {
        assert!(prepare(b"just text").is_err());
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(340 * 1024), "340 KB");
        assert_eq!(size(1024 * 1024), "1 MB");
        assert_eq!(size(1_258_291), "1.2 MB");
    }
}
