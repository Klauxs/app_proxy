//! Optional provider display metadata. Never derive names from URL paths or tokens.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use reqwest::{Url, header::HeaderMap};

fn safe(value: &str, source: &Url) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 160
        || value.chars().any(|c| {
            c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        || value.contains(['/', '\\', '?', '&', '=', '@', ':'])
        || source
            .query_pairs()
            .any(|(_, token)| !token.is_empty() && value.contains(token.as_ref()))
        || source
            .path_segments()
            .into_iter()
            .flatten()
            .any(|part| part.len() >= 16 && value.contains(part))
    {
        return None;
    }
    let value = [".yaml", ".yml", ".json", ".txt"]
        .iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value)
        .trim();
    (!value.is_empty()).then(|| value.chars().take(40).collect())
}

pub(super) fn from_headers(headers: &HeaderMap, source: &Url) -> Option<String> {
    if let Some(raw) = headers
        .get("profile-title")
        .filter(|v| v.as_bytes().len() <= 1024)
        .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
    {
        let decoded = if let Some(encoded) = raw.strip_prefix("base64:") {
            STANDARD
                .decode(encoded)
                .or_else(|_| STANDARD_NO_PAD.decode(encoded))
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
        } else {
            Some(raw.to_owned())
        };
        if let Some(title) = decoded.and_then(|v| safe(&v, source)) {
            return Some(title);
        }
    }
    let disposition = headers.get("content-disposition")?.to_str().ok()?;
    if disposition.len() > 2048 {
        return None;
    }
    // Prefer the UTF-8 filename parameter when both forms are supplied.
    for key in ["filename*", "filename"] {
        for field in disposition.split(';').skip(1) {
            let Some((name, value)) = field.trim().split_once('=') else {
                continue;
            };
            if !name.eq_ignore_ascii_case(key) {
                continue;
            }
            let value = value.trim().trim_matches('"');
            let decoded = if key == "filename*" {
                let mut parts = value.splitn(3, '\'');
                if !parts.next()?.eq_ignore_ascii_case("utf-8") {
                    continue;
                }
                parts.next()?;
                percent_encoding::percent_decode_str(parts.next()?)
                    .decode_utf8()
                    .ok()?
                    .into_owned()
            } else {
                value.to_owned()
            };
            if let Some(title) = safe(&decoded, source) {
                return Some(title);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_title_and_utf8_filename_are_display_metadata_only() {
        let source =
            Url::parse("https://sub.example.com/path-secret-123456789?token=private-token")
                .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "profile-title",
            format!("base64:{}", STANDARD.encode("示例订阅"))
                .parse()
                .unwrap(),
        );
        assert_eq!(from_headers(&headers, &source).as_deref(), Some("示例订阅"));
        headers.remove("profile-title");
        headers.insert(
            "content-disposition",
            "attachment; filename*=UTF-8''%E7%A4%BA%E4%BE%8B.yaml"
                .parse()
                .unwrap(),
        );
        assert_eq!(from_headers(&headers, &source).as_deref(), Some("示例"));
        headers.remove("content-disposition");
        for bad in [
            "private-token",
            "https://example.com/?token=private-token",
            "path-secret-123456789",
            "\u{1b}[31m",
            "\u{202e}spoof",
        ] {
            headers.insert(
                "profile-title",
                format!("base64:{}", STANDARD.encode(bad)).parse().unwrap(),
            );
            assert_eq!(from_headers(&headers, &source), None);
        }
    }
}
