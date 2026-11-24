use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{CONTENT_TYPE, REFERER};
use tokio::io::AsyncWriteExt;

use crate::util::{self, RemotePost};

#[derive(Debug)]
pub enum Outcome {
    Saved(PathBuf),
    Skipped(PathBuf),
    DryRun,
}

pub async fn download_post(
    client: &reqwest::Client,
    dir: &Path,
    post: &RemotePost,
    dry_run: bool,
) -> Result<Outcome> {
    if dry_run {
        println!("#{} {} ", post.id, post.kind.label());
        for url in &post.urls {
            println!("  {url}");
        }
        return Ok(Outcome::DryRun);
    }

    if let Some(path) = util::existing_download(dir, post.id) {
        return Ok(Outcome::Skipped(path));
    }

    let mut errors = Vec::new();
    for url in &post.urls {
        if !util::acceptable_url(url) {
            continue;
        }
        match fetch_one(client, url, dir, post.id).await {
            Ok(path) => return Ok(Outcome::Saved(path)),
            Err(err) => errors.push(util::redact_secrets(&err.to_string())),
        }
    }

    if errors.is_empty() {
        bail!("у поста #{} нет годной ссылки на оригинал", post.id);
    }
    bail!("пост #{}: {}", post.id, errors.join(" | "))
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    id: u64,
) -> Result<PathBuf> {
    let mut last_err = None;
    for attempt in 1..=3 {
        match fetch_once(client, url, dir, id).await {
            Ok(path) => return Ok(path),
            Err(err) if attempt < 3 && is_transient(&err) => {
                last_err = Some(err);
                tokio::time::sleep(Duration::from_millis(700 * attempt)).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("не удалось скачать {url}")))
}

async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    id: u64,
) -> Result<PathBuf> {
    let response = client
        .get(url)
        .header(REFERER, "https://rule34.xxx/")
        .header(reqwest::header::ACCEPT, "*/*")
        .send()
        .await
        .map_err(util::net_err)?;

    let status = response.status();
    if status.as_u16() == 404 || status.as_u16() == 410 {
        bail!("HTTP {status}");
    }
    if !status.is_success() {
        bail!("HTTP {status}");
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if content_type
        .as_deref()
        .is_some_and(|ct| ct.starts_with("text/html"))
    {
        bail!("вместо файла пришла HTML-страница");
    }

    let ext = util::choose_ext(url, content_type.as_deref());
    let final_path = dir.join(format!("{id}.{ext}"));
    if final_path.exists() {
        return Ok(final_path);
    }
    let part_path = dir.join(format!("{id}.{ext}.part"));

    let write_result = write_response(response, &part_path).await;
    if let Err(err) = write_result {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(err);
    }

    tokio::fs::rename(&part_path, &final_path)
        .await
        .with_context(|| format!("не удалось сохранить {}", final_path.display()))?;
    Ok(final_path)
}

async fn write_response(mut response: reqwest::Response, part_path: &Path) -> Result<()> {
    let mut file = tokio::fs::File::create(part_path)
        .await
        .with_context(|| format!("не удалось создать {}", part_path.display()))?;
    let mut total = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(util::net_err)? {
        file.write_all(&chunk).await?;
        total += chunk.len() as u64;
    }
    file.flush().await?;
    if total == 0 {
        bail!("пустой ответ");
    }
    Ok(())
}

fn is_transient(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    ["HTTP 429", "HTTP 500", "HTTP 502", "HTTP 503", "HTTP 504"]
        .iter()
        .any(|code| text.contains(code))
        || text.contains("error sending request")
        || text.contains("timed out")
        || text.contains("connection")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_status_is_retried() {
        let err = anyhow!("HTTP 503");
        assert!(is_transient(&err));
        assert!(!is_transient(&anyhow!("HTTP 404")));
    }
}
