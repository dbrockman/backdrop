//! Formatting of numbers, sizes, and dates for display.

use backdrop_core::image::ImageFormat;
use backdrop_core::providers::ContentRating;
use chrono::{DateTime, Local};

/// Formats `n` with thousands separators and the matching noun, e.g. "1,234 images".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// `n` with thousands separators, e.g. "1,234".
pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A file size in decimal units, e.g. "4.2 MB".
pub fn file_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1000.;
    let mut unit = 0;
    while size >= 999.95 && unit < UNITS.len() - 1 {
        size /= 1000.;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// The display name of an image format.
pub fn image_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::Webp => "WebP",
    }
}

/// How a content rating is written: "SFW", "Sketchy", or "NSFW".
pub fn content_rating(rating: ContentRating) -> &'static str {
    match rating {
        ContentRating::Sfw => "SFW",
        ContentRating::Sketchy => "Sketchy",
        ContentRating::Nsfw => "NSFW",
    }
}

/// "3840 × 2160".
pub fn dimensions(width: u32, height: u32) -> String {
    format!("{} × {}", grouped(width as usize), grouped(height as usize))
}

/// The aspect ratio as small whole numbers when it has them ("16:9", "4:3"),
/// otherwise as a ratio to one ("2.37:1").
pub fn aspect_ratio(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return String::new();
    }
    let divisor = gcd(width, height);
    let (w, h) = match (width / divisor, height / divisor) {
        // Conventionally named 16:10 rather than its reduced form.
        (8, 5) => (16, 10),
        (5, 8) => (10, 16),
        ratio => ratio,
    };
    if w <= 32 && h <= 32 {
        format!("{w}:{h}")
    } else if width >= height {
        format!("{:.2}:1", width as f64 / height as f64)
    } else {
        format!("1:{:.2}", height as f64 / width as f64)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// A local date and time, e.g. "13 Oct 2026, 14:05", from nanoseconds since
/// the Unix epoch.
pub fn date_time(unix_nanos: i64) -> String {
    DateTime::from_timestamp_nanos(unix_nanos)
        .with_timezone(&Local)
        .format("%-d %b %Y, %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_grouped_and_pluralized() {
        assert_eq!(count(0, "image", "images"), "0 images");
        assert_eq!(count(1, "image", "images"), "1 image");
        assert_eq!(count(999, "image", "images"), "999 images");
        assert_eq!(count(1234, "image", "images"), "1,234 images");
        assert_eq!(count(1234567, "image", "images"), "1,234,567 images");
    }

    #[test]
    fn file_sizes_use_decimal_units() {
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(1500), "1.5 KB");
        assert_eq!(file_size(4_200_000), "4.2 MB");
        assert_eq!(file_size(999_960), "1.0 MB");
        assert_eq!(file_size(1_600_000_000), "1.6 GB");
    }

    #[test]
    fn aspect_ratios_reduce_or_fall_back_to_decimals() {
        assert_eq!(aspect_ratio(3840, 2160), "16:9");
        assert_eq!(aspect_ratio(2560, 1600), "16:10");
        assert_eq!(aspect_ratio(1080, 1920), "9:16");
        assert_eq!(aspect_ratio(1024, 1024), "1:1");
        assert_eq!(aspect_ratio(2560, 1080), "2.37:1");
        assert_eq!(aspect_ratio(1080, 2560), "1:2.37");
    }

    #[test]
    fn dimensions_are_grouped() {
        assert_eq!(dimensions(3840, 2160), "3,840 × 2,160");
    }
}
