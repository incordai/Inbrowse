mod batch;

use std::sync::Arc;
use std::time::Instant;

mod pool;

use clap::{Parser, Subcommand};
use inbrowse::{BrowserContext, Page};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

#[derive(Parser)]
#[command(
    name = "inbrowse",
    version = env!("INBROWSE_BUILD_VERSION"),
    about = "Inbrowse - A lightweight headless browser for web scraping and automation",
)]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, default_value_t = 9222)]
    port: u16,

    #[arg(long, global = true)]
    proxy: Option<String>,

    /// Comma-separated proxy URLs to rotate across, per browser context (IP
    /// rotation for scraping at volume). Each may carry auth, e.g.
    /// `http://user:pass@host:port,socks5://host2:port`. Sets `INBROWSE_PROXIES`.
    /// A single `--proxy` always overrides the pool.
    #[arg(long, global = true)]
    proxy_pool: Option<String>,

    /// Enable stealth mode (consistent browser fingerprint, and with the
    /// `stealth` build feature, TLS impersonation plus tracker blocking).
    /// Global: applies to fetch, serve, scrape, and mcp.
    #[arg(long, global = true)]
    stealth: bool,

    /// Respect robots.txt before navigating to an HTTP(S) URL.
    /// Global: applies to fetch and scrape.
    #[arg(long, global = true)]
    obey_robots: bool,

    /// Override the User-Agent sent with every request.
    /// Global: applies to fetch, serve, scrape, and mcp. A subcommand-level
    /// --user-agent still wins where one exists.
    #[arg(long, global = true)]
    user_agent: Option<String>,

    #[arg(long)]
    storage_dir: Option<std::path::PathBuf>,

    /// Permit fetches to loopback, RFC1918, and link-local addresses.
    /// Default is to block them (SSRF fix from #4). Use this for local
    /// development against http://localhost:N or http://192.168.x.y.
    /// Equivalent to `INBROWSE_ALLOW_PRIVATE_NETWORK=1` but per-process
    /// and survives in command pipelines.
    #[arg(long, global = true)]
    allow_private_network: bool,

    /// Pass raw flags to V8, in the same form V8/Chromium/Node accept
    /// (e.g. `"--max-old-space-size=4096 --max-semi-space-size=64 --expose-gc"`).
    /// Applied once at startup before any isolate is created.
    #[arg(long, value_name = "FLAGS", allow_hyphen_values = true)]
    v8_flags: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(short, long, default_value_t = 9222)]
        port: u16,

        // Bind address. Defaults to 127.0.0.1 (loopback only) for safety.
        // Set to 0.0.0.0 to listen on all interfaces (e.g. inside a Docker
        // container where you want the port to be reachable from the host
        // via -p mapping).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long, default_value_t = 1)]
        workers: u16,

        /// Maximum live CDP connections. Each connection runs on its own OS
        /// thread with its own V8 isolates, so this bounds the server's thread
        /// and memory footprint. Connections beyond the limit are refused with
        /// a 503 rather than queued. With --workers N the limit applies to each
        /// worker process, so the server-wide maximum is N times this value.
        #[arg(long, default_value_t = inbrowse_cdp::DEFAULT_MAX_CONNECTIONS)]
        max_connections: usize,

        /// Recycle a worker after it has served this many CDP connections
        /// (/json discovery requests do not count). Needs --workers > 1. The
        /// worker drains and a replacement takes its place; off by default.
        #[arg(long, value_name = "N")]
        worker_max_connections: Option<usize>,

        /// Recycle a worker whose resident memory exceeds this many MB,
        /// sampled every 2s from /proc (Linux only). Needs --workers > 1.
        #[arg(long, value_name = "MB")]
        worker_max_rss: Option<u64>,

        /// Recycle a worker after it has run this many seconds. Needs
        /// --workers > 1.
        #[arg(long, value_name = "SECS")]
        worker_max_age: Option<u64>,

        /// Seconds a replaced worker may keep serving its live connections
        /// before it is killed.
        #[arg(long, value_name = "SECS", default_value_t = 120)]
        worker_drain_timeout: u64,

        /// Atomically publish the bound address after V8 initialization.
        #[arg(long, value_name = "PATH")]
        ready_file: Option<std::path::PathBuf>,

        /// Allow CDP clients to navigate to file:// URLs. Off by
        /// default so a CDP connection cannot read arbitrary local
        /// files. Enable only when serving local HTML for testing
        /// and the port is on a trusted network.
        #[arg(long)]
        allow_file_access: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,

        /// Recursively load TTF, TTC, OTF, and OTC files from this directory.
        /// Repeat for multiple directories. Requires a render-enabled build.
        #[arg(long = "font-dir", value_name = "DIR")]
        font_dirs: Vec<std::path::PathBuf>,

        /// Suppress all logs (same as on `fetch`). Useful when scraping pages
        /// that flood the console with per-page script warnings (issue #264).
        #[arg(long)]
        quiet: bool,
    },

    Fetch {
        // Optional so a batch run can pass URLs via --file instead. A single
        // positional URL keeps the original one-shot behaviour.
        url: Option<String>,

        // Default is html. Kept as Option so we can tell whether --dump was
        // explicitly passed: a bare --eval returns its own value, while --eval
        // combined with --dump (or --selector) runs the eval, lets its async
        // work settle, then reads the page (issue #248).
        #[arg(long)]
        dump: Option<DumpFormat>,

        /// Read newline-delimited URLs from a file (one per line; blank lines
        /// and lines starting with `#` are skipped). Use `-` for stdin. Enables
        /// batch mode: every URL is fetched raw (--dump original) and one JSON
        /// status line is printed per URL. For rendered/DOM batch output use
        /// `scrape` instead (issue #349).
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        /// Number of URLs fetched concurrently in batch mode. Ignored without
        /// --file.
        #[arg(long, default_value_t = std::num::NonZeroUsize::new(1).unwrap())]
        concurrency: std::num::NonZeroUsize,

        #[arg(long)]
        selector: Option<String>,

        /// Keep or drop page boilerplate (navigation, header, footer, sidebars)
        /// in `--dump text`, `markdown` and `json`. Default: drop.
        #[arg(long, value_enum)]
        boilerplate: Option<Boilerplate>,

        /// Maximum adaptive post-load settle time in seconds. When supplied
        /// explicitly, this is a fixed delay; the default is a 5-second cap
        /// that returns once the page is quiescent.
        #[arg(long)]
        wait: Option<u64>,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        /// Seconds one page script may run before it is stopped and reported
        /// on stderr (default 5; INBROWSE_SCRIPT_TIMEOUT_MS also sets it).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        script_timeout: Option<u64>,

        /// Also wait for `setTimeout` timers due within this many milliseconds
        /// of the settle start. The default settle waits only for timers a few
        /// hundred milliseconds out; its 5 s ceiling grows to cover MS.
        #[arg(long, value_name = "MS", conflicts_with = "wait")]
        settle_timers: Option<u64>,

        #[arg(long, default_value = "load")]
        wait_until: String,

        #[arg(long)]
        user_agent: Option<String>,

        /// Seed a request cookie before navigating, "name=value" (repeatable).
        /// Scoped to the fetched URL's domain. Useful to set site preferences
        /// that change server-rendered output — e.g. Amazon's `i18n-prefs=USD`
        /// forces the marketplace's currency regardless of the egress IP geo.
        #[arg(long = "cookie")]
        cookies: Vec<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,

        #[arg(long, short)]
        quiet: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,

        /// Capture the settled page as a PNG. Requires the `render` feature.
        #[arg(long, short = 's', value_name = "FILE", conflicts_with = "file")]
        screenshot: Option<std::path::PathBuf>,

        /// Recursively load TTF, TTC, OTF, and OTC files from this directory,
        /// as on `serve`, so screenshots can use system fonts. Repeat for
        /// multiple directories. Requires a render-enabled build.
        #[arg(long = "font-dir", value_name = "DIR")]
        font_dirs: Vec<std::path::PathBuf>,
    },

    Scrape {
        /// URLs to scrape. `-` reads a newline-delimited list from stdin
        /// (blank lines and lines starting with `#` are skipped).
        urls: Vec<String>,

        /// Also read newline-delimited URLs from a file, or from stdin with
        /// `-`, using the same rules as a `-` URL.
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, default_value_t = std::num::NonZeroUsize::new(10).unwrap())]
        concurrency: std::num::NonZeroUsize,

        #[arg(long, default_value = "json")]
        format: String,

        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        /// As on `fetch`: after load each page settles until quiet (5 s
        /// ceiling) before `--eval`; this also waits for `setTimeout` timers
        /// due within MS.
        #[arg(long, value_name = "MS")]
        settle_timers: Option<u64>,

        #[arg(long, short)]
        quiet: bool,
    },

    Mcp {
        /// Recursively load additional font files before the first render.
        #[arg(long = "font-dir", value_name = "DIR")]
        font_dirs: Vec<std::path::PathBuf>,

        #[arg(long)]
        http: bool,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long, default_value_t = 3000)]
        port: u16,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        /// Close the browser's pages after this many seconds with no MCP
        /// request. The next call opens a new page with the same cookies and
        /// storage. 0 keeps pages open until `browser_close` or exit.
        #[arg(long, value_name = "SECS", default_value_t = inbrowse_mcp::DEFAULT_IDLE_CLOSE.as_secs())]
        idle_timeout: u64,
    },

    /// Print the text of a PDF, Word, PowerPoint, OpenDocument, spreadsheet,
    /// CSV or plain-text file as JSON: `{format, text, sections, warnings}`.
    #[cfg(feature = "extract")]
    Extract {
        /// A local path, or an http(s):// or file:// URL to download.
        target: String,

        /// File name, extension or MIME type; only needed to mark plain text
        /// as CSV. Defaults to the response Content-Type or the file name.
        #[arg(long)]
        hint: Option<String>,

        /// Password for an encrypted PDF.
        #[arg(long)]
        password: Option<String>,

        /// Seconds allowed for the download and the extraction together.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },

    /// Research sources: posts with engagement counts from Reddit, X, Hacker
    /// News, GitHub or Polymarket, printed as JSON. API keys come from
    /// INBROWSE_REDDIT_CLIENT_ID + INBROWSE_REDDIT_CLIENT_SECRET,
    /// INBROWSE_X_BEARER_TOKEN and INBROWSE_GITHUB_TOKEN; account cookies from
    /// --cookies or the --storage-dir profile.
    #[cfg(feature = "social")]
    Social {
        #[command(subcommand)]
        action: SocialAction,
    },

    /// Take still frames (PNG), 16 kHz mono WAV audio and captions from an
    /// MP4, WebM, Matroska or WebVTT file, and print a JSON report with the
    /// file paths.
    #[cfg(feature = "media")]
    Media {
        /// A local path, or an http(s):// or file:// URL to download.
        target: String,

        /// Folder for the PNG and WAV files. Default: a new folder under
        /// INBROWSE_MEDIA_DIR or the system temp folder.
        #[arg(long)]
        out_dir: Option<std::path::PathBuf>,

        /// Take a frame at least every this many seconds.
        #[arg(long, default_value_t = 10.0)]
        every: f64,

        /// Most frames to write (0 to 200).
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(0..=200))]
        max_frames: u32,

        /// Do not add frames where the picture changes sharply.
        #[arg(long)]
        no_scene_changes: bool,

        /// Do not write the audio track.
        #[arg(long)]
        no_audio: bool,

        /// Scale frames down so the longer side is at most this many pixels.
        #[arg(long, default_value_t = 2048, value_parser = clap::value_parser!(u32).range(16..))]
        max_side: u32,

        /// Also read the text in each frame with the OCR model in the Incord
        /// memory engine (INBROWSE_OCR_URL); adds `ocr` to each frame.
        #[cfg(feature = "ocr")]
        #[arg(long)]
        ocr: bool,

        /// Seconds allowed for the download, the extraction and OCR together.
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: u64,

        /// Write the JSON report here instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },

    /// Read the text in an image (PNG, JPEG, WebP, GIF or BMP) with the OCR
    /// model in the Incord memory engine, and print it as JSON with each
    /// line's box and confidence. The engine's address comes from
    /// INBROWSE_OCR_URL (default http://127.0.0.1:9180, where Watchman runs
    /// it), its key (if any) from INBROWSE_OCR_KEY. Big images
    /// are split into overlapping tiles, never refused.
    #[cfg(feature = "ocr")]
    Ocr {
        /// A local path, or an http(s):// or file:// URL to download.
        target: String,

        /// Seconds allowed for the download and OCR together.
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: u64,

        /// Write the JSON here instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },
}

#[cfg(feature = "social")]
#[derive(Subcommand, Debug)]
enum SocialAction {
    /// Search one source for posts from the last --days days.
    Search {
        /// reddit, x, hackernews, github or polymarket.
        source: String,
        query: String,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=365))]
        days: u32,
        #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        /// relevance, recent or top.
        #[arg(long, default_value = "relevance")]
        sort: String,
        /// Subreddit (Reddit only).
        #[arg(long)]
        community: Option<String>,
        /// JSON cookie list for signed-in access (same format as a
        /// --storage-dir cookies.json, or a browser cookie export).
        #[arg(long)]
        cookies: Option<std::path::PathBuf>,
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },
    /// One post and its top comments (Reddit, Hacker News or GitHub URL).
    Thread {
        url: String,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(0..=50))]
        comments: u32,
        #[arg(long)]
        cookies: Option<std::path::PathBuf>,
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },
}

/// Whether `--dump text`, `markdown` and `json` keep the page's navigation,
/// header, footer and sidebars. Those inside an article, main or section are
/// part of it and always stay.
#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
enum Boilerplate {
    Keep,
    Drop,
}

#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
enum DumpFormat {
    Html,
    Text,
    Links,
    Markdown,
    /// One JSON object for the page: `url`, `status`, `title`, `description`,
    /// `lang`, `publishedTime`, `content` (the markdown) and the `links` and
    /// `images` maps. See docs/CLI-reference.md.
    Json,
    /// Stream the raw HTTP response body verbatim (binary-safe).
    /// Bypasses the browser/JS layer — useful for fetching images,
    /// JSON, JS, CSS, or any non-HTML resource (cf. issue #117).
    Original,
    /// One JSON object per line listing every sub-resource URL the
    /// rendered page references (script src, link href, img src,
    /// iframe src, media sources, embed/object data). Lets callers
    /// replay the asset graph with their own HTTP client when they
    /// need the originals alongside the page (cf. issue 124).
    Assets,
    /// Dump all cookies in the browser jar as a JSON array, including
    /// HttpOnly cookies that are inaccessible via document.cookie.
    /// Useful for extracting session tokens set by anti-bot challenges.
    Cookies,
}

fn print_banner(port: u16) {
    println!(
        r#"
  _____         _
 |_   _|       | |
   | |   _ __  | |__   _ __   ___  __      __ ___   ___
   | |  | '_ \ | '_ \ | '__| / _ \ \ \ /\ / // __| / _ \
  _| |_ | | | || |_) || |   | (_) | \ V  V / \__ \|  __/
 |_____||_| |_||_.__/ |_|    \___/   \_/\_/  |___/ \___|

  Headless Browser v{}
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#,
        env!("INBROWSE_BUILD_VERSION"),
        port
    );
}

fn select_log_filter(verbose: bool, quiet: bool) -> &'static str {
    if verbose {
        "debug"
    } else if quiet {
        "off"
    } else {
        "warn"
    }
}

fn is_quiet_command(cmd: &Option<Command>) -> bool {
    // `extract` prints JSON on stdout, where log lines would corrupt it.
    #[cfg(feature = "extract")]
    if matches!(cmd, Some(Command::Extract { .. })) {
        return true;
    }
    #[cfg(feature = "media")]
    if matches!(cmd, Some(Command::Media { .. })) {
        return true;
    }
    #[cfg(feature = "social")]
    if matches!(cmd, Some(Command::Social { .. })) {
        return true;
    }
    #[cfg(feature = "ocr")]
    if matches!(cmd, Some(Command::Ocr { .. })) {
        return true;
    }
    matches!(
        cmd,
        Some(Command::Fetch { quiet: true, .. })
            | Some(Command::Scrape { quiet: true, .. })
            | Some(Command::Serve { quiet: true, .. })
    )
}

fn configure_font_directories(font_dirs: &[std::path::PathBuf]) -> anyhow::Result<()> {
    if font_dirs.is_empty() {
        return Ok(());
    }
    for directory in font_dirs {
        if !directory.is_dir() {
            anyhow::bail!(
                "Font directory does not exist or is not a directory: {}",
                directory.display()
            );
        }
    }

    #[cfg(feature = "render")]
    {
        if !inbrowse_js::configure_font_directories(font_dirs.to_vec()) {
            anyhow::bail!("Font directories must be configured before the first render");
        }
        Ok(())
    }
    #[cfg(not(feature = "render"))]
    anyhow::bail!("--font-dir requires a render-enabled build")
}

fn merge_proxy(global_proxy: Option<String>, command_proxy: Option<String>) -> Option<String> {
    command_proxy.or(global_proxy)
}

fn merge_user_agent(
    global_user_agent: Option<String>,
    command_user_agent: Option<String>,
) -> Option<String> {
    command_user_agent.or(global_user_agent)
}

