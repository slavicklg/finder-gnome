// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Shared bounded preview workers; widgets and media streams stay on the GTK thread.
pub(crate) mod svg;
use crate::{files::FileItem, i18n::tr};
use adw::prelude::*;
use gtk::{cairo, gdk, gdk_pixbuf};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
};

const CACHE_LIMIT: usize = 128 * 1024 * 1024;
const TEXT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct Raster {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
    pub pages: i32,
}
impl Raster {
    pub fn texture(&self) -> gdk::Texture {
        gdk::MemoryTexture::new(
            self.width,
            self.height,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(self.pixels.clone()),
            self.width as usize * 4,
        )
        .upcast()
    }
}
#[derive(Clone, Debug)]
pub enum PreviewData {
    Raster(Raster),
    Text(String, bool),
    Unavailable(String),
    Media(std::path::PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    uri: String,
    size: i64,
    modified: (i64, u32),
    extent: i32,
    page: i32,
}
struct Request {
    file: PreviewFile,
    key: Key,
    cancel: gio::Cancellable,
    reply: async_channel::Sender<Arc<PreviewData>>,
}
struct PreviewFile {
    file: gio::File,
    mime: String,
    is_dir: bool,
}
impl From<&FileItem> for PreviewFile {
    fn from(file: &FileItem) -> Self {
        Self {
            file: file.file.clone(),
            mime: file.mime.clone(),
            is_dir: file.is_dir,
        }
    }
}
type Cache = Arc<Mutex<VecDeque<(Key, Arc<PreviewData>)>>>;

#[derive(Clone)]
pub struct PreviewService {
    sender: mpsc::Sender<Request>,
    cache: Cache,
}
pub struct PreviewTicket {
    cancel: gio::Cancellable,
}
impl Drop for PreviewTicket {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl PreviewService {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Request>();
        let receiver = Arc::new(Mutex::new(receiver));
        let cache: Cache = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..2 {
            let receiver = receiver.clone();
            let cache = cache.clone();
            std::thread::Builder::new()
                .name(format!("preview-{index}"))
                .spawn(move || {
                    loop {
                        let request = { receiver.lock().unwrap().recv() };
                        let Ok(request) = request else {
                            break;
                        };
                        if request.cancel.is_cancelled() {
                            continue;
                        }
                        let result = Arc::new(
                            load(
                                &request.file,
                                request.key.extent,
                                request.key.page,
                                &request.cancel,
                            )
                            .unwrap_or_else(PreviewData::Unavailable),
                        );
                        if request.cancel.is_cancelled() {
                            continue;
                        }
                        if let PreviewData::Raster(image) = result.as_ref() {
                            if image.pixels.len() <= CACHE_LIMIT {
                                let mut cache = cache.lock().unwrap();
                                cache.retain(|(key, _)| key != &request.key);
                                cache.push_back((request.key.clone(), result.clone()));
                                while cache
                                    .iter()
                                    .map(|(_, data)| match data.as_ref() {
                                        PreviewData::Raster(r) => r.pixels.len(),
                                        _ => 0,
                                    })
                                    .sum::<usize>()
                                    > CACHE_LIMIT
                                {
                                    cache.pop_front();
                                }
                            }
                        }
                        let _ = request.reply.send_blocking(result);
                    }
                })
                .expect("preview worker");
        }
        Self { sender, cache }
    }

