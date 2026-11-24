#![cfg_attr(windows, windows_subsystem = "windows")]

mod api;
mod download;
mod gui;
mod html;
mod safety;
mod util;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;

use crate::download::Outcome;
use crate::util::{Kind, RemotePost};

#[derive(Parser, Debug)]
#[command(
    name = "rule34-fetcher",
    version,
    about = "Ищет посты на rule34.xxx по тегам и скачивает оригиналы изображений и видео",
    after_help = "Теги пишутся так же, как в поле поиска сайта: несколько слов через пробел.\n\
Видео берётся из источника плеера (mp4, если он есть). Изображение — полный файл, а не превью.\n\n\
Ключ API: https://rule34.xxx/index.php?page=account&s=options\n\
Положите user_id и api_key в файл r34.env или передайте флагами.\n\
Без ключа можно передать cookie своего браузера после проверки Cloudflare."
)]
struct Cli {
    /// Теги через пробел. Если не указаны, программа спросит их в консоли
    #[arg()]
    tags: Vec<String>,

    /// Внутренний режим, используется окном приложения для выполнения задачи
    #[arg(long, hide = true)]
    worker_cli: bool,

    /// Сколько постов скачать за один запрос
    #[arg(short, long, default_value_t = 20)]
    limit: usize,

    /// Размер страницы API (1–1000)
    #[arg(long, default_value_t = 100)]
    page_size: usize,

    /// Первая страница поиска (нумерация с 1)
    #[arg(long, default_value_t = 1)]
    from_page: u32,

    /// Последняя страница поиска включительно (нумерация с 1)
    #[arg(long, default_value_t = 200)]
    to_page: u32,

    /// Папка, куда складывать файлы
    #[arg(short, long, default_value = "downloads")]
    output: PathBuf,

    /// User ID из настроек аккаунта
    #[arg(long, env = "R34_USER_ID")]
    user_id: Option<String>,

    /// API key из настроек аккаунта
    #[arg(long, env = "R34_API_KEY")]
    api_key: Option<String>,

    /// Строка Cookie из браузера, если API-ключа нет
    #[arg(long, env = "R34_COOKIE")]
    cookie: Option<String>,

    /// Пауза между скачиваниями, миллисекунды
    #[arg(long, default_value_t = 400)]
    delay_ms: u64,

    /// Только показать, что будет скачано
    #[arg(long)]
    dry_run: bool,
}

struct Creds {
    user_id: Option<String>,
    api_key: Option<String>,
    cookie: Option<String>,
}