/// Normalize a raw `--v8-flags` value into the string we'll hand to V8.
/// Returns `None` when the user didn't pass the flag, passed an empty string,
/// or passed only whitespace; in those cases V8 is left untouched.
fn normalize_v8_flags(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Default V8 flags applied at startup unless the user disabled them via
/// `--v8-flags`. The default heap matches headless Chrome (~4 GB) so pages
/// that ship heavy fingerprinting or analytics bundles
/// (e.g. demo.fingerprint.com — issue #199) don't SIGTRAP out of the box.
/// V8 parses flags left-to-right and later wins, so anything the user
/// passes via `--v8-flags` overrides these.
///
/// `--max-semi-space-size=4` caps V8's young generation (default 16 MB per
/// semi-space) so a parse/JS allocation burst does not inflate RSS, and
/// `--optimize-for-size` trades memory-heavy codegen choices for a smaller
/// footprint. Together they cut RSS ~18% on heavy pages (ycombinator.com
/// 173 MB -> 140 MB) at no measurable speed cost (V8 still JITs hot paths).
#[cfg(target_pointer_width = "64")]
const DEFAULT_V8_FLAGS: &str =
    "--max-old-space-size=4096 --max-semi-space-size=4 --optimize-for-size";
#[cfg(not(target_pointer_width = "64"))]
const DEFAULT_V8_FLAGS: &str =
    "--max-old-space-size=1024 --max-semi-space-size=4 --optimize-for-size";

fn effective_v8_flags(user: Option<&str>) -> String {
    match normalize_v8_flags(user) {
        Some(u) => format!("{} {}", DEFAULT_V8_FLAGS, u),
        None => DEFAULT_V8_FLAGS.to_string(),
    }
}

const CLI_STACK_BYTES: usize = 512 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("inbrowse-main".to_string())
        // V8 derives its stack guard from the current native thread. Deep but
        // valid hostile documents can otherwise exhaust the platform's small
        // default stack while the page realm is initialized. This reserves
        // address space; pages are committed only as the stack is used.
        .stack_size(CLI_STACK_BYTES)
        .spawn(run_cli)?
        .join()
        .map_err(|_| anyhow::anyhow!("inbrowse main thread panicked"))?
}