    pub fn request(
        &self,
        file: FileItem,
        extent: i32,
        page: i32,
    ) -> (PreviewTicket, async_channel::Receiver<Arc<PreviewData>>) {
        let cancel = gio::Cancellable::new();
        let (reply, receiver) = async_channel::bounded(1);
        let key = Key {
            uri: file.file.uri().to_string(),
            size: file.size,
            modified: (file.modified, file.modified_usec),
            extent,
            page,
        };
        let cached = {
            let mut cache = self.cache.lock().unwrap();
            cache
                .iter()
                .position(|(k, _)| k == &key)
                .and_then(|index| cache.remove(index))
                .map(|entry| {
                    let result = entry.1.clone();
                    cache.push_back(entry);
                    result
                })
        };
        if let Some(result) = cached {
            let _ = reply.try_send(result);
        } else {
            let _ = self.sender.send(Request {
                file: PreviewFile::from(&file),
                key,
                cancel: cancel.clone(),
                reply,
            });
        }
        (PreviewTicket { cancel }, receiver)
    }
}

pub fn supports_thumbnail(file: &FileItem) -> bool {
    !file.is_dir && (file.mime.starts_with("image/") || file.mime == "application/pdf")
}
pub fn is_media(file: &FileItem) -> bool {
    file.mime.starts_with("audio/") || file.mime.starts_with("video/")
}

fn remote_cache(
    file: &PreviewFile,
    cancel: &gio::Cancellable,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    const LIMIT: usize = 50 * 1024 * 1024;
    let info = file
        .file
        .query_info(
            "standard::size,time::modified,time::modified-usec",
            gio::FileQueryInfoFlags::NONE,
            Some(cancel),
        )
        .map_err(|e| e.to_string())?;
    if info.size() > LIMIT as i64 {
        return Err(tr("Remote previews are limited to 50 MiB per file."));
    }
    let mut checksum = glib::Checksum::new(glib::ChecksumType::Sha256).unwrap();
    checksum.update(
        format!(
            "{}:{}:{}:{}",
            file.file.uri(),
            info.size(),
            info.attribute_uint64("time::modified"),
            info.attribute_uint32("time::modified-usec")
        )
        .as_bytes(),
    );
    #[cfg(not(test))]
    let directory = glib::user_cache_dir().join("ubuntu-finder/previews");
    #[cfg(test)]
    let directory = {
        static CACHE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| tempfile::tempdir().unwrap())
            .path()
            .to_path_buf()
    };
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(checksum.string().unwrap());
    if path.is_file() {
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Ok(path);
    }
    let temporary = path.with_extension(format!(
        "{}.{}.partial",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let stream = file.file.read(Some(cancel)).map_err(|e| e.to_string())?;
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let mut total = 0;
        loop {
            let bytes = stream
                .read_bytes(65536, Some(cancel))
                .map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                break;
            }
            total += bytes.len();
            if total > LIMIT {
                return Err(tr("Remote previews are limited to 50 MiB per file."));
            }
            output.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        let mut entries: Vec<_> = std::fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.metadata().ok().map(|m| {
                    (
                        e.path(),
                        m.len(),
                        m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    )
                })
            })
            .filter(|(p, _, _)| p.extension().is_none())
            .collect();
        entries.sort_by_key(|(_, _, t)| *t);
        let mut size: u64 = entries.iter().map(|(_, s, _)| s).sum();
        for (entry, bytes, _) in entries {
            if size <= 512 * 1024 * 1024 {
                break;
            }
            if entry != path && std::fs::remove_file(entry).is_ok() {
                size -= bytes;
            }
        }
        Ok(path.clone())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

fn load(
    file: &PreviewFile,
    extent: i32,
    page: i32,
    cancel: &gio::Cancellable,
) -> Result<PreviewData, String> {
    if file.is_dir {
        return Err(tr("Folder contents can be opened in the file browser."));
    }
    if !file.file.is_native()
        && !(file.mime.starts_with("text/")
            || matches!(
                file.mime.as_str(),
                "application/json" | "application/xml" | "application/javascript"
            ))
    {
        let path = remote_cache(file, cancel)?;
        if file.mime.starts_with("audio/") || file.mime.starts_with("video/") {
            return Ok(PreviewData::Media(path));
        }
        return load(
            &PreviewFile {
                file: gio::File::for_path(path),
                mime: file.mime.clone(),
                is_dir: file.is_dir,
            },
            extent,
            page,
            cancel,
        );
    }

    if file.mime == "application/pdf" {
        return pdf(file, extent, page, cancel).map(PreviewData::Raster);
    }
    if file.mime.starts_with("image/") {
        let path = file
            .file
            .path()
            .ok_or_else(|| tr("Only local previews are supported."))?;
        if svg::is_svg(&file.file, &file.mime, cancel)? {
            return svg::load(&file.file, extent, cancel).map(PreviewData::Raster);
        }
        let (_, width, height) = gdk_pixbuf::Pixbuf::file_info(&path)
            .ok_or_else(|| tr("The image format is not supported or the file is damaged."))?;
        if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > 40_000_000 {
            return Err(tr("The image exceeds the 40 megapixel preview limit."));
        }
        let stream = file.file.read(Some(cancel)).map_err(|e| e.to_string())?;
        let limit = if extent == 0 {
            width.max(height)
        } else {
            extent
        };
        let pixbuf =
            gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, limit, limit, true, Some(cancel))
                .map_err(|e| e.to_string())?;
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
        if cancel.is_cancelled() {
            return Err(tr("Preview cancelled."));
        }
        let bytes = pixbuf.read_pixel_bytes();
        let width = pixbuf.width();
        let height = pixbuf.height();
        let channels = pixbuf.n_channels() as usize;
        let stride = pixbuf.rowstride() as usize;
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let offset = y * stride + x * channels;
                pixels.extend_from_slice(&bytes[offset..offset + 3]);
                pixels.push(if pixbuf.has_alpha() {
                    bytes[offset + 3]
                } else {
                    255
                });
            }
        }
        return Ok(PreviewData::Raster(Raster {
            width,
            height,
            pixels,
            pages: 0,
        }));
    }
    if file.mime.starts_with("text/")
        || gio::content_type_is_a(&file.mime, "text/plain")
        || matches!(
            file.mime.as_str(),
            "application/json" | "application/xml" | "application/javascript"
        )
    {
        let stream = file.file.read(Some(cancel)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        while bytes.len() <= TEXT_LIMIT {
            let chunk = stream
                .read_bytes((TEXT_LIMIT + 1 - bytes.len()).min(8192), Some(cancel))
                .map_err(|e| e.to_string())?;
            if chunk.is_empty() {
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let truncated = bytes.len() > TEXT_LIMIT;
        bytes.truncate(TEXT_LIMIT);
        if bytes.contains(&0) {
            return Err(tr("This file does not contain plain text."));
        }
        return Ok(PreviewData::Text(
            String::from_utf8_lossy(&bytes).into_owned(),
            truncated,
        ));
    }
    Err(tr("A preview is not available for this file type."))
}

fn pdf(
    file: &PreviewFile,
    extent: i32,
    page: i32,
    cancel: &gio::Cancellable,
) -> Result<Raster, String> {
    let document =
        poppler::Document::from_gfile(&file.file, None, Some(cancel)).map_err(|e| e.to_string())?;
    let pages = document.n_pages();
    let page = document
        .page(page.clamp(0, pages.saturating_sub(1)))
        .ok_or_else(|| tr("The PDF contains no readable pages."))?;
    let (width, height) = page.size();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(tr("Invalid PDF page dimensions."));
    }
    let scale = if extent == 0 {
        96.0 / 72.0
    } else {
        f64::from(extent) / width.max(height)
    };
    let scale = scale
        .min((4_000_000.0 / (width * height)).sqrt())
        .min(4096.0 / width.max(height));
    let w = (width * scale).ceil().max(1.0) as i32;
    let h = (height * scale).ceil().max(1.0) as i32;
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
    {
        let context = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
        context.set_source_rgb(1.0, 1.0, 1.0);
        context.paint().map_err(|e| e.to_string())?;
        context.scale(scale, scale);
        page.render(&context);
    }
    if cancel.is_cancelled() {
        return Err(tr("Preview cancelled."));
    }
    let stride = surface.stride() as usize;
    let bytes = surface.data().map_err(|e| e.to_string())?;
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * stride + x * 4;
            #[cfg(target_endian = "little")]
            pixels.extend_from_slice(&[bytes[i + 2], bytes[i + 1], bytes[i], 255]);
            #[cfg(target_endian = "big")]
            pixels.extend_from_slice(&[bytes[i + 1], bytes[i + 2], bytes[i + 3], 255]);
        }
    }
    Ok(Raster {
        width: w,
        height: h,
        pixels,
        pages,
    })
}

