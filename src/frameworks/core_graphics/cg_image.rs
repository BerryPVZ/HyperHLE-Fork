/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CGImage.h`

use super::cg_color_space::{
    kCGColorSpaceGenericRGB, CGColorSpaceCreateWithName, CGColorSpaceGetModel, CGColorSpaceRef,
};
use super::cg_data_provider::{self, CGDataProviderRef};
use super::CGFloat;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::{CFRelease, CFRetain, CFTypeRef};
use crate::frameworks::foundation::ns_string;
use crate::image::Image;
use crate::mem::{ConstPtr, GuestUSize};
use crate::objc::{autorelease, id, nil, objc_classes, ClassExports, HostObject, ObjC};
use crate::Environment;

pub type CGImageAlphaInfo = u32;
pub const kCGImageAlphaNone: CGImageAlphaInfo = 0;
pub const kCGImageAlphaPremultipliedLast: CGImageAlphaInfo = 1;
pub const kCGImageAlphaPremultipliedFirst: CGImageAlphaInfo = 2;
pub const kCGImageAlphaLast: CGImageAlphaInfo = 3;
pub const kCGImageAlphaFirst: CGImageAlphaInfo = 4;
pub const kCGImageAlphaNoneSkipLast: CGImageAlphaInfo = 5;
pub const kCGImageAlphaNoneSkipFirst: CGImageAlphaInfo = 6;
pub const kCGImageAlphaOnly: CGImageAlphaInfo = 7;

pub type CGImageSourceRef = CFTypeRef;
pub type CGImageSourceStatus = i32;
pub const kCGImageStatusUnexpectedEOF: CGImageSourceStatus = -5;
pub const kCGImageStatusInvalidData: CGImageSourceStatus = -4;
pub const kCGImageStatusUnknownType: CGImageSourceStatus = -3;
pub const kCGImageStatusReadingHeader: CGImageSourceStatus = -2;
pub const kCGImageStatusIncomplete: CGImageSourceStatus = -1;
pub const kCGImageStatusComplete: CGImageSourceStatus = 0;

pub type CGImageByteOrderInfo = u32;
pub const kCGImageByteOrderMask: CGImageByteOrderInfo = 0x7000;
pub const kCGImageByteOrderDefault: CGImageByteOrderInfo = 0 << 12;
#[allow(dead_code)]
pub const kCGImageByteOrder16Little: CGImageByteOrderInfo = 1 << 12;
#[allow(dead_code)]
pub const kCGImageByteOrder32Little: CGImageByteOrderInfo = 2 << 12;
#[allow(dead_code)]
pub const kCGImageByteOrder16Big: CGImageByteOrderInfo = 3 << 12;
pub const kCGImageByteOrder32Big: CGImageByteOrderInfo = 4 << 12;

pub type CGBitmapInfo = u32;
pub const kCGBitmapAlphaInfoMask: CGBitmapInfo = 0x1F;
pub const kCGBitmapByteOrderMask: CGBitmapInfo = kCGImageByteOrderMask;

pub const CLASSES: ClassExports = objc_classes! {
(env, this, _cmd);

@implementation _touchHLE_CGImage: NSObject

- (id)systemUptime {
    nil
}

- (id)tick_audio {
    nil
}

@end
};

#[derive(Default)]
struct CGImageHostObject {
    image: Image,
    /// Whether this image came from a 16-bit RGB bitmap context (5 bits per
    /// component, top bit unused). The pixels are still stored as RGBA8; this
    /// only changes the format the image reports to the app.
    rgb555: bool,
    /// Set for images created by `CGImageMaskCreate`. An image mask is a
    /// single-component coverage bitmap, not colour data, and CoreGraphics
    /// reports a different format for it (`CGImageIsMask` is true,
    /// `CGImageGetColorSpace` is NULL, one component per pixel).
    mask: Option<ImageMask>,
}
impl HostObject for CGImageHostObject {}

/// The packed bitmap an image mask was created from, kept alongside the RGBA
/// pixels so `CGImageGetDataProvider`, `CGImageGetBytesPerRow` and
/// `CGImageCreateWithImageInRect` can work with the samples the app supplied
/// instead of our internal pixel layout.
#[derive(Clone)]
struct ImageMask {
    bits_per_component: u32,
    bits_per_pixel: u32,
    bytes_per_row: u32,
    /// Rows of `bytes_per_row` bytes, samples packed most-significant-bit
    /// first, as CoreGraphics requires.
    samples: Vec<u8>,
}

impl ImageMask {
    /// Sample of the pixel at `x`, `y`, or `None` if it is outside the bitmap.
    fn sample_at(&self, x: usize, y: usize) -> Option<u32> {
        mask_sample_at(
            &self.samples,
            self.bytes_per_row as usize,
            x,
            y,
            self.bits_per_pixel as usize,
            self.bits_per_component as usize,
        )
    }

    /// Build a mask in the same sample format from one sample per pixel.
    fn from_samples(&self, samples: &[u32], width: u32, height: u32) -> ImageMask {
        let (samples, bytes_per_row) = pack_mask_samples(
            samples,
            width as usize,
            height as usize,
            self.bits_per_component as usize,
            self.bits_per_pixel as usize,
        );
        ImageMask {
            bits_per_component: self.bits_per_component,
            bits_per_pixel: self.bits_per_pixel,
            bytes_per_row: bytes_per_row as u32,
            samples,
        }
    }
}

/// Read one sample of a packed, single-component bitmap at `x`, `y`.
///
/// Samples are packed most-significant-bit first and each row starts at a
/// multiple of `bytes_per_row`, like the grey bitmaps CoreGraphics, TIFF and
/// PNG use. Only the first sample of each pixel is read, as an image mask has
/// no use for the others.
fn mask_sample_at(
    src: &[u8],
    bytes_per_row: usize,
    x: usize,
    y: usize,
    bits_per_pixel: usize,
    bits_per_component: usize,
) -> Option<u32> {
    if bits_per_component == 0 || bits_per_component > 8 || bits_per_pixel < bits_per_component {
        return None;
    }
    let bit = x.checked_mul(bits_per_pixel)?;
    let offset_in_byte = bit % 8;
    if offset_in_byte + bits_per_component > 8 {
        // We only decode formats where a sample never straddles two bytes,
        // which is true of every legal mask format.
        return None;
    }
    let byte = *src.get(y.checked_mul(bytes_per_row)? + bit / 8)?;
    Some(
        ((byte >> (8 - bits_per_component - offset_in_byte)) as u32)
            & ((1 << bits_per_component) - 1),
    )
}

/// Pack one sample per pixel into a packed bitmap; the inverse of
/// [mask_sample_at]. Returns the bytes and the row stride they use.
fn pack_mask_samples(
    samples: &[u32],
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
) -> (Vec<u8>, usize) {
    let bytes_per_row = (width * bits_per_pixel).div_ceil(8);
    let mut bitmap = vec![0u8; bytes_per_row * height];
    let max_sample = (1u32 << bits_per_component) - 1;
    for y in 0..height {
        for x in 0..width {
            let sample = samples[y * width + x].min(max_sample);
            let bit = x * bits_per_pixel;
            let shift = 8 - bits_per_component - (bit % 8);
            bitmap[y * bytes_per_row + bit / 8] |= (sample as u8) << shift;
        }
    }
    (bitmap, bytes_per_row)
}

/// Coverage of a mask sample, as the alpha value our RGBA pixels carry.
///
/// Mask samples are an inverse alpha: 0 is painted (fully opaque) and the
/// maximum sample value is not painted at all. The optional decode array
/// CoreGraphics takes remaps the sample's 0.0-1.0 range.
fn mask_alpha(sample: u32, max_sample: u32, decode: (CGFloat, CGFloat)) -> u8 {
    let value = if max_sample == 0 {
        0.0
    } else {
        sample as f32 / max_sample as f32
    };
    let (min, max) = decode;
    let coverage = 1.0 - (min + value * (max - min)).clamp(0.0, 1.0);
    (coverage * 255.0).round().clamp(0.0, 255.0) as u8
}

pub type CGImageRef = CFTypeRef;

pub fn CGImageRelease(env: &mut Environment, c: CGImageRef) {
    if !c.is_null() {
        CFRelease(env, c);
    }
}

pub fn CGImageRetain(env: &mut Environment, c: CGImageRef) -> CGImageRef {
    if !c.is_null() {
        CFRetain(env, c)
    } else {
        c
    }
}

pub fn from_image(env: &mut Environment, image: Image) -> CGImageRef {
    let host_obj = Box::new(CGImageHostObject {
        image,
        ..Default::default()
    });
    let class = env.objc.get_known_class("_touchHLE_CGImage", &mut env.mem);
    env.objc.alloc_object(class, host_obj, &mut env.mem)
}

/// Like [from_image], for an image made from a 16-bit RGB bitmap context.
/// Apps can look at the reported format to pick how to convert the image:
/// DoDonPachi Resurrection only keeps its sprites' transparency if the image
/// says it is 16 bits per pixel.
pub fn from_rgb555_image(env: &mut Environment, image: Image) -> CGImageRef {
    let cg_image = from_image(env, image);
    env.objc.borrow_mut::<CGImageHostObject>(cg_image).rgb555 = true;
    cg_image
}

fn image_mask<'a>(objc: &'a ObjC, image: CGImageRef) -> Option<&'a ImageMask> {
    objc.borrow::<CGImageHostObject>(image).mask.as_ref()
}

fn is_mask(env: &Environment, image: CGImageRef) -> bool {
    env.objc.borrow::<CGImageHostObject>(image).mask.is_some()
}

/// The packed samples of an image mask, for `CGDataProvider`'s benefit.
pub fn borrow_mask_samples<'a>(objc: &'a ObjC, image: CGImageRef) -> Option<&'a [u8]> {
    image_mask(objc, image).map(|mask| mask.samples.as_slice())
}

/// Copy a `CGImage`, keeping every format flag (`rgb555`, image mask and its
/// packed samples). The pixels themselves are shared, like `CGImageCreateCopy`.
fn copy_image(env: &mut Environment, image: CGImageRef) -> CGImageRef {
    let (image_data, rgb555, mask) = {
        let host_obj = env.objc.borrow::<CGImageHostObject>(image);
        (
            host_obj.image.clone(),
            host_obj.rgb555,
            host_obj.mask.clone(),
        )
    };
    let new_host_obj = CGImageHostObject {
        image: image_data,
        rgb555,
        mask,
    };
    let class = env.objc.get_known_class("_touchHLE_CGImage", &mut env.mem);
    env.objc
        .alloc_object(class, Box::new(new_host_obj), &mut env.mem)
}

pub fn borrow_image(objc: &ObjC, image: CGImageRef) -> &Image {
    // ВНИМАНИЕ: Если здесь передан null, эмулятор упадет.
    // Но CoreGraphics функции ниже теперь защищены.
    &objc.borrow::<CGImageHostObject>(image).image
}

pub fn borrow_image_mut(objc: &mut ObjC, image: CGImageRef) -> &mut Image {
    &mut objc.borrow_mut::<CGImageHostObject>(image).image
}

fn CGImageCreateCopyWithColorSpace(
    env: &mut Environment,
    image: CGImageRef,
    color_space: CGColorSpaceRef,
) -> CGImageRef {
    if image.is_null() {
        return nil;
    }

    let image_color_space = CGImageGetColorSpace(env, image);
    if image_color_space.is_null() {
        return nil;
    }

    assert_eq!(
        CGColorSpaceGetModel(env, image_color_space),
        CGColorSpaceGetModel(env, color_space)
    );

    copy_image(env, image)
}

fn CGImageCreateWithPNGDataProvider(
    env: &mut Environment,
    source: CGDataProviderRef,
    decode: ConstPtr<CGFloat>,
    _should_interpolate: bool,
    _intent: i32,
) -> CGImageRef {
    if source.is_null() {
        return nil;
    }
    assert!(decode.is_null());

    let bytes = cg_data_provider::borrow_bytes(env, source);
    let Ok(image) = Image::from_bytes(bytes) else {
        return nil;
    };

    from_image(env, image)
}

fn CGImageCreateWithJPEGDataProvider(
    env: &mut Environment,
    source: CGDataProviderRef,
    decode: ConstPtr<CGFloat>,
    _should_interpolate: bool,
    _intent: i32,
) -> CGImageRef {
    if source.is_null() {
        return nil;
    }
    assert!(decode.is_null());

    let bytes = cg_data_provider::borrow_bytes(env, source);
    let Ok(image) = Image::from_bytes(bytes) else {
        return nil;
    };

    from_image(env, image)
}

fn CGImageGetAlphaInfo(env: &mut Environment, image: CGImageRef) -> CGImageAlphaInfo {
    if image.is_null() {
        return kCGImageAlphaNone;
    }
    let host_obj = env.objc.borrow::<CGImageHostObject>(image);
    // An image mask has a single coverage component: no colour channels and
    // no alpha channel either, the coverage *is* the pixel value.
    if host_obj.mask.is_some() {
        return kCGImageAlphaNone;
    }
    if host_obj.rgb555 {
        return kCGImageAlphaNoneSkipFirst;
    }
    kCGImageAlphaPremultipliedLast
}

fn CGImageGetColorSpace(env: &mut Environment, image: CGImageRef) -> CGColorSpaceRef {
    if image.is_null() {
        return nil;
    }
    // "If the image is an image mask, this function returns NULL."
    if is_mask(env, image) {
        return nil;
    }
    let srgb_name = ns_string::get_static_str(env, kCGColorSpaceGenericRGB);
    CGColorSpaceCreateWithName(env, srgb_name)
}

pub fn CGImageGetWidth(env: &mut Environment, image: CGImageRef) -> GuestUSize {
    if image.is_null() {
        return 0;
    }
    let (width, _height) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    width
}

pub fn CGImageGetHeight(env: &mut Environment, image: CGImageRef) -> GuestUSize {
    if image.is_null() {
        return 0;
    }
    let (_width, height) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    height
}

fn CGImageGetBitsPerPixel(env: &mut Environment, image: CGImageRef) -> GuestUSize {
    if image.is_null() {
        return 0;
    }
    let host_obj = env.objc.borrow::<CGImageHostObject>(image);
    if let Some(mask) = &host_obj.mask {
        return mask.bits_per_pixel;
    }
    if host_obj.rgb555 {
        return 16;
    }
    32
}

fn CGImageGetBytesPerRow(env: &mut Environment, image: CGImageRef) -> GuestUSize {
    if image.is_null() {
        return 0;
    }
    let host_obj = env.objc.borrow::<CGImageHostObject>(image);
    if let Some(mask) = &host_obj.mask {
        return mask.bytes_per_row;
    }
    let (width, _height) = host_obj.image.dimensions();
    width * 4
}

fn CGImageGetDataProvider(env: &mut Environment, image: CGImageRef) -> CGDataProviderRef {
    if image.is_null() {
        return nil;
    }
    let cg_data_provider = cg_data_provider::from_cg_image(env, image);
    autorelease(env, cg_data_provider)
}

fn CGImageGetBitsPerComponent(env: &mut Environment, image: CGImageRef) -> GuestUSize {
    if image.is_null() {
        return 0;
    }
    let host_obj = env.objc.borrow::<CGImageHostObject>(image);
    if let Some(mask) = &host_obj.mask {
        return mask.bits_per_component;
    }
    if host_obj.rgb555 {
        return 5;
    }
    8
}

/// Copy of an existing CGImage — same pixels, same format flags.
fn CGImageCreateCopy(env: &mut Environment, image: CGImageRef) -> CGImageRef {
    if image.is_null() {
        return nil;
    }
    copy_image(env, image)
}

/// Crop to a sub-rectangle. CGRect is in pixels (no coordinate transform here).
fn CGImageCreateWithImageInRect(
    env: &mut Environment,
    image: CGImageRef,
    rect: super::CGRect,
) -> CGImageRef {
    if image.is_null() {
        return nil;
    }

    let (img_w, img_h) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();

    // Clamp rect to image bounds.
    let x = (rect.origin.x as u32).min(img_w);
    let y = (rect.origin.y as u32).min(img_h);
    let width = (rect.size.width as u32).min(img_w.saturating_sub(x));
    let height = (rect.size.height as u32).min(img_h.saturating_sub(y));

    if width == 0 || height == 0 {
        return nil;
    }

    let src_pixels = env.objc.borrow::<CGImageHostObject>(image).image.pixels();

    // Copy the sub-region row by row (RGBA — 4 bytes per pixel).
    let mut dst = vec![0u8; (width * height * 4) as usize];
    for row in 0..height {
        let src_start = ((y + row) * img_w + x) as usize * 4;
        let dst_start = (row * width) as usize * 4;
        dst[dst_start..dst_start + width as usize * 4]
            .copy_from_slice(&src_pixels[src_start..src_start + width as usize * 4]);
    }

    let cropped_mask = image_mask(&env.objc, image).map(|mask| {
        let mut samples = Vec::with_capacity((width * height) as usize);
        for row in 0..height as usize {
            for col in 0..width as usize {
                samples.push(
                    mask.sample_at(x as usize + col, y as usize + row)
                        .unwrap_or(0),
                );
            }
        }
        mask.from_samples(&samples, width, height)
    });

    let cg_image = from_image(env, Image::from_pixels(width, height, dst));
    env.objc.borrow_mut::<CGImageHostObject>(cg_image).mask = cropped_mask;
    cg_image
}

/// Create an image masked by an image mask.
///
/// CoreGraphics requires the second argument to be an image mask (see
/// [CGImageMaskCreate]); its samples are an inverse alpha, so the source
/// image's premultiplied pixels are scaled by the mask's coverage. A mask of
/// a different size is sampled at the source image's pixel positions, as
/// `CGContextDrawImage` would do when drawing both into the same rectangle.
fn CGImageCreateWithMask(env: &mut Environment, image: CGImageRef, mask: CGImageRef) -> CGImageRef {
    if image.is_null() || mask.is_null() {
        return nil;
    }
    if !is_mask(env, mask) {
        // Anything else is a programming error on the app's part; keep the
        // unmasked image rather than guessing at what it wanted.
        log!("CGImageCreateWithMask: mask argument is not an image mask, ignoring it");
        return CGImageCreateCopy(env, image);
    }

    let (width, height) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    let (mask_width, mask_height, mask_pixels) = {
        let host_obj = env.objc.borrow::<CGImageHostObject>(mask);
        let (mask_width, mask_height) = host_obj.image.dimensions();
        (mask_width, mask_height, host_obj.image.pixels().to_vec())
    };
    if width == 0 || height == 0 || mask_width == 0 || mask_height == 0 {
        return nil;
    }

    let mut pixels = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();

    // The mask's pixels already carry the coverage the decode array asked for,
    // so they can be used as-is: a fully opaque mask pixel multiplies by 255,
    // a fully transparent one clears the pixel.
    for y in 0..height as usize {
        let mask_y = y * mask_height as usize / height as usize;
        for x in 0..width as usize {
            let mask_x = x * mask_width as usize / width as usize;
            let coverage = mask_pixels[(mask_y * mask_width as usize + mask_x) * 4 + 3] as u32;
            if coverage == 255 {
                continue;
            }
            let offset = (y * width as usize + x) * 4;
            // The image's pixels are premultiplied, so scaling every channel
            // keeps them consistent.
            for channel in &mut pixels[offset..offset + 4] {
                *channel = ((*channel as u32 * coverage + 127) / 255) as u8;
            }
        }
    }

    from_image(env, Image::from_pixels(width, height, pixels))
}

/// Invert-mask stub — just copy.
fn CGImageCreateMaskWithImageMask(env: &mut Environment, mask_image: CGImageRef) -> CGImageRef {
    log!("CGImageCreateMaskWithImageMask: stubbed");
    CGImageCreateCopy(env, mask_image)
}

// MARK: - Additional accessors

fn CGImageGetBitmapInfo(env: &mut Environment, image: CGImageRef) -> CGBitmapInfo {
    if image.is_null() {
        return 0;
    }
    if is_mask(env, image) {
        // An image mask has a single coverage component and no alpha channel,
        // with the default byte order, which is what CoreGraphics reports.
        return kCGImageAlphaNone | kCGImageByteOrderDefault;
    }
    // Report premultiplied-last RGBA, big-endian 32-bit — matches our Image
    // format.
    kCGImageAlphaPremultipliedLast | kCGImageByteOrder32Big
}

/// Decode array — we always use the default (nil / identity mapping).
fn CGImageGetDecode(_env: &mut Environment, _image: CGImageRef) -> ConstPtr<CGFloat> {
    ConstPtr::null()
}

fn CGImageGetShouldInterpolate(_env: &mut Environment, image: CGImageRef) -> bool {
    if image.is_null() {
        return false;
    }
    true
}

/// CGColorRenderingIntent — 0 = kCGRenderingIntentDefault.
fn CGImageGetRenderingIntent(_env: &mut Environment, _image: CGImageRef) -> i32 {
    0
}

fn CGImageIsMask(env: &mut Environment, image: CGImageRef) -> bool {
    !image.is_null() && is_mask(env, image)
}

/// `CGImageCreate` — create a bitmap image from raw pixel data provided by a
/// `CGDataProvider`.  Only the RGBA/RGBX 8-bit-per-component layout that our
/// `Image` type uses internally is fully supported; other configurations are
/// converted to that format on a best-effort basis so that apps which call this
/// function to compose textures at runtime can proceed.
fn CGImageCreate(
    env: &mut Environment,
    width: GuestUSize,
    height: GuestUSize,
    bits_per_component: GuestUSize,
    bits_per_pixel: GuestUSize,
    bytes_per_row: GuestUSize,
    _space: CGColorSpaceRef,
    bitmap_info: CGBitmapInfo,
    provider: CGDataProviderRef,
    _decode: ConstPtr<CGFloat>,
    _should_interpolate: bool,
    _intent: i32,
) -> CGImageRef {
    if provider.is_null() || width == 0 || height == 0 {
        return nil;
    }

    let src = cg_data_provider::borrow_bytes(env, provider);

    let alpha_info = bitmap_info & kCGBitmapAlphaInfoMask;
    let byte_order = bitmap_info & kCGBitmapByteOrderMask;

    let expected_len = bytes_per_row as usize * height as usize;
    if src.len() < expected_len {
        log!(
            "CGImageCreate: provider has {} bytes but {}×{} at {} bpr needs {}",
            src.len(),
            width,
            height,
            bytes_per_row,
            expected_len,
        );
        return nil;
    }

    let mut dst = vec![0u8; (width * height * 4) as usize];

    let src_components = (bits_per_pixel / bits_per_component) as usize;
    let src_bytes_per_pixel = (bits_per_pixel / 8) as usize;

    for row in 0..height as usize {
        for col in 0..width as usize {
            let si = row * bytes_per_row as usize + col * src_bytes_per_pixel;
            let di = (row * width as usize + col) * 4;

            if bits_per_component == 8 && src_components >= 3 {
                let (r, g, b, a) = match (alpha_info, byte_order) {
                    (kCGImageAlphaPremultipliedFirst, _)
                    | (kCGImageAlphaFirst, _)
                    | (kCGImageAlphaNoneSkipFirst, _) => {
                        let a = if alpha_info == kCGImageAlphaNoneSkipFirst {
                            255
                        } else {
                            src[si]
                        };
                        (src[si + 1], src[si + 2], src[si + 3], a)
                    }
                    (kCGImageAlphaPremultipliedLast, _) | (kCGImageAlphaLast, _) => {
                        (src[si], src[si + 1], src[si + 2], src[si + 3])
                    }
                    (kCGImageAlphaNoneSkipLast, _) => (src[si], src[si + 1], src[si + 2], 255),
                    _ => {
                        if src_components >= 4 {
                            (src[si], src[si + 1], src[si + 2], src[si + 3])
                        } else {
                            (src[si], src[si + 1], src[si + 2], 255)
                        }
                    }
                };
                dst[di] = r;
                dst[di + 1] = g;
                dst[di + 2] = b;
                dst[di + 3] = a;
            } else if bits_per_component == 8 && src_components == 1 {
                let v = src[si];
                dst[di] = v;
                dst[di + 1] = v;
                dst[di + 2] = v;
                dst[di + 3] = 255;
            } else {
                dst[di] = 0;
                dst[di + 1] = 0;
                dst[di + 2] = 0;
                dst[di + 3] = 255;
            }
        }
    }

    let image = Image::from_pixels(width, height, dst);
    from_image(env, image)
}

/// `CGImageMaskCreate` — create an image mask from a one-component bitmap.
///
/// <https://developer.apple.com/documentation/coregraphics/1455142-cgimagemaskcreate>
///
/// "Legal values for bits per component are 1, 2, 4 and 8. Bits per pixel
/// must be either the same as bits per component or 8, with exception of 8
/// bits per component where bits per pixel can be also 16 or 32."
/// (`CoreGraphics/CGImage.h`)
///
/// A mask is a stencil rather than a picture: its samples are an inverse
/// alpha, where 0 paints and the maximum value is transparent, so it is kept
/// as premultiplied RGBA with black colour channels and the coverage in the
/// alpha channel. The packed samples the app supplied are kept as well, so
/// that `CGImageGetDataProvider` and `CGImageGetBytesPerRow` describe the mask
/// the way the app built it.
fn CGImageMaskCreate(
    env: &mut Environment,
    width: GuestUSize,
    height: GuestUSize,
    bits_per_component: GuestUSize,
    bits_per_pixel: GuestUSize,
    bytes_per_row: GuestUSize,
    provider: CGDataProviderRef,
    decode: ConstPtr<CGFloat>,
    _should_interpolate: bool,
) -> CGImageRef {
    if provider.is_null() || width == 0 || height == 0 {
        return nil;
    }

    let bpc = bits_per_component as usize;
    let bpp = bits_per_pixel as usize;
    let bytes_per_row = bytes_per_row as usize;
    if !matches!(bpc, 1 | 2 | 4 | 8) || bpp < bpc || bpp % bpc != 0 {
        log!(
            "CGImageMaskCreate: unsupported format ({} bits per component, {} bits per pixel)",
            bits_per_component,
            bits_per_pixel
        );
        return nil;
    }

    let src = cg_data_provider::borrow_bytes(env, provider).to_vec();
    let row_bytes = (width as usize * bpp).div_ceil(8);
    if bytes_per_row < row_bytes {
        log!(
            "CGImageMaskCreate: {} bytes per row cannot hold {} pixels at {} bits per pixel",
            bytes_per_row,
            width,
            bits_per_pixel
        );
        return nil;
    }
    // The last row is often stored without its padding, so only the bytes the
    // pixels actually occupy have to be there.
    let needed = (height as usize - 1) * bytes_per_row + row_bytes;
    if src.len() < needed {
        log!(
            "CGImageMaskCreate: provider has {} bytes but {}×{} at {} bytes per row needs {}",
            src.len(),
            width,
            height,
            bytes_per_row,
            needed
        );
        return nil;
    }

    // The decode array maps the sample range onto 0.0-1.0; it is normally
    // nil, meaning the identity mapping.
    let decode_range = if decode.is_null() {
        (0.0, 1.0)
    } else {
        let min: CGFloat = env.mem.read(decode);
        let max: CGFloat = env.mem.read(decode + 1);
        (min, max)
    };

    let max_sample = ((1u32 << bpc) - 1) as u32;
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let sample = mask_sample_at(&src, bytes_per_row, x, y, bpp, bpc).unwrap_or(max_sample);
            let offset = (y * width as usize + x) * 4;
            pixels[offset + 3] = mask_alpha(sample, max_sample, decode_range);
        }
    }

    // Keep the padded rows so the stride stays meaningful for the app.
    let mut samples = vec![0u8; bytes_per_row * height as usize];
    let copy_len = needed.min(src.len());
    samples[..copy_len].copy_from_slice(&src[..copy_len]);

    let cg_image = from_image(env, Image::from_pixels(width, height, pixels));
    env.objc.borrow_mut::<CGImageHostObject>(cg_image).mask = Some(ImageMask {
        bits_per_component: bpc as u32,
        bits_per_pixel: bpp as u32,
        bytes_per_row: bytes_per_row as u32,
        samples,
    });
    cg_image
}

// =========================================================================
// MARK: - New creation functions
// =========================================================================

/// `CGImageCreateWithBitmapContext` — not a real public API but some apps
/// call an internal variant. We treat it the same as CGImageCreate with
/// premultiplied-last RGBA.
fn CGImageCreateWithBitmapData(
    env: &mut Environment,
    width: GuestUSize,
    height: GuestUSize,
    data: crate::mem::ConstPtr<u8>,
    bytes_per_row: GuestUSize,
    bitmap_info: CGBitmapInfo,
) -> CGImageRef {
    if data.is_null() || width == 0 || height == 0 {
        return nil;
    }
    let len = bytes_per_row as usize * height as usize;
    let src = env.mem.bytes_at(data, len as u32);
    let mut dst = vec![0u8; (width * height * 4) as usize];
    let alpha_info = bitmap_info & kCGBitmapAlphaInfoMask;
    let bpp = if alpha_info == kCGImageAlphaNone {
        3usize
    } else {
        4
    };
    for row in 0..height as usize {
        for col in 0..width as usize {
            let si = row * bytes_per_row as usize + col * bpp;
            let di = (row * width as usize + col) * 4;
            if bpp == 4 {
                let (r, g, b, a) = match alpha_info {
                    kCGImageAlphaPremultipliedFirst
                    | kCGImageAlphaFirst
                    | kCGImageAlphaNoneSkipFirst => (
                        src[si + 1],
                        src[si + 2],
                        src[si + 3],
                        if alpha_info == kCGImageAlphaNoneSkipFirst {
                            255
                        } else {
                            src[si]
                        },
                    ),
                    _ => (
                        src[si],
                        src[si + 1],
                        src[si + 2],
                        if alpha_info == kCGImageAlphaNoneSkipLast {
                            255
                        } else {
                            src[si + 3]
                        },
                    ),
                };
                dst[di] = r;
                dst[di + 1] = g;
                dst[di + 2] = b;
                dst[di + 3] = a;
            } else {
                dst[di] = src[si];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si + 2];
                dst[di + 3] = 255;
            }
        }
    }
    from_image(env, Image::from_pixels(width, height, dst))
}

/// Flip an image vertically (used by some bitmap context reads).
fn CGImageCreateFlippedVertically(env: &mut Environment, image: CGImageRef) -> CGImageRef {
    if image.is_null() {
        return nil;
    }
    let (w, h) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    let src = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();
    let mut dst = vec![0u8; src.len()];
    for row in 0..h as usize {
        let src_row = (h as usize - 1 - row) * w as usize * 4;
        let dst_row = row * w as usize * 4;
        dst[dst_row..dst_row + w as usize * 4]
            .copy_from_slice(&src[src_row..src_row + w as usize * 4]);
    }
    from_image(env, Image::from_pixels(w, h, dst))
}

/// Scale an image to a new size using nearest-neighbour sampling.
fn CGImageCreateScaled(
    env: &mut Environment,
    image: CGImageRef,
    new_width: GuestUSize,
    new_height: GuestUSize,
) -> CGImageRef {
    if image.is_null() || new_width == 0 || new_height == 0 {
        return nil;
    }
    let (src_w, src_h) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    let src = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();
    let mut dst = vec![0u8; (new_width * new_height * 4) as usize];
    for dy in 0..new_height as usize {
        let sy = (dy * src_h as usize) / new_height as usize;
        for dx in 0..new_width as usize {
            let sx = (dx * src_w as usize) / new_width as usize;
            let si = (sy * src_w as usize + sx) * 4;
            let di = (dy * new_width as usize + dx) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    from_image(env, Image::from_pixels(new_width, new_height, dst))
}

// =========================================================================
// MARK: - New accessor functions
// =========================================================================

/// `CFTypeID CGImageGetTypeID()` — returns a stable non-zero type ID.
fn CGImageGetTypeID(_env: &mut Environment) -> u32 {
    // Any stable non-zero value is fine — apps use this for type checking.
    0x0000_BABE
}

/// `CGImageRef CGImageCreateWithImageInRect` — already exists, this is the
/// `CGImageGetUTType` variant that returns the UTI string.
fn CGImageGetUTType(_env: &mut Environment, _image: CGImageRef) -> crate::objc::id {
    // Return nil — we don't track the original file type.
    nil
}

fn CGImageGetPixelFormatInfo(_env: &mut Environment, image: CGImageRef) -> u32 {
    if image.is_null() {
        return 0;
    }
    0 // kCGImagePixelFormatPacked
}

/// Read raw RGBA pixels from a CGImage into a guest buffer.
/// `out_data` must be at least `width * height * 4` bytes.
fn CGImageReadPixels(env: &mut Environment, image: CGImageRef, out_data: crate::mem::MutPtr<u8>) {
    if image.is_null() || out_data.is_null() {
        return;
    }
    let pixels = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();
    let dst = env.mem.bytes_at_mut(out_data, pixels.len() as u32);
    dst.copy_from_slice(&pixels);
}

/// Premultiply alpha on a copy of the image.
fn CGImageCreatePremultiplied(env: &mut Environment, image: CGImageRef) -> CGImageRef {
    if image.is_null() {
        return nil;
    }
    let (w, h) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    let mut pixels = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
        px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
        px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
    }
    from_image(env, Image::from_pixels(w, h, pixels))
}

/// Un-premultiply alpha on a copy of the image.
fn CGImageCreateUnpremultiplied(env: &mut Environment, image: CGImageRef) -> CGImageRef {
    if image.is_null() {
        return nil;
    }
    let (w, h) = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .dimensions();
    let mut pixels = env
        .objc
        .borrow::<CGImageHostObject>(image)
        .image
        .pixels()
        .to_vec();
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 {
            px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    from_image(env, Image::from_pixels(w, h, pixels))
}

fn CGImageSourceCreateWithData(
    env: &mut Environment,
    data: CFTypeRef,
    _options: CFTypeRef,
) -> CGImageSourceRef {
    // Try to decode the data as an image and wrap it in a CGImageSource-like
    // object. We reuse CGImageRef as the source ref since we only support
    // single-frame sources.
    if data.is_null() {
        return nil;
    }
    // data is an NSData* / CFDataRef — get its bytes.
    let bytes_ptr: crate::mem::ConstPtr<u8> = crate::objc::msg![env; data bytes];
    let length: crate::frameworks::foundation::NSUInteger = crate::objc::msg![env; data length];
    if bytes_ptr.is_null() || length == 0 {
        return nil;
    }
    let bytes = env.mem.bytes_at(bytes_ptr, length);
    match crate::image::Image::from_bytes(bytes) {
        Ok(image) => {
            let img_ref = from_image(env, image);
            // Retain to match "Create" rule.
            crate::frameworks::core_foundation::CFRetain(env, img_ref)
        }
        Err(_) => {
            log_dbg!("CGImageSourceCreateWithData: failed to decode image");
            nil
        }
    }
}

fn CGImageSourceCreateWithURL(
    env: &mut Environment,
    url: CFTypeRef,
    _options: CFTypeRef,
) -> CGImageSourceRef {
    // Delegate to NSData loading then create from data.
    if url.is_null() {
        return nil;
    }
    let data: crate::objc::id = crate::objc::msg_class![env; NSData dataWithContentsOfURL:url];
    if data == nil {
        return nil;
    }
    CGImageSourceCreateWithData(env, data, nil)
}

fn CGImageSourceGetCount(_env: &mut Environment, source: CGImageSourceRef) -> u32 {
    if source.is_null() {
        0
    } else {
        1
    }
}

fn CGImageSourceGetStatus(_env: &mut Environment, source: CGImageSourceRef) -> CGImageSourceStatus {
    if source.is_null() {
        kCGImageStatusInvalidData
    } else {
        kCGImageStatusComplete
    }
}

fn CGImageSourceGetStatusAtIndex(
    _env: &mut Environment,
    source: CGImageSourceRef,
    _index: u32,
) -> CGImageSourceStatus {
    if source.is_null() {
        kCGImageStatusInvalidData
    } else {
        kCGImageStatusComplete
    }
}

fn CGImageSourceCreateImageAtIndex(
    env: &mut Environment,
    source: CGImageSourceRef,
    index: u32,
    _options: CFTypeRef,
) -> CGImageRef {
    if source.is_null() || index != 0 {
        return nil;
    }
    // source IS the CGImageRef (see CGImageSourceCreateWithData).
    crate::frameworks::core_foundation::CFRetain(env, source)
}

fn CGImageSourceCopyPropertiesAtIndex(
    _env: &mut Environment,
    _source: CGImageSourceRef,
    _index: u32,
    _options: CFTypeRef,
) -> CFTypeRef {
    // Return nil — we don't track image metadata.
    nil
}

fn CGImageSourceCopyProperties(
    _env: &mut Environment,
    _source: CGImageSourceRef,
    _options: CFTypeRef,
) -> CFTypeRef {
    nil
}

fn CGImageSourceGetTypeID(_env: &mut Environment) -> u32 {
    0x0000_CAFE
}

fn CGImageSourceCreateThumbnailAtIndex(
    env: &mut Environment,
    source: CGImageSourceRef,
    index: u32,
    _options: CFTypeRef,
) -> CGImageRef {
    // Return the full image as the thumbnail — no downscaling.
    CGImageSourceCreateImageAtIndex(env, source, index, nil)
}

pub const FUNCTIONS: FunctionExports = &[
    // Existing entries ...
    export_c_func!(CGImageCreate(_, _, _, _, _, _, _, _, _, _, _)),
    export_c_func!(CGImageRelease(_)),
    export_c_func!(CGImageRetain(_)),
    export_c_func!(CGImageCreateCopyWithColorSpace(_, _)),
    export_c_func!(CGImageCreateWithPNGDataProvider(_, _, _, _)),
    export_c_func!(CGImageCreateWithJPEGDataProvider(_, _, _, _)),
    export_c_func!(CGImageCreateWithImageInRect(_, _)),
    export_c_func!(CGImageMaskCreate(_, _, _, _, _, _, _, _)),
    export_c_func!(CGImageCreateWithMask(_, _)),
    export_c_func!(CGImageCreateMaskWithImageMask(_)),
    export_c_func!(CGImageCreateCopy(_)),
    export_c_func!(CGImageGetAlphaInfo(_)),
    export_c_func!(CGImageGetBitmapInfo(_)),
    export_c_func!(CGImageGetColorSpace(_)),
    export_c_func!(CGImageGetWidth(_)),
    export_c_func!(CGImageGetHeight(_)),
    export_c_func!(CGImageGetBitsPerPixel(_)),
    export_c_func!(CGImageGetBitsPerComponent(_)),
    export_c_func!(CGImageGetBytesPerRow(_)),
    export_c_func!(CGImageGetDataProvider(_)),
    export_c_func!(CGImageGetDecode(_)),
    export_c_func!(CGImageGetShouldInterpolate(_)),
    export_c_func!(CGImageGetRenderingIntent(_)),
    export_c_func!(CGImageIsMask(_)),
    // New entries
    export_c_func!(CGImageGetTypeID()),
    export_c_func!(CGImageGetUTType(_)),
    export_c_func!(CGImageGetPixelFormatInfo(_)),
    export_c_func!(CGImageCreateWithBitmapData(_, _, _, _, _)),
    export_c_func!(CGImageCreateFlippedVertically(_)),
    export_c_func!(CGImageCreateScaled(_, _, _)),
    export_c_func!(CGImageReadPixels(_, _)),
    export_c_func!(CGImageCreatePremultiplied(_)),
    export_c_func!(CGImageCreateUnpremultiplied(_)),
    // CGImageSource (ImageIO.framework)
    export_c_func!(CGImageSourceGetTypeID()),
    export_c_func!(CGImageSourceCreateWithData(_, _)),
    export_c_func!(CGImageSourceCreateWithURL(_, _)),
    export_c_func!(CGImageSourceGetCount(_)),
    export_c_func!(CGImageSourceGetStatus(_)),
    export_c_func!(CGImageSourceGetStatusAtIndex(_, _)),
    export_c_func!(CGImageSourceCreateImageAtIndex(_, _, _)),
    export_c_func!(CGImageSourceCopyPropertiesAtIndex(_, _, _)),
    export_c_func!(CGImageSourceCopyProperties(_, _)),
    export_c_func!(CGImageSourceCreateThumbnailAtIndex(_, _, _)),
];

/// `CGImageProperty*` / `kCGImageDestination*` / `kCGImageSource*` are
/// `extern const CFStringRef` keys published by `ImageIO.framework`.
/// touchHLE has no live ImageIO pipeline, but apps embed references to
/// these symbols in their non-lazy symbol pointer table — usually to
/// read EXIF / GIF / PNG / JPEG metadata back from images they decoded
/// elsewhere — so we publish them as CFString placeholders matching
/// Apple's literal keys. With these in place dictionary-key identity
/// comparisons (e.g. `dict[(__bridge id)kCGImagePropertyOrientation]`)
/// continue to work, even if the dictionary itself is empty.
/// <https://developer.apple.com/documentation/imageio/image_properties>
pub const CONSTANTS: ConstantExports = &[
    (
        "_kCGImagePropertyPixelWidth",
        HostConstant::NSString("PixelWidth"),
    ),
    (
        "_kCGImagePropertyPixelHeight",
        HostConstant::NSString("PixelHeight"),
    ),
    (
        "_kCGImagePropertyOrientation",
        HostConstant::NSString("Orientation"),
    ),
    (
        "_kCGImagePropertyDPIWidth",
        HostConstant::NSString("DPIWidth"),
    ),
    (
        "_kCGImagePropertyDPIHeight",
        HostConstant::NSString("DPIHeight"),
    ),
    ("_kCGImagePropertyDepth", HostConstant::NSString("Depth")),
    (
        "_kCGImagePropertyColorModel",
        HostConstant::NSString("ColorModel"),
    ),
    (
        "_kCGImagePropertyHasAlpha",
        HostConstant::NSString("HasAlpha"),
    ),
    (
        "_kCGImagePropertyIsFloat",
        HostConstant::NSString("IsFloat"),
    ),
    (
        "_kCGImagePropertyIsIndexed",
        HostConstant::NSString("IsIndexed"),
    ),
    (
        "_kCGImagePropertyProfileName",
        HostConstant::NSString("ProfileName"),
    ),
    (
        "_kCGImagePropertyFileSize",
        HostConstant::NSString("FileSize"),
    ),
    // Format-specific subdictionary keys.
    (
        "_kCGImagePropertyJFIFDictionary",
        HostConstant::NSString("{JFIF}"),
    ),
    (
        "_kCGImagePropertyJFIFIsProgressive",
        HostConstant::NSString("IsProgressive"),
    ),
    (
        "_kCGImagePropertyJFIFVersion",
        HostConstant::NSString("JFIFVersion"),
    ),
    (
        "_kCGImagePropertyJFIFXDensity",
        HostConstant::NSString("XDensity"),
    ),
    (
        "_kCGImagePropertyJFIFYDensity",
        HostConstant::NSString("YDensity"),
    ),
    (
        "_kCGImagePropertyJFIFDensityUnit",
        HostConstant::NSString("DensityUnit"),
    ),
    (
        "_kCGImagePropertyGIFDictionary",
        HostConstant::NSString("{GIF}"),
    ),
    (
        "_kCGImagePropertyGIFLoopCount",
        HostConstant::NSString("LoopCount"),
    ),
    (
        "_kCGImagePropertyGIFDelayTime",
        HostConstant::NSString("DelayTime"),
    ),
    (
        "_kCGImagePropertyGIFUnclampedDelayTime",
        HostConstant::NSString("UnclampedDelayTime"),
    ),
    (
        "_kCGImagePropertyGIFHasGlobalColorMap",
        HostConstant::NSString("HasGlobalColorMap"),
    ),
    (
        "_kCGImagePropertyGIFImageColorMap",
        HostConstant::NSString("ImageColorMap"),
    ),
    (
        "_kCGImagePropertyPNGDictionary",
        HostConstant::NSString("{PNG}"),
    ),
    (
        "_kCGImagePropertyPNGInterlaceType",
        HostConstant::NSString("InterlaceType"),
    ),
    ("_kCGImagePropertyPNGGamma", HostConstant::NSString("Gamma")),
    (
        "_kCGImagePropertyPNGsRGBIntent",
        HostConstant::NSString("sRGBIntent"),
    ),
    (
        "_kCGImagePropertyPNGChromaticities",
        HostConstant::NSString("Chromaticities"),
    ),
    // Parent metadata dictionaries. Apple documents these as
    // `extern const CFStringRef` keys whose string values match
    // dictionary names embedded in image containers (EXIF, TIFF, etc.).
    // <https://developer.apple.com/documentation/imageio/image_properties>
    (
        "_kCGImagePropertyExifDictionary",
        HostConstant::NSString("{Exif}"),
    ),
    (
        "_kCGImagePropertyTIFFDictionary",
        HostConstant::NSString("{TIFF}"),
    ),
    (
        "_kCGImagePropertyIPTCDictionary",
        HostConstant::NSString("{IPTC}"),
    ),
    (
        "_kCGImagePropertyGPSDictionary",
        HostConstant::NSString("{GPS}"),
    ),
    (
        "_kCGImagePropertyRawDictionary",
        HostConstant::NSString("{RAW}"),
    ),
    (
        "_kCGImagePropertyCIFFDictionary",
        HostConstant::NSString("{CIFF}"),
    ),
    (
        "_kCGImagePropertyExifAuxDictionary",
        HostConstant::NSString("{ExifAux}"),
    ),
    (
        "_kCGImagePropertyMakerCanonDictionary",
        HostConstant::NSString("{MakerCanon}"),
    ),
    (
        "_kCGImagePropertyMakerNikonDictionary",
        HostConstant::NSString("{MakerNikon}"),
    ),
    (
        "_kCGImagePropertyMakerMinoltaDictionary",
        HostConstant::NSString("{MakerMinolta}"),
    ),
    (
        "_kCGImagePropertyMakerFujiDictionary",
        HostConstant::NSString("{MakerFuji}"),
    ),
    (
        "_kCGImagePropertyMakerOlympusDictionary",
        HostConstant::NSString("{MakerOlympus}"),
    ),
    (
        "_kCGImagePropertyMakerPentaxDictionary",
        HostConstant::NSString("{MakerPentax}"),
    ),
    (
        "_kCGImageProperty8BIMDictionary",
        HostConstant::NSString("{8BIM}"),
    ),
    (
        "_kCGImagePropertyDNGDictionary",
        HostConstant::NSString("{DNG}"),
    ),
    // Common EXIF subkeys. These are accessed via the {Exif}
    // sub-dictionary; we expose them as literal CFString constants so
    // guest comparisons work whether or not the dictionary is populated.
    (
        "_kCGImagePropertyExifExposureTime",
        HostConstant::NSString("ExposureTime"),
    ),
    (
        "_kCGImagePropertyExifApertureValue",
        HostConstant::NSString("ApertureValue"),
    ),
    (
        "_kCGImagePropertyExifShutterSpeedValue",
        HostConstant::NSString("ShutterSpeedValue"),
    ),
    (
        "_kCGImagePropertyExifFNumber",
        HostConstant::NSString("FNumber"),
    ),
    (
        "_kCGImagePropertyExifISOSpeedRatings",
        HostConstant::NSString("ISOSpeedRatings"),
    ),
    (
        "_kCGImagePropertyExifDateTimeOriginal",
        HostConstant::NSString("DateTimeOriginal"),
    ),
    (
        "_kCGImagePropertyExifDateTimeDigitized",
        HostConstant::NSString("DateTimeDigitized"),
    ),
    (
        "_kCGImagePropertyExifPixelXDimension",
        HostConstant::NSString("PixelXDimension"),
    ),
    (
        "_kCGImagePropertyExifPixelYDimension",
        HostConstant::NSString("PixelYDimension"),
    ),
    (
        "_kCGImagePropertyExifFocalLength",
        HostConstant::NSString("FocalLength"),
    ),
    (
        "_kCGImagePropertyExifFlash",
        HostConstant::NSString("Flash"),
    ),
    (
        "_kCGImagePropertyExifWhiteBalance",
        HostConstant::NSString("WhiteBalance"),
    ),
    (
        "_kCGImagePropertyExifColorSpace",
        HostConstant::NSString("ColorSpace"),
    ),
    (
        "_kCGImagePropertyExifSubjectArea",
        HostConstant::NSString("SubjectArea"),
    ),
    (
        "_kCGImagePropertyExifSubjectDistance",
        HostConstant::NSString("SubjectDistance"),
    ),
    (
        "_kCGImagePropertyExifFlashEnergy",
        HostConstant::NSString("FlashEnergy"),
    ),
    (
        "_kCGImagePropertyExifUserComment",
        HostConstant::NSString("UserComment"),
    ),
    (
        "_kCGImagePropertyExifBodySerialNumber",
        HostConstant::NSString("BodySerialNumber"),
    ),
    (
        "_kCGImagePropertyExifCameraOwnerName",
        HostConstant::NSString("CameraOwnerName"),
    ),
    (
        "_kCGImagePropertyExifLensModel",
        HostConstant::NSString("LensModel"),
    ),
    (
        "_kCGImagePropertyExifLensMake",
        HostConstant::NSString("LensMake"),
    ),
    (
        "_kCGImagePropertyExifLensSerialNumber",
        HostConstant::NSString("LensSerialNumber"),
    ),
    // ImageDestination options.
    (
        "_kCGImageDestinationLossyCompressionQuality",
        HostConstant::NSString("kCGImageDestinationLossyCompressionQuality"),
    ),
    (
        "_kCGImageDestinationBackgroundColor",
        HostConstant::NSString("kCGImageDestinationBackgroundColor"),
    ),
    // ImageSource options.
    (
        "_kCGImageSourceShouldCache",
        HostConstant::NSString("kCGImageSourceShouldCache"),
    ),
    (
        "_kCGImageSourceShouldAllowFloat",
        HostConstant::NSString("kCGImageSourceShouldAllowFloat"),
    ),
    (
        "_kCGImageSourceCreateThumbnailFromImageIfAbsent",
        HostConstant::NSString("kCGImageSourceCreateThumbnailFromImageIfAbsent"),
    ),
    (
        "_kCGImageSourceCreateThumbnailFromImageAlways",
        HostConstant::NSString("kCGImageSourceCreateThumbnailFromImageAlways"),
    ),
    (
        "_kCGImageSourceThumbnailMaxPixelSize",
        HostConstant::NSString("kCGImageSourceThumbnailMaxPixelSize"),
    ),
    (
        "_kCGImageSourceCreateThumbnailWithTransform",
        HostConstant::NSString("kCGImageSourceCreateThumbnailWithTransform"),
    ),
    (
        "_kCGImageSourceTypeIdentifierHint",
        HostConstant::NSString("kCGImageSourceTypeIdentifierHint"),
    ),
];

#[cfg(test)]
mod mask_tests {
    use super::*;

    #[test]
    fn one_bit_mask_rows_are_msb_first_and_padded_to_the_stride() {
        // Two rows of eight 1-bit pixels, one byte per row.
        let bitmap = [0b1000_0001, 0b0111_1110];
        let row = |y| {
            (0..8)
                .map(|x| mask_sample_at(&bitmap, 1, x, y, 1, 1).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(row(0), vec![1, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(row(1), vec![0, 1, 1, 1, 1, 1, 1, 0]);
        // Reading past the end is not a panic.
        assert_eq!(mask_sample_at(&bitmap, 1, 0, 2, 1, 1), None);
    }

    #[test]
    fn four_bit_mask_samples_do_not_straddle_bytes() {
        // Four 4-bit pixels per byte, then a row pad of two bytes.
        let bitmap = [0x12, 0x34, 0xff, 0xff, 0x56, 0x78, 0xff, 0xff];
        let samples = (0..4)
            .map(|x| mask_sample_at(&bitmap, 4, x, 1, 4, 4).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(samples, vec![5, 6, 7, 8]);
        assert_eq!(mask_sample_at(&bitmap, 4, 0, 0, 4, 4), Some(1));
    }

    #[test]
    fn packed_samples_round_trip() {
        for bits_per_component in [1usize, 2, 4, 8] {
            let (width, height) = (7usize, 3usize);
            let max_sample = (1u32 << bits_per_component) - 1;
            let samples = (0..width * height)
                .map(|i| i as u32 & max_sample)
                .collect::<Vec<_>>();
            let (bitmap, bytes_per_row) = pack_mask_samples(
                &samples,
                width,
                height,
                bits_per_component,
                bits_per_component,
            );
            assert_eq!(bitmap.len(), bytes_per_row * height);
            let mut read_back = Vec::with_capacity(samples.len());
            for y in 0..height {
                for x in 0..width {
                    let sample = mask_sample_at(
                        &bitmap,
                        bytes_per_row,
                        x,
                        y,
                        bits_per_component,
                        bits_per_component,
                    )
                    .unwrap();
                    read_back.push(sample);
                }
            }
            assert_eq!(read_back, samples);
        }
    }

    #[test]
    fn mask_samples_are_an_inverse_alpha() {
        // A sample of zero paints, the maximum sample value is transparent.
        assert_eq!(mask_alpha(0, 255, (0.0, 1.0)), 255);
        assert_eq!(mask_alpha(255, 255, (0.0, 1.0)), 0);
        assert_eq!(mask_alpha(1, 1, (0.0, 1.0)), 0);
        assert_eq!(mask_alpha(1, 2, (0.0, 1.0)), 128);
        // An inverted decode array swaps the two ends.
        assert_eq!(mask_alpha(0, 255, (1.0, 0.0)), 0);
        assert_eq!(mask_alpha(255, 255, (1.0, 0.0)), 255);
    }
}