#[tokio::main]
async fn main() {
    util::enable_utf8_console();
    if let Err(err) = run().await {
        eprintln!("Ошибка: {:#}", err);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    if std::env::args_os().len() == 1 {
        gui::run().map_err(|err| anyhow!("не удалось открыть окно приложения: {err}"))?;
        return Ok(());
    }
    let cli = Cli::parse();
    if cli.limit == 0 || cli.limit > 20_000 {
        bail!("лимит должен быть от 1 до 20000");
    }
    if cli.from_page == 0 || cli.to_page > 200 || cli.from_page > cli.to_page {
        bail!("диапазон страниц должен быть от 1 до 200, начало не позже конца");
    }
    if !(1..=1000).contains(&cli.page_size) {
        bail!("размер страницы должен быть от 1 до 1000");
    }

    let creds = load_creds(&cli)?;
    let client = reqwest::Client::builder()
        .user_agent(util::USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("не удалось создать HTTP-клиент")?;

    if cli.tags.is_empty() {
        interactive(&client, &creds, &cli).await
    } else {
        let tags = cli.tags.join(" ");
        fetch_tags(&client, &creds, &cli, &tags).await
    }
}

async fn interactive(client: &reqwest::Client, creds: &Creds, cli: &Cli) -> Result<()> {
    println!("Введите теги так же, как в поиске на сайте. Пустая строка завершает программу.");
    loop {
        print!("Теги: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line).context("не удалось прочитать ввод")?;
        if read == 0 {
            break;
        }
        let tags = line.trim();
        if tags.is_empty() {
            break;
        }
        if let Err(err) = fetch_tags(client, creds, cli, tags).await {
            eprintln!("Ошибка: {:#}", err);
        }
        println!();
    }
    Ok(())
}

async fn fetch_tags(
    client: &reqwest::Client,
    creds: &Creds,
    cli: &Cli,
    tags: &str,
) -> Result<()> {
    let tags = tags.trim();
    if tags.is_empty() {
        bail!("теги не заданы");
    }
    if let Some(tag) = safety::blocked_tag(tags) {
        bail!("тег «{tag}» относится к контенту с несовершеннолетними, запрос остановлен");
    }

    let posts = collect(
        client,
        creds,
        tags,
        cli.limit,
        cli.page_size,
        cli.delay_ms,
        cli.from_page,
        cli.to_page,
    )
    .await?;
    if posts.is_empty() {
        println!("По тегам «{tags}» ничего не найдено.");
        return Ok(());
    }

    let dir = cli.output.join(util::folder_name(tags));
    if !cli.dry_run {
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("не удалось создать папку {}", dir.display()))?;
    }

    println!(
        "Найдено {} постов по «{tags}». Папка: {}",
        posts.len(),
        dir.display()
    );

    let mut saved = 0usize;
    let mut skipped = 0usize;
    let mut blocked = 0usize;
    let mut failed = 0usize;

    for (index, post) in posts.iter().enumerate() {
        if let Some(tag) = safety::blocked_tag(&post.tags) {
            blocked += 1;
            eprintln!(
                "[{}/{}] пропуск #{}: в тегах поста есть «{tag}»",
                index + 1,
                posts.len(),
                post.id
            );
            continue;
        }

        print!(
            "[{}/{}] {} #{} ... ",
            index + 1,
            posts.len(),
            post.kind.label(),
            post.id
        );
        io::stdout().flush().ok();

        match download::download_post(client, &dir, post, cli.dry_run).await {
            Ok(Outcome::Saved(path)) => {
                saved += 1;
                println!("{}", path.display());
            }
            Ok(Outcome::Skipped(path)) => {
                skipped += 1;
                println!("уже есть {}", path.display());
            }
            Ok(Outcome::DryRun) => {
                saved += 1;
            }
            Err(err) => {
                failed += 1;
                println!("ошибка");
                eprintln!("  {}", util::redact_secrets(&format!("{err:#}")));
            }
        }

        if cli.delay_ms > 0 && index + 1 < posts.len() && !cli.dry_run {
            tokio::time::sleep(Duration::from_millis(cli.delay_ms)).await;
        }
    }

    println!("Готово: сохранено {saved}, уже было {skipped}, пропущено {blocked}, ошибок {failed}.");
    if failed > 0 {
        bail!("часть файлов не скачалась");
    }
    Ok(())
}

async fn collect(
    client: &reqwest::Client,
    creds: &Creds,
    tags: &str,
    limit: usize,
    page_size: usize,
    delay_ms: u64,
    from_page: u32,
    to_page: u32,
) -> Result<Vec<RemotePost>> {
    match (&creds.user_id, &creds.api_key) {
        (Some(user_id), Some(api_key)) => {
            collect_api(
                client, user_id, api_key, tags, limit, page_size, delay_ms, from_page, to_page,
            )
            .await
        }
        _ => {
            let cookie = creds.cookie.as_deref().ok_or_else(|| anyhow!(api::auth_hint()))?;
            collect_html(client, cookie, tags, limit, delay_ms, from_page, to_page).await
        }
    }
}

async fn collect_api(
    client: &reqwest::Client,
    user_id: &str,
    api_key: &str,
    tags: &str,
    limit: usize,
    page_size: usize,
    delay_ms: u64,
    from_page: u32,
    to_page: u32,
) -> Result<Vec<RemotePost>> {
    let mut posts = Vec::new();
    let mut pid = from_page - 1;
    while posts.len() < limit && pid < to_page {
        let need = (limit - posts.len()).min(page_size);
        eprintln!("Ищу «{tags}» через API, страница {}", pid + 1);
        let mut page = api::search_page(client, user_id, api_key, tags, pid, need).await?;
        if page.is_empty() {
            break;
        }
        let got = page.len();
        posts.append(&mut page);
        if got < need {
            break;
        }
        pid += 1;
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }
    posts.truncate(limit);
    Ok(posts)
}

async fn collect_html(
    client: &reqwest::Client,
    cookie: &str,
    tags: &str,
    limit: usize,
    delay_ms: u64,
    from_page: u32,
    to_page: u32,
) -> Result<Vec<RemotePost>> {
    let mut posts = Vec::new();
    let mut pid = (from_page - 1).saturating_mul(42);
    let mut pages = 0u32;
    let max_pages = to_page - from_page + 1;
    while posts.len() < limit && pages < max_pages {
        pages += 1;
        eprintln!("Ищу «{tags}» на сайте, pid={pid}");
        let page = html::search_page(client, cookie, tags, pid).await?;
        if page.thumbs.is_empty() {
            break;
        }
        for thumb in &page.thumbs {
            if posts.len() >= limit {
                break;
            }
            let kind = if thumb.video_thumb { Kind::Video } else { Kind::Image };
            eprint!("  открываю {} #{} ... ", kind.label(), thumb.id);
            io::stderr().flush().ok();
            match html::resolve_post(client, cookie, thumb).await {
                Ok(post) => {
                    eprintln!("{} ссылок", post.urls.len());
                    posts.push(post);
                }
                Err(err) => eprintln!("пропуск: {err}"),
            }
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
        match page.next_pid {
            Some(next) if next != pid => pid = next,
            _ => break,
        }
    }
    Ok(posts)
}

fn load_creds(cli: &Cli) -> Result<Creds> {
    let mut user_id = nonempty(cli.user_id.clone());
    let mut api_key = nonempty(cli.api_key.clone());
    let mut cookie = nonempty(cli.cookie.clone());

    for path in env_files() {
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        for (key, value) in parse_env_file(&text) {
            match key.as_str() {
                "user_id" if user_id.is_none() => user_id = Some(value),
                "api_key" if api_key.is_none() => api_key = Some(value),
                "cookie" if cookie.is_none() => cookie = Some(value),
                _ => {}
            }
        }
    }

    if user_id.is_some() ^ api_key.is_some() {
        bail!("нужны оба значения: user_id и api_key");
    }
    if user_id.is_none() && cookie.is_none() {
        bail!("{}", api::auth_hint());
    }

    Ok(Creds {
        user_id,
        api_key,
        cookie,
    })
}

pub(crate) fn api_credentials_for_gui() -> Result<(String, String)> {
    let (user_id, api_key) = saved_api_credentials()?;
    if user_id.trim().is_empty() || api_key.trim().is_empty() {
        bail!("для подсчёта результатов нужны user_id и api_key в r34.env");
    }
    Ok((user_id, api_key))
}

pub(crate) fn saved_api_credentials() -> Result<(String, String)> {
    let path = util::project_dir().join("r34.env");
    if !path.is_file() {
        return Ok((String::new(), String::new()));
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;
    let mut user_id = String::new();
    let mut api_key = String::new();
    for (key, value) in parse_env_file(&contents) {
        match key.as_str() {
            "user_id" => user_id = value,
            "api_key" => api_key = value,
            _ => {}
        }
    }
    Ok((user_id, api_key))
}

pub(crate) fn save_api_credentials(user_id: &str, api_key: &str) -> Result<()> {
    let user_id = user_id.trim();
    let api_key = api_key.trim();
    if user_id.is_empty() || api_key.is_empty() {
        bail!("заполните оба поля: user_id и api_key");
    }
    if user_id.contains(['\r', '\n', '=']) || api_key.contains(['\r', '\n']) {
        bail!("значения не должны содержать переносы строк");
    }

    let path = util::project_dir().join("r34.env");
    let original = if path.is_file() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?
    } else {
        String::new()
    };
    let mut output = Vec::new();
    let mut wrote_user_id = false;
    let mut wrote_api_key = false;
    for line in original.lines() {
        let entry = line.trim().trim_start_matches('\u{feff}');
        let key = entry.split_once('=').map(|(key, _)| key.trim().to_ascii_lowercase());
        match key.as_deref() {
            Some("user_id") if !wrote_user_id => {
                output.push(format!("user_id={user_id}"));
                wrote_user_id = true;
            }
            Some("api_key") if !wrote_api_key => {
                output.push(format!("api_key={api_key}"));
                wrote_api_key = true;
            }
            Some("user_id" | "api_key") => {}
            _ => output.push(line.to_owned()),
        }
    }
    if !wrote_user_id {
        output.push(format!("user_id={user_id}"));
    }
    if !wrote_api_key {
        output.push(format!("api_key={api_key}"));
    }
    std::fs::write(&path, format!("{}\n", output.join("\n")))
        .with_context(|| format!("не удалось сохранить {}", path.display()))
}

fn env_files() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("r34.env"), util::project_dir().join("r34.env")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("r34.env"));
        }
    }
    paths
}

fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        pairs.push((key, value.to_string()));
    }
    pairs
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let text = text.trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_keeps_cookie_equals_signs() {
        let pairs = parse_env_file("user_id=15\napi_key=abc\ncookie=a=b; c=d\n# comment\n");
        assert_eq!(pairs[0], ("user_id".into(), "15".into()));
        assert_eq!(pairs[2].1, "a=b; c=d");
    }
}