pub struct PreviewPane {
    pub widget: gtk::Box,
    body: gtk::Box,
    name: gtk::Label,
    metadata: gtk::Label,
    page_label: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    zoom: gtk::ToggleButton,
    open_buttons: [gtk::Button; 2],
    file: RefCell<Option<FileItem>>,
    ticket: RefCell<Option<PreviewTicket>>,
    media: RefCell<Option<gtk::MediaFile>>,
    generation: Cell<u64>,
    page: Cell<i32>,
    pages: Cell<i32>,
    remote_enabled: Cell<bool>,
    service: PreviewService,
    window: glib::WeakRef<adw::ApplicationWindow>,
}

impl PreviewPane {
    pub fn new(
        service: PreviewService,
        window: &adw::ApplicationWindow,
        open: impl Fn(FileItem) + 'static,
    ) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.add_css_class("preview-pane");
        let name = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["title-3"])
            .build();
        let metadata = gtk::Label::builder()
            .wrap(true)
            .selectable(true)
            .css_classes(["dim-label"])
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_hexpand(true);
        body.set_vexpand(true);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.set_halign(gtk::Align::Center);
        let previous = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(tr("Previous Page"))
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text(tr("Next Page"))
            .build();
        let page_label = gtk::Label::new(None);
        let zoom = gtk::ToggleButton::with_label("100%");
        zoom.set_tooltip_text(Some(&tr("Actual Size")));
        let open_button = gtk::Button::with_label(&tr("Open"));
        let choose = gtk::Button::with_label(&tr("Open With…"));
        for child in [&previous, &next] {
            controls.append(child);
        }
        controls.append(&page_label);
        controls.append(&zoom);
        controls.append(&open_button);
        controls.append(&choose);
        widget.append(&name);
        widget.append(&body);
        widget.append(&metadata);
        widget.append(&controls);
        let view = Rc::new(Self {
            widget,
            body,
            name,
            metadata,
            page_label,
            previous,
            next,
            zoom,
            open_buttons: [open_button.clone(), choose.clone()],
            file: RefCell::new(None),
            ticket: RefCell::new(None),
            media: RefCell::new(None),
            generation: Cell::new(0),
            page: Cell::new(0),
            pages: Cell::new(0),
            remote_enabled: Cell::new(false),
            service,
            window: window.downgrade(),
        });
        for (button, delta) in [(&view.previous, -1), (&view.next, 1)] {
            let weak = Rc::downgrade(&view);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.change_page(delta);
                }
            });
        }
        let weak = Rc::downgrade(&view);
        view.zoom.connect_toggled(move |_| {
            if let Some(view) = weak.upgrade() {
                view.load_current();
            }
        });
        let weak = Rc::downgrade(&view);
        open_button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                if let Some(file) = view.file.borrow().clone() {
                    open(file);
                }
            }
        });
        let weak = Rc::downgrade(&view);
        choose.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.open_with();
            }
        });
        let weak = Rc::downgrade(&view);
        view.widget.connect_unmap(move |_| {
            if let Some(view) = weak.upgrade() {
                view.stop();
            }
        });
        view
    }

    pub fn show_file(self: &Rc<Self>, file: Option<FileItem>) {
        self.stop();
        self.remote_enabled.set(false);
        self.page.set(0);
        self.pages.set(0);
        *self.file.borrow_mut() = file;
        self.zoom.set_active(false);
        self.load_current();
    }
    pub fn request_preview(self: &Rc<Self>) {
        self.remote_enabled.set(true);
        self.load_current();
    }
    pub fn stop(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.ticket.borrow_mut().take();
        if let Some(media) = self.media.borrow_mut().take() {
            media.pause();
            media.clear();
        }
    }
    pub fn change_page(self: &Rc<Self>, delta: i32) {
        let next = self.page.get() + delta;
        if next >= 0 && next < self.pages.get() {
            self.page.set(next);
            self.load_current();
        }
    }
    #[cfg(test)]
    pub(crate) fn test_ready(&self) -> bool {
        self.body
            .first_child()
            .is_some_and(|w| !w.is::<gtk::Spinner>())
    }
    #[cfg(test)]
    pub(crate) fn test_raster(&self) -> bool {
        fn contains(widget: &gtk::Widget) -> bool {
            if widget.is::<gtk::Picture>() {
                return true;
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if contains(&widget) {
                    return true;
                }
                child = widget.next_sibling();
            }
            false
        }
        contains(self.body.upcast_ref())
    }
    #[cfg(test)]
    pub(crate) fn test_message(&self) -> String {
        self.body
            .first_child()
            .and_then(|w| {
                if let Some(s) = w.downcast_ref::<adw::StatusPage>() {
                    s.description().map(|s| s.to_string())
                } else {
                    w.downcast_ref::<gtk::Button>()
                        .and_then(|b| b.label())
                        .map(|s| s.to_string())
                }
            })
            .unwrap_or_default()
    }
    #[cfg(test)]
    pub(crate) fn test_pages(&self) -> i32 {
        self.pages.get()
    }
    #[cfg(test)]
    pub(crate) fn test_media(&self) -> Option<gtk::MediaFile> {
        self.media.borrow().clone()
    }

    fn clear_body(&self) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
    }
    fn unavailable(&self, message: &str) {
        self.clear_body();
        self.body.append(
            &adw::StatusPage::builder()
                .title(tr("Preview unavailable"))
                .description(message)
                .icon_name("dialog-information-symbolic")
                .build(),
        );
    }
    fn show_media(self: &Rc<Self>, file: &gio::File, mime: &str) {
        let media = gtk::MediaFile::for_file(file);
        media.set_playing(false);
        if mime.starts_with("video/") {
            let video = gtk::Video::for_media_stream(Some(&media));
            video.set_autoplay(false);
            video.set_hexpand(true);
            video.set_vexpand(true);
            self.body.append(&video);
        } else {
            let icon = gtk::Image::from_icon_name("audio-x-generic");
            icon.set_pixel_size(96);
            icon.set_vexpand(true);
            self.body.append(&icon);
            self.body.append(&gtk::MediaControls::new(Some(&media)));
        }
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        media.connect_error_notify(move |media| {
            if let Some(view) = weak.upgrade() {
                if view.generation.get() == generation {
                    if let Some(error) = media.error() {
                        media.pause();
                        view.unavailable(&error.to_string());
                    }
                }
            }
        });
        *self.media.borrow_mut() = Some(media);
    }
    fn load_current(self: &Rc<Self>) {
        self.stop();
        self.clear_body();
        self.previous.set_visible(false);
        self.next.set_visible(false);
        self.page_label.set_visible(false);
        for button in &self.open_buttons {
            button.set_sensitive(self.file.borrow().is_some());
        }
        self.zoom.set_visible(false);
        let Some(file) = self.file.borrow().clone() else {
            self.name.set_label(&tr("Select a file to preview"));
            self.metadata.set_label("");
            return;
        };
        self.name.set_label(&file.name);
        self.metadata.set_label(
            &tr("{kind} · {size} · {modified}")
                .replace("{kind}", &file.kind)
                .replace("{size}", &file.size_text())
                .replace("{modified}", &file.date_text()),
        );
        self.zoom.set_visible(supports_thumbnail(&file));
        if !file.file.is_native() && !self.remote_enabled.get() {
            let button = gtk::Button::with_label(&tr("Preview — download on request"));
            button.set_halign(gtk::Align::Center);
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.request_preview();
                }
            });
            self.body.append(&button);
            return;
        }
        if is_media(&file) && file.file.is_native() {
            self.show_media(&file.file, &file.mime);
            return;
        }
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_vexpand(true);
        self.body.append(&spinner);
        let extent = if self.zoom.is_active() {
            0
        } else {
            1280 * self.widget.scale_factor()
        };
        let (ticket, result) = self.service.request(file, extent, self.page.get());
        *self.ticket.borrow_mut() = Some(ticket);
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::MainContext::default().spawn_local(async move {
            let Ok(result) = result.recv().await else {
                return;
            };
            let Some(view) = weak.upgrade().filter(|v| v.generation.get() == generation) else {
                return;
            };
            view.clear_body();
            match result.as_ref() {
                PreviewData::Raster(image) => {
                    let picture = gtk::Picture::for_paintable(&image.texture());
                    picture.set_can_shrink(!view.zoom.is_active());
                    picture.set_content_fit(gtk::ContentFit::Contain);
                    picture.set_hexpand(true);
                    picture.set_vexpand(true);
                    view.body.append(
                        &gtk::ScrolledWindow::builder()
                            .child(&picture)
                            .hexpand(true)
                            .vexpand(true)
                            .build(),
                    );
                    view.pages.set(image.pages);
                    if image.pages > 0 {
                        view.previous.set_visible(true);
                        view.next.set_visible(true);
                        view.page_label.set_visible(true);
                        view.previous.set_sensitive(view.page.get() > 0);
                        view.next.set_sensitive(view.page.get() + 1 < image.pages);
                        view.page_label.set_label(
                            &tr("Page {page} of {pages}")
                                .replace("{page}", &(view.page.get() + 1).to_string())
                                .replace("{pages}", &image.pages.to_string()),
                        );
                    }
                }
                PreviewData::Text(text, truncated) => {
                    let text_view = gtk::TextView::builder()
                        .editable(false)
                        .cursor_visible(false)
                        .monospace(true)
                        .wrap_mode(gtk::WrapMode::WordChar)
                        .left_margin(12)
                        .right_margin(12)
                        .build();
                    text_view.buffer().set_text(text);
                    view.body.append(
                        &gtk::ScrolledWindow::builder()
                            .child(&text_view)
                            .hexpand(true)
                            .vexpand(true)
                            .build(),
                    );
                    if *truncated {
                        view.body
                            .append(&gtk::Label::new(Some(&tr("Showing the first 256 KiB."))));
                    }
                }
                PreviewData::Media(path) => {
                    let mime = view
                        .file
                        .borrow()
                        .as_ref()
                        .map(|f| f.mime.clone())
                        .unwrap_or_default();
                    view.show_media(&gio::File::for_path(path), &mime);
                }
                PreviewData::Unavailable(error) => view.unavailable(error),
            }
        });
    }

    fn open_with(&self) {
        let (Some(window), Some(file)) = (self.window.upgrade(), self.file.borrow().clone()) else {
            return;
        };
        let dialog = adw::AlertDialog::builder().heading(tr("Open With")).build();
        dialog.add_response("close", &tr("Close"));
        dialog.set_close_response("close");
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        let mut apps = gio::AppInfo::all_for_type(&file.mime);
        if apps.is_empty() {
            apps = gio::AppInfo::all()
                .into_iter()
                .filter(|a| a.should_show())
                .collect();
        }
        apps.sort_by_key(|app| glib::CollationKey::from(app.display_name()));
        for app in apps {
            let button = gtk::Button::with_label(&app.display_name());
            button.add_css_class("flat");
            let file = file.file.clone();
            let weak = window.downgrade();
            let weak_dialog = dialog.downgrade();
            button.connect_clicked(move |_| {
                let Some(window) = weak.upgrade() else {
                    return;
                };
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.force_close();
                }
                let weak = window.downgrade();
                app.launch_uris_async(
                    &[file.uri().as_str()],
                    Some(&gtk::prelude::WidgetExt::display(&window).app_launch_context()),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let (Err(error), Some(window)) = (result, weak.upgrade()) {
                            let dialog = adw::AlertDialog::builder()
                                .heading(tr("Unable to open file"))
                                .body(error.to_string())
                                .build();
                            dialog.add_response("close", &tr("Close"));
                            dialog.set_close_response("close");
                            dialog.present(Some(&window));
                        }
                    },
                );
            });
            list.append(&button);
        }
        dialog.set_extra_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .min_content_height(200)
                .max_content_height(360)
                .propagate_natural_height(true)
                .build(),
        ));
        dialog.present(Some(&window));
    }
}
impl Drop for PreviewPane {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::files::ATTRIBUTES;
    fn info(path: &std::path::Path) -> PreviewFile {
        let file = gio::File::for_path(path);
        let meta = file
            .query_info(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .unwrap();
        PreviewFile::from(&FileItem::from_info(&file.parent().unwrap(), &meta))
    }
    #[test]
    fn text_is_bounded_and_binary_is_not_interpreted() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("large.txt");
        std::fs::write(&file, vec![b'a'; TEXT_LIMIT + 100]).unwrap();
        let result = load(&info(&file), 256, 0, &gio::Cancellable::new()).unwrap();
        assert!(matches!(result, PreviewData::Text(text, true) if text.len() == TEXT_LIMIT));
        std::fs::write(&file, b"a\0b").unwrap();
        assert!(load(&info(&file), 256, 0, &gio::Cancellable::new()).is_err());
    }

