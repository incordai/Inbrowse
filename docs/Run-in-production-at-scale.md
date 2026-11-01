## Docker

```bash
# The container runs as uid 65532, so a mounted storage dir must be writable
# by it. Without this the cookie jar silently fails to persist.
sudo install -d -o 65532 -g 65532 /srv/inbrowse/data

docker run -d \
  --name inbrowse \
  --restart unless-stopped \
  -p 127.0.0.1:9222:9222 \
  -e INBROWSE_CDP_TOKEN="$(openssl rand -hex 32)" \
  -v /srv/inbrowse/data:/data \
  inbrowse \
  serve --host 0.0.0.0 --storage-dir /data --stealth
```

`inbrowse` is the image built from the repository's `Dockerfile` with
`docker build -t inbrowse .`. It runs `inbrowse serve` by default. Override with arguments after the image name.

### The container does not run as root

The image is built on `gcr.io/distroless/cc-debian12:nonroot` and runs as
uid/gid **65532**. Inbrowse executes untrusted page JavaScript in-process through
V8, so a V8 exploit lands with the process's privileges — there is no reason for
those to be root's.

Two consequences worth knowing:

- **A mounted `--storage-dir` must be writable by uid 65532**, as above. This is
  the one thing that breaks quietly rather than loudly. Verified: with an
  unwritable storage dir Inbrowse completes the run, exits `0`, and prints no
  warning — the cookie jar simply never persists. Check that
  `{storage-dir}/cookies.json` exists after your first run rather than assuming
  it does.
- **Nothing in the image needs a privileged operation.** It binds an
  unprivileged port, reads the CA bundle, and writes only to the storage dir and
  a temp dir. Verified in the non-root image: an HTTPS fetch succeeds, so the CA
  bundle is readable, and a writable storage dir is populated.

### Why the in-container bind is `0.0.0.0`

A container-loopback bind is unreachable through `-p`, so `--host 0.0.0.0` is
required for the published port to work at all. Publish to **host loopback**
(`-p 127.0.0.1:9222:9222`, as above) rather than `-p 9222:9222`: the latter
exposes the port on every host interface, and Docker's iptables rules bypass
most host firewalls.

The container bind requires `INBROWSE_CDP_TOKEN`. Send it as a bearer token from
the CDP client and still publish the port to host loopback where possible.

## Systemd

`/etc/systemd/system/inbrowse.service`:

```ini
[Unit]
Description=Inbrowse headless browser
After=network.target

[Service]
ExecStart=/usr/local/bin/inbrowse serve --port 9222 --stealth --storage-dir /var/lib/inbrowse
Restart=always
RestartSec=5
User=inbrowse
Group=inbrowse
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```bash
systemctl enable --now inbrowse
journalctl -fu inbrowse
```

## Workers

`inbrowse serve --workers N` runs N CDP server workers behind the listener.

```bash
inbrowse serve --workers 4
```

Use one worker per CPU core. Each worker handles its own pool of pages. Sessions are sticky to a worker.

Worker memory can grow over days. Recycle workers with `--worker-max-connections`, `--worker-max-rss` (MB, Linux) or `--worker-max-age` (seconds). A replacement starts first and takes all new connections; the old worker finishes its live connections, then exits. `--worker-drain-timeout` (default 120 s) bounds that wait, after which remaining connections are closed, so set it above your longest session.

```bash
inbrowse serve --workers 50 --worker-max-connections 500 --worker-max-rss 1500
```

## V8 heap

The V8 old-generation ceiling is 4 GB on 64-bit systems. The defaults request `--max-semi-space-size=4`, but the current V8's `--optimize-for-size` implication overrides the effective semi-space cap to 1 MiB. Override the old-generation ceiling with:

```bash
inbrowse --v8-flags "--max-old-space-size=2048" serve
```

Flags you pass are appended after the defaults, so this old-generation override retains memory tuning. To change the young-generation cap, also pass `--no-optimize-for-size`; V8's flag implications otherwise override your cap. Disabling size optimization can save GC CPU but increase peak memory. Benchmark both before changing production defaults.

## Parallel scrape

`inbrowse scrape` fans out a list of URLs across worker processes:

```bash
inbrowse scrape \
  --concurrency 20 \
  --format json \
  --timeout 60 \
  url1 url2 url3 ...
