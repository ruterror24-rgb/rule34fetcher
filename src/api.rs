use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::util::{self, Kind, RemotePost};

#[derive(Clone, Debug)]
pub struct TagSuggestion {
    pub tag: String,
    pub count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiPost {
    id: u64,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    file_url: Option<String>,
    #[serde(default)]
    sample_url: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    directory: Option<Directory>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Directory {
    Num(u64),
    Text(String),
}

impl Directory {
    fn as_text(&self) -> String {
        match self {
            Directory::Num(n) => n.to_string(),
            Directory::Text(s) => s.clone(),
        }
    }
}

pub async fn search_page(
    client: &reqwest::Client,
    user_id: &str,
    api_key: &str,
    tags: &str,
    pid: u32,
    limit: usize,
) -> Result<Vec<RemotePost>> {
    let url = reqwest::Url::parse_with_params(
        "https://api.rule34.xxx/index.php",
        &[
            ("page", "dapi"),
            ("s", "post"),
            ("q", "index"),
            ("json", "1"),
            ("tags", tags),
            ("pid", &pid.to_string()),
            ("limit", &limit.to_string()),
            ("user_id", user_id),
            ("api_key", api_key),
        ],
    )
    .context("не удалось собрать URL поиска")?;

    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .send()
        .await
        .map_err(util::net_err)
        .context("поиск через API не удался")?;

    let status = response.status();
    let body = response.text().await.map_err(util::net_err)?;
    if !status.is_success() {
        bail!(
            "API ответил HTTP {status}: {}",
            snippet(&util::redact_secrets(&body))
        );
    }
    parse_posts(&body)
}

pub async fn tag_suggestions(
    client: &reqwest::Client,
    query: &str,
    credentials: Option<(&str, &str)>,
) -> Result<Vec<TagSuggestion>> {
    let url = reqwest::Url::parse_with_params(
        "https://api.rule34.xxx/autocomplete.php",
        &[("q", query)],
    )
    .context("не удалось собрать URL автодополнения")?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .send()
        .await
        .map_err(util::net_err)
        .context("не удалось получить подсказки тегов")?;
    if !response.status().is_success() {
        bail!("автодополнение вернуло HTTP {}", response.status());
    }
    let body = response.text().await.map_err(util::net_err)?;
    let value: Value = serde_json::from_str(body.trim())
        .context("сайт вернул неожиданный формат подсказок тегов")?;
    let mut suggestions = Vec::new();
    collect_suggestions(&value, &mut suggestions);

    // The autocomplete endpoint only returns tags beginning with the query.
    // The tag DAPI supports SQL-LIKE name_pattern, so query it too for
    // matches embedded anywhere in the tag (e.g. terraria -> nurse_(terraria)).
    let literal_query = query.trim().replace('%', "").replace('_', r"\_");
    if !literal_query.is_empty() {
        let pattern = format!("%{literal_query}%");
        let mut params = vec![
            ("page", "dapi"),
            ("s", "tag"),
            ("q", "index"),
            ("json", "1"),
            ("limit", "100"),
            ("orderby", "count"),
            ("order", "DESC"),
            ("name_pattern", pattern.as_str()),
        ];
        if let Some((user_id, api_key)) = credentials {
            params.push(("user_id", user_id));
            params.push(("api_key", api_key));
        }
        let tag_url = reqwest::Url::parse_with_params("https://api.rule34.xxx/index.php", &params)?;
        if let Ok(response) = client
            .get(tag_url)
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(tag_body) = response.text().await {
                    if let Ok(tag_value) = serde_json::from_str::<Value>(tag_body.trim()) {
                        collect_suggestions(&tag_value, &mut suggestions);
                    } else {
                        // The tag DAPI may return XML even when json=1 is set.
                        collect_xml_tag_suggestions(&tag_body, &mut suggestions);
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    suggestions.retain(|suggestion| {
        !suggestion.tag.trim().is_empty()
            && normalize_tag(&suggestion.tag).contains(&normalize_tag(query))
            && seen.insert(normalize_tag(&suggestion.tag))
    });
    // The GUI displays only a few rows. Put embedded matches first so the
    // prefix-only autocomplete results cannot push them out of the popup.
    let normalized_query = normalize_tag(query);
    suggestions.sort_by(|left, right| {
        let left_prefix = normalize_tag(&left.tag).starts_with(&normalized_query);
        let right_prefix = normalize_tag(&right.tag).starts_with(&normalized_query);
        left_prefix
            .cmp(&right_prefix)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.tag.cmp(&right.tag))
    });
    Ok(suggestions)
}

fn collect_xml_tag_suggestions(body: &str, output: &mut Vec<TagSuggestion>) {
    let document = scraper::Html::parse_document(body);
    let Ok(selector) = scraper::Selector::parse("tag") else {
        return;
    };
    for element in document.select(&selector) {
        let Some(tag) = element.value().attr("name").filter(|name| !name.trim().is_empty()) else {
            continue;
        };
        output.push(TagSuggestion {
            tag: tag.trim().to_owned(),
            count: element.value().attr("count").and_then(|count| count.parse().ok()),
        });
    }
}

pub async fn search_count(
    client: &reqwest::Client,
    user_id: &str,
    api_key: &str,
    tags: &str,
) -> Result<u64> {
    let url = reqwest::Url::parse_with_params(
        "https://api.rule34.xxx/index.php",
        &[
            ("page", "dapi"),
            ("s", "post"),
            ("q", "index"),
            ("json", "1"),
            ("tags", tags),
            ("pid", "0"),
            ("limit", "1"),
            ("user_id", user_id),
            ("api_key", api_key),
        ],
    )?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .send()
        .await
        .map_err(util::net_err)
        .context("не удалось получить общее число результатов")?;
    let status = response.status();
    let body = response.text().await.map_err(util::net_err)?;
    if !status.is_success() {
        bail!("подсчёт результатов вернул HTTP {status}");
    }
    if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
        if let Some(count) = value
            .get("count")
            .and_then(parse_count_value)
            .or_else(|| value.get("posts_count").and_then(parse_count_value))
        {
            return Ok(count);
        }
    }

    let fallback_url = reqwest::Url::parse_with_params(
        "https://api.rule34.xxx/index.php",
        &[
            ("page", "dapi"),
            ("s", "post"),
            ("q", "index"),
            ("tags", tags),
            ("pid", "0"),
            ("limit", "1"),
            ("user_id", user_id),
            ("api_key", api_key),
        ],
    )?;
    let fallback_response = client
        .get(fallback_url)
        .header(reqwest::header::ACCEPT, "application/xml, text/xml, */*")
        .send()
        .await
        .map_err(util::net_err)
        .context("не удалось получить XML-счётчик результатов")?;
    if !fallback_response.status().is_success() {
        bail!("XML-подсчёт результатов вернул HTTP {}", fallback_response.status());
    }
    let fallback_body = fallback_response.text().await.map_err(util::net_err)?;
    let document = scraper::Html::parse_document(&fallback_body);
    let selector = scraper::Selector::parse("posts").expect("valid selector");
    if let Some(count) = document
        .select(&selector)
        .next()
        .and_then(|posts| posts.value().attr("count"))
        .and_then(|text| text.parse().ok())
    {
        return Ok(count);
    }
    bail!("API не вернул счётчик результатов")
}

fn collect_suggestions(value: &Value, output: &mut Vec<TagSuggestion>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_suggestions(item, output);
            }
        }
        Value::Object(map) => {
            let value_tag = map.get("value").and_then(Value::as_str).map(str::trim);
            let label = map.get("label").and_then(Value::as_str).map(str::trim);
            let candidate = value_tag
                .filter(|text| !text.is_empty())
                .or_else(|| map.get("name").and_then(Value::as_str).map(str::trim))
                .filter(|text| !text.is_empty())
                .or_else(|| label.and_then(strip_count_from_label));
            let count = ["count", "post_count", "postCount", "posts_count"]
                .iter()
                .find_map(|key| map.get(*key).and_then(parse_count_value))
                .or_else(|| label.and_then(count_from_label));
            if let Some(tag) = candidate {
                output.push(TagSuggestion {
                    tag: tag.to_owned(),
                    count,
                });
            } else {
                for item in map.values() {
                    collect_suggestions(item, output);
                }
            }
        }
        Value::String(text) => output.push(TagSuggestion {
            tag: strip_count_from_label(text).unwrap_or(text.trim()).to_owned(),
            count: count_from_label(text),
        }),
        _ => {}
    }
}

