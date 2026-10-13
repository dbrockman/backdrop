//! Small encoded images for tests.

use std::io::Cursor;

use image::{ImageFormat, Rgb, RgbImage};

/// A PNG whose pixels depend on `seed`, so images with different seeds have
/// different contents.
pub(crate) fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    encode(width, height, seed, ImageFormat::Png)
}

pub(crate) fn jpeg(width: u32, height: u32) -> Vec<u8> {
    encode(width, height, 0, ImageFormat::Jpeg)
}

/// A JPEG stored `width` × `height` whose EXIF orientation turns it a quarter
/// turn clockwise, as phone cameras write portrait photos.
pub(crate) fn rotated_jpeg(width: u32, height: u32) -> Vec<u8> {
    let jpeg = jpeg(width, height);
    // APP1 segment: "Exif\0\0", a little-endian TIFF header, and one IFD
    // entry: tag 0x0112 (orientation), type SHORT, count 1, value 6.
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
    exif.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
    exif.extend([0, 0, 0, 0]);
    let length = (exif.len() + 2) as u16;
    let mut bytes = jpeg[..2].to_vec();
    bytes.extend([0xFF, 0xE1]);
    bytes.extend(length.to_be_bytes());
    bytes.extend(exif);
    bytes.extend(&jpeg[2..]);
    bytes
}

fn encode(width: u32, height: u32, seed: u8, format: ImageFormat) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        Rgb([seed, (x % 256) as u8, (y % 256) as u8])
    });
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}
