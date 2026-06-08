use anyhow::{bail, Context, Result};
use clap::Parser;
use encoding_rs::WINDOWS_1251;
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use reqwest::blocking::Client;
use reqwest::header;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://rutracker.org/forum";

#[derive(Parser)]
#[command(name = "rutracker", about = "Search RuTracker for torrents")]
struct Cli {
    /// Search query (shows popular recent torrents if omitted)
    query: Vec<String>,

    /// Max results to show
    #[arg(short = 'n', long, default_value = "20")]
    limit: usize,

    /// Force fresh login (ignore saved session)
    #[arg(long)]
    relogin: bool,
}

#[derive(Deserialize)]
struct Config {
    username: String,
    password: String,
    #[serde(default)]
    base_url: Option<String>,
}

struct Torrent {
    title: String,
    size: String,
    seeds: String,
    magnet: String,
    topic_id: String,
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("rutracker")
}

fn session_path() -> PathBuf {
    config_dir().join("session")
}

fn load_config() -> Result<Config> {
    let path = config_dir().join("config.toml");

    let mut config = if let Ok(content) = fs::read_to_string(&path) {
        toml::from_str::<Config>(&content).context("Failed to parse config.toml")?
    } else {
        let user = std::env::var("RUTRACKER_USER");
        let pass = std::env::var("RUTRACKER_PASS");
        match (user, pass) {
            (Ok(username), Ok(password)) => Config {
                username,
                password,
                base_url: None,
            },
            _ => bail!(
                "Config not found at {path}\n\n\
                 Create it:\n  mkdir -p {dir}\n  \
                 cat > {path} << 'EOF'\n  \
                 username = \"your_username\"\n  \
                 password = \"your_password\"\n  \
                 EOF\n\n\
                 Or set RUTRACKER_USER and RUTRACKER_PASS env vars.",
                path = path.display(),
                dir = config_dir().display(),
            ),
        }
    };

    if let Ok(u) = std::env::var("RUTRACKER_USER") {
        config.username = u;
    }
    if let Ok(p) = std::env::var("RUTRACKER_PASS") {
        config.password = p;
    }

    Ok(config)
}

fn save_session(cookie: &str) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    fs::write(session_path(), cookie)?;
    Ok(())
}

fn load_session() -> Option<String> {
    fs::read_to_string(session_path())
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn build_client(base_url: &str, session_cookie: Option<&str>) -> Result<Client> {
    let jar = Arc::new(reqwest::cookie::Jar::default());

    if let Some(cookie) = session_cookie {
        let url: reqwest::Url = base_url.parse().context("Invalid base URL")?;
        jar.add_cookie_str(&format!("bb_session={}", cookie), &url);
    }

    Client::builder()
        .cookie_provider(jar)
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")
}


fn encode_win1251(s: &str) -> Vec<u8> {
    let (bytes, _, _) = WINDOWS_1251.encode(s);
    bytes.into_owned()
}

fn decode_win1251(bytes: &[u8]) -> String {
    let (text, _, _) = WINDOWS_1251.decode(bytes);
    text.into_owned()
}

fn is_logged_in(html: &str) -> bool {
    html.contains("logged-in-username") || html.contains("id=\"logged-in-username\"")
        || (html.contains("tracker.php") && !html.contains("login-form-full"))
}

fn login(base_url: &str, config: &Config) -> Result<String> {
    // Use a separate no-redirect client to capture the Set-Cookie header
    let login_client = Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let username_bytes = encode_win1251(&config.username);
    let password_bytes = encode_win1251(&config.password);
    let username_enc = percent_encode(&username_bytes, NON_ALPHANUMERIC);
    let password_enc = percent_encode(&password_bytes, NON_ALPHANUMERIC);

    let body = format!(
        "login_username={}&login_password={}&login=%C2%F5%EE%E4",
        username_enc, password_enc
    );

    let resp = login_client
        .post(format!("{}/login.php", base_url))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .context("Login request failed — is rutracker.org reachable?")?;

    // Extract bb_session from Set-Cookie headers
    let session: Option<String> = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|s| s.starts_with("bb_session="))
        .and_then(|s| {
            s.strip_prefix("bb_session=")
                .and_then(|rest| rest.split(';').next())
                .map(String::from)
        });

    let status = resp.status();
    let html = decode_win1251(&resp.bytes()?);

    // A redirect (302) with a bb_session cookie = success
    if let Some(session) = session {
        if !session.is_empty() && session != "deleted" {
            return Ok(session);
        }
    }

    // If we got HTML back, check for error messages
    if html.contains("captcha") {
        bail!("Login requires captcha — log in via browser first");
    }
    if html.contains("login-form-full") || html.contains("\"login-form\"") || !status.is_redirection() {
        bail!(
            "Login failed — check credentials in {}",
            config_dir().join("config.toml").display()
        );
    }

    bail!("Login failed — no session cookie received");
}