fn strip_count_from_label(label: &str) -> Option<&str> {
    let open = label.rfind('(')?;
    let suffix = label[open + 1..].strip_suffix(')')?.trim();
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') {
        return None;
    }
    Some(label[..open].trim())
}

fn count_from_label(label: &str) -> Option<u64> {
    let open = label.rfind('(')?;
    let suffix = label[open + 1..].strip_suffix(')')?.trim();
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') {
        return None;
    }
    suffix.replace([',', '.'], "").parse().ok()
}

fn parse_count_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn normalize_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase().replace(' ', "_")
}

fn parse_posts(body: &str) -> Result<Vec<RemotePost>> {
    let trimmed = body.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("missing authentication") {
            bail!("API отклонил запрос: неверный или пустой user_id / api_key");
        }
        bail!("API вернул не JSON: {}", snippet(trimmed));
    }

    let value: Value = serde_json::from_str(trimmed)
        .with_context(|| format!("не удалось разобрать JSON: {}", snippet(trimmed)))?;

    match value {
        Value::Array(items) => items
            .into_iter()
            .map(post_from_value)
            .collect::<Result<Vec<_>>>()
            .map(|posts| posts.into_iter().flatten().collect()),
        Value::Object(map) => {
            if map.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let message = map
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search failed");
                bail!("API: {message}");
            }
            if let Some(message) = map.get("message").and_then(|v| v.as_str()) {
                let lower = message.to_ascii_lowercase();
                if lower.contains("auth") || lower.contains("api key") || lower.contains("user") {
                    bail!("API: {message}");
                }
            }
            let list = map
                .get("post")
                .or_else(|| map.get("posts"))
                .and_then(|v| v.as_array());
            if let Some(items) = list {
                return items
                    .iter()
                    .cloned()
                    .map(post_from_value)
                    .collect::<Result<Vec<_>>>()
                    .map(|posts| posts.into_iter().flatten().collect());
            }
            bail!("неожиданный ответ API: {}", snippet(trimmed));
        }
        _ => bail!("неожиданный ответ API: {}", snippet(trimmed)),
    }
}

