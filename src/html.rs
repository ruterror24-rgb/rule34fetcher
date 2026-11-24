use anyhow::{anyhow, bail, Result};
use scraper::{ElementRef, Html, Selector};

use crate::util::{self, Kind, RemotePost};

pub struct Thumb {
    pub id: u64,
    pub page_url: String,
    pub video_thumb: bool,
}

pub struct SearchPage {
    pub thumbs: Vec<Thumb>,
    pub next_pid: Option<u32>,
}

pub async fn search_page(
    client: &reqwest::Client,
    cookie: &str,
    tags: &str,
    pid: u32,
) -> Result<SearchPage> {
    let url = reqwest::Url::parse_with_params(
        "https://rule34.xxx/index.php",
        &[
            ("page", "post"),
            ("s", "list"),
            ("tags", tags),
            ("pid", &pid.to_string()),
        ],
    )?;
    let body = get_html(client, cookie, url).await?;
    if is_challenge(&body) {
        bail!("сайт показал проверку Cloudflare. Обновите cookie браузера и запустите снова");
    }
    Ok(parse_search(&body))
}

pub async fn resolve_post(
    client: &reqwest::Client,
    cookie: &str,
    thumb: &Thumb,
) -> Result<RemotePost> {
    let url = reqwest::Url::parse(&thumb.page_url)
        .map_err(|_| anyhow!("битая ссылка поста #{}", thumb.id))?;
    let body = get_html(client, cookie, url).await?;
    if is_challenge(&body) {
        bail!("проверка Cloudflare на странице поста #{}", thumb.id);
    }
    parse_post(thumb.id, &body, thumb.video_thumb)
        .ok_or_else(|| anyhow!("на странице #{} нет оригинала", thumb.id))
}

async fn get_html(client: &reqwest::Client, cookie: &str, url: reqwest::Url) -> Result<String> {
    let response = client
        .get(url)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::REFERER, "https://rule34.xxx/")
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(util::net_err)?;
    let status = response.status();
    let body = response.text().await.map_err(util::net_err)?;
    if !status.is_success() && !is_challenge(&body) {
        bail!("страница ответила HTTP {status}");
    }
    Ok(body)
}

pub fn parse_search(html: &str) -> SearchPage {
    let document = Html::parse_document(html);
    let scoped = Selector::parse("div.image-list img.preview, span.thumb img.preview").unwrap();
    let any = Selector::parse("img.preview").unwrap();
    let mut images: Vec<ElementRef> = document.select(&scoped).collect();
    if images.is_empty() {
        images = document.select(&any).collect();
    }

    let mut thumbs = Vec::new();
    for image in images {
        let Some(href) = ancestor_attr(image, "a", "href") else {
            continue;
        };
        let Some(id) = query_param(&href, "id").and_then(|v| v.parse().ok()) else {
            continue;
        };
        let class_list: Vec<_> = image.value().classes().collect();
        thumbs.push(Thumb {
            id,
            page_url: util::absolute_url(&href),
            video_thumb: class_list.iter().any(|class| *class == "webm-thumb"),
        });
    }

    SearchPage {
        thumbs,
        next_pid: next_pid(&document),
    }
}

pub fn parse_post(id: u64, html: &str, video_thumb: bool) -> Option<RemotePost> {
    let document = Html::parse_document(html);
    let video_sel = Selector::parse("video#gelcomVideoPlayer").unwrap();
    if let Some(video) = document.select(&video_sel).next() {
        let urls = video_sources(video);
        if !urls.is_empty() {
            return Some(RemotePost {
                id,
                tags: page_tags(&document),
                kind: Kind::Video,
                urls,
            });
        }
    }

    if video_thumb {
        return None;
    }

    let image_sel = Selector::parse("img#image").unwrap();
    let image = document.select(&image_sel).next()?;
    let url = image_original(image)?;
    Some(RemotePost {
        id,
        tags: page_tags(&document),
        kind: Kind::Image,
        urls: vec![url],
    })
}

fn page_tags(document: &Html) -> String {
    let sel = Selector::parse("#tag-sidebar a, #tag-list a").unwrap();
    let mut tags = Vec::new();
    for link in document.select(&sel) {
        let href = link.value().attr("href").unwrap_or("");
        if !href.contains("tags=") {
            continue;
        }
        let text: String = link.text().collect();
        let text = text.trim();
        if text.is_empty() || text == "?" {
            continue;
        }
        tags.push(text.to_string());
    }
    tags.join(" ")
}