```

Reads URLs from stdin:

```bash
cat urls.txt | inbrowse scrape --concurrency 20 -
```

Requires `inbrowse-worker` next to `inbrowse` in `PATH`.

## Resource limits

Per-process memory cap with systemd:

```ini
[Service]
MemoryMax=4G
MemoryHigh=3G
```

Per-container with Docker:

```bash
docker run --memory=4g --cpus=2 ...
```

## Reverse proxy

Expose inbrowse on TLS through nginx or caddy:

```nginx
location /inbrowse/ {
  proxy_pass http://127.0.0.1:9222/;
  proxy_http_version 1.1;
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
  proxy_read_timeout 86400;
}
```

CDP needs WebSocket upgrade and long read timeouts.

## Authentication

Inbrowse requires `INBROWSE_CDP_TOKEN` (at least 32 bytes) for every non-loopback
CDP bind. Pass it in the client's `Authorization` header. Also:

- Bind to `127.0.0.1` and require SSH for access (default).
- Put it behind a reverse proxy that enforces an additional auth boundary.
- Use Docker network isolation.

Never bind `0.0.0.0` on a public IP without one of the above.

## MCP HTTP transport

`inbrowse mcp --http` binds `127.0.0.1` by default. A non-loopback bind requires a bearer token. Browser origins are denied by default; set an allowlist only when a browser-based MCP client needs access:

```bash
INBROWSE_MCP_TOKEN="$(openssl rand -hex 32)" \
INBROWSE_MCP_ALLOWED_ORIGINS="https://app.example.com" \
  inbrowse mcp --http --host 0.0.0.0 --port 3000
```

Request bodies and headers, batch size, pending requests, and connections are bounded. Keep the service on an internal network even with authentication. See [Use the MCP server](Use-the-MCP-server.md).

## Observability

```bash
inbrowse serve --verbose
RUST_LOG=inbrowse=debug inbrowse serve
```

`--verbose` enables info-level logs. `RUST_LOG=inbrowse=debug` enables debug-level. Logs go to stderr.

## Reliability and timeouts

The engine is hardened so one page cannot hang, crash, or wedge a worker. A V8 watchdog terminates runaway scripts and microtask storms, DOM ops are panic-safe, cyclic DOM mutations are rejected, and the CDP server terminates any single command that overruns its budget so a hung session cannot stall the others. Scripted `fetch()`/XHR and navigation are timeout-bounded. You can point the server at arbitrary or heavy pages without a stuck worker.

Tune the bounds with environment variables (see [Environment variables](Environment-variables.md)):

```bash
INBROWSE_NAV_TIMEOUT_MS=60000 \
INBROWSE_SCRIPT_DEADLINE_MS=45000 \
INBROWSE_MODULE_BUDGET_MS=10000 \
INBROWSE_CDP_COMMAND_TIMEOUT_MS=70000 \
INBROWSE_FETCH_TIMEOUT_MS=20000 \
  inbrowse serve
```

`INBROWSE_NAV_TIMEOUT_MS` is the per-navigation ceiling (default 30000). `INBROWSE_SCRIPT_DEADLINE_MS` bounds the complete script phase (default 30000), while `INBROWSE_MODULE_BUDGET_MS` bounds each enhancement module's graph loading and evaluation (default 3000; unmounted SPA shells use the full script deadline). `INBROWSE_CDP_COMMAND_TIMEOUT_MS` is the outer per-CDP-command V8 deadline (default 60000, `0` disables), so keep it above the navigation ceiling. `INBROWSE_FETCH_TIMEOUT_MS` bounds scripted fetch/XHR and module network requests (default 30000).