fn operator_network_error(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    match inbrowse_net::private_network_error_hint(&message) {
        Some(hint) => format!("{}\n{}", message, hint),
        None => message,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run_cli() -> anyhow::Result<()> {
    let args = Args::parse();

    // Pin the process timezone before V8/ICU reads it. V8 sources the zone for
    // both Date (getTimezoneOffset, toString) and Intl.DateTimeFormat from TZ; left
    // unset it defaults to UTC for Date while the page layer advertised a different
    // zone, a cross-surface mismatch fingerprinting scripts flag. Default to
    // Europe/Berlin; set INBROWSE_TIMEZONE to match the exit IP's region. An existing
    // TZ from the host is respected.
    // SAFETY: runs before any V8 isolate or worker thread starts, so the env is
    // effectively single threaded here.
    if let Some(tz) = std::env::var("INBROWSE_TIMEZONE")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        unsafe {
            std::env::set_var("TZ", tz);
        }
    } else if std::env::var_os("TZ").is_none() {
        unsafe {
            std::env::set_var("TZ", "Europe/Berlin");
        }
    }

    let quiet = is_quiet_command(&args.command);
    let filter = select_log_filter(args.verbose, quiet);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    let v8_flags = effective_v8_flags(args.v8_flags.as_deref());
    tracing::debug!("V8 flags: {}", v8_flags);
    inbrowse_js::set_v8_flags(&v8_flags);

    // The js-side fetch path (op_fetch_url) reads INBROWSE_ALLOW_PRIVATE_NETWORK
    // directly for its SSRF gate. Mirror the CLI flag into the env var so
    // iframe loads and JS fetch() see the same policy the http_client layer
    // already uses (issue #33).
    if args.allow_private_network {
        // SAFETY: set_var is unsafe in newer rustc; this runs before any
        // spawned thread inspects the env, so it's effectively single
        // threaded at this point.
        unsafe {
            std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
        }
    }

    let global_proxy = args.proxy.clone();
    let global_user_agent = args.user_agent.clone();
    let global_storage_dir = args.storage_dir.clone();
    let stealth = args.stealth;
    let obey_robots = args.obey_robots;

    // --proxy-pool seeds INBROWSE_PROXIES before any BrowserContext is built, so
    // ProxyPool::global() (read lazily during context construction) picks it up.
    if let Some(pool) = args.proxy_pool.as_deref() {
        if !pool.trim().is_empty() {
            std::env::set_var("INBROWSE_PROXIES", pool);
            let count = pool.split(',').filter(|s| !s.trim().is_empty()).count();
            tracing::info!(
                proxies = count,
                "proxy pool configured (per-context rotation)"
            );
        }
    }

    match args.command {
        Some(Command::Serve {
            port,
            host,
            proxy,
            user_agent,
            workers,
            max_connections,
            worker_max_connections,
            worker_max_rss,
            worker_max_age,
            worker_drain_timeout,
            ready_file,
            allow_file_access,
            storage_dir,
            font_dirs,
            quiet: _,
        }) => {
            let user_agent = merge_user_agent(global_user_agent.clone(), user_agent);
            let policy = pool::RecyclePolicy {
                max_connections: worker_max_connections,
                max_rss_mb: worker_max_rss,
                max_age: worker_max_age.map(Duration::from_secs),
            };
            if policy.enabled() && workers <= 1 {
                anyhow::bail!("--worker-max-* options require --workers > 1");
            }
            if policy.max_rss_mb.is_some() && !cfg!(target_os = "linux") {
                tracing::warn!("--worker-max-rss is only supported on Linux and is ignored");
            }
            // Fall back to INBROWSE_PROXY so a proxy can be supplied without
            // putting credentials on the command line. The multi-worker load
            // balancer passes the proxy to each worker this way (issue #366).
            let proxy = merge_proxy(global_proxy.clone(), proxy).or_else(|| {
                std::env::var("INBROWSE_PROXY")
                    .ok()
                    .filter(|s| !s.is_empty())
            });
            configure_font_directories(&font_dirs)?;
            if ready_file.is_some() && workers != 1 {
                anyhow::bail!("--ready-file requires --workers 1");
            }
            if port != 0 {
                print_banner(port);
            }
            if let Some(ref dir) = storage_dir {
                tracing::info!("Storage dir: {}", dir.display());
            }
            if let Some(ref proxy) = proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            if let Some(ref ua) = user_agent {
                tracing::info!("User-Agent: {}", ua);
            }
            for directory in &font_dirs {
                tracing::info!("Font dir: {}", directory.display());
            }
            if stealth {
                #[cfg(feature = "stealth")]
                tracing::info!(
                    "Stealth mode enabled (TLS fingerprint impersonation + tracker blocking)"
                );
                #[cfg(not(feature = "stealth"))]
                tracing::info!("Stealth mode enabled (tracker blocking)");
            }

            if workers > 1 {
                tracing::info!("{} worker processes", workers);
                run_multi_worker_serve(
                    port,
                    host,
                    workers,
                    proxy,
                    stealth,
                    user_agent,
                    font_dirs,
                    max_connections,
                    policy,
                    Duration::from_secs(worker_drain_timeout),
                )
                .await?;
            } else {
                inbrowse_cdp::start_with_serve_options_limit_and_ready_file(
                    port,
                    &host,
                    proxy,
                    stealth,
                    user_agent,
                    allow_file_access,
                    storage_dir,
                    args.allow_private_network,
                    max_connections,
                    ready_file.as_deref(),
                )
                .await?;
            }
        }
        Some(Command::Fetch {
            url,
            dump,
            selector,
            boilerplate,
            wait,
            timeout,
            script_timeout,
            settle_timers,
            wait_until,
            user_agent,
            cookies,
            eval,
            output,
            quiet,
            storage_dir,
            file,
            concurrency,
            screenshot,
            font_dirs,
        }) => {
            configure_font_directories(&font_dirs)?;
            let user_agent = merge_user_agent(global_user_agent.clone(), user_agent);
            if let Some(file) = file {
                if url.is_some() {
                    anyhow::bail!("Pass URLs via a positional argument or --file, not both.");
                }
                if screenshot.is_some() {
                    anyhow::bail!(
                        "--screenshot is only supported for a single URL, not --file batch mode."
                    );
                }
                // Batch mode is raw HTTP only. Rendering each URL through the
                // browser/JS stack is what `scrape` is for.
                match dump {
                    None | Some(DumpFormat::Original) => {}
                    Some(_) => anyhow::bail!(
                        "batch mode (--file) only supports --dump original. Use `scrape` for rendered/DOM output."
                    ),
                }
                let urls = read_urls_from_file(&file)?;
                run_batch_fetch(
                    urls,
                    concurrency.get(),
                    timeout,
                    user_agent,
                    global_proxy,
                    output,
                    quiet,
                    stealth,
                )
                .await?;
            } else {
                let url = url.ok_or_else(|| {
                    anyhow::anyhow!(
                        "No URL provided. Pass a URL, or a list of URLs with --file <path>."
                    )
                })?;
                let wait_is_fixed = wait.is_some();
                let wait_secs = wait.unwrap_or_else(|| adaptive_settle_secs(settle_timers));
                run_fetch(
                    &url,
                    dump,
                    selector,
                    boilerplate,
                    wait_secs,
                    wait_is_fixed,
                    timeout,
                    script_timeout,
                    settle_timers,
                    &wait_until,
                    user_agent,
                    cookies,
                    stealth,
                    eval,
                    output,
                    quiet,
                    global_proxy,
                    storage_dir,
                    args.allow_private_network,
                    obey_robots,
                    screenshot,
                )
                .await?;
            }
        }
        Some(Command::Scrape {
            urls,
            file,
            eval,
            concurrency,
            format,
            timeout,
            settle_timers,
            quiet,
        }) => {
            run_parallel_scrape(
                scrape_url_list(urls, file.as_deref())?,
                eval,
                concurrency.get(),
                &format,
                timeout,
                settle_timers,
                quiet,
                global_proxy,
                stealth,
                obey_robots,
                global_user_agent.clone(),
            )
            .await?;
        }
        Some(Command::Mcp {
            font_dirs,
            http,
            host,
            port,
            proxy,
            user_agent,
            idle_timeout,
        }) => {
            let user_agent = merge_user_agent(global_user_agent.clone(), user_agent);
            configure_font_directories(&font_dirs)?;
            let mcp_proxy = merge_proxy(global_proxy.clone(), proxy);
            let idle_close = (idle_timeout > 0).then(|| Duration::from_secs(idle_timeout));
            // `inbrowse mcp` builds its BrowserContext from the environment, so
            // the global profile flag has to be handed over explicitly.
            if let Some(ref dir) = global_storage_dir {
                // SAFETY: single threaded here, before the MCP server starts.
                unsafe {
                    std::env::set_var("INBROWSE_STORAGE_DIR", dir);
                }
            }
            if http {
                inbrowse_mcp::http::run(host, port, mcp_proxy, user_agent, stealth, idle_close)
                    .await?;
            } else {
                inbrowse_mcp::run(mcp_proxy, user_agent, stealth, idle_close).await?;
            }
        }
        #[cfg(feature = "extract")]
        Some(Command::Extract {
            target,
            hint,
            password,
            timeout,
            output,
        }) => {
            let choices = ExtractChoices {
                hint,
                options: inbrowse_extract::Options { password },
                timeout_secs: timeout,
            };
            run_extract(
                &target,
                choices,
                global_user_agent.clone(),
                global_proxy,
                stealth,
                output,
            )
            .await?;
        }
        #[cfg(feature = "social")]
        Some(Command::Social { action }) => {
            run_social(
                action,
                global_user_agent.clone(),
                global_proxy,
                stealth,
                global_storage_dir.clone(),
            )
            .await?;
        }
        #[cfg(feature = "media")]
        Some(Command::Media {
            target,
            out_dir,
            every,
            max_frames,
            no_scene_changes,
            no_audio,
            max_side,
            #[cfg(feature = "ocr")]
            ocr,
            timeout,
            output,
        }) => {
            let options = inbrowse_media::Options {
                every_secs: every,
                max_frames: max_frames as usize,
                scene_changes: !no_scene_changes,
                audio: !no_audio,
                max_side,
                ..inbrowse_media::Options::default()
            };
            #[cfg(not(feature = "ocr"))]
            let ocr = false;
            run_media(
                &target,
                out_dir,
                options,
                ocr,
                timeout,
                global_user_agent.clone(),
                global_proxy,
                stealth,
                output,
            )
            .await?;
        }
        #[cfg(feature = "ocr")]
        Some(Command::Ocr {
            target,
            timeout,
            output,
        }) => {
            run_ocr(
                &target,
                timeout,
                global_user_agent.clone(),
                global_proxy,
                stealth,
                output,
            )
            .await?;
        }
        None => {
            print_banner(args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            inbrowse_cdp::start_with_options(args.port, args.proxy, stealth).await?;
        }
    }

    Ok(())
}

async fn wait_for_serve_worker(
    child: &mut tokio::process::Child,
    port: u16,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("worker on port {} exited during startup: {}", port, status);
        }
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => return Ok(()),
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// How often each supervisor re-checks its recycle policy (RSS and age have no
/// event to wait on; the connection limit also wakes it directly).
const RECYCLE_POLL: Duration = Duration::from_secs(2);

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

type WorkerCommand = Arc<dyn Fn(u16) -> TokioCommand + Send + Sync>;

/// Replace a dead worker, retrying until one starts. Always picks a fresh port.
/// Returns `None` once the balancer is shutting down.
async fn respawn_worker(
    slot: &pool::Slot,
    make_cmd: &WorkerCommand,
    max_connections: Option<usize>,
    stop: &tokio::sync::watch::Receiver<()>,
) -> Option<tokio::process::Child> {
    // A worker that dies at once (bad flags, missing library) would otherwise
    // be respawned ten times a second for as long as the server runs. The
    // pause doubles after each failed attempt, up to a few seconds.
    const FIRST_RETRY: Duration = Duration::from_millis(100);
    const MAX_RETRY: Duration = Duration::from_secs(5);
    let mut retry = FIRST_RETRY;
    let mut stopping = stop.clone();
    loop {
        if stop.has_changed().unwrap_or(true) {
            return None;
        }
        // Shutdown must not wait out the pause.
        tokio::select! {
            () = tokio::time::sleep(retry) => {}
            _ = stopping.changed() => return None,
        }
        retry = (retry * 2).min(MAX_RETRY);
        let port = match free_port() {
            Ok(port) => port,
            Err(error) => {
                tracing::warn!("worker port allocation failed: {}", error);
                continue;
            }
        };
        match make_cmd(port).spawn() {
            Ok(mut replacement) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                match wait_for_serve_worker(&mut replacement, port, deadline).await {
                    Ok(()) => {
                        slot.replace(pool::Worker::new(port, max_connections, slot.wake.clone()));
                        return Some(replacement);
                    }
                    Err(error) => tracing::warn!("worker {} restart failed: {}", port, error),
                }
            }
            Err(error) => tracing::warn!("worker {} spawn failed: {}", port, error),
        }
    }
}

/// Let a replaced worker finish its live connections, then kill it. On
/// shutdown it is stopped like the active workers.
async fn drain_worker(
    mut child: tokio::process::Child,
    old: Arc<pool::Worker>,
    timeout: Duration,
    mut stop: tokio::sync::watch::Receiver<()>,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let outcome = loop {
        if old.active() == 0 {
            break "drained".to_string();
        }
        if tokio::time::Instant::now() >= deadline {
            break format!("drain timeout, dropping {} live connections", old.active());
        }
        tokio::select! {
            _ = child.wait() => break "exited while draining".to_string(),
            _ = stop.changed() => {
                stop_worker(&mut child).await;
                break "stopped at shutdown".to_string();
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    };
    let _ = child.kill().await;
    tracing::info!("worker on port {} retired: {}", old.port, outcome);
}

async fn supervise_worker(
    slot: Arc<pool::Slot>,
    mut child: tokio::process::Child,
    make_cmd: WorkerCommand,
    policy: pool::RecyclePolicy,
    drain_timeout: Duration,
    recycles: Arc<tokio::sync::Semaphore>,
    mut stop: tokio::sync::watch::Receiver<()>,
) {
    // Replaced workers still draining; awaited at shutdown so they are stopped
    // gracefully too.
    let mut drains = tokio::task::JoinSet::new();
    let mut tick = tokio::time::interval(RECYCLE_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        while drains.try_join_next().is_some() {}
        let worker = slot.current();
        tokio::select! {
            _ = stop.changed() => {
                tokio::join!(stop_worker(&mut child), async {
                    while drains.join_next().await.is_some() {}
                });
                return;
            }
            status = child.wait() => {
                worker.set_ready(false);
                tracing::warn!("worker on port {} exited: {:?}", worker.port, status);
                let Some(replacement) =
                    respawn_worker(&slot, &make_cmd, policy.max_connections, &stop).await
                else {
                    while drains.join_next().await.is_some() {}
                    return;
                };
                child = replacement;
                continue;
            }
            _ = slot.wake.notified() => {}
            _ = tick.tick() => {}
        }
        if !policy.enabled() {
            continue;
        }
        let rss = policy.max_rss_mb.and(child.id()).and_then(pool::rss_mb);
        let Some(reason) = policy.reason(worker.served(), rss, worker.born.elapsed()) else {
            continue;
        };
        // Bound concurrent replacement startups (not draining); if no permit
        // is free, retry on the next tick and keep serving meanwhile.
        let Ok(permit) = recycles.clone().try_acquire_owned() else {
            continue;
        };
        let started = Instant::now();
        let spawned = free_port().and_then(|port| make_cmd(port).spawn().map(|c| (port, c)));
        let (port, mut next) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                tracing::warn!(
                    "recycle of worker on port {} failed to spawn: {}",
                    worker.port,
                    error
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        if let Err(error) = wait_for_serve_worker(&mut next, port, deadline).await {
            tracing::warn!(
                "recycle of worker on port {} aborted, replacement failed: {}",
                worker.port,
                error
            );
            let _ = next.kill().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let old = slot.replace(pool::Worker::new(
            port,
            policy.max_connections,
            slot.wake.clone(),
        ));
        tracing::info!(
            "recycling worker on port {} (pid {:?}, served {}, live {}, age {}s): {}; replacement on port {} ready in {} ms",
            old.port, child.id(), old.served(), old.active(), old.born.elapsed().as_secs(),
            reason, port, started.elapsed().as_millis()
        );
        let retired = std::mem::replace(&mut child, next);
        drop(permit);
        drains.spawn(drain_worker(retired, old, drain_timeout, stop.clone()));
    }
}

async fn run_multi_worker_serve(
    port: u16,
    host: String,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    font_dirs: Vec<std::path::PathBuf>,
    max_connections: usize,
    policy: pool::RecyclePolicy,
    drain_timeout: Duration,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::TcpListener;

    let exe = std::env::current_exe()?;
    // Register before spawning workers so an early SIGTERM is not fatal to the
    // balancer alone and leaves no orphans.
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    // Claim the public port before starting children so another process cannot
    // take it during worker startup.
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    // Internal worker ports are implementation details. Asking the OS for
    // free ports avoids assuming that every port adjacent to the public one is
    // available (or that `port + workers` cannot overflow).
    let mut reservations = Vec::with_capacity(workers as usize);
    for _ in 0..workers {
        let reservation = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let worker_port = reservation.local_addr()?.port();
        reservations.push((worker_port, reservation));
    }
    let host_for_workers = host.clone();
    let make_cmd: WorkerCommand = Arc::new(move |worker_port: u16| {
        let mut cmd = TokioCommand::new(&exe);
        cmd.kill_on_drop(true);
        cmd.arg("serve").arg("--port").arg(worker_port.to_string());
        // Same limit per worker, not split: connections are not spread evenly,
        // so a split would refuse clients while other workers have room.
        cmd.arg("--max-connections")
            .arg(max_connections.to_string());
        // Workers receive the client-facing Host header through the TCP
        // load balancer. Let their CDP security gate accept that public port
        // while it continues to reject foreign hosts and browser origins.
        cmd.env("INBROWSE_CDP_FORWARDED_HOST", &host_for_workers);
        cmd.env("INBROWSE_CDP_FORWARDED_PORT", port.to_string());
        if let Some(ref p) = proxy {
            // Pass the proxy (which may embed credentials) via the environment,
            // not argv. A --proxy flag is visible in `ps`/`/proc/<pid>/cmdline`
            // to any local user; INBROWSE_PROXY is only readable by the owner
            // (issue #366). The worker's serve path reads this env as a fallback.
            cmd.env("INBROWSE_PROXY", p);
        }
        if let Some(ref ua) = user_agent {
            cmd.arg("--user-agent").arg(ua);
        }
        for directory in &font_dirs {
            cmd.arg("--font-dir").arg(directory);
        }
        if stealth {
            cmd.arg("--stealth");
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::inherit());
        cmd
    });
    let mut children = Vec::new();

    for (index, (worker_port, reservation)) in reservations.into_iter().enumerate() {
        drop(reservation);
        let child = make_cmd(worker_port).spawn()?;
        tracing::info!("Worker {} on port {}", index + 1, worker_port);
        children.push((child, worker_port));
    }

    // Wait only until every worker has bound its control port. The old fixed
    // 500 ms sleep dominated multi-worker startup even when workers were ready
    // in a few milliseconds.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    for (child, worker_port) in children.iter_mut() {
        wait_for_serve_worker(child, *worker_port, deadline).await?;
    }

    // At most a tenth of the pool (minimum one) starts a replacement at a
    // time, so a fleet that reaches its limit together does not stampede the
    // CPU with simultaneous V8 startups.
    let recycles = Arc::new(tokio::sync::Semaphore::new((workers as usize / 10).max(1)));
    let mut slots = Vec::with_capacity(workers as usize);
    let mut supervisors = tokio::task::JoinSet::new();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    for (child, worker_port) in children {
        let wake = Arc::new(tokio::sync::Notify::new());
        let first = pool::Worker::new(worker_port, policy.max_connections, wake.clone());
        let slot = pool::Slot::new(first, wake);
        slots.push(slot.clone());
        supervisors.spawn(supervise_worker(
            slot,
            child,
            make_cmd.clone(),
            policy,
            drain_timeout,
            recycles.clone(),
            stop_rx.clone(),
        ));
    }

    // The load balancer is bound to the requested host, not hardcoded loopback.
    // With --host 0.0.0.0 (e.g. in Docker) the single-worker path already binds
    // all interfaces; the multi-worker balancer must too, or the mapped port is
    // refused from outside the container (issue #336). Workers stay on loopback
    // and are only reached by the balancer.
    tracing::info!("Load balancer on {}:{}, {} workers", host, port, workers);
    if policy.enabled() {
        tracing::info!(
            "Worker recycling: {:?}, drain timeout {}s",
            policy,
            drain_timeout.as_secs()
        );
    }

    let mut next_worker = 0usize;

    loop {
        let (client_stream, peer_addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = &mut shutdown => break,
        };
        if let Err(error) = client_stream.set_nodelay(true) {
            tracing::warn!("client {} TCP_NODELAY failed: {}", peer_addr, error);
        }
        let Some(conn) = pool::pick(&slots, &mut next_worker) else {
            tokio::spawn(async move {
                let mut client = client_stream;
                let _ = client
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n")
                    .await;
                let _ = client.shutdown().await;
            });
            continue;
        };

        let worker_port = conn.port();
        tracing::debug!("Routing {} to worker port {}", peer_addr, worker_port);

        let mut peek_buf = [0u8; 4];
        match client_stream.peek(&mut peek_buf).await {
            Ok(0) => continue,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!("client {} closed before dispatch: {}", peer_addr, error);
                continue;
            }
        }

        if &peek_buf == b"GET " {
            let mut full_peek = [0u8; 256];
            let n = match client_stream.peek(&mut full_peek).await {
                Ok(n) => n,
                Err(error) => {
                    tracing::debug!("client {} closed before dispatch: {}", peer_addr, error);
                    continue;
                }
            };
            let request_line = String::from_utf8_lossy(&full_peek[..n]);

            if request_line.contains("/json") {
                let worker_addr = format!("127.0.0.1:{}", worker_port);
                match tokio::net::TcpStream::connect(&worker_addr).await {
                    Ok(mut worker_stream) => {
                        if let Err(error) = worker_stream.set_nodelay(true) {
                            tracing::warn!("worker {} TCP_NODELAY failed: {}", worker_addr, error);
                        }
                        tokio::spawn(async move {
                            // Held while proxying so a draining worker waits for it.
                            let _conn = conn;
                            let std_stream = match client_stream.into_std() {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to convert client to std stream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let mut client = match tokio::net::TcpStream::from_std(std_stream) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to recreate tokio TcpStream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream)
                                .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("/json worker {} unreachable: {}", worker_addr, e);
                        tokio::spawn(async move {
                            let mut s = client_stream;
                            let _ = s
                                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                                .await;
                            let _ = s.shutdown().await;
                        });
                    }
                }
                continue;
            }
        }

        let worker_addr = format!("127.0.0.1:{}", worker_port);
        conn.count_session();
        tokio::spawn(async move {
            let _conn = conn;
            match tokio::net::TcpStream::connect(&worker_addr).await {
                Ok(mut worker_stream) => {
                    if let Err(error) = worker_stream.set_nodelay(true) {
                        tracing::warn!("worker {} TCP_NODELAY failed: {}", worker_addr, error);
                        return;
                    }
                    let mut client = client_stream;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
                }
                Err(e) => {
                    tracing::warn!("worker {} unreachable: {}", worker_addr, e);
                    let mut s = client_stream;
                    let _ = s
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = s.shutdown().await;
                }
            }
        });
    }

    tracing::info!("Shutting down, stopping workers");
    drop(stop_tx);
    // Workers are stopped in parallel by their supervisors; anything left after
    // the grace period is aborted, and kill_on_drop then kills the child.
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while supervisors.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

/// Resolves on SIGTERM or SIGINT (Ctrl-C elsewhere). Handlers are installed
/// when this is called, not on first poll.
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        Ok(async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async {
            let _ = tokio::signal::ctrl_c().await;
        })
    }
}

/// Ask a worker to exit (SIGTERM lets it flush cookies), then kill it if it is
/// still running after a short grace period.
async fn stop_worker(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain signal send to a child we still own (not yet reaped).
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

/// Ceiling of the adaptive settle (no `--wait`) in seconds: 5, or enough to
/// reach the end of a longer `--settle-timers` horizon.
fn adaptive_settle_secs(settle_timers_ms: Option<u64>) -> u64 {
    const DEFAULT_SETTLE_SECS: u64 = 5;
    settle_timers_ms.map_or(DEFAULT_SETTLE_SECS, |ms| {
        DEFAULT_SETTLE_SECS.max(ms.div_ceil(1000))
    })
}

/// Settle the page. A fixed `--wait` runs the whole delay. The adaptive
/// settle returns once the page is quiet, which can be before one-shot timers
/// due within its ceiling; those are returned so the caller can say so (#13).
async fn settle_page(
    page: &mut Page,
    wait_secs: u64,
    fixed: bool,
) -> Option<inbrowse_js::runtime::PendingTimeouts> {
    let wait_ms = wait_secs.saturating_mul(1000);
    if fixed {
        page.settle_for_duration(wait_ms).await;
        return None;
    }
    let started = Instant::now();
    page.settle(wait_ms).await;
    let remaining_ms = wait_ms.saturating_sub(started.elapsed().as_millis() as u64);
    page.pending_timeouts()
        .filter(|pending| pending.next_ms <= remaining_ms as f64)
}

/// The stderr note for timers the last settle did not wait for; `--quiet`
/// leaves it out.
fn note_settle_left(left: Option<inbrowse_js::runtime::PendingTimeouts>, quiet: bool) {
    if let Some(pending) = left.filter(|_| !quiet) {
        eprintln!("{}", pending_timers_note(pending));
    }
}

fn pending_timers_note(pending: inbrowse_js::runtime::PendingTimeouts) -> String {
    let plural = if pending.count == 1 { "" } else { "s" };
    format!(
        "note: {} timer{plural} pending (next in {:.1}s); use --settle-timers <MS> or --wait <SECS> to wait for page work they start",
        pending.count,
        pending.next_ms / 1000.0
    )
}

fn configure_fetch_navigation_timeout(page: &mut Page, timeout_secs: u64) {
    page.set_navigation_timeout(Duration::from_secs(timeout_secs));
}

async fn run_fetch(
    url_str: &str,
    dump: Option<DumpFormat>,
    selector: Option<String>,
    boilerplate: Option<Boilerplate>,
    wait_secs: u64,
    wait_is_fixed: bool,
    timeout_secs: u64,
    script_timeout_secs: Option<u64>,
    settle_timers_ms: Option<u64>,
    wait_until: &str,
    user_agent: Option<String>,
    cookies: Vec<String>,
    stealth: bool,
    eval: Option<String>,
    output: Option<std::path::PathBuf>,
    quiet: bool,
    proxy: Option<String>,
    storage_dir: Option<std::path::PathBuf>,
    allow_private_network: bool,
    obey_robots: bool,
    screenshot: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // Whether the user explicitly passed --dump. With --eval also present this
    // decides whether we return the eval value or read the page after the
    // eval's async work settles (issue #248).
    let dump_specified = dump.is_some();
    let dump = dump.unwrap_or(DumpFormat::Html);
    if selector.is_some()
        && matches!(
            dump,
            DumpFormat::Original | DumpFormat::Assets | DumpFormat::Cookies
        )
    {
        let name = clap::ValueEnum::to_possible_value(&dump)
            .map(|value| value.get_name().to_string())
            .unwrap_or_default();
        anyhow::bail!(
            "--selector narrows html, text, markdown, json and links output; --dump {name} has no element to narrow to"
        );
    }
    if boilerplate.is_some()
        && !matches!(
            dump,
            DumpFormat::Text | DumpFormat::Markdown | DumpFormat::Json
        )
    {
        anyhow::bail!("--boilerplate applies to --dump text, markdown and json only");
    }
    let drop_boilerplate = boilerplate != Some(Boilerplate::Keep);

    // --dump original short-circuits the browser stack entirely: fetch the raw
    // response body via HTTP and stream the bytes verbatim. Useful for binary
    // payloads (images, fonts, …) and any non-HTML resource where parsing the
    // body through the DOM/JS layer would corrupt or discard data.
    if dump == DumpFormat::Original {
        let bytes =
            fetch_original_bytes(url_str, proxy, user_agent.clone(), timeout_secs, stealth).await?;
        write_or_print_bytes(&bytes, output.as_ref()).await?;
        return Ok(());
    }

    let mut context = BrowserContext::with_storage_and_network(
        "fetch".to_string(),
        proxy,
        stealth,
        user_agent.clone(),
        storage_dir.clone(),
        allow_private_network,
    );
    context.obey_robots = obey_robots;
    // The browser layer refuses file:// unless the context opts in. A local
    // user running `inbrowse fetch file://...` on their own files is the
    // intended case; only network-facing servers (serve, mcp) keep it off.
    context.set_allow_file_access(true);
    let context = Arc::new(context);
    let mut page = Page::new("fetch-page".to_string(), context.clone());
    // Keep the browser's end-to-end navigation ceiling aligned with the CLI
    // request deadline. Previously Page retained its independent 30s default,
    // so `fetch --timeout 50` could still fail after 30 seconds.
    configure_fetch_navigation_timeout(&mut page, timeout_secs);
    if let Some(seconds) = script_timeout_secs {
        page.set_script_timeout(Duration::from_secs(seconds));
    }
    if let Some(ms) = settle_timers_ms {
        page.set_settle_timer_horizon(Duration::from_millis(ms));
    }
    // A screenshot viewport is also the navigation viewport: responsive
    // frameworks must build the DOM for the same dimensions we later paint.
    // Previously page JS saw a randomized screen-sized innerWidth while the
    // screenshot used these values only at the final raster step.
    let screenshot_viewport = screenshot.as_ref().map(|_| {
        let width = std::env::var("INBROWSE_SHOT_W")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1280.0);
        let height = std::env::var("INBROWSE_SHOT_H")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(720.0);
        (width, height)
    });
    if let Some(viewport) = screenshot_viewport {
        page.set_viewport(viewport);
    }

    if let Some(ref ua) = user_agent {
        page.http_client.set_user_agent(ua).await;
    }

    // Seed request cookies (--cookie "name=value") into the jar before
    // navigating, scoped to the target URL's domain, so they're sent on the
    // very first request. Lets callers set server-side preferences such as
    // Amazon's `i18n-prefs=<CUR>` (force marketplace currency over egress geo).
    if !cookies.is_empty() {
        if let Ok(nav_url) = url::Url::parse(url_str) {
            for c in &cookies {
                let c = c.trim();
                if c.is_empty() {
                    continue;
                }
                // Accept "name=value" or a full Set-Cookie string; ensure a path.
                let set_cookie = if c.contains("; ") || c.to_ascii_lowercase().contains("path=") {
                    c.to_string()
                } else {
                    format!("{c}; Path=/")
                };
                context.cookie_jar.set_cookie(&set_cookie, &nav_url);
            }
        } else if !quiet {
            eprintln!("Warning: cannot seed --cookie, invalid URL: {url_str}");
        }
    }

    let wait_condition = inbrowse::lifecycle::WaitUntil::from_str(wait_until);

    if !quiet {
        eprintln!("Fetching {}...", url_str);
    }

    // The paired corpus opts into a truthful capture boundary: its read-only
    // evaluation runs after all settle passes and the final scroll reassert,
    // immediately before screenshot paint. Ordinary CLI evaluation retains
    // its existing evaluate-then-settle behavior when this private variable is
    // absent.
    let eval_at_capture_boundary = screenshot.is_some()
        && eval.is_some()
        && std::env::var("INBROWSE_SHOT_EVAL_AT_CAPTURE").is_ok_and(|value| value == "1");
    let controlled_scroll_request = screenshot.as_ref().and_then(|_| {
        let raw_y = std::env::var("INBROWSE_SHOT_SCROLL_Y").ok()?;
        let x = match std::env::var("INBROWSE_SHOT_SCROLL_X") {
            Ok(raw) => raw.parse::<f64>().ok().filter(|value| value.is_finite())?,
            Err(_) => 0.0,
        };
        let requested_y = if raw_y.eq_ignore_ascii_case("bottom") {
            "document.documentElement.scrollHeight".to_string()
        } else {
            raw_y
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| value.to_string())?
        };
        Some((x, requested_y))
    });

    // Process-level hard deadline. A synchronous hang inside a Rust op invoked
    // from page JS cannot be cancelled by tokio (there is no await to interrupt)
    // nor by the V8 watchdog (terminate_execution only unwinds JS bytecode, not
    // native Rust running beneath a V8->op call). As an absolute backstop so one
    // fetch can never wedge the worker, a daemon thread force-exits if the whole
    // operation overruns navigation + every configured settle pass + grace. A
    // normal fetch returns first and the process exits before this fires.
    {
        let settle_passes = if eval_at_capture_boundary {
            1 + u64::from(controlled_scroll_request.is_some())
        } else if eval.is_some() && (screenshot.is_some() || selector.is_some() || dump_specified) {
            2
        } else {
            1
        };
        let hard = Duration::from_secs(
            timeout_secs
                .saturating_add(wait_secs.saturating_mul(settle_passes))
                .saturating_add(10),
        );
        std::thread::spawn(move || {
            std::thread::sleep(hard);
            eprintln!(
                "inbrowse: hard timeout exceeded ({}s); forcing exit",
                hard.as_secs()
            );
            std::process::exit(124);
        });
    }

    match timeout(
        Duration::from_secs(timeout_secs),
        page.navigate_with_wait(url_str, wait_condition),
    )
    .await
    {
        Ok(result) => result.map_err(|e| {
            anyhow::anyhow!(
                "Failed to navigate to {}: {}",
                url_str,
                operator_network_error(e)
            )
        })?,
        Err(_) => anyhow::bail!(
            "Timed out navigating to {} after {}s",
            url_str,
            timeout_secs
        ),
    }

    // Printed even with --quiet: the page did not run all of its scripts, so
    // what follows describes an incomplete page.
    for warning in page.take_script_warnings() {
        eprintln!("Warning: {warning}");
    }
    if !quiet {
        eprintln!("Page loaded: {} - \"{}\"", page.url_string(), page.title);
    }

    // --wait is a post-load settle: drive the event loop so timers, async work,
    // and completion callbacks (e.g. testharness's add_completion_callback) run
    // before we read the page. Returns early once the loop is idle, so static
    // pages stay fast.
    let mut settle_left = settle_page(&mut page, wait_secs, wait_is_fixed).await;

    // Printed after the screenshot capture, which only the render build has.
    #[cfg(feature = "render")]
    let mut deferred_eval_output = None;
    let initial_controlled_scroll = if eval_at_capture_boundary {
        controlled_scroll_request
            .as_ref()
            .map(|(x, requested_y)| {
                page.evaluate(&format!(
                    "(()=>{{\
                 const requestedX={x},requestedY={requested_y};\
                 const preInitial={{x:window.scrollX,y:window.scrollY}};\
                 window.scrollTo(requestedX,requestedY);\
                 return {{requested:{{x:requestedX,y:requestedY}},\
                 preInitialActual:preInitial,\
                 postInitialActual:{{x:window.scrollX,y:window.scrollY}},\
                 initialBehavior:'authored',\
                 initialPhase:'before-controlled-scroll-settle'}}\
                 }})()"
                ))
            })
            .transpose()
            .map_err(|error| anyhow::anyhow!("capture scroll setup failed: {error}"))?
    } else {
        None
    };
    if initial_controlled_scroll.is_some() {
        settle_left = settle_page(&mut page, wait_secs, wait_is_fixed).await;
    }

    if !eval_at_capture_boundary {
        if let Some(ref expr) = eval {
            let result = evaluate_user_expression(&mut page, &context, expr, timeout_secs).await?;

            // A bare --eval (no --selector, --dump, or --screenshot) returns the
            // eval value directly. Screenshot captures continue below so an
            // evaluation such as scrollTo() affects the painted viewport
            // instead of being silently ignored.
            if !dump_specified && selector.is_none() && screenshot.is_none() {
                note_settle_left(settle_left, quiet);
                let rendered = match result {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                write_or_print(rendered, output.as_ref()).await?;
                context.save_storage();
                return Ok(());
            }
            #[cfg(feature = "render")]
            if screenshot.is_some() {
                deferred_eval_output = Some(result);
            }

            // --eval combined with --selector, --dump, and/or --screenshot
            // typically kicks off async work (a fetch promise, a timer, a scroll
            // listener) that writes the DOM. Drive the event loop again so that
            // work completes, then fall through to selector/capture/dump instead
            // of returning the still-pending eval value (issue #248).
            settle_left = settle_page(&mut page, wait_secs, wait_is_fixed).await;
        }
    }
    note_settle_left(settle_left, quiet);

    // --selector waits for its first match, then narrows the dump to it.
    let scope = match selector {
        Some(ref sel) => {
            if let Some(Err(error)) = page.with_dom(|dom| dom.query_selector(sel)) {
                anyhow::bail!("invalid --selector '{sel}': {error}");
            }
            if !wait_for_selector(&mut page, sel, wait_secs).await {
                anyhow::bail!("--selector '{sel}' matched nothing within {wait_secs}s");
            }
            page.with_dom(|dom| dom.query_selector(sel).ok().flatten())
                .flatten()
        }
        None => None,
    };

    // Token-captcha auto-solve (opt-in: INBROWSE_CAPTCHA_API_KEY). Detects
    // reCAPTCHA/hCaptcha/Turnstile, solves via CapSolver/2Captcha, injects the
    // token + submits. No-op when unconfigured or no token captcha is present.
    // Behavioral systems (PerimeterX/DataDome) are NOT handled here.
    if let Some(cfg) = inbrowse::captcha::CaptchaConfig::from_env() {
        // Token-widget captchas: solve → inject token → submit.
        match inbrowse::captcha::try_solve(&mut page, url_str, &cfg).await {
            Ok(true) => {
                if !quiet {
                    eprintln!("Solved token captcha; waiting for page to proceed");
                }
                page.settle(wait_secs.max(5) * 1000).await;
            }
            Ok(false) => {}
            Err(e) => {
                if !quiet {
                    eprintln!("Captcha solve failed: {e}");
                }
            }
        }
        // Type-B anti-bot challenges (AWS WAF / Cloudflare / DataDome): solve →
        // set the clearance cookie (via inbrowse's egress proxy) → RE-FETCH so the
        // real content loads with the cookie attached.
        match inbrowse::captcha::try_solve_challenge(&mut page, &cfg).await {
            Ok(true) => {
                if !quiet {
                    eprintln!("Solved anti-bot challenge; re-fetching with clearance cookie");
                }
                let wc = inbrowse::lifecycle::WaitUntil::from_str(wait_until);
                let _ = timeout(
                    Duration::from_secs(timeout_secs),
                    page.navigate_with_wait(url_str, wc),
                )
                .await;
                page.settle(wait_secs.max(3) * 1000).await;
            }
            Ok(false) => {}
            Err(e) => {
                if !quiet {
                    eprintln!("Challenge solve failed: {e}");
                }
            }
        }
    }

    // --screenshot renders the settled, optionally evaluated page to a PNG.
    // Requires the render feature; without it, page.screenshot is absent and
    // we report clearly.
    if let Some(ref path) = screenshot {
        #[cfg(feature = "render")]
        {
            let resource_deadline_ms = std::env::var("INBROWSE_RENDER_RESOURCE_DEADLINE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(3_000);
            let _ = page
                .prepare_screenshot_resources(resource_deadline_ms)
                .await;
            // Default CSS-pixel viewport, matching the engine's innerWidth/Height.
            // INBROWSE_SHOT_W / INBROWSE_SHOT_H override it (e.g. a tall viewport to
            // capture below-the-fold content in one shot).
            let viewport = screenshot_viewport.unwrap_or((1280.0, 720.0));
            // Ordinary screenshots sample the live document timeline. The
            // comparison harness can request an exact instant (normally T=0)
            // so both engines paint the same animation frame.
            let requested_animation_sample = std::env::var("INBROWSE_SHOT_ANIMATION_TIME_MS")
                .ok()
                .map(|raw| {
                    let milliseconds = raw.parse::<f32>().map_err(|_| {
                        anyhow::anyhow!(
                            "INBROWSE_SHOT_ANIMATION_TIME_MS must be a finite non-negative number"
                        )
                    })?;
                    if !milliseconds.is_finite() || milliseconds < 0.0 {
                        anyhow::bail!(
                            "INBROWSE_SHOT_ANIMATION_TIME_MS must be a finite non-negative number"
                        );
                    }
                    Ok(inbrowse::AnimationSampleTime { milliseconds })
                })
                .transpose()?;
            let capture_screenshot = |page: &Page| match requested_animation_sample {
                Some(sample) => page.screenshot_at_animation_time(viewport, sample),
                None => page.screenshot(viewport),
            };
            // The parity harness performs one throwaway paint in both engines
            // before observing image/font readiness. Inbrowse resolves retained
            // render resources during prepare/paint, so sampling first would
            // compare pre-paint Inbrowse state with post-load Chromium state.
            // Keep this private opt-in out of ordinary CLI screenshots.
            let warmup_capture =
                std::env::var("INBROWSE_SHOT_RESOURCE_WARMUP").is_ok_and(|value| value == "1");
            if warmup_capture {
                if capture_screenshot(&page).is_none() {
                    anyhow::bail!("resource warm-up screenshot failed: page has no DOM to render");
                }
                // Give completion callbacks one bounded task turn before the
                // capture-boundary evaluation reads resource state.
                page.settle(1).await;
            }
            // Paired renderer captures need a stable final coordinate after
            // the post-eval settle. Authored smooth scrolling and scroll
            // anchoring may legitimately move an earlier scrollTo while the
            // page changes above the viewport, so the comparison harness opts
            // into one instant reassertion at the actual capture boundary.
            // Ordinary CLI screenshots are unchanged when these private
            // capture-environment variables are absent.
            let controlled_scroll = controlled_scroll_request
                .as_ref()
                .map(|(x, requested_y)| {
                    page.evaluate(&format!(
                        "(()=>{{\
                         const requestedX={x},requestedY={requested_y};\
                         const preReassert={{x:window.scrollX,y:window.scrollY}};\
                         const root=document.documentElement;\
                         const previous=root?root.style.getPropertyValue('scroll-behavior'):'';\
                         const priority=root?root.style.getPropertyPriority('scroll-behavior'):'';\
                         if(root)root.style.setProperty('scroll-behavior','auto','important');\
                         window.scrollTo(requestedX,requestedY);\
                         if(root){{if(previous)root.style.setProperty('scroll-behavior',previous,priority);\
                         else root.style.removeProperty('scroll-behavior')}}\
                         return {{requested:{{x:requestedX,y:requestedY}},\
                         preReassertActual:preReassert,\
                         finalReassertActual:{{x:window.scrollX,y:window.scrollY}},\
                         behavior:'instant',\
                         phase:'immediately-before-capture-state-and-screenshot'}}\
                         }})()"
                    ))
                })
                .transpose()
                .map_err(|error| anyhow::anyhow!("capture scroll reassertion failed: {error}"))?;
            if eval_at_capture_boundary {
                if let Some(ref expr) = eval {
                    deferred_eval_output = Some(
                        evaluate_user_expression(&mut page, &context, expr, timeout_secs).await?,
                    );
                }
            }
            let capture_state = deferred_eval_output
                .as_ref()
                .map(|_| {
                    page.evaluate(
                        "(()=>({\
                         scrollX:window.scrollX,scrollY:window.scrollY,\
                         innerWidth:window.innerWidth,innerHeight:window.innerHeight,\
                         scrollWidth:document.documentElement?document.documentElement.scrollWidth:0,\
                         scrollHeight:document.documentElement?document.documentElement.scrollHeight:0\
                         }))()",
                    )
                })
                .transpose()
                .map_err(|error| anyhow::anyhow!("capture state sample failed: {error}"))?;
            match capture_screenshot(&page) {
                Some(bytes) => std::fs::write(path, &bytes)?,
                None => anyhow::bail!("screenshot failed: page has no DOM to render"),
            }
            // A screenshot+eval command used to ignore the expression
            // completely. Emit both its value and a standard state sampled
            // after the post-eval settle so automation can record the exact
            // live viewport that was painted.
            if let Some(result) = deferred_eval_output {
                let mut controlled_scroll_report = controlled_scroll;
                if let (Some(report), Some(initial)) = (
                    controlled_scroll_report.as_mut(),
                    initial_controlled_scroll.as_ref(),
                ) {
                    if let (Some(report), Some(initial)) =
                        (report.as_object_mut(), initial.as_object())
                    {
                        for key in [
                            "preInitialActual",
                            "postInitialActual",
                            "initialBehavior",
                            "initialPhase",
                        ] {
                            if let Some(value) = initial.get(key) {
                                report.insert(key.to_string(), value.clone());
                            }
                        }
                    }
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "evaluation": result,
                        "controlledScroll": controlled_scroll_report,
                        "resourceWarmup": {
                            "performed": warmup_capture,
                            "discardedShots": if warmup_capture { 1 } else { 0 },
                            "taskTurnMs": if warmup_capture { 1 } else { 0 },
                            "phase": "before-final-scroll-reassert-and-state-sample",
                        },
                        "captureState": capture_state.unwrap_or(serde_json::Value::Null),
                    })
                );
            }
            if !quiet {
                eprintln!(
                    "Screenshot written: {} ({} bytes)",
                    path.display(),
                    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
                );
            }
            context.save_storage();
            return Ok(());
        }
        #[cfg(not(feature = "render"))]
        {
            anyhow::bail!(
                "--screenshot {} requires a build with the render feature (cargo build --features render)",
                path.display()
            );
        }
    }

    let rendered = match dump {
        DumpFormat::Html => dump_html(&page, scope),
        DumpFormat::Text => dump_text(&mut page, scope, drop_boilerplate),
        DumpFormat::Links => dump_links(&mut page, scope),
        DumpFormat::Markdown => dump_markdown(&mut page, selector.as_deref(), drop_boilerplate)?,
        DumpFormat::Json => dump_json(&mut page, selector.as_deref(), drop_boilerplate)?,
        DumpFormat::Assets => dump_assets(&page),
        DumpFormat::Cookies => dump_cookies(&page),
        // Handled above via the short-circuit branch; unreachable here.
        DumpFormat::Original => unreachable!("Original dump handled before page navigation"),
    };
    write_or_print(rendered, output.as_ref()).await?;

    // Save cookies to disk if storage_dir is configured
    context.save_storage();

    Ok(())
}

async fn fetch_original_response(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
    stealth: bool,
) -> anyhow::Result<inbrowse_net::Response> {
    let url = url::Url::parse(url_str)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url_str, e))?;

    // `--dump original` short-circuits the browser stack (see run_fetch) and
    // builds its own client here instead of going through BrowserContext /
    // Page::do_fetch, which is where --stealth is normally applied. Without
    // this, the request stays on plain HTTP/1.1 with no TLS impersonation
    // regardless of --stealth (issue #482). file:// has no TLS handshake to
    // impersonate and the wreq client only speaks http(s), so it is excluded
    // here the same way InbrowseHttpClient::fetch_with_method excludes it
    // internally.
    if stealth && url.scheme() != "file" {
        #[cfg(feature = "stealth")]
        {
            // `false` matches the reqwest path below (`with_options`); the
            // CLI mirrors --allow-private-network into
            // INBROWSE_ALLOW_PRIVATE_NETWORK at startup, which this client
            // honours.
            let client = inbrowse_net::StealthHttpClient::with_proxy(
                Arc::new(inbrowse_net::CookieJar::new()),
                proxy.as_deref(),
                false,
            )
            .with_user_agent(user_agent);
            return match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => {
                    anyhow::bail!("Failed to fetch {}: {}", url_str, operator_network_error(e))
                }
                Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
            };
        }
    }

    let client = inbrowse_net::InbrowseHttpClient::with_options(
        Arc::new(inbrowse_net::CookieJar::new()),
        proxy.as_deref(),
    );
    // Same local-user opt-in as `fetch`: `--dump original file://...` reads
    // the user's own file. The client refuses file:// unless told otherwise.
    client.set_allow_file_access(true);
    if let Some(ua) = user_agent {
        client.set_user_agent(&ua).await;
    }

    match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, operator_network_error(e)),
        Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
    }
}

