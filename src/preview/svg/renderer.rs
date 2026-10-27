// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Narrow librsvg C ABI (all functions available in librsvg 2.52 / Ubuntu 24.04).
//! No file/base URI is supplied; external resources cannot be loaded by librsvg.
use super::{PIXEL_LIMIT, Raster, tr};
use glib::translate::from_glib_full;
use gtk::cairo;
use std::ptr;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Length {
    length: f64,
    unit: i32,
}
#[repr(C)]
#[derive(Default)]
struct Rectangle {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
type HandlePtr = *mut glib::gobject_ffi::GObject;
// Link the stable runtime SONAME: no new C headers or librsvg development
// package are required. Ubuntu 24.04 ships librsvg 2.58 with this same ABI.
#[link(name = "librsvg-2.so.2", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn rsvg_handle_new_from_data(
        data: *const u8,
        len: usize,
        error: *mut *mut glib::ffi::GError,
    ) -> HandlePtr;
    fn rsvg_handle_set_dpi(handle: HandlePtr, dpi: f64);
    fn rsvg_handle_get_intrinsic_size_in_pixels(
        handle: HandlePtr,
        width: *mut f64,
        height: *mut f64,
    ) -> i32;
    fn rsvg_handle_get_intrinsic_dimensions(
        handle: HandlePtr,
        has_width: *mut i32,
        width: *mut Length,
        has_height: *mut i32,
        height: *mut Length,
        has_viewbox: *mut i32,
        viewbox: *mut Rectangle,
    );
    fn rsvg_handle_set_stylesheet(
        handle: HandlePtr,
        css: *const u8,
        len: usize,
        error: *mut *mut glib::ffi::GError,
    ) -> i32;
    fn rsvg_handle_render_document(
        handle: HandlePtr,
        cr: *mut cairo::ffi::cairo_t,
        viewport: *const Rectangle,
        error: *mut *mut glib::ffi::GError,
    ) -> i32;
}
struct Handle(HandlePtr);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            glib::gobject_ffi::g_object_unref(self.0);
        }
    }
}
fn error_message(error: *mut glib::ffi::GError) -> String {
    if error.is_null() {
        return tr("Invalid SVG document.");
    }
    // SAFETY: librsvg transfers ownership of this GError to its caller.
    let error: glib::Error = unsafe { from_glib_full(error) };
    error.to_string()
}
fn xml_text(bytes: &[u8]) -> Result<std::borrow::Cow<'_, str>, String> {
    let invalid = || tr("Invalid SVG XML encoding.");
    let utf32 = if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        Some((true, 4))
    } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        Some((false, 4))
    } else if bytes.starts_with(&[b'<', 0, 0, 0]) {
        Some((true, 0))
    } else if bytes.starts_with(&[0, 0, 0, b'<']) {
        Some((false, 0))
    } else {
        None
    };
    if let Some((little, offset)) = utf32 {
        let bytes = &bytes[offset..];
        if bytes.len() % 4 != 0 {
            return Err(invalid());
        }
        let text = bytes
            .chunks_exact(4)
            .map(|chunk| {
                let raw = chunk.try_into().unwrap();
                char::from_u32(if little {
                    u32::from_le_bytes(raw)
                } else {
                    u32::from_be_bytes(raw)
                })
                .ok_or_else(invalid)
            })
            .collect::<Result<String, _>>()?;
        return Ok(text.into());
    }
    let utf16 = if bytes.starts_with(&[0xff, 0xfe]) {
        Some((true, 2))
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some((false, 2))
    } else if bytes.starts_with(&[b'<', 0]) {
        Some((true, 0))
    } else if bytes.starts_with(&[0, b'<']) {
        Some((false, 0))
    } else {
        None
    };
    if let Some((little, offset)) = utf16 {
        let bytes = &bytes[offset..];
        if bytes.len() % 2 != 0 {
            return Err(invalid());
        }
        let units = bytes.chunks_exact(2).map(|chunk| {
            let raw = chunk.try_into().unwrap();
            if little {
                u16::from_le_bytes(raw)
            } else {
                u16::from_be_bytes(raw)
            }
        });
        return char::decode_utf16(units)
            .map(|value| value.map_err(|_| invalid()))
            .collect::<Result<String, _>>()
            .map(Into::into);
    }
    std::str::from_utf8(bytes)
        .map(Into::into)
        .map_err(|_| invalid())
}

