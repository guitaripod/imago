pub fn print_guide() {
    println!(
        r#"imago — agent-native Instagram profile archive
homepage: https://midgarcorp.cc/imago

QUICK START
  1. Log into Instagram in your browser (Chrome/Chromium/Brave/Vivaldi/Edge/Opera).
  2. imago auth login                # reads your session from the browser
  3. imago get https://www.instagram.com/<user>/
  4. imago watch add <user> && imago watch sync   # weekly via cron

COMMANDS
  imago guide
  imago auth login [--browser NAME] [--session-id S --csrf-token C]
  imago auth status [--json]
  imago auth logout
  imago get <url|@user|user> [--json] [--force] [--output DIR]
  imago watch add <url|user> [--json] [--no-fetch] [--note TEXT]
  imago watch list [--json]
  imago watch remove <user> [--json]
  imago watch sync [user…] [--json] [--full]
  imago version [--json]

  Bare: imago <url|user>  →  get

AUTH
  Anonymous access only sees a profile's first 12 posts; full archives need a
  logged-in session. `imago auth login` derives it from your browser — no manual
  cookie copy. Pin a browser with --browser, or pass cookies directly.

  Credential priority:
    --session-id / --csrf-token  (or a browser, when omitted)
    IMAGO_SESSION_ID + IMAGO_CSRF_TOKEN
    ~/.config/imago/credentials.json
    OS keyring service "imago"

PATHS
  config:  $XDG_CONFIG_HOME/imago/   (credentials.json, config.yaml)
  data:    $XDG_DATA_HOME/imago/     (watchlist.json, jobs/, logs/)
  media:   ./downloads/<username>/   (override with --output / config)

WHAT GETS DOWNLOADED
  profile grid posts: images, videos/reels, every carousel slide
  files: {{shortcode}}.jpg|.mp4  or  {{shortcode}}_{{NN}}.jpg|.mp4
  not in v1: stories, highlights, tagged tab, DMs

DEDUP / RESUME
  Disk scan is truth — re-runs skip existing keys.
  Incomplete jobs auto-resume from $XDG_DATA_HOME/imago/jobs/<user>.json
  --force resets job state (still skips files on disk)

WATCH / WEEKLY
  imago watch add natgeo
  imago watch sync --json          # early-stops when a page is fully known
  imago watch sync --full          # full re-scan
  cron example:
    0 3 * * 0  imago watch sync --json >>/var/log/imago.log 2>&1

EXIT CODES
  0  ok
  1  usage / bad input
  2  auth dead (re-login)
  3  partial (some watch targets failed)
  10 unexpected

JSON
  Pass --json for a single JSON object on stdout. Progress goes to stderr
  unless --json (then quiet spinner off). Logs always append to
  $XDG_DATA_HOME/imago/logs/imago.log

RATE LIMITS
  Instagram soft-blocks with 401/429 "Please wait a few minutes".
  imago waits (up to 30 min between tries) and retries forever until the
  profile is fully archived. Only a dead session (login redirect) is fatal
  (exit 2). Re-run the same command to resume if you kill the process.

DEFAULT OUTPUT
  ./downloads/<username>/
"#
    );
}