async fn fetch_original_bytes(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
    stealth: bool,
) -> anyhow::Result<Vec<u8>> {
    Ok(
        fetch_original_response(url_str, proxy, user_agent, timeout_secs, stealth)
            .await?
            .body,
    )
}

/// What `inbrowse extract` was asked to do with the document, besides where it
/// is: the `--hint`, the `--password` (as extractor options) and the `--timeout`.
/// Built in `run_cli`, read by `run_extract`.
#[cfg(feature = "extract")]
struct ExtractChoices {
    hint: Option<String>,
    options: inbrowse_extract::Options,
    timeout_secs: u64,
}

/// `inbrowse extract`: read or download one document and print its text as
/// JSON (inbrowse-extract crate). Downloads go through
/// `fetch_original_response`, so --proxy, --stealth, --user-agent and the
/// private-network gate behave as for `fetch --dump original`.
#[cfg(feature = "extract")]
async fn run_extract(
    target: &str,
    choices: ExtractChoices,
    user_agent: Option<String>,
    proxy: Option<String>,
    stealth: bool,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let ExtractChoices {
        hint,
        options,
        timeout_secs,
    } = choices;
    // Backstop as in run_fetch: a parser stuck in native code on a hostile
    // document cannot be cancelled by the timeout below.
    let hard = Duration::from_secs(timeout_secs.saturating_add(5));
    std::thread::spawn(move || {
        std::thread::sleep(hard);
        eprintln!(
            "inbrowse: hard timeout exceeded ({}s); forcing exit",
            hard.as_secs()
        );
        std::process::exit(124);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);

    // A Windows path such as C:\a.pdf parses as a URL with scheme "c".
    let is_url =
        url::Url::parse(target).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "file"));
    let (bytes, hint) = if is_url {
        let resp =
            fetch_original_response(target, proxy, user_agent, timeout_secs, stealth).await?;
        if resp.status >= 400 {
            anyhow::bail!("{} returned HTTP {}", target, resp.status);
        }
        let hint = hint
            .or_else(|| resp.headers.get("content-type").cloned())
            .or_else(|| {
                resp.url
                    .path_segments()
                    .and_then(|mut s| s.next_back())
                    .map(String::from)
            });
        (resp.body, hint)
    } else {
        let path = std::path::Path::new(target);
        let limit = inbrowse_extract::Limits::default().max_input_bytes as u64;
        let len = tokio::fs::metadata(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
            .len();
        if len > limit {
            anyhow::bail!(
                "{} is {} bytes; the limit is {}",
                path.display(),
                len,
                limit
            );
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
        let hint = hint.or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()));
        (bytes, hint)
    };

    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let work = tokio::task::spawn_blocking(move || {
        inbrowse_extract::extract_with_limits(
            &bytes,
            hint.as_deref(),
            &inbrowse_extract::Limits::default(),
            &options,
        )
    });
    let document = match timeout(remaining, work).await {
        Ok(joined) => joined.map_err(|e| anyhow::anyhow!("extraction task failed: {}", e))??,
        Err(_) => anyhow::bail!("Timed out extracting {} after {}s", target, timeout_secs),
    };
    let mut json = serde_json::to_vec(&document)?;
    json.push(b'\n');
    write_or_print_bytes(&json, output.as_ref()).await
}

