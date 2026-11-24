use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub fn project_dir() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    };
    let Some(exe_dir) = exe.parent() else {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    };
    if matches!(exe_dir.file_name().and_then(|name| name.to_str()), Some("debug" | "release"))
        && exe_dir.parent().and_then(Path::file_name).and_then(|name| name.to_str())
            == Some("target")
    {
        return exe_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| exe_dir.to_path_buf());
    }
    exe_dir.to_path_buf()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "изображение",
            Kind::Video => "видео",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemotePost {
    pub id: u64,
    pub tags: String,
    pub kind: Kind,
    pub urls: Vec<String>,
}

pub fn extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

pub fn is_media_ext(ext: &str) -> bool {
    matches!(
        ext,
        "jpg" | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "avif"
            | "bmp"
            | "mp4"
            | "webm"
            | "m4v"
            | "mov"
    )
}

pub fn ext_from_content_type(content_type: &str) -> Option<&'static str> {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    Some(match ct.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        _ => return None,
    })
}

pub fn choose_ext(url: &str, content_type: Option<&str>) -> String {
    if let Some(ext) = extension(url) {
        if is_media_ext(&ext) {
            return if ext == "jpeg" { "jpg".into() } else { ext };
        }
    }
    if let Some(ct) = content_type {
        if let Some(ext) = ext_from_content_type(ct) {
            return ext.to_string();
        }
    }
    extension(url).unwrap_or_else(|| "bin".into())
}

pub fn is_video_file(url: &str, tags: &str) -> bool {
    if matches!(
        extension(url).as_deref(),
        Some("mp4" | "webm" | "m4v" | "mov")
    ) {
        return true;
    }
    tags.split_whitespace().any(|tag| {
        matches!(
            tag.to_ascii_lowercase().as_str(),
            "video" | "webm" | "mp4" | "sound"
        )
    })
}

/// Для видео API часто отдаёт webm, а в плеере оригинал в mp4 лежит по тому же пути.
pub fn video_candidates(file_url: &str) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(mp4) = swap_ext(file_url, "mp4") {
        urls.push(mp4);
    }
    if !file_url.is_empty() {
        urls.push(file_url.to_string());
    }
    dedup_urls(urls)
}

pub fn prefer_video_order(urls: Vec<String>) -> Vec<String> {
    let mut urls = dedup_urls(urls);
    urls.sort_by_key(|url| match extension(url).as_deref() {
        Some("mp4" | "m4v") => 0,
        Some("webm") => 1,
        _ => 2,
    });
    urls
}

fn swap_ext(url: &str, new_ext: &str) -> Option<String> {
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    let hashless = path.split('#').next().unwrap_or(path);
    let dot = hashless.rfind('.')?;
    let slash = hashless.rfind('/').unwrap_or(0);
    if dot <= slash {
        return None;
    }
    let ext = &hashless[dot + 1..];
    if !matches!(ext.to_ascii_lowercase().as_str(), "webm" | "mp4" | "m4v" | "mov") {
        return None;
    }
    let mut swapped = format!("{}.{new_ext}", &hashless[..dot]);
    if let Some(query) = query {
        swapped.push('?');
        swapped.push_str(query);
    }
    Some(swapped)
}

pub fn dedup_urls(urls: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    urls.into_iter()
        .map(|url| url.trim().to_string())
        .filter(|url| acceptable_url(url))
        .filter(|url| seen.insert(url.clone()))
        .collect()
}

pub fn acceptable_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "https" | "http"))
        .unwrap_or(false)
}

pub fn absolute_url(href: &str) -> String {
    let href = href.trim();
    if href.starts_with("https://") || href.starts_with("http://") {
        href.to_string()
    } else if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = href.strip_prefix('/') {
        format!("https://rule34.xxx/{rest}")
    } else {
        format!("https://rule34.xxx/{href}")
    }
}

pub fn folder_name(tags: &str) -> String {
    let joined = tags.split_whitespace().collect::<Vec<_>>().join("_");
    let cleaned: String = joined
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let mut short: String = cleaned.chars().take(80).collect();
    short = short.trim().trim_matches('.').to_string();
    if short.is_empty() {
        "untagged".into()
    } else {
        short
    }
}

pub fn existing_download(dir: &Path, id: u64) -> Option<PathBuf> {
    let prefix = id.to_string();
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".part") {
            continue;
        }
        if name == prefix || name.strip_prefix(&prefix).is_some_and(|rest| rest.starts_with('.')) {
            return Some(path);
        }
    }
    None
}

pub fn redact_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &lower[i..];
        let key_len = if rest.starts_with("api_key=") {
            Some("api_key=".len())
        } else if rest.starts_with("user_id=") {
            Some("user_id=".len())
        } else {
            None
        };
        if let Some(key_len) = key_len {
            out.push_str(&text[i..i + key_len]);
            out.push_str("***");
            i += key_len;
            while i < text.len() {
                let b = text.as_bytes()[i];
                if b == b'&' || b.is_ascii_whitespace() || b == b'"' || b == b'\'' || b == b')' {
                    break;
                }
                i += 1;
            }
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn net_err(err: reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(redact_secrets(&err.to_string()))
}

#[cfg(windows)]
pub fn enable_utf8_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(code_page: u32) -> i32;
        fn SetConsoleCP(code_page: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}

#[cfg(not(windows))]
pub fn enable_utf8_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp4_is_preferred_over_webm() {
        let urls = video_candidates("https://wimg.rule34.xxx/images/1/file.webm");
        assert_eq!(
            urls,
            vec![
                "https://wimg.rule34.xxx/images/1/file.mp4".to_string(),
                "https://wimg.rule34.xxx/images/1/file.webm".to_string(),
            ]
        );
    }

    #[test]
    fn video_order_keeps_mp4_first() {
        let urls = prefer_video_order(vec![
            "https://cdn.example/a.webm".into(),
            "https://cdn.example/a.mp4".into(),
        ]);
        assert_eq!(urls[0], "https://cdn.example/a.mp4");
    }

    #[test]
    fn folder_strips_windows_reserved_chars() {
        assert_eq!(folder_name(r#"foo:bar*baz"#), "foo_bar_baz");
    }

    #[test]
    fn secrets_are_redacted() {
        let raw = "https://api.rule34.xxx/index.php?user_id=42&api_key=SECRET&tags=cat";
        let redacted = redact_secrets(raw);
        assert!(!redacted.contains("SECRET"));
        assert!(!redacted.contains("user_id=42"));
        assert!(redacted.contains("user_id=***"));
        assert!(redacted.contains("api_key=***"));
        assert!(redacted.contains("tags=cat"));
    }
}