fn post_from_value(value: Value) -> Result<Option<RemotePost>> {
    let post: ApiPost = serde_json::from_value(value).context("битая запись поста в ответе API")?;
    Ok(remote_from_api(post))
}

fn remote_from_api(post: ApiPost) -> Option<RemotePost> {
    let file_url = first_nonempty([
        post.file_url.as_deref(),
        direct_url(post.directory.as_ref(), post.image.as_deref()).as_deref(),
        post.sample_url.as_deref(),
    ])?;
    let kind = if util::is_video_file(&file_url, &post.tags) {
        Kind::Video
    } else {
        Kind::Image
    };
    let urls = match kind {
        Kind::Video => util::video_candidates(&file_url),
        Kind::Image => util::dedup_urls(vec![file_url]),
    };
    if urls.is_empty() {
        return None;
    }
    Some(RemotePost {
        id: post.id,
        tags: post.tags,
        kind,
        urls,
    })
}

fn direct_url(directory: Option<&Directory>, image: Option<&str>) -> Option<String> {
    let directory = directory?.as_text();
    let image = image?.trim();
    if directory.is_empty() || image.is_empty() {
        return None;
    }
    Some(format!(
        "https://wimg.rule34.xxx/images/{directory}/{image}"
    ))
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty() && util::acceptable_url(value) || value.starts_with("//"))
        .map(|value| {
            if value.starts_with("//") {
                util::absolute_url(value)
            } else {
                value.to_string()
            }
        })
}

fn snippet(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = compact.chars().take(180).collect();
    if compact.chars().count() > 180 {
        short.push('…');
    }
    short
}

pub fn auth_hint() -> &'static str {
    "\
Нужен свой доступ к rule34.xxx. Анонимный API отключён, а HTML-страницы закрыты проверкой Cloudflare.

Вариант 1 — API-ключ:
  откройте https://rule34.xxx/index.php?page=account&s=options
  и запишите в r34.env рядом с программой:
    user_id=ВАШ_ID
    api_key=ВАШ_КЛЮЧ

Вариант 2 — cookie браузера, если ключа нет:
  откройте сайт, пройдите проверку и передайте свой Cookie
  флагом --cookie или строкой cookie=... в r34.env."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_and_image_posts() {
        let body = r#"[
            {"id": 10, "tags": "video sound", "file_url": "https://wimg.rule34.xxx/images/1/a.webm", "image": "a.webm"},
            {"id": 11, "tags": "photo", "file_url": "https://wimg.rule34.xxx/images/1/b.jpg", "image": "b.jpg"}
        ]"#;
        let posts = parse_posts(body).unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].kind, Kind::Video);
        assert_eq!(posts[0].urls[0], "https://wimg.rule34.xxx/images/1/a.mp4");
        assert_eq!(posts[1].kind, Kind::Image);
        assert_eq!(posts[1].urls[0], "https://wimg.rule34.xxx/images/1/b.jpg");
    }

    #[test]
    fn builds_url_from_directory_when_file_url_missing() {
        let body = r#"[{"id": 7, "tags": "solo", "directory": 99, "image": "hash.png"}]"#;
        let posts = parse_posts(body).unwrap();
        assert_eq!(
            posts[0].urls[0],
            "https://wimg.rule34.xxx/images/99/hash.png"
        );
    }

    #[test]
    fn rejects_missing_auth_text() {
        let err = parse_posts("Missing authentication. Go to api.rule34.xxx").unwrap_err();
        assert!(err.to_string().contains("user_id"));
    }
}