/// API key for `source` from the user's own environment, as the per-call
/// `credentials` object the MCP tool takes. Nothing is stored.
#[cfg(feature = "social")]
fn social_credentials(source: &str) -> Option<serde_json::Value> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (provider, key, secret) = match source {
        "reddit" => (
            "reddit",
            var("INBROWSE_REDDIT_CLIENT_ID")?,
            var("INBROWSE_REDDIT_CLIENT_SECRET"),
        ),
        "x" | "twitter" => ("x", var("INBROWSE_X_BEARER_TOKEN")?, None),
        "github" => ("github", var("INBROWSE_GITHUB_TOKEN")?, None),
        _ => return None,
    };
    let mut c = serde_json::json!({ "kind": "api_key", "provider": provider, "key": key });
    if let Some(secret) = secret {
        c["secret"] = serde_json::Value::String(secret);
    }
    Some(c)
}

/// `inbrowse social`: the MCP `social_search` / `social_thread` tools on a
/// browser context built from the CLI flags (inbrowse-mcp `social::call`, one
/// code path for both). Cookies come from --cookies and/or the --storage-dir
/// profile; API keys from `social_credentials`.
#[cfg(feature = "social")]
async fn run_social(
    action: SocialAction,
    user_agent: Option<String>,
    proxy: Option<String>,
    stealth: bool,
    storage_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let (tool, args, cookies, output) = match action {
        SocialAction::Search {
            source,
            query,
            days,
            limit,
            sort,
            community,
            cookies,
            output,
        } => {
            let source = source.to_ascii_lowercase();
            let mut args = serde_json::json!({
                "source": source, "query": query, "days": days, "limit": limit, "sort": sort
            });
            if let Some(c) = community {
                args["community"] = serde_json::Value::String(c);
            }
            if let Some(c) = social_credentials(&source) {
                args["credentials"] = c;
            }
            ("social_search", args, cookies, output)
        }
        SocialAction::Thread {
            url,
            comments,
            cookies,
            output,
        } => {
            let host = url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .unwrap_or_default();
            let source = if host.ends_with("reddit.com") || host.ends_with("redd.it") {
                "reddit"
            } else if host.ends_with("github.com") {
                "github"
            } else {
                ""
            };
            let mut args = serde_json::json!({ "url": url, "comments": comments });
            if let Some(c) = social_credentials(source) {
                args["credentials"] = c;
            }
            ("social_thread", args, cookies, output)
        }
    };
    let context = BrowserContext::with_storage_and_network(
        "social".to_string(),
        proxy,
        stealth,
        user_agent,
        storage_dir,
        false,
    );
    if let Some(path) = cookies {
        let count = context.cookie_jar.load_from_file(&path).map_err(|e| {
            anyhow::anyhow!("Failed to read cookies from {}: {}", path.display(), e)
        })?;
        if count == 0 {
            anyhow::bail!("{} holds no cookies", path.display());
        }
    }
    let context = Arc::new(context);
    let result = inbrowse_mcp::social::call(tool, &args, &context)
        .await
        .expect("social tool name");
    let text = result.map_err(|e| anyhow::anyhow!(e))?;
    let mut bytes = text.into_bytes();
    bytes.push(b'\n');
    write_or_print_bytes(&bytes, output.as_ref()).await
}