fn video_sources(video: ElementRef) -> Vec<String> {
    let source_sel = Selector::parse("source").unwrap();
    let mut urls = Vec::new();
    for source in video.select(&source_sel) {
        if let Some(src) = source.value().attr("src") {
            urls.push(util::absolute_url(src));
        }
    }
    if let Some(src) = video.value().attr("src") {
        urls.push(util::absolute_url(src));
    }
    util::prefer_video_order(urls)
}

fn image_original(image: ElementRef) -> Option<String> {
    let src = image.value().attr("src").map(util::absolute_url);
    let parent = ancestor_attr(image, "a", "href").map(|href| util::absolute_url(&href));
    match (src, parent) {
        (Some(src), Some(href)) if is_sample(&src) && is_direct_file(&href) => Some(href),
        (Some(src), _) => Some(src),
        (None, Some(href)) if is_direct_file(&href) => Some(href),
        _ => None,
    }
}

fn is_sample(url: &str) -> bool {
    let url = url.to_ascii_lowercase();
    url.contains("/samples/") || url.contains("/thumbnails/") || url.contains("/preview/")
}

fn is_direct_file(url: &str) -> bool {
    util::extension(url).is_some_and(|ext| util::is_media_ext(&ext))
}

fn ancestor_attr(element: ElementRef, name: &str, attr: &str) -> Option<String> {
    let mut current = element.parent();
    while let Some(node) = current {
        if let Some(element) = ElementRef::wrap(node) {
            if element.value().name() == name {
                return element.value().attr(attr).map(str::to_string);
            }
        }
        current = node.parent();
    }
    None
}

fn next_pid(document: &Html) -> Option<u32> {
    let sel = Selector::parse("a[alt='next']").unwrap();
    for link in document.select(&sel) {
        if let Some(href) = link.value().attr("href") {
            if let Some(pid) = query_param(href, "pid").and_then(|v| v.parse().ok()) {
                return Some(pid);
            }
        }
    }
    None
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("please enter the captcha")
        || lower.contains("cf-chl")
        || lower.contains("just a moment")
        || lower.contains("enable javascript and cookies")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Kind;

    #[test]
    fn search_reads_both_preview_classes_and_next_page() {
        let html = r#"
            <div class="image-list">
              <span class="thumb" id="s10">
                <a href="index.php?page=post&s=view&id=10">
                  <img class="preview webm-thumb" src="//cdn.example/t/10.jpg">
                </a>
              </span>
              <span class="thumb" id="s11">
                <a href="index.php?page=post&s=view&id=11">
                  <img class="preview" src="//cdn.example/t/11.jpg">
                </a>
              </span>
            </div>
            <a alt="next" href="index.php?page=post&s=list&tags=foo&pid=42">»</a>
        "#;
        let page = parse_search(html);
        assert_eq!(page.thumbs.len(), 2);
        assert!(page.thumbs[0].video_thumb);
        assert!(!page.thumbs[1].video_thumb);
        assert_eq!(page.thumbs[0].id, 10);
        assert_eq!(page.next_pid, Some(42));
    }

    #[test]
    fn video_page_prefers_mp4_from_player() {
        let html = r#"
            <video id="gelcomVideoPlayer" width="1000" height="800">
              <source src="https://wimg.rule34.xxx/images/1/clip.webm" type="video/webm">
              <source src="https://wimg.rule34.xxx/images/1/clip.mp4" type="video/mp4">
            </video>
        "#;
        let post = parse_post(5, html, true).unwrap();
        assert_eq!(post.kind, Kind::Video);
        assert_eq!(post.urls[0], "https://wimg.rule34.xxx/images/1/clip.mp4");
    }

    #[test]
    fn image_page_uses_full_file_instead_of_sample() {
        let html = r#"
            <div class="content">
              <a href="https://wimg.rule34.xxx/images/2/pic.jpg">
                <img id="image" src="https://wimg.rule34.xxx/samples/2/sample_pic.jpg">
              </a>
            </div>
        "#;
        let post = parse_post(6, html, false).unwrap();
        assert_eq!(post.kind, Kind::Image);
        assert_eq!(post.urls[0], "https://wimg.rule34.xxx/images/2/pic.jpg");
    }
}