fn validate_xml(bytes: &[u8]) -> Result<(), String> {
    let text = xml_text(bytes)?;
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 100_000,
        ..Default::default()
    };
    let document =
        roxmltree::Document::parse_with_options(&text, options).map_err(|e| e.to_string())?;
    let root = document.root_element();
    if root.tag_name().name() != "svg"
        || root.tag_name().namespace() != Some("http://www.w3.org/2000/svg")
    {
        return Err(tr("The document is not an SVG image."));
    }
    for node in document.descendants() {
        if node.ancestors().take(130).count() >= 130 {
            return Err(tr("SVG nesting exceeds the preview limit."));
        }
    }
    for name in ["width", "height"] {
        if let Some(value) = root.attribute(name) {
            let value = value.trim().to_ascii_lowercase();
            let value = value.as_str();
            if value == "auto" {
                continue;
            }
            let numeric = ["px", "em", "ex", "in", "cm", "mm", "pt", "pc", "%"]
                .into_iter()
                .find_map(|unit| value.strip_suffix(unit))
                .unwrap_or(value)
                .trim();
            let number = numeric
                .parse::<f64>()
                .map_err(|_| tr("Invalid SVG dimensions."))?;
            if !number.is_finite() || number <= 0.0 {
                return Err(tr("Invalid SVG dimensions."));
            }
            // librsvg may recover from CSS numeric overflow by using auto/100%.
            // Do not let an enormous explicit length silently become a small SVG.
            if number > f64::from(i32::MAX) {
                return Err(tr("The image exceeds the 40 megapixel preview limit."));
            }
        }
    }
    if let Some(value) = root.attribute("viewBox") {
        let values: Vec<_> = value
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .take(5)
            .map(str::parse::<f64>)
            .collect();
        if values.len() != 4
            || values
                .iter()
                .any(|v| !v.as_ref().is_ok_and(|v| v.is_finite()))
            || values[2].as_ref().is_ok_and(|v| *v <= 0.0)
            || values[3].as_ref().is_ok_and(|v| *v <= 0.0)
        {
            return Err(tr("Invalid SVG viewBox."));
        }
    }
    Ok(())
}
impl Handle {
    fn pixels(&self) -> Option<(f64, f64)> {
        let (mut width, mut height) = (0.0, 0.0);
        // SAFETY: valid handle, properly initialized output storage.
        (unsafe { rsvg_handle_get_intrinsic_size_in_pixels(self.0, &mut width, &mut height) } != 0)
            .then_some((width, height))
    }
    fn stylesheet(&self, value: &[u8]) -> Result<(), String> {
        let mut error = ptr::null_mut();
        if unsafe { rsvg_handle_set_stylesheet(self.0, value.as_ptr(), value.len(), &mut error) }
            == 0
        {
            return Err(error_message(error));
        }
        Ok(())
    }
    fn dimensions(&self) -> Result<(f64, f64), String> {
        if let Some(size) = self.pixels() {
            return Ok(size);
        }
        let (mut width, mut height, mut viewbox, mut has_viewbox) = (
            Length::default(),
            Length::default(),
            Rectangle::default(),
            0,
        );
        unsafe {
            rsvg_handle_get_intrinsic_dimensions(
                self.0,
                ptr::null_mut(),
                &mut width,
                ptr::null_mut(),
                &mut height,
                &mut has_viewbox,
                &mut viewbox,
            );
        }
        // Unit 0 is percent (including SVG2's 'auto' / omitted dimensions).
        // Resolve an absolute axis with librsvg's CSS/font engine by temporarily
        // making the OTHER axis absolute. Restore the stylesheet before rendering.
        let absolute_width = if width.unit != 0 {
            self.stylesheet(b":root { height: 1px !important; }")?;
            self.pixels().map(|size| size.0)
        } else {
            None
        };
        self.stylesheet(b"")?;
        let absolute_height = if height.unit != 0 {
            self.stylesheet(b":root { width: 1px !important; }")?;
            self.pixels().map(|size| size.1)
        } else {
            None
        };
        self.stylesheet(b"")?;
        // Relative SVGs need an application-chosen viewport. Use viewBox as
        // the natural canvas (infer a missing axis from its aspect ratio), or
        // the SVG replaced-element default 300x150 when no viewBox is present.
        let ratio = (has_viewbox != 0).then_some(viewbox.width / viewbox.height);
        let width = absolute_width.unwrap_or_else(|| {
            absolute_height
                .zip(ratio)
                .map(|(h, ratio)| h * ratio)
                .unwrap_or(if has_viewbox != 0 {
                    viewbox.width
                } else {
                    300.0
                })
        });
        let height = absolute_height.unwrap_or_else(|| {
            absolute_width
                .zip(ratio)
                .map(|(w, ratio)| w / ratio)
                .unwrap_or(if has_viewbox != 0 {
                    viewbox.height
                } else {
                    150.0
                })
        });
        Ok((width, height))
    }
}
fn checked_dimensions(width: f64, height: f64) -> Result<(i32, i32), String> {
    let (width, height) = (width.ceil(), height.ceil());
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || width > i32::MAX as f64
        || height > i32::MAX as f64
        || width * height > PIXEL_LIMIT as f64
    {
        return Err(tr("The image exceeds the 40 megapixel preview limit."));
    }
    Ok((width as i32, height as i32))
}
pub(super) fn render(bytes: &[u8], extent: i32) -> Result<Raster, String> {
    validate_xml(bytes)?;
    let mut error = ptr::null_mut();
    let pointer = unsafe { rsvg_handle_new_from_data(bytes.as_ptr(), bytes.len(), &mut error) };
    if pointer.is_null() {
        return Err(error_message(error));
    }
    let handle = Handle(pointer);
    unsafe {
        rsvg_handle_set_dpi(handle.0, 96.0);
    }
    let (width, height) = handle.dimensions()?;
    checked_dimensions(width, height)?; // Before ANY Cairo/Pixbuf pixel allocation.
    if extent < 0 {
        return Err(tr("Invalid preview size."));
    }
    let scale = if extent == 0 {
        1.0
    } else {
        f64::from(extent) / width.max(height)
    };
    let (w, h) = checked_dimensions(width * scale, height * scale)?;
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(|e| e.to_string())?;
    {
        let context = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
        let viewport = Rectangle {
            x: 0.0,
            y: 0.0,
            width: f64::from(w),
            height: f64::from(h),
        };
        if unsafe {
            rsvg_handle_render_document(handle.0, context.to_raw_none(), &viewport, &mut error)
        } == 0
        {
            return Err(error_message(error));
        }
    }
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| e.to_string())?;
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let index = y * stride + x * 4;
            #[cfg(target_endian = "little")]
            let (red, green, blue, alpha) = (
                data[index + 2],
                data[index + 1],
                data[index],
                data[index + 3],
            );
            #[cfg(target_endian = "big")]
            let (red, green, blue, alpha) = (
                data[index + 1],
                data[index + 2],
                data[index + 3],
                data[index],
            );
            let straight = |channel: u8| {
                if alpha == 0 {
                    0
                } else {
                    (u32::from(channel) * 255 / u32::from(alpha)).min(255) as u8
                }
            };
            pixels.extend_from_slice(&[straight(red), straight(green), straight(blue), alpha]);
        }
    }
    Ok(Raster {
        width: w,
        height: h,
        pixels,
        pages: 0,
    })
}