/// `inbrowse media`: read or download one video, audio or WebVTT file, write
/// frames and audio with inbrowse-media, and print the report as JSON with an
/// added `out_dir`. Downloads go through `fetch_original_response`, as in
/// `run_extract` (its sibling for documents).
#[cfg(feature = "media")]
#[allow(clippy::too_many_arguments)]
async fn run_media(
    target: &str,
    out_dir: Option<std::path::PathBuf>,
    mut options: inbrowse_media::Options,
    ocr: bool,
    timeout_secs: u64,
    user_agent: Option<String>,
    proxy: Option<String>,
    stealth: bool,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // Backstop as in run_extract: a decoder stuck in native code cannot be
    // cancelled by the deadline below.
    let hard = Duration::from_secs(timeout_secs.saturating_add(5));
    std::thread::spawn(move || {
        std::thread::sleep(hard);
        eprintln!(
            "inbrowse: hard timeout exceeded ({}s); forcing exit",
            hard.as_secs()
        );
        std::process::exit(124);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);

    let is_url =
        url::Url::parse(target).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "file"));
    let (bytes, hint) = if is_url {
        let resp =
            fetch_original_response(target, proxy, user_agent, timeout_secs, stealth).await?;
        if resp.status >= 400 {
            anyhow::bail!("{} returned HTTP {}", target, resp.status);
        }
        let hint = resp
            .url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .map(String::from);
        (resp.body, hint)
    } else {
        let path = std::path::Path::new(target);
        let len = tokio::fs::metadata(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
            .len();
        if len > options.max_input_bytes as u64 {
            anyhow::bail!(
                "{} is {} bytes; the limit is {}",
                path.display(),
                len,
                options.max_input_bytes
            );
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
        (
            bytes,
            path.file_name().map(|n| n.to_string_lossy().into_owned()),
        )
    };

    let out_dir = match out_dir {
        Some(dir) => dir,
        None => inbrowse_media::new_output_dir()?,
    };
    options.deadline = Some(deadline);
    let dir = out_dir.clone();
    let work = tokio::task::spawn_blocking(move || {
        inbrowse_media::extract(bytes.into(), hint.as_deref(), &dir, &options)
    });
    // Decoding stops itself at the deadline; the margin lets it write what it has.
    let remaining =
        deadline.saturating_duration_since(std::time::Instant::now()) + Duration::from_secs(3);
    let report = match timeout(remaining, work).await {
        Ok(joined) => joined.map_err(|e| anyhow::anyhow!("media task failed: {}", e))??,
        Err(_) => anyhow::bail!("Timed out reading {} after {}s", target, timeout_secs),
    };
    let mut value = serde_json::to_value(&report)?;
    if let Some(map) = value.as_object_mut() {
        map.insert(
            "out_dir".into(),
            serde_json::Value::String(out_dir.to_string_lossy().into_owned()),
        );
    }
    // Same code path as the MCP `media_extract` tool's `ocr` option.
    #[cfg(feature = "ocr")]
    if ocr {
        inbrowse_mcp::ocr::annotate_frames(&mut value, Some(deadline)).await;
    }
    #[cfg(not(feature = "ocr"))]
    let _ = ocr;
    let mut json = serde_json::to_vec(&value)?;
    json.push(b'\n');
    write_or_print_bytes(&json, output.as_ref()).await
}

/// `inbrowse ocr`: read or download one image and print its text as JSON,
/// through the same functions as the MCP `image_ocr` tool (inbrowse-mcp
/// `ocr`, inbrowse-ocr). Downloads go through `fetch_original_response`, as
/// in `run_extract` and `run_media`.
#[cfg(feature = "ocr")]
async fn run_ocr(
    target: &str,
    timeout_secs: u64,
    user_agent: Option<String>,
    proxy: Option<String>,
    stealth: bool,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let hard = Duration::from_secs(timeout_secs.saturating_add(5));
    std::thread::spawn(move || {
        std::thread::sleep(hard);
        eprintln!(
            "inbrowse: hard timeout exceeded ({}s); forcing exit",
            hard.as_secs()
        );
        std::process::exit(124);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let limit = inbrowse_mcp::ocr::MAX_INPUT_BYTES;

    let is_url =
        url::Url::parse(target).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "file"));
    let bytes = if is_url {
        let resp =
            fetch_original_response(target, proxy, user_agent, timeout_secs, stealth).await?;
        if resp.status >= 400 {
            anyhow::bail!("{} returned HTTP {}", target, resp.status);
        }
        resp.body
    } else {
        let path = std::path::Path::new(target);
        let len = tokio::fs::metadata(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
            .len();
        if len > limit {
            anyhow::bail!(
                "{} is {} bytes; the limit is {}",
                path.display(),
                len,
                limit
            );
        }
        tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
    };
    let result = inbrowse_mcp::ocr::recognize(&bytes, Some(deadline))
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut json = serde_json::to_vec(&inbrowse_mcp::ocr::result_json(target, &result))?;
    json.push(b'\n');
    write_or_print_bytes(&json, output.as_ref()).await
}

/// Read newline-delimited URLs from `path` (or stdin when `path` is `-`), as
/// parsed by [`parse_url_list`]. Callers: `fetch --file` (run_cli) and
/// [`scrape_url_list`].
fn read_urls_from_file(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let content = if path == std::path::Path::new("-") {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| anyhow::anyhow!("Failed to read URLs from stdin: {}", e))?;
        s
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
    };
    Ok(parse_url_list(&content))
}

/// One URL per line. Blank lines and `#` comments are dropped, and surrounding
/// whitespace is trimmed so a list copy-pasted with indentation still works.
fn parse_url_list(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// The URL list for `scrape`: positional URLs in order, a positional `-`
/// replaced by the list read from stdin, then the list from `--file` (a path,
/// or `-` for stdin). Stdin can be read only once, so naming it twice is an
/// error.
fn scrape_url_list(
    positional: Vec<String>,
    file: Option<&std::path::Path>,
) -> anyhow::Result<Vec<String>> {
    let stdin = std::path::Path::new("-");
    let stdin_uses = positional.iter().filter(|url| url.as_str() == "-").count()
        + usize::from(file == Some(stdin));
    if stdin_uses > 1 {
        anyhow::bail!("stdin can be read only once: pass `-` as a URL or `--file -`, not both");
    }
    let mut urls = Vec::with_capacity(positional.len());
    for url in positional {
        if url == "-" {
            urls.extend(read_urls_from_file(stdin)?);
        } else {
            urls.push(url);
        }
    }
    if let Some(path) = file {
        urls.extend(read_urls_from_file(path)?);
    }
    Ok(urls)
}

/// Batch raw fetch: run `--dump original` over many URLs concurrently and print
/// one JSON status line per URL (issue #349). This is the raw-resource-check
/// counterpart to `scrape`; it never renders, so there is no browser/JS cost
/// per URL. Output stays in input order regardless of completion order.
async fn run_batch_fetch(
    urls: Vec<String>,
    concurrency: usize,
    timeout_secs: u64,
    user_agent: Option<String>,
    proxy: Option<String>,
    output: Option<std::path::PathBuf>,
    quiet: bool,
    stealth: bool,
) -> anyhow::Result<()> {
    let total = urls.len();
    if total == 0 {
        anyhow::bail!("No URLs to fetch (--file was empty).");
    }

    if !quiet {
        eprintln!(
            "Fetching {} URLs with {} concurrent request(s) (per-fetch timeout: {}s)...",
            total, concurrency, timeout_secs
        );
    }

    let start = Instant::now();
    let user_agent = Arc::new(user_agent);
    let proxy = Arc::new(proxy);

    let completed = batch::run_bounded(urls.into_iter().enumerate(), concurrency, |(i, url)| {
        let user_agent = user_agent.clone();
        let proxy = proxy.clone();

        async move {
            let task_start = Instant::now();
            let result = fetch_original_response(
                &url,
                (*proxy).clone(),
                (*user_agent).clone(),
                timeout_secs,
                stealth,
            )
            .await;
            let elapsed_ms = task_start.elapsed().as_millis();

            let line = match result {
                Ok(resp) => serde_json::json!({
                    "url": url,
                    "ok": (200..400).contains(&resp.status),
                    "status": resp.status,
                    "content_type": resp.headers.get("content-type").cloned().unwrap_or_default(),
                    "bytes": resp.body.len(),
                    "elapsed_ms": elapsed_ms,
                }),
                Err(e) => serde_json::json!({
                    "url": url,
                    "ok": false,
                    "error": e.to_string(),
                    "elapsed_ms": elapsed_ms,
                }),
            };
            (i, line)
        }
    })
    .await;

    let mut results: Vec<Option<serde_json::Value>> = vec![None; total];
    let mut failures = 0usize;
    for result in completed {
        if let Ok((i, line)) = result {
            if !line["ok"].as_bool().unwrap_or(false) {
                failures += 1;
            }
            results[i] = Some(line);
        } else {
            failures += 1;
        }
    }

    let mut out = String::new();
    for line in results.into_iter().flatten() {
        out.push_str(&serde_json::to_string(&line).unwrap_or_default());
        out.push('\n');
    }

    if let Some(path) = output {
        tokio::fs::write(&path, out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
        stdout
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to flush stdout: {}", e))?;
    }

    if !quiet {
        eprintln!(
            "Done: {} URLs in {:.1}s ({} ok, {} failed).",
            total,
            start.elapsed().as_secs_f64(),
            total - failures,
            failures
        );
    }

    Ok(())
}

async fn write_or_print(
    content: String,
    output: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    if let Some(path) = output {
        tokio::fs::write(path, content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        println!("{}", content);
    }
    Ok(())
}

async fn write_or_print_bytes(
    bytes: &[u8],
    output: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    if let Some(path) = output {
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        // Write raw bytes to stdout — never println! (would append a newline
        // and break binary payloads like JPEG/PNG).
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
        stdout
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to flush stdout: {}", e))?;
    }
    Ok(())
}

async fn wait_for_selector(page: &mut Page, selector: &str, timeout_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        let found = page
            .with_dom(|dom| dom.query_selector(selector).ok().flatten().is_some())
            .unwrap_or(false);

        if found {
            return true;
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        // The selector may be created by a timer, dynamic import, or fetch
        // completion. Sleeping without pumping V8 makes those callbacks unable
        // to run, so a valid selector wait always times out. Drive one bounded
        // event-loop slice, then retain a 100ms polling cadence if it returned
        // idle immediately.
        let slice_started = tokio::time::Instant::now();
        page.settle(100).await;
        let spent = slice_started.elapsed();
        let cadence = tokio::time::Duration::from_millis(100);
        if spent < cadence {
            tokio::time::sleep(cadence - spent).await;
        }
    }
}

fn dump_cookies(page: &Page) -> String {
    let cookies = page.context.cookie_jar.get_all_cookies();
    serde_json::to_string_pretty(&cookies).unwrap_or_else(|_| "[]".to_string())
}

/// The document's HTML, or with `scope` (`--selector`) that element's.
fn dump_html(page: &Page, scope: Option<inbrowse_dom::NodeId>) -> String {
    page.with_dom(|dom| {
        if let Some(root) = scope {
            dom.outer_html(root)
        } else if let Ok(Some(html_node)) = dom.query_selector("html") {
            let html = dom.outer_html(html_node);
            format!("<!DOCTYPE html>\n{}", html)
        } else {
            let doc = dom.document();
            dom.inner_html(doc)
        }
    })
    .unwrap_or_default()
}

/// Whether anything but the browser's own stylesheet can style the page: a
/// `<style>` or stylesheet `<link>`, a `style` attribute, or a `<dialog>`,
/// which the browser's stylesheet hides until it is open. Without one the
/// style cascade has nothing to add to markup (`hidden`).
#[cfg(feature = "render")]
fn has_author_styles(dom: &inbrowse_dom::DomTree) -> bool {
    dom.descendants(dom.document())
        .into_iter()
        .filter_map(|id| dom.get_node(id))
        .any(|node| {
            node.get_attribute("style").is_some()
                || node
                    .as_element()
                    .is_some_and(|name| match name.local.as_ref() {
                        "style" | "dialog" => true,
                        "link" => node.get_attribute("rel").is_some_and(|rel| {
                            rel.split_ascii_whitespace()
                                .any(|kind| kind.eq_ignore_ascii_case("stylesheet"))
                        }),
                        _ => false,
                    })
        })
}

/// The body's readable text, or with `scope` (`--selector`) that element's.
fn dump_text(
    page: &mut Page,
    scope: Option<inbrowse_dom::NodeId>,
    drop_boilerplate: bool,
) -> String {
    // Render builds cascade the page's styles to find what it hides, unless
    // nothing could style it; others hide by markup (`hidden`) only.
    #[cfg(feature = "render")]
    let styles = if page.with_dom(has_author_styles).unwrap_or(false) {
        page.js
            .as_ref()
            .map(|js| js.accessibility_styles())
            .unwrap_or_default()
    } else {
        Default::default()
    };
    #[cfg(not(feature = "render"))]
    let styles = std::collections::HashMap::new();
    page.with_dom(|dom| {
        let root = scope.or_else(|| dom.query_selector("body").ok().flatten());
        root.map(|root| {
            extract_readable_text(dom, root, drop_boilerplate, &styles)
                .trim()
                .to_string()
        })
        .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Run a `--eval` expression for `run_fetch` through `Page::evaluate_checked`
/// (DevTools console semantics, promise awaited, `timeout_secs` for the run and
/// the wait together). A thrown error, rejected promise, syntax error or
/// timeout fails the command with the JavaScript message; storage is saved
/// first, since whatever the script wrote before it threw did happen.
async fn evaluate_user_expression(
    page: &mut Page,
    context: &BrowserContext,
    expression: &str,
    timeout_secs: u64,
) -> anyhow::Result<serde_json::Value> {
    match page
        .evaluate_checked(expression, Duration::from_secs(timeout_secs))
        .await
    {
        Ok(value) => Ok(value),
        Err(error) => {
            context.save_storage();
            Err(anyhow::anyhow!("--eval failed: {error}"))
        }
    }
}

/// The page as one JSON object (`--dump json`), pretty-printed.
fn dump_json(
    page: &mut Page,
    selector: Option<&str>,
    drop_boilerplate: bool,
) -> anyhow::Result<String> {
    let json = inbrowse::page_json::page_json(page, selector, drop_boilerplate, Page::evaluate)
        .map_err(|error| anyhow::anyhow!("page JSON failed: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("--selector matched nothing"))?;
    Ok(serde_json::to_string_pretty(&json)?)
}

/// The body as markdown, or with `selector` (`--selector`) its first match.
fn dump_markdown(
    page: &mut Page,
    selector: Option<&str>,
    drop_boilerplate: bool,
) -> anyhow::Result<String> {
    let script = inbrowse_js::markdown::html_to_markdown_js(selector, drop_boilerplate);
    let result = page
        .evaluate(&script)
        .map_err(|error| anyhow::anyhow!("markdown conversion failed: {error}"))?;
    result
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("markdown conversion did not return text"))
}

fn is_html_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')
}

/// Builds `--dump text` output following the `innerText` algorithm: text runs
/// separated by collapsible whitespace, required line breaks that merge with
/// each other (the largest request wins, none at the start or end), literal
/// newlines from `<br>`, and tabs between table cells. Used only by
/// [`extract_readable_text`].
#[derive(Default)]
struct ReadableText {
    out: String,
    /// Whitespace seen since the last text: becomes one space before the next.
    pending_space: bool,
    /// Largest line-break count requested since the last text.
    pending_breaks: u8,
    /// A table cell ended since the last text in the same row.
    pending_tab: bool,
}

impl ReadableText {
    fn request_breaks(&mut self, count: u8) {
        self.pending_breaks = self.pending_breaks.max(count);
        self.pending_space = false;
    }

    /// A literal line break (`<br>`): never merged with requested breaks.
    fn push_newline(&mut self) {
        self.flush_separators();
        self.out.push('\n');
        self.pending_space = false;
    }

    /// Text of a node: collapsed unless `preformatted` (inside `<pre>`).
    fn push_text(&mut self, contents: &str, preformatted: bool) {
        if preformatted {
            if !contents.is_empty() {
                self.flush_separators();
                self.out.push_str(contents);
            }
            return;
        }
        let trimmed = contents.trim_matches(is_html_whitespace);
        let begins_with_space = contents.starts_with(is_html_whitespace);
        if trimmed.is_empty() {
            self.pending_space |= begins_with_space;
            return;
        }
        // Read before flushing: flushing clears the pending separators.
        let breaks = self.pending_breaks > 0;
        let tab = self.pending_tab;
        let space = self.pending_space || begins_with_space;
        self.flush_separators();
        if !breaks
            && !tab
            && space
            && !self.out.is_empty()
            && !self.out.ends_with(char::is_whitespace)
        {
            self.out.push(' ');
        }
        self.out.push_str(trimmed);
        self.pending_space = contents.ends_with(is_html_whitespace);
    }

    /// Emits the line breaks or the tab owed before the next text. Breaks
    /// owed before any text are dropped, as are trailing spaces before them.
    fn flush_separators(&mut self) {
        if self.pending_breaks > 0 {
            if !self.out.is_empty() {
                let kept = self.out.trim_end_matches([' ', '\t']).len();
                self.out.truncate(kept);
                for _ in 0..self.pending_breaks {
                    self.out.push('\n');
                }
            }
        } else if self.pending_tab && !self.out.is_empty() {
            self.out.push('\t');
        }
        self.pending_breaks = 0;
        self.pending_tab = false;
        self.pending_space = false;
    }
}

/// Whether an element is page boilerplate: navigation, the header and footer
/// of the page, and sidebars. A header or footer inside an article, main or
/// section belongs to it, and so does an aside inside an article or main.
/// `role` (banner, contentinfo, navigation, complementary) overrides the tag.
/// The markdown converter applies the same rule (`isBoilerplate`).
fn is_boilerplate(tag: &str, role: Option<&str>, in_article: bool, in_section: bool) -> bool {
    let kind = match role.map(str::to_ascii_lowercase).as_deref() {
        Some("navigation") => "nav",
        Some("complementary") => "aside",
        Some("banner") => "header",
        Some("contentinfo") => "footer",
        _ => tag,
    };
    match kind {
        "nav" => true,
        "aside" => !in_article,
        "header" | "footer" => !in_article && !in_section,
        _ => false,
    }
}

/// The text a reader sees in the subtree at `node_id`, for `--dump text`
/// (`dump_text`). Follows the `innerText` rules ([`ReadableText`]) with one
/// deliberate addition: with `drop_boilerplate`, page boilerplate
/// ([`is_boilerplate`]) is dropped. `script`, `style`, `template`, `noscript`,
/// `hidden` subtrees, subtrees `styles` give `display: none`, text under
/// `visibility: hidden` (a descendant can set it visible again) and the
/// options of a `<select>` other than its selected one are not text. `styles`
/// is empty when the build has no style cascade; only markup hides then.
fn extract_readable_text(
    dom: &inbrowse_dom::DomTree,
    node_id: inbrowse_dom::NodeId,
    drop_boilerplate: bool,
    styles: &std::collections::HashMap<
        inbrowse_dom::NodeId,
        inbrowse_js::runtime::AccessibilityStyle,
    >,
) -> String {
    use inbrowse_dom::NodeData;

    // Iterative DFS over an explicit work stack. A recursive walk overflowed the
    // call stack (a hard abort, not a catchable panic) on deeply nested pages,
    // taking down the process on `--dump text` (issue #362, the CLI counterpart
    // of the serialize/textContent paths made iterative in inbrowse-dom). The
    // `Leave*` items run after an element's children, since the stack is LIFO.
    enum Work {
        /// A node and whether `visibility: hidden` is inherited from above it.
        Visit(inbrowse_dom::NodeId, bool),
        LeaveBlock(u8),
        LeaveCell,
        LeavePre,
        LeaveArticle,
        LeaveSection,
    }

    /// Whether `tag` opens an article or main (`true`) or a section (`false`).
    fn content_kind(tag: &str, role: Option<&str>) -> Option<bool> {
        match (tag, role) {
            ("article" | "main", _) | (_, Some("main" | "article")) => Some(true),
            ("section", _) => Some(false),
            _ => None,
        }
    }

    // Defense-in-depth cap mirroring DomTree::descendants; never reached on a
    // valid tree since append_child / insert_before reject cycles.
    const MAX_NODES: usize = 5_000_000;

    let mut text = ReadableText::default();
    let mut preformatted = 0usize;
    // Open articles and sections around the node being visited, starting with
    // those around `node_id` itself.
    let (mut in_article, mut in_section) = (0usize, 0usize);
    // The nearest ancestor that sets `visibility` decides what `node_id` inherits.
    let mut inherited_hidden = None;
    let mut ancestor = dom.get_node(node_id).and_then(|node| node.parent);
    while let Some(id) = ancestor {
        let Some(node) = dom.get_node(id) else { break };
        if inherited_hidden.is_none() {
            inherited_hidden = styles.get(&id).and_then(|style| style.visibility_hidden);
        }
        if let Some(name) = node.as_element() {
            match content_kind(name.local.as_ref(), node.get_attribute("role")) {
                Some(true) => in_article += 1,
                Some(false) => in_section += 1,
                None => {}
            }
        }
        ancestor = node.parent;
    }
    let mut stack: Vec<Work> = vec![Work::Visit(node_id, inherited_hidden.unwrap_or(false))];
    let mut visited = 0usize;

    while let Some(work) = stack.pop() {
        let (id, parent_hidden) = match work {
            Work::LeaveBlock(breaks) => {
                text.request_breaks(breaks);
                continue;
            }
            Work::LeaveCell => {
                text.pending_tab = true;
                continue;
            }
            Work::LeavePre => {
                preformatted -= 1;
                text.request_breaks(1);
                continue;
            }
            Work::LeaveArticle => {
                in_article -= 1;
                continue;
            }
            Work::LeaveSection => {
                in_section -= 1;
                continue;
            }
            Work::Visit(id, hidden) => (id, hidden),
        };

        visited += 1;
        if visited > MAX_NODES {
            break;
        }
        let Some(node) = dom.get_node(id) else {
            continue;
        };

        match &node.data {
            NodeData::Text { contents } => {
                if !parent_hidden {
                    text.push_text(contents, preformatted > 0);
                }
            }
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();
                let role = node.get_attribute("role");
                let style = styles.get(&id).copied().unwrap_or_default();
                if node.get_attribute("hidden").is_some()
                    || style.display_none
                    || matches!(
                        tag,
                        "script" | "style" | "template" | "noscript" | "head" | "title"
                    )
                    || (drop_boilerplate
                        && id != node_id
                        && is_boilerplate(tag, role, in_article > 0, in_section > 0))
                {
                    continue;
                }
                // `visibility: hidden` hides this element's own text and
                // breaks; a descendant can set `visible` again.
                let hidden = style.visibility_hidden.unwrap_or(parent_hidden);
                match content_kind(tag, role) {
                    Some(true) => {
                        in_article += 1;
                        stack.push(Work::LeaveArticle);
                    }
                    Some(false) => {
                        in_section += 1;
                        stack.push(Work::LeaveSection);
                    }
                    None => {}
                }
                let children = match tag {
                    "br" => {
                        if !hidden {
                            text.push_newline();
                        }
                        continue;
                    }
                    "select" if hidden => Vec::new(),
                    "select" => selected_option(dom, id).into_iter().collect(),
                    _ => dom.children(id),
                };
                match tag {
                    _ if hidden => {}
                    "p" => {
                        text.request_breaks(2);
                        stack.push(Work::LeaveBlock(2));
                    }
                    "pre" => {
                        text.request_breaks(1);
                        preformatted += 1;
                        stack.push(Work::LeavePre);
                    }
                    "tr" => {
                        // A new row starts on its own line, never after a tab.
                        text.pending_tab = false;
                        text.request_breaks(1);
                        stack.push(Work::LeaveBlock(1));
                    }
                    "td" | "th" => stack.push(Work::LeaveCell),
                    "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "hr"
                    | "blockquote" | "section" | "article" | "main" | "figure" | "figcaption"
                    | "table" | "caption" | "thead" | "tbody" | "tfoot" | "dl" | "dt" | "dd"
                    | "ul" | "ol" | "form" | "fieldset" | "legend" | "address" | "details"
                    | "summary" | "body" => {
                        text.request_breaks(1);
                        stack.push(Work::LeaveBlock(1));
                    }
                    _ => {}
                }
                for child_id in children.into_iter().rev() {
                    stack.push(Work::Visit(child_id, hidden));
                }
            }
            _ => {
                for child_id in dom.children(id).into_iter().rev() {
                    stack.push(Work::Visit(child_id, parent_hidden));
                }
            }
        }
    }

    text.out
}

/// The option a `<select>` shows: the last one with the `selected` attribute,
/// else the first one that is not disabled. `innerText` renders only that.
fn selected_option(
    dom: &inbrowse_dom::DomTree,
    select: inbrowse_dom::NodeId,
) -> Option<inbrowse_dom::NodeId> {
    let options: Vec<_> = dom
        .descendants(select)
        .into_iter()
        .filter_map(|id| dom.get_node(id))
        .filter(|node| {
            node.as_element()
                .is_some_and(|name| name.local.as_ref() == "option")
        })
        .collect();
    options
        .iter()
        .rev()
        .find(|node| node.get_attribute("selected").is_some())
        .or_else(|| {
            options
                .iter()
                .find(|node| node.get_attribute("disabled").is_none())
        })
        .map(|node| node.id)
}

async fn run_parallel_scrape(
    urls: Vec<String>,
    eval: Option<String>,
    concurrency: usize,
    format: &str,
    timeout_secs: u64,
    settle_timers_ms: Option<u64>,
    quiet: bool,
    proxy: Option<String>,
    stealth: bool,
    obey_robots: bool,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    let total = urls.len();
    let settle_ms = adaptive_settle_secs(settle_timers_ms).saturating_mul(1000);
    let start = Instant::now();

    if total == 0 {
        anyhow::bail!("No URLs provided. Pass at least one URL to scrape.");
    }

    if !quiet {
        eprintln!(
            "Scraping {} URLs with {} concurrent workers (per-worker timeout: {}s)...",
            total, concurrency, timeout_secs
        );
    }

    let worker_name = if cfg!(windows) {
        "inbrowse-worker.exe"
    } else {
        "inbrowse-worker"
    };
    let worker_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(worker_name)))
        .unwrap_or_else(|| std::path::PathBuf::from(worker_name));

    if !worker_path.exists() {
        anyhow::bail!(
            "Worker binary not found at {}. Build with: cargo build --release",
            worker_path.display()
        );
    }

    let eval = Arc::new(eval);
    let worker_path = Arc::new(worker_path);
    let worker_timeout = Duration::from_secs(timeout_secs);
    let read_timeout = Duration::from_secs(timeout_secs.min(30));
    let shutdown_timeout = Duration::from_secs(5);

    let completed = batch::run_bounded(urls.into_iter().enumerate(), concurrency, |(i, url)| {
        let eval = eval.clone();
        let worker_path = worker_path.clone();
        let proxy = proxy.clone();
        let user_agent = user_agent.clone();

        async move {
            let task_start = Instant::now();

            let mut child = match TokioCommand::new(worker_path.as_ref())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .env("INBROWSE_PROXY", proxy.as_deref().unwrap_or(""))
                .env("INBROWSE_STEALTH", if stealth { "1" } else { "" })
                .env("INBROWSE_OBEY_ROBOTS", if obey_robots { "1" } else { "" })
                .env("INBROWSE_USER_AGENT", user_agent.as_deref().unwrap_or(""))
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    return serde_json::json!({
                        "url": url,
                        "error": format!("Failed to spawn worker: {}", e),
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };

            let mut stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdin",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let stdout = match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdout",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let mut reader = BufReader::new(stdout);

            let worker_result: Result<serde_json::Value, String> =
                match timeout(worker_timeout, async {
                    let nav_cmd = serde_json::json!({
                        "cmd": "navigate",
                        "url": url,
                        "settleMs": settle_ms,
                        "settleTimersMs": settle_timers_ms.unwrap_or(0),
                    });
                    let mut line = serde_json::to_string(&nav_cmd).unwrap();
                    line.push('\n');
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        return Err("Write failed".to_string());
                    }
                    if stdin.flush().await.is_err() {
                        return Err("Write failed".to_string());
                    }

                    let mut resp_line = String::new();
                    match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                        Ok(Ok(bytes)) if bytes > 0 => {}
                        Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                        Err(_) => return Err("timeout".to_string()),
                    };

                    let nav_resp: serde_json::Value = serde_json::from_str(resp_line.trim())
                        .unwrap_or(serde_json::json!({"ok": false}));

                    if !nav_resp["ok"].as_bool().unwrap_or(false) {
                        return Err(nav_resp["error"]
                            .as_str()
                            .unwrap_or("navigate failed")
                            .to_string());
                    }

                    let title = nav_resp["result"]["title"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let script_warnings = nav_resp["result"]["warnings"].clone();

                    // The worker answers {ok, result} or {ok: false, error};
                    // an eval failure keeps the page's title and is reported
                    // in the URL's `error` field.
                    let (eval_result, eval_error) = if let Some(ref expr) = *eval {
                        let eval_cmd = serde_json::json!({"cmd": "evaluate", "expression": expr});
                        let mut line = serde_json::to_string(&eval_cmd).unwrap();
                        line.push('\n');
                        if stdin.write_all(line.as_bytes()).await.is_err() {
                            return Err("Write failed".to_string());
                        }
                        if stdin.flush().await.is_err() {
                            return Err("Write failed".to_string());
                        }

                        let mut resp_line = String::new();
                        match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                            Ok(Ok(bytes)) if bytes > 0 => {
                                let resp: serde_json::Value =
                                    serde_json::from_str(resp_line.trim()).map_err(|error| {
                                        format!("Invalid worker response: {error}")
                                    })?;
                                if resp["ok"].as_bool() == Some(true) {
                                    (resp["result"].clone(), None)
                                } else {
                                    let error = resp["error"]
                                        .as_str()
                                        .unwrap_or("evaluation failed without a message")
                                        .to_string();
                                    (serde_json::Value::Null, Some(error))
                                }
                            }
                            Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                            Err(_) => return Err("timeout".to_string()),
                        }
                    } else {
                        (serde_json::Value::Null, None)
                    };

                    let shutdown_cmd = serde_json::json!({"cmd": "shutdown"});
                    let mut line = serde_json::to_string(&shutdown_cmd).unwrap();
                    line.push('\n');
                    let _ = stdin.write_all(line.as_bytes()).await;
                    let _ = stdin.flush().await;
                    let _ = timeout(shutdown_timeout, child.wait()).await;

                    let mut record = serde_json::json!({
                        "url": url,
                        "title": title,
                        "eval": eval_result,
                        "time_ms": task_start.elapsed().as_millis(),
                        "worker": i,
                    });
                    if let Some(error) = eval_error {
                        record["error"] = serde_json::Value::String(error);
                    }
                    if script_warnings
                        .as_array()
                        .is_some_and(|warnings| !warnings.is_empty())
                    {
                        record["warnings"] = script_warnings;
                    }
                    Ok(record)
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("timeout".to_string()),
                };

            match worker_result {
                Ok(result) => result,
                Err(error) => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    serde_json::json!({
                        "url": url,
                        "error": error,
                        "time_ms": task_start.elapsed().as_millis(),
                    })
                }
            }
        }
    })
    .await;

    let mut results = Vec::new();
    for result in completed {
        match result {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({"error": e.to_string()})),
        }
    }

    let total_time = start.elapsed();

    if format == "json" {
        let output = serde_json::json!({
            "total_urls": total,
            "concurrency": concurrency,
            "total_time_ms": total_time.as_millis(),
            "avg_time_ms": total_time.as_millis() as f64 / total as f64,
            "results": results,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for r in &results {
            let url = r["url"].as_str().unwrap_or("?");
            let title = r["title"].as_str().unwrap_or("");
            let time = r["time_ms"].as_u64().unwrap_or(0);
            let eval = &r["eval"];
            if let Some(error) = r["error"].as_str() {
                println!("{}ms\t{}\terror: {}", time, url, error);
            } else if eval.is_null() {
                println!("{}ms\t{}\t{}", time, url, title);
            } else {
                println!("{}ms\t{}\t{}", time, url, eval);
            }
        }
        if !quiet {
            eprintln!(
                "\nTotal: {}ms for {} URLs ({} concurrent)",
                total_time.as_millis(),
                total,
                concurrency
            );
        }
    }

    Ok(())
}

/// Every link, or with `scope` (`--selector`) those inside that element (and
/// the element itself when it is a link).
fn dump_links(page: &mut Page, scope: Option<inbrowse_dom::NodeId>) -> String {
    // The document's base URL, which honours `<base href>`, as the markdown
    // and JSON output resolve against.
    let base_url = page
        .evaluate("document.baseURI")
        .ok()
        .and_then(|base| base.as_str().and_then(|base| url::Url::parse(base).ok()))
        .or_else(|| page.url.clone());
    page.with_dom(|dom| {
        let mut rendered = Vec::new();
        let links = match scope {
            Some(root) => {
                let root_is_link = dom
                    .get_node(root)
                    .is_some_and(|node| node.as_element().is_some_and(|name| &*name.local == "a"));
                root_is_link
                    .then_some(root)
                    .into_iter()
                    .chain(dom.query_selector_all_from(root, "a").unwrap_or_default())
                    .collect()
            }
            None => dom.query_selector_all("a").unwrap_or_default(),
        };
        for link_id in links {
            if let Some(node) = dom.get_node(link_id) {
                let href = node.get_attribute("href").unwrap_or_default().to_string();
                let text = dom.text_content(link_id);
                let text = text.trim();

                let full_url = if href.starts_with("http://") || href.starts_with("https://") {
                    href.clone()
                } else if let Some(ref base) = base_url {
                    base.join(&href)
                        .map(|u| u.to_string())
                        .unwrap_or(href.clone())
                } else {
                    href.clone()
                };

                if !full_url.is_empty() {
                    if text.is_empty() {
                        rendered.push(full_url);
                    } else {
                        rendered.push(format!("{}\t{}", full_url, text));
                    }
                }
            }
        }
        rendered.join("\n")
    })
    .unwrap_or_default()
}

/// Selectors paired with the attribute whose URL we extract and the
/// asset kind we surface. Order is stable so the output of
/// `--dump assets` is deterministic across runs.
const ASSET_SELECTORS: &[(&str, &str, &str)] = &[
    ("script[src]", "src", "script"),
    ("link[href]", "href", "link"),
    ("img[src]", "src", "image"),
    ("iframe[src]", "src", "iframe"),
    ("source[src]", "src", "media"),
    ("video[src]", "src", "video"),
    ("audio[src]", "src", "audio"),
    ("embed[src]", "src", "embed"),
    ("object[data]", "data", "object"),
];

/// Map a `<link>` element's `rel` token to a more specific asset
/// kind so consumers can filter (e.g. just stylesheets, just icons).
/// Unknown / missing `rel` falls back to a generic "link" so the
/// caller still sees the URL.
fn link_kind_from_rel(rel: &str) -> &'static str {
    match rel
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "stylesheet" => "stylesheet",
        "icon" | "shortcut" => "icon",
        "manifest" => "manifest",
        "preload" => "preload",
        "prefetch" => "prefetch",
        "modulepreload" => "modulepreload",
        "dns-prefetch" => "dns-prefetch",
        "preconnect" => "preconnect",
        "alternate" => "alternate",
        _ => "link",
    }
}

/// Resolve a raw `src`/`href`/`data` attribute against the page's
/// base URL. Mirrors `dump_links`'s logic so `--dump assets` and
/// `--dump links` agree on absolute-URL semantics.
fn resolve_asset_url(raw: &str, base_url: Option<&url::Url>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if let Some(base) = base_url {
        if let Ok(joined) = base.join(trimmed) {
            return Some(joined.to_string());
        }
    }
    Some(trimmed.to_string())
}

/// Walk the rendered DOM and emit one NDJSON line per discoverable
/// sub-resource. Pure over `DomTree`/`Url` so unit tests can drive
/// it from a fixture HTML without standing up a browser.
fn extract_assets(dom: &inbrowse_dom::DomTree, base_url: Option<&url::Url>) -> String {
    let mut out: Vec<String> = Vec::new();
    for (selector, attr, default_kind) in ASSET_SELECTORS {
        let nodes = dom.query_selector_all(selector).unwrap_or_default();
        for node_id in nodes {
            let Some(node) = dom.get_node(node_id) else {
                continue;
            };
            let raw = node.get_attribute(attr).unwrap_or_default().to_string();
            let Some(url) = resolve_asset_url(&raw, base_url) else {
                continue;
            };

            let kind = if *default_kind == "link" {
                let rel = node.get_attribute("rel").unwrap_or_default().to_string();
                link_kind_from_rel(&rel)
            } else {
                *default_kind
            };

            let record = serde_json::json!({
                "url": url,
                "type": kind,
            });
            out.push(record.to_string());
        }
    }
    out.join("\n")
}

fn dump_assets(page: &Page) -> String {
    let base_url = page.url.clone();
    let dom_ndjson = page
        .with_dom(|dom| extract_assets(dom, base_url.as_ref()))
        .unwrap_or_default();

    let mut lines: Vec<String> = dom_ndjson
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    // URLs already listed from static DOM attributes, so a resource the script
    // fetches that the markup also references is not emitted twice.
    let mut seen: std::collections::HashSet<String> = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        .collect();

    // Resources pulled in by JS fetch()/XHR, which leave no static DOM tag
    // (issue #301).
    for url in page.fetched_urls() {
        if seen.insert(url.clone()) {
            lines.push(serde_json::json!({ "url": url, "type": "fetch" }).to_string());
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        configure_fetch_navigation_timeout, effective_v8_flags, extract_assets,
        extract_readable_text, fetch_original_bytes, is_quiet_command, link_kind_from_rel,
        merge_proxy, merge_user_agent, normalize_v8_flags, read_urls_from_file, resolve_asset_url,
        scrape_url_list, select_log_filter, write_or_print, write_or_print_bytes, Args, Command,
        DumpFormat, DEFAULT_V8_FLAGS,
    };
    use clap::Parser;
    use inbrowse_dom::parse_html;

    // Issue #117 — `--dump original` short-circuits the browser stack and
    // streams the raw response body verbatim, including for binary payloads.
    //
    // Two tests below pin the behaviour:
    //   1. clap accepts `--dump original` as a valid DumpFormat variant.
    //   2. `fetch_original_bytes` returns the exact bytes a `file://` URL
    //      points at (binary-safe round-trip — no UTF-8 decode, no trailing
    //      newline, no DOM mutation).
    //   3. `write_or_print_bytes` writes raw bytes to a file without the
    //      trailing newline that `println!` would add.
    #[test]
    fn parsed_fetch_dump_original_is_accepted_by_clap() {
        let args = Args::try_parse_from([
            "inbrowse",
            "fetch",
            "--dump",
            "original",
            "https://example.com/image.jpg",
        ])
        .expect("clap should accept --dump original");
        match args.command {
            Some(Command::Fetch { dump, .. }) => {
                assert_eq!(dump, Some(DumpFormat::Original));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    // Issue #349 — batch mode: `fetch --file urls.txt --dump original
    // --concurrency N` with no positional URL.
    #[test]
    fn parsed_fetch_file_and_concurrency() {
        let args = Args::try_parse_from([
            "inbrowse",
            "fetch",
            "--file",
            "urls.txt",
            "--dump",
            "original",
            "--concurrency",
            "25",
        ])
        .expect("clap should accept --file with --concurrency and no positional URL");
        match args.command {
            Some(Command::Fetch {
                url,
                file,
                concurrency,
                dump,
                ..
            }) => {
                assert!(url.is_none());
                assert_eq!(file, Some(std::path::PathBuf::from("urls.txt")));
                assert_eq!(concurrency.get(), 25);
                assert_eq!(dump, Some(DumpFormat::Original));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn concurrency_rejects_zero() {
        // NonZeroUsize means --concurrency 0 is a parse error, not a silent hang
        // on a zero-permit semaphore.
        let err =
            Args::try_parse_from(["inbrowse", "fetch", "--file", "u.txt", "--concurrency", "0"]);
        assert!(err.is_err());
    }

    // Issue #1278 — a top-level --user-agent is global, so it reaches the
    // subcommand whether it appears before or after the subcommand, and a
    // subcommand-level value still wins.
    #[test]
    fn user_agent_before_subcommand_reaches_fetch() {
        let args = Args::try_parse_from([
            "inbrowse",
            "--user-agent",
            "TestUA/1.0",
            "fetch",
            "https://example.com",
        ])
        .expect("clap should accept a top-level --user-agent before the subcommand");
        let global = args.user_agent.clone();
        match args.command {
            Some(Command::Fetch { user_agent, .. }) => {
                assert_eq!(
                    merge_user_agent(global, user_agent),
                    Some("TestUA/1.0".to_string())
                );
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn subcommand_user_agent_wins_over_global() {
        let args = Args::try_parse_from([
            "inbrowse",
            "--user-agent",
            "Global/1.0",
            "fetch",
            "--user-agent",
            "Sub/2.0",
            "https://example.com",
        ])
        .expect("clap should accept both a global and a subcommand --user-agent");
        let global = args.user_agent.clone();
        match args.command {
            Some(Command::Fetch { user_agent, .. }) => {
                assert_eq!(
                    merge_user_agent(global, user_agent),
                    Some("Sub/2.0".to_string())
                );
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn scrape_accepts_global_user_agent() {
        let args = Args::try_parse_from([
            "inbrowse",
            "scrape",
            "https://example.com",
            "--user-agent",
            "TestUA/1.0",
        ])
        .expect("scrape should accept --user-agent via the global flag");
        assert_eq!(args.user_agent, Some("TestUA/1.0".to_string()));
    }

    #[test]
    fn merge_user_agent_prefers_command_value() {
        assert_eq!(
            merge_user_agent(Some("G".to_string()), Some("C".to_string())),
            Some("C".to_string())
        );
        assert_eq!(
            merge_user_agent(Some("G".to_string()), None),
            Some("G".to_string())
        );
        assert_eq!(
            merge_user_agent(None, Some("C".to_string())),
            Some("C".to_string())
        );
        assert_eq!(merge_user_agent(None, None), None);
    }

    #[test]
    fn scrape_url_list_keeps_positional_order_and_appends_the_file_list() {
        let path =
            std::env::temp_dir().join(format!("inbrowse_scrape_list_{}.txt", std::process::id()));
        std::fs::write(&path, "https://c.example/\n# skip\n\n").unwrap();
        let urls = scrape_url_list(
            vec![
                "https://a.example/".to_string(),
                "https://b.example/".to_string(),
            ],
            Some(&path),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            urls,
            [
                "https://a.example/",
                "https://b.example/",
                "https://c.example/"
            ]
        );
        let twice = scrape_url_list(vec!["-".to_string()], Some(std::path::Path::new("-")));
        assert!(twice.unwrap_err().to_string().contains("stdin"));
    }

    #[test]
    fn read_urls_skips_blanks_and_comments() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("inbrowse_urls_{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "https://a.example/one.js\n\n  # a comment\n   https://b.example/two.css  \nhttps://c.example/three.json\n",
        )
        .unwrap();
        let urls = read_urls_from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            urls,
            vec![
                "https://a.example/one.js".to_string(),
                "https://b.example/two.css".to_string(),
                "https://c.example/three.json".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_original_bytes_returns_file_contents_verbatim() {
        // A real binary payload: a 1×1 transparent PNG (89 50 4E 47 …) —
        // exactly the kind of resource #117 wants to stream without HTML/
        // JS rendering.
        const PNG_BYTES: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let path = std::env::temp_dir().join(format!(
            "inbrowse-fetch-original-test-{}.png",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::write(&path, PNG_BYTES)
            .await
            .expect("seed temp PNG fixture");

        let file_url = format!("file://{}", path.display());
        let bytes = fetch_original_bytes(&file_url, None, None, 5, false)
            .await
            .expect("fetch_original_bytes should round-trip the file body");

        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(
            bytes, PNG_BYTES,
            "raw response body must match the file byte-for-byte"
        );
    }

    // A stealth-enabled build routes `--dump original` through
    // StealthHttpClient (wreq), which only speaks http(s). file:// must keep
    // working the same as without --stealth instead of being handed to wreq
    // (issue #482).
    #[tokio::test(flavor = "current_thread")]
    async fn fetch_original_bytes_file_url_ignores_stealth_flag() {
        const PNG_BYTES: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let path = std::env::temp_dir().join(format!(
            "inbrowse-fetch-original-stealth-test-{}.png",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::write(&path, PNG_BYTES)
            .await
            .expect("seed temp PNG fixture");

        let file_url = format!("file://{}", path.display());
        let bytes = fetch_original_bytes(&file_url, None, None, 5, true)
            .await
            .expect("fetch_original_bytes should still round-trip file:// with stealth=true");

        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(
            bytes, PNG_BYTES,
            "stealth=true must not change file:// handling"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_or_print_bytes_writes_without_trailing_newline() {
        // Regression guard for #117: stdout must receive raw bytes. The file
        // path used here exercises the file-output branch — println!-style
        // output (used by write_or_print) would append a 0x0A byte and
        // corrupt binary payloads. write_or_print_bytes must not.
        let payload: &[u8] = &[0x00, 0xFF, b'h', b'i', 0x00];
        let path = std::env::temp_dir().join(format!(
            "inbrowse-write-bytes-test-{}.bin",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;

        write_or_print_bytes(payload, Some(&path))
            .await
            .expect("write_or_print_bytes should write the file");

        let read_back = tokio::fs::read(&path).await.expect("read back");
        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(
            read_back, payload,
            "file bytes must match the payload exactly"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_or_print_writes_output_file_with_tokio_fs() {
        let path = std::env::temp_dir().join(format!(
            "inbrowse-fetch-output-test-{}.txt",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;

        write_or_print("rendered output".to_string(), Some(&path))
            .await
            .expect("write output file");

        let content = tokio::fs::read_to_string(&path)
            .await
            .expect("read output file");
        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(content, "rendered output");
    }

    #[test]
    fn default_filter_is_warn() {
        assert_eq!(select_log_filter(false, false), "warn");
    }

    #[test]
    fn verbose_filter_is_debug() {
        assert_eq!(select_log_filter(true, false), "debug");
    }

    #[test]
    fn quiet_filter_is_off() {
        assert_eq!(select_log_filter(false, true), "off");
    }

    #[test]
    fn verbose_wins_over_quiet() {
        assert_eq!(select_log_filter(true, true), "debug");
    }

    #[test]
    fn parsed_fetch_with_quiet_flag_is_detected() {
        let args = Args::try_parse_from(["inbrowse", "fetch", "--quiet", "https://example.com"])
            .expect("clap should accept --quiet on fetch");
        assert!(is_quiet_command(&args.command));
    }

    #[test]
    fn parsed_fetch_without_quiet_is_not_detected() {
        let args = Args::try_parse_from(["inbrowse", "fetch", "https://example.com"])
            .expect("clap should accept fetch without --quiet");
        assert!(!is_quiet_command(&args.command));
    }

    #[test]
    fn parsed_serve_command_is_not_quiet() {
        let args = Args::try_parse_from(["inbrowse", "serve"]).expect("clap should accept serve");
        assert!(!is_quiet_command(&args.command));
    }

    #[test]
    fn parsed_serve_accepts_repeated_font_directories() {
        let args = Args::try_parse_from([
            "inbrowse",
            "serve",
            "--font-dir",
            "/fonts/cjk",
            "--font-dir",
            "/fonts/brand",
        ])
        .expect("clap should accept repeatable --font-dir");
        match args.command {
            Some(Command::Serve { font_dirs, .. }) => assert_eq!(
                font_dirs,
                [
                    std::path::PathBuf::from("/fonts/cjk"),
                    std::path::PathBuf::from("/fonts/brand"),
                ]
            ),
            _ => panic!("expected Serve command"),
        }
    }

    #[test]
    fn parsed_mcp_accepts_repeated_font_directories() {
        let args = Args::try_parse_from([
            "inbrowse",
            "mcp",
            "--font-dir",
            "/fonts/cjk",
            "--font-dir",
            "/fonts/brand",
        ])
        .expect("clap should accept MCP font directories");
        match args.command {
            Some(Command::Mcp { font_dirs, .. }) => assert_eq!(
                font_dirs,
                [
                    std::path::PathBuf::from("/fonts/cjk"),
                    std::path::PathBuf::from("/fonts/brand")
                ]
            ),
            _ => panic!("expected Mcp command"),
        }
    }

    #[test]
    fn no_subcommand_is_not_quiet() {
        assert!(!is_quiet_command(&None));
    }

    #[test]
    fn parsed_v8_flags_global_arg() {
        let args = Args::try_parse_from([
            "inbrowse",
            "--v8-flags",
            "--max-old-space-size=4096 --max-semi-space-size=64",
            "fetch",
            "https://example.com",
        ])
        .expect("clap should accept --v8-flags as a global arg");
        assert_eq!(
            args.v8_flags.as_deref(),
            Some("--max-old-space-size=4096 --max-semi-space-size=64"),
        );
    }

    #[test]
    fn v8_flags_default_is_none() {
        let args = Args::try_parse_from(["inbrowse", "fetch", "https://example.com"])
            .expect("clap should accept fetch without --v8-flags");
        assert!(args.v8_flags.is_none());
    }

    #[test]
    fn parsed_v8_flags_with_serve_subcommand() {
        let args = Args::try_parse_from([
            "inbrowse",
            "--v8-flags",
            "--max-old-space-size=2048",
            "serve",
            "--port",
            "9333",
        ])
        .expect("clap should accept --v8-flags with serve");
        assert_eq!(args.v8_flags.as_deref(), Some("--max-old-space-size=2048"));
    }

    #[test]
    fn parsed_v8_flags_with_scrape_subcommand() {
        let args = Args::try_parse_from([
            "inbrowse",
            "--v8-flags",
            "--expose-gc",
            "scrape",
            "https://a.com",
            "https://b.com",
        ])
        .expect("clap should accept --v8-flags with scrape");
        assert_eq!(args.v8_flags.as_deref(), Some("--expose-gc"));
    }

    #[test]
    fn parsed_v8_flags_empty_string_is_accepted() {
        let args =
            Args::try_parse_from(["inbrowse", "--v8-flags", "", "fetch", "https://example.com"])
                .expect("clap should accept empty --v8-flags value");
        assert_eq!(args.v8_flags.as_deref(), Some(""));
    }

    #[test]
    fn normalize_v8_flags_returns_none_when_unset() {
        assert_eq!(normalize_v8_flags(None), None);
    }

    #[test]
    fn normalize_v8_flags_returns_none_for_empty_or_whitespace() {
        assert_eq!(normalize_v8_flags(Some("")), None);
        assert_eq!(normalize_v8_flags(Some("   ")), None);
        assert_eq!(normalize_v8_flags(Some("\t\n")), None);
    }

    #[test]
    fn normalize_v8_flags_trims_surrounding_whitespace() {
        assert_eq!(
            normalize_v8_flags(Some("  --max-old-space-size=4096  ")).as_deref(),
            Some("--max-old-space-size=4096"),
        );
    }

    #[test]
    fn normalize_v8_flags_preserves_multi_flag_string() {
        let input = "--max-old-space-size=4096 --max-semi-space-size=64 --expose-gc";
        assert_eq!(normalize_v8_flags(Some(input)).as_deref(), Some(input));
    }

    #[test]
    fn effective_v8_flags_returns_default_when_unset() {
        assert_eq!(effective_v8_flags(None), DEFAULT_V8_FLAGS);
        assert_eq!(effective_v8_flags(Some("")), DEFAULT_V8_FLAGS);
        assert_eq!(effective_v8_flags(Some("   ")), DEFAULT_V8_FLAGS);
    }

    #[test]
    fn effective_v8_flags_user_overrides_default() {
        // V8 parses left-to-right and later wins, so the user value must
        // come after the default in the merged string.
        let user = "--max-old-space-size=8192";
        let merged = effective_v8_flags(Some(user));
        assert!(merged.starts_with(DEFAULT_V8_FLAGS));
        assert!(merged.ends_with(user));
    }

    #[test]
    fn effective_v8_flags_appends_user_extras() {
        let merged = effective_v8_flags(Some("--expose-gc"));
        assert!(merged.contains(DEFAULT_V8_FLAGS));
        assert!(merged.contains("--expose-gc"));
    }

    #[test]
    fn parsed_fetch_quiet_resolves_to_off_filter() {
        let args =
            Args::try_parse_from(["inbrowse", "fetch", "--quiet", "https://example.com"]).unwrap();
        let filter = select_log_filter(args.verbose, is_quiet_command(&args.command));
        assert_eq!(filter, "off");
    }

    #[test]
    fn fetch_wait_distinguishes_adaptive_default_from_fixed_delay() {
        let default = Args::try_parse_from(["inbrowse", "fetch", "https://example.com"]).unwrap();
        match default.command {
            Some(Command::Fetch { wait, .. }) => assert_eq!(wait, None),
            _ => panic!("expected Fetch command"),
        }

        let fixed =
            Args::try_parse_from(["inbrowse", "fetch", "https://example.com", "--wait", "0"])
                .unwrap();
        match fixed.command {
            Some(Command::Fetch { wait, .. }) => assert_eq!(wait, Some(0)),
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn fetch_screenshot_has_a_short_alias_and_rejects_batch_mode() {
        let args =
            Args::try_parse_from(["inbrowse", "fetch", "https://example.com", "-s", "page.png"])
                .unwrap();
        match args.command {
            Some(Command::Fetch { screenshot, .. }) => {
                assert_eq!(screenshot, Some(std::path::PathBuf::from("page.png")));
            }
            _ => panic!("expected Fetch command"),
        }

        assert!(Args::try_parse_from([
            "inbrowse",
            "fetch",
            "--file",
            "urls.txt",
            "--screenshot",
            "page.png",
        ])
        .is_err());
    }

    fn configured_fetch_timeout(args: Args) -> std::time::Duration {
        let timeout = match args.command {
            Some(Command::Fetch { timeout, .. }) => timeout,
            _ => panic!("expected Fetch command"),
        };
        let context = std::sync::Arc::new(inbrowse::BrowserContext::with_storage_and_network(
            "cli-timeout-test".to_string(),
            None,
            false,
            None,
            None,
            true,
        ));
        let mut page = inbrowse::Page::new("cli-timeout-test".to_string(), context);
        configure_fetch_navigation_timeout(&mut page, timeout);
        page.navigation_timeout()
    }

    #[test]
    fn fetch_timeout_sets_the_page_navigation_budget() {
        let args = Args::try_parse_from([
            "inbrowse",
            "fetch",
            "https://example.com",
            "--timeout",
            "50",
        ])
        .unwrap();
        assert_eq!(
            configured_fetch_timeout(args),
            std::time::Duration::from_secs(50)
        );
    }

    #[test]
    fn fetch_default_navigation_budget_remains_thirty_seconds() {
        let args = Args::try_parse_from(["inbrowse", "fetch", "https://example.com"]).unwrap();
        assert_eq!(
            configured_fetch_timeout(args),
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn matcher_still_uses_fetch_variant() {
        let cmd = Some(Command::Fetch {
            url: Some("https://x".to_string()),
            dump: Some(super::DumpFormat::Html),
            selector: None,
            boilerplate: None,
            file: None,
            concurrency: std::num::NonZeroUsize::new(1).unwrap(),
            wait: Some(5),
            timeout: 30,
            script_timeout: None,
            settle_timers: None,
            wait_until: "load".to_string(),
            user_agent: None,
            cookies: vec![],
            eval: None,
            quiet: true,
            output: None,
            storage_dir: None,
            screenshot: None,
            font_dirs: vec![],
        });
        assert!(is_quiet_command(&cmd));
    }

    fn body_text(html: &str) -> String {
        let dom = parse_html(html);
        let body = dom
            .query_selector("body")
            .ok()
            .flatten()
            .expect("body must exist");
        extract_readable_text(&dom, body, true, &Default::default())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn layout_text(html: &str) -> String {
        let dom = parse_html(html);
        let body = dom
            .query_selector("body")
            .ok()
            .flatten()
            .expect("body must exist");
        extract_readable_text(&dom, body, true, &Default::default())
    }

    #[test]
    fn table_cells_are_tab_separated_and_rows_are_lines() {
        assert_eq!(
            layout_text("<table><caption>Price table</caption><tr><th>Item</th><th>Price</th></tr><tr><td>Tea</td><td>$2</td></tr></table>"),
            "Price table\nItem\tPrice\nTea\t$2",
        );
    }

    #[test]
    fn empty_blocks_do_not_add_blank_lines() {
        assert_eq!(
            layout_text(
                "<p>One</p><div></div><div><div></div></div>\n  \n<div>Two</div><p>Three</p>"
            ),
            "One\n\nTwo\n\nThree",
        );
    }

    #[test]
    fn a_select_shows_only_its_selected_option() {
        assert_eq!(
            layout_text("<form><select><option>a</option><option selected>b</option></select> <button>Go</button></form>"),
            "b Go",
        );
        assert_eq!(
            layout_text("<select><option disabled>x</option><option>y</option></select>"),
            "y"
        );
    }

    #[test]
    fn br_and_pre_keep_their_line_structure() {
        assert_eq!(layout_text("a<br>b"), "a\nb");
        assert_eq!(
            layout_text("<p>x</p><pre>  keep\n    this</pre><p>y</p>"),
            "x\n\n  keep\n    this\n\ny"
        );
    }

    #[test]
    fn hidden_and_template_content_is_not_text() {
        assert_eq!(
            layout_text("<p>shown</p><p hidden>secret</p><template>tmpl</template>"),
            "shown"
        );
    }

    #[test]
    fn skips_nav_header_footer_aside() {
        let text = body_text(
            r#"<html><body>
                <header>SITE HEADER</header>
                <nav>NAV LINKS</nav>
                <aside>SIDEBAR</aside>
                <main><p>Article body.</p></main>
                <footer>FOOTER</footer>
            </body></html>"#,
        );
        assert!(text.contains("Article body."), "main content kept: {text}");
        for boilerplate in ["SITE HEADER", "NAV LINKS", "SIDEBAR", "FOOTER"] {
            assert!(
                !text.contains(boilerplate),
                "boilerplate '{boilerplate}' leaked through: {text}"
            );
        }
    }

    #[test]
    fn still_skips_script_and_style() {
        // Regression guard for the original skip list.
        let text = body_text(
            r#"<html><body>
                <p>Hello.</p>
                <script>console.log("nope")</script>
                <style>.x { color: red }</style>
            </body></html>"#,
        );
        assert!(text.contains("Hello."));
        assert!(!text.contains("console.log"));
        assert!(!text.contains("color: red"));
    }

    #[test]
    fn command_proxy_overrides_global_proxy() {
        let proxy = merge_proxy(
            Some("http://global.example:8080".to_string()),
            Some("socks5://127.0.0.1:1080".to_string()),
        );

        assert_eq!(proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn global_proxy_is_used_when_command_proxy_is_absent() {
        let proxy = merge_proxy(Some("http://global.example:8080".to_string()), None);

        assert_eq!(proxy.as_deref(), Some("http://global.example:8080"));
    }

    #[test]
    fn parsed_fetch_dump_assets_is_accepted_by_clap() {
        let args = Args::try_parse_from([
            "inbrowse",
            "fetch",
            "--dump",
            "assets",
            "https://example.com",
        ])
        .expect("clap should accept --dump assets");
        match args.command {
            Some(Command::Fetch { dump, .. }) => {
                assert_eq!(dump, Some(DumpFormat::Assets));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn resolve_asset_url_keeps_absolute_unchanged() {
        let base = url::Url::parse("https://page.test/a/b").unwrap();
        let abs = "https://cdn.test/x.js";
        assert_eq!(resolve_asset_url(abs, Some(&base)).as_deref(), Some(abs));
    }

    #[test]
    fn resolve_asset_url_joins_relative_against_base() {
        let base = url::Url::parse("https://page.test/a/b").unwrap();
        let rel = "/static/x.js";
        assert_eq!(
            resolve_asset_url(rel, Some(&base)).as_deref(),
            Some("https://page.test/static/x.js"),
        );
    }

    #[test]
    fn resolve_asset_url_drops_empty() {
        let base = url::Url::parse("https://page.test/").unwrap();
        assert!(resolve_asset_url("", Some(&base)).is_none());
        assert!(resolve_asset_url("   ", Some(&base)).is_none());
    }

    #[test]
    fn link_kind_from_rel_handles_common_values() {
        assert_eq!(link_kind_from_rel("stylesheet"), "stylesheet");
        assert_eq!(link_kind_from_rel("icon"), "icon");
        // First token wins for multi-token rel (e.g. "shortcut icon").
        assert_eq!(link_kind_from_rel("shortcut icon"), "icon");
        assert_eq!(link_kind_from_rel("manifest"), "manifest");
        assert_eq!(link_kind_from_rel("preload"), "preload");
        assert_eq!(link_kind_from_rel("prefetch"), "prefetch");
        assert_eq!(link_kind_from_rel("modulepreload"), "modulepreload");
        assert_eq!(link_kind_from_rel("dns-prefetch"), "dns-prefetch");
        assert_eq!(link_kind_from_rel("preconnect"), "preconnect");
        assert_eq!(link_kind_from_rel("alternate"), "alternate");
        // Empty / unknown falls back to generic "link" so URL is still emitted.
        assert_eq!(link_kind_from_rel(""), "link");
        assert_eq!(link_kind_from_rel("noopener"), "link");
    }

    #[test]
    fn extract_assets_covers_every_resource_tag() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/site.css">
            <link rel="icon" href="/favicon.ico">
            <link rel="preload" href="/font.woff2">
            <link href="/no-rel.css">
            <script src="/app.js"></script>
        </head><body>
            <img src="/logo.png">
            <iframe src="/frame.html"></iframe>
            <video src="/clip.mp4"><source src="/clip.webm"></video>
            <audio src="/track.mp3"></audio>
            <embed src="/widget.swf">
            <object data="/doc.pdf"></object>
        </body></html>"#;
        let dom = inbrowse_dom::parse_html(html);
        let base = url::Url::parse("https://example.test/page").unwrap();
        let ndjson = extract_assets(&dom, Some(&base));
        let records: Vec<serde_json::Value> = ndjson
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line must be valid JSON"))
            .collect();

        // Every emitted record must have an absolute URL on example.test
        // and a non-empty type string. Pin specific entries so a regression
        // in selectors or kind mapping fails loudly.
        for r in &records {
            let url = r["url"].as_str().unwrap();
            assert!(
                url.starts_with("https://example.test/"),
                "url not absolute: {url}",
            );
            assert!(!r["type"].as_str().unwrap().is_empty());
        }

        let pairs: Vec<(String, String)> = records
            .iter()
            .map(|r| {
                (
                    r["url"].as_str().unwrap().to_string(),
                    r["type"].as_str().unwrap().to_string(),
                )
            })
            .collect();

        assert!(pairs.contains(&(
            "https://example.test/app.js".to_string(),
            "script".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/site.css".to_string(),
            "stylesheet".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/favicon.ico".to_string(),
            "icon".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/font.woff2".to_string(),
            "preload".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/no-rel.css".to_string(),
            "link".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/logo.png".to_string(),
            "image".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/frame.html".to_string(),
            "iframe".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/clip.mp4".to_string(),
            "video".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/clip.webm".to_string(),
            "media".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/track.mp3".to_string(),
            "audio".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/widget.swf".to_string(),
            "embed".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/doc.pdf".to_string(),
            "object".to_string(),
        )));
    }

    #[test]
    fn extract_assets_skips_empty_attributes() {
        let html = r#"<html><body>
            <script src=""></script>
            <img src="   ">
            <iframe src="/ok.html"></iframe>
        </body></html>"#;
        let dom = inbrowse_dom::parse_html(html);
        let base = url::Url::parse("https://example.test/").unwrap();
        let ndjson = extract_assets(&dom, Some(&base));
        let lines: Vec<&str> = ndjson.lines().collect();
        // Only the iframe with a non-empty src survives.
        assert_eq!(lines.len(), 1, "got {lines:?}");
        assert!(lines[0].contains("\"https://example.test/ok.html\""));
        assert!(lines[0].contains("\"iframe\""));
    }
}
