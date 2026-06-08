# rutracker

CLI tool to search [RuTracker.org](https://rutracker.org) for torrents and fetch magnet links.

## Install

```bash
cargo install --git https://github.com/kernelzeroday/rutracker
```

Or build from source:

```bash
git clone https://github.com/kernelzeroday/rutracker.git
cd rutracker
cargo build --release
# binary at target/release/rutracker
```

## Config

Save credentials in `~/.config/rutracker/config.toml`:

```toml
username = "your_username"
password = "your_password"
```

Or set the `RUTRACKER_USER` and `RUTRACKER_PASS` environment variables.

Optionally, override the base URL:

```toml
base_url = "https://rutracker.org/forum"
```

## Usage

```
Search RuTracker for torrents

Usage: rutracker [OPTIONS] [QUERY]...

Arguments:
  [QUERY]...  Search query (shows popular recent torrents if omitted)

Options:
  -n, --limit <LIMIT>  Max results to show [default: 20]
      --relogin        Force fresh login (ignore saved session)
  -h, --help           Print help
```

### Examples

Search for "ubuntu linux":

```bash
rutracker ubuntu linux
```

Show more results:

```bash
rutracker -n 50 debian
```

Show recent popular torrents (no query):

```bash
rutracker
```

Force a fresh login:

```bash
rutracker --relogin arch linux
```

## How it works

1. Session is persisted to `~/.config/rutracker/session` after login and reused on subsequent runs
2. Searches the RuTracker tracker page with the given query (Windows-1251 encoded)
3. Parses torrent title, size, seed count, and magnet links
4. If a magnet link isn't in the search results, fetches the individual topic page to extract it

## Notes

- If RuTracker presents a captcha, log in through a browser first, then the CLI can reuse the session
- The CLI is blocking/synchronous (reqwest blocking client)