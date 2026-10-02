//! Remote images, fetched only after the user asks for them.

use gpui::ImageFormat;
use std::{io::Read, time::Duration};

const MAX_BYTES: u64 = 20 * 1024 * 1024;

pub fn is_remote(source: &str) -> bool {
    let lower = source.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// Download an image (at most 20 MiB) and identify its format from its bytes.
pub fn fetch_image(url: &str) -> Result<(ImageFormat, Vec<u8>), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("Rusidian/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("无法创建网络请求：{error}"))?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("无法下载图片：{error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES)
    {
        return Err("图片超过 20 MiB，未加载".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取图片：{error}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("图片超过 20 MiB，未加载".into());
    }
    let format = sniff_format(&bytes).ok_or("下载的内容不是支持的图片格式")?;
    Ok((format, bytes))
}

fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(ImageFormat::Tiff)
    } else if bytes.starts_with(&[0, 0, 1, 0]) {
        Some(ImageFormat::Ico)
    } else {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
        (head.contains("<svg")).then_some(ImageFormat::Svg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_remote_sources_and_image_bytes() {
        assert!(is_remote("https://example.com/a.png"));
        assert!(is_remote("HTTP://example.com/a.png"));
        assert!(!is_remote("assets/a.png"));
        assert!(!is_remote("file:///a.png"));
        assert!(matches!(
            sniff_format(b"\x89PNG\r\n\x1a\n...."),
            Some(ImageFormat::Png)
        ));
        assert!(matches!(
            sniff_format(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(ImageFormat::Jpeg)
        ));
        assert!(matches!(
            sniff_format(b"<?xml version=\"1.0\"?><svg xmlns=...>"),
            Some(ImageFormat::Svg)
        ));
        assert!(sniff_format(b"<html>not an image</html>").is_none());
    }
}