fn fetch_tracker(client: &Client, base_url: &str, query: Option<&str>) -> Result<String> {
    let url = match query {
        Some(q) => {
            let query_bytes = encode_win1251(q);
            let encoded = percent_encode(&query_bytes, NON_ALPHANUMERIC);
            format!("{}/tracker.php?nm={}&o=10&s=2", base_url, encoded)
        }
        None => format!("{}/tracker.php?o=10&s=2&tm=3", base_url),
    };

    let resp = client.get(&url).send().context("Search request failed")?;
    Ok(decode_win1251(&resp.bytes()?))
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn parse_results(html: &str, limit: usize) -> Vec<Torrent> {
    let doc = Html::parse_document(html);
    let row_sel = Selector::parse("tr.hl-tr").unwrap();
    let title_sel = Selector::parse(".t-title a").unwrap();
    let size_sel = Selector::parse("td.tor-size").unwrap();
    let seed_sel = Selector::parse("b.seedmed, b.seed").unwrap();
    let magnet_sel = Selector::parse("a.magnet-link, a[href^=\"magnet:\"]").unwrap();

    let mut results = Vec::new();

    for row in doc.select(&row_sel) {
        if results.len() >= limit {
            break;
        }

        let title = match row.select(&title_sel).next() {
            Some(el) => {
                let t: String = el.text().collect();
                let t = t.trim().to_string();
                if t.is_empty() {
                    continue;
                }
                t
            }
            None => continue,
        };

        let size = row
            .select(&size_sel)
            .next()
            .and_then(|el| el.value().attr("data-ts_text"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(format_size)
            .unwrap_or_default();

        let seeds = row
            .select(&seed_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let magnet = row
            .select(&magnet_sel)
            .next()
            .and_then(|el| el.value().attr("href"))
            .unwrap_or("")
            .to_string();

        let topic_id = row
            .value()
            .attr("data-topic_id")
            .unwrap_or("")
            .to_string();

        results.push(Torrent {
            title,
            size,
            seeds,
            magnet,
            topic_id,
        });
    }

    results
}

fn fetch_magnet(client: &Client, base_url: &str, topic_id: &str) -> Result<String> {
    let url = format!("{}/viewtopic.php?t={}", base_url, topic_id);
    let resp = client.get(&url).send()?;
    let html = decode_win1251(&resp.bytes()?);

    let doc = Html::parse_document(&html);
    let sel = Selector::parse("a.magnet-link, a[href^=\"magnet:\"]").unwrap();

    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr("href"))
        .map(String::from)
        .context("Magnet link not found on topic page")
}

fn display(results: &[Torrent]) {
    if results.is_empty() {
        eprintln!("No results found.");
        return;
    }

    for t in results {
        println!("{}", t.title);
        let mut meta = Vec::new();
        if !t.size.is_empty() {
            meta.push(format!("size={}", t.size));
        }
        if !t.seeds.is_empty() {
            meta.push(format!("seeds={}", t.seeds));
        }
        if !meta.is_empty() {
            println!("  {}", meta.join(" "));
        }
        if !t.magnet.is_empty() {
            println!("  {}", t.magnet);
        }
        println!();
    }
}

fn ensure_logged_in(base_url: &str, config: &Config, force_login: bool) -> Result<Client> {
    // Try saved session first
    if !force_login {
        if let Some(session) = load_session() {
            let client = build_client(base_url, Some(&session))?;
            eprint!("Checking session... ");
            match client.get(format!("{}/index.php", base_url)).send() {
                Ok(resp) => {
                    let html = decode_win1251(&resp.bytes()?);
                    if is_logged_in(&html) {
                        eprintln!("ok");
                        return Ok(client);
                    }
                    eprintln!("expired");
                }
                Err(_) => {
                    eprintln!("failed");
                }
            }
        }
    }

    eprint!("Logging in... ");
    let session = login(base_url, config)?;
    let _ = save_session(&session);
    let client = build_client(base_url, Some(&session))?;
    eprintln!("ok");

    Ok(client)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config()?;
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/')
        .to_string();

    let client = ensure_logged_in(&base_url, &config, cli.relogin)?;

    let query = if cli.query.is_empty() {
        None
    } else {
        Some(cli.query.join(" "))
    };

    eprint!("Searching... ");
    let html = fetch_tracker(&client, &base_url, query.as_deref())?;
    let mut results = parse_results(&html, cli.limit);
    eprintln!("{} results", results.len());

    let missing: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, t)| t.magnet.is_empty() && !t.topic_id.is_empty())
        .map(|(i, _)| i)
        .collect();

    if !missing.is_empty() {
        for (done, &i) in missing.iter().enumerate() {
            eprint!(
                "\rFetching magnet links ({}/{})... ",
                done + 1,
                missing.len()
            );
            if let Ok(magnet) = fetch_magnet(&client, &base_url, &results[i].topic_id) {
                results[i].magnet = magnet;
            }
        }
        eprintln!("done");
    }

    println!();
    display(&results);

    Ok(())
}
