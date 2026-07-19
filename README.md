# imago

**Agent-native Instagram profile archive.** Point it at a profile, walk away — every
photo, video, and carousel slide lands on your disk. Watch profiles and backfill weekly
without re-downloading a thing.

**Homepage:** [midgarcorp.cc/imago](https://midgarcorp.cc/imago)

```bash
imago auth login                          # reads your session from the browser
imago get https://www.instagram.com/natgeo/
imago watch add natgeo
imago watch sync --json                   # weekly via cron
```

## Why

- **Complete.** Grid images, videos/reels, and every slide of every carousel — each as its
  own file with a stable key, so re-runs never re-download.
- **Unattended.** Auto-resumes interrupted jobs, waits out Instagram's soft rate limits,
  resumes truncated video downloads, and only stops for real when the session is dead.
- **No manual cookie wrangling.** `imago auth login` derives your logged-in session straight
  from your browser. No devtools, no copy-paste.
- **Agent-native.** `--json` on every command, a machine-readable `imago guide`, stable exit
  codes, and file logs — built for scripts and coding agents as much as humans.

## Install

**Arch Linux** — from the [AUR](https://aur.archlinux.org/packages/imago):

```bash
paru -S imago   # or: yay -S imago
```

**Prebuilt binary** (Linux x86-64, macOS) — from [Releases](https://github.com/guitaripod/imago/releases):

```bash
# example: Linux
curl -L https://github.com/guitaripod/imago/releases/latest/download/imago-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install imago /usr/local/bin/
```

**From source** (needs a Rust toolchain):

```bash
cargo install --git https://github.com/guitaripod/imago
```

> Requires **libcurl** on the system (present on virtually every Linux/macOS install). imago
> uses your system's TLS stack deliberately — a vendored one gets fingerprinted and blocked.

## Authentication

Instagram serves only a profile's **first 12 posts** anonymously; a full archive needs a
logged-in session. imago never asks for your password — it reuses the session already in
your browser.

```bash
# Log into instagram.com in Chrome / Chromium / Brave / Vivaldi / Edge / Opera, then:
imago auth login                 # auto-detects the browser
imago auth login --browser Brave # or pin one
imago auth status                # check it's alive
```

Prefer to paste cookies yourself (any browser, headless boxes, CI)? Copy `sessionid` and
`csrftoken` from devtools → Application → Cookies → instagram.com:

```bash
imago auth login --session-id '…' --csrf-token '…'
# or export IMAGO_SESSION_ID / IMAGO_CSRF_TOKEN
```

Credentials are stored in `~/.config/imago/credentials.json` (and the OS keyring when
available).

## Usage

```bash
imago get <url | @user | user>     # full archive (bare `imago <user>` works too)
imago get natgeo --force           # reset job state (still skips files on disk)
imago get natgeo --output ~/pics   # choose the output directory

imago watch add natgeo             # track a profile
imago watch list
imago watch sync                   # incremental: stops when a page is fully known
imago watch sync --full            # re-scan everything
imago watch remove natgeo
```

Weekly backfill via cron:

```cron
0 3 * * 0  imago watch sync --json >> ~/.local/share/imago/logs/cron.log 2>&1
```

## What gets downloaded

Profile grid posts — images, videos/reels, and every carousel slide — as
`<shortcode>.jpg|.mp4` or `<shortcode>_<NN>.jpg|.mp4` under `./downloads/<username>/`
(override with `--output`). A `metadata.json` records captions, timestamps, and media ids.

Not in v1: stories, highlights, the tagged tab, DMs.

**Dedup & resume:** the files on disk are the source of truth. Re-runs skip existing keys,
interrupted jobs auto-resume, and `watch sync` early-stops once a page is entirely known.

## Agent contract

| | |
|---|---|
| JSON | `--json` on every command → one object on stdout |
| Playbook | `imago guide` (machine-readable) |
| Logs | `$XDG_DATA_HOME/imago/logs/imago.log` |
| Exit codes | `0` ok · `1` usage · `2` auth dead · `3` partial · `10` unexpected |

## Paths

| What | Where |
|------|-------|
| credentials | `~/.config/imago/credentials.json` |
| watchlist / jobs / logs | `~/.local/share/imago/` |
| media | `./downloads/<username>/` |

## Rate limits

Instagram soft-blocks with `401`/`429` "Please wait a few minutes." imago backs off (up to
30 min between tries) and retries **forever** until the archive is complete — it never
aborts a run on a rate limit. Only a dead session is fatal (exit `2`); kill and re-run the
same command any time to resume.

## Legal

imago is a personal archival tool that reuses your own logged-in session, the same way your
browser does. You are responsible for complying with Instagram's Terms of Service and
applicable law. Don't use it as a bulk commercial scraper or to redistribute others' work.

## Build

```bash
cargo build --release
cargo test
```

## License

MIT