    pub(crate) fn pdf_fixture(path: &std::path::Path) {
        let stream = "0.15 0.45 0.9 rg 30 30 180 100 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Contents 6 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut output = "%PDF-1.4\n".to_string();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = output.len();
        output.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()));
        for offset in offsets.iter().skip(1) {
            output.push_str(&format!("{offset:010} 00000 n \n"));
        }
        output.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        ));
        std::fs::write(path, output).unwrap();
    }

    #[test]
    fn png_and_jpeg_keep_the_raster_preview_path() {
        let temp = tempfile::tempdir().unwrap();
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 4).unwrap();
        pixbuf.fill(0xff0000ff);
        for format in ["png", "jpeg"] {
            let path = temp.path().join(format!("image.{format}"));
            pixbuf.savev(&path, format, &[]).unwrap();
            let file = info(&path);
            let cancel = gio::Cancellable::new();
            assert!(!svg::is_svg(&file.file, &file.mime, &cancel).unwrap());
            let PreviewData::Raster(raster) = load(&file, 0, 0, &cancel).unwrap() else {
                panic!("Expected a raster preview for {format}");
            };
            assert_eq!((raster.width, raster.height), (8, 4));
            assert!(
                raster
                    .pixels
                    .chunks_exact(4)
                    .all(|p| { p[0] >= 250 && p[1] <= 5 && p[2] <= 5 && p[3] == 255 })
            );
        }
    }

    #[test]
    fn pdf_pages_and_image_limits() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("document.pdf");
        pdf_fixture(&document);
        let file = info(&document);
        let raster = pdf(&file, 512, 1, &gio::Cancellable::new()).unwrap();
        assert_eq!(raster.pages, 2);
        assert_eq!(
            raster.pixels.len(),
            raster.width as usize * raster.height as usize * 4
        );
        assert!(raster.pixels.len() <= 16 * 1024 * 1024);
        let image = temp.path().join("oversized.svg");
        std::fs::write(
            &image,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100000\" height=\"100000\"></svg>",
        )
        .unwrap();
        assert!(load(&info(&image), 512, 0, &gio::Cancellable::new()).is_err());
    }
    #[test]
    fn remote_cache_size_limit_and_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.bin");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(51 * 1024 * 1024)
            .unwrap();
        let file = info(&path);
        assert!(
            remote_cache(&file, &gio::Cancellable::new())
                .unwrap_err()
                .contains("50 MiB")
        );
        std::fs::write(&path, b"small cached file").unwrap();
        let cached = remote_cache(&info(&path), &gio::Cancellable::new()).unwrap();
        assert_eq!(std::fs::read(&cached).unwrap(), b"small cached file");
        let cancel = gio::Cancellable::new();
        cancel.cancel();
        assert!(remote_cache(&info(&path), &cancel).is_err());
    }
}
