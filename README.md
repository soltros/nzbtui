# nzbtui

A standalone, zero-web-server terminal Usenet client written in Rust with [Ratatui](https://ratatui.rs).

Search Newznab indexers, enqueue downloads directly, stream articles over multi-connection TLS NNTP with priority backup server fallback, decode yEnc with CRC32 checks, verify/repair with PAR2 (downloading `.vol*.par2` recovery blocks on demand), and auto-extract archives — all inside a single terminal binary.

## Features

- **No web server / daemon required** — pure native TUI application.
- **Direct Newznab Search** — configure one or multiple Newznab indexers (NZBgeek, DrunkenSlug, NZBfinder, etc.) and search concurrently with category filters and sorting.
- **Multi-Provider NNTP/TLS Engine** — pure-Rust `rustls` TLS (no OpenSSL system library dependency), persistent connection pooling, and priority-based fallback to backup/fill providers on `430 Article Not Found`.
- **yEnc Decoder** — single-part and multi-part `=ybegin` / `=ypart` / `=yend` decoding with CRC32 verification and direct byte-offset writes for low memory usage.
- **Smart Post-Processing** — verifies `.par2` index first, downloads `.vol*.par2` recovery volumes only when blocks are damaged/missing, repairs with `par2`, extracts `.rar` / `.7z` / `.zip` / `.tar.*` (including NZB password detection), and cleans up temporary/archive files.
- **Local `.nzb` Import** — press `I` inside the TUI or launch with `nzbtui --import file.nzb`.
- **In-TUI Configuration** — add/edit/delete providers and indexers, live-test NNTP credentials & indexer API keys, and toggle post-processing options without hand-editing files. Config is stored at `~/.config/nzbtui/config.json` with `0600` permissions.

## Keybindings

| Key | Action |
|---|---|
| `1` / `2` / `3` / `4` or `Tab` | Switch between Search, Downloads, Settings, Logs |
| `/` or `s` | Focus search input (`Enter` to search, `Esc` to blur) |
| `c` / `i` / `o` | Cycle Category / Indexer / Sort order in Search |
| `Enter` or `d` | Download selected search result |
| `I` | Import a local `.nzb` file |
| `Space` / `p` | Pause or resume selected download job |
| `P` | Pause or resume all downloads |
| `x` / `Delete` | Cancel job or remove history item |
| `K` / `J` | Move queued job priority up / down |
| `h` / `l` | Switch sections in Settings (Providers / Indexers / General) |
| `a` / `e` / `t` / `d` | Add / Edit / Test Connection / Delete in Settings |
| `?` | Help overlay |
| `q` / `Ctrl+C` | Quit |

## Building the Ubuntu `.deb` Package

```bash
./scripts/build-deb.sh
sudo apt install ./target/debian/nzbtui_0.1.0_amd64.deb
```

Using `sudo apt install ./target/debian/...deb` (instead of `dpkg -i`) automatically installs recommended post-processing tools (`par2`, `7zip`/`p7zip-full`, `unrar`, `unzip`) if they are not already present.

## Configuration File

Created automatically at `~/.config/nzbtui/config.json` (`chmod 600`) when saving in Settings, or you can edit it directly:

```json
{
  "downloads": {
    "download_dir": "/home/you/Downloads/nzbtui",
    "temp_dir": "/home/you/.local/share/nzbtui/incomplete",
    "max_concurrent_jobs": 1,
    "post_process": {
      "verify_par2": true,
      "repair_par2": true,
      "extract_archives": true,
      "cleanup_after_extract": true,
      "create_job_subfolder": true
    }
  },
  "providers": [
    {
      "name": "Primary",
      "host": "news.example.com",
      "port": 563,
      "tls": true,
      "username": "your-user",
      "password": "your-password",
      "connections": 10,
      "priority": 0,
      "enabled": true
    }
  ],
  "indexers": [
    {
      "name": "NZBgeek",
      "url": "https://api.nzbgeek.info",
      "api_key": "your-nzbgeek-api-key",
      "enabled": true
    }
  ],
  "ui": {
    "tick_rate_ms": 200,
    "search_limit": 100
  }
}
```

## License

Licensed under the GNU General Public License v3.0 (GPL-3.0-or-later). See [LICENSE](LICENSE).
