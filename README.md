# imago

Agent-native Instagram profile archive.

Drop a profile URL. Get every photo, video, and carousel slide. Watch profiles and backfill weekly without duplicates.

**Homepage:** [midgarcorp.cc/imago](https://midgarcorp.cc/imago)

```bash
imago auth login --session-id '…' --csrf-token '…'
imago get https://www.instagram.com/natgeo/
imago watch add natgeo
imago watch sync --json
```

## Install

```bash
cargo install --path .
# or download a release binary when published
```

## Agent playbook

```bash
imago guide
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | ok |
| 1 | usage |
| 2 | auth dead |
| 3 | partial failure |
| 10 | unexpected |

## Paths

| What | Where |
|------|--------|
| credentials | `~/.config/imago/credentials.json` |
| watchlist / jobs / logs | `~/.local/share/imago/` |
| media | `./downloads/<username>/` |

## Supersedes

**igscraper** (Go) is deprecated. Same cookies work via env `IGSCRAPER_SESSION_ID` / `IGSCRAPER_CSRF_TOKEN` during migration, or re-run `imago auth login`.

## License

MIT
