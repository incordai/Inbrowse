## Linux x86_64

```bash
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-x86_64-linux.tar.gz
tar xzf inbrowse-x86_64-linux.tar.gz
./inbrowse --version
```

## Linux ARM64

```bash
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-aarch64-linux.tar.gz
tar xzf inbrowse-aarch64-linux.tar.gz
./inbrowse --version
```

Linux builds target Ubuntu 22.04 and require glibc 2.35+.

## macOS Apple Silicon

```bash
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-aarch64-macos.tar.gz
tar xzf inbrowse-aarch64-macos.tar.gz
./inbrowse --version
```

## macOS Intel

```bash
curl -LO https://github.com/incordai/Inbrowse/releases/latest/download/inbrowse-x86_64-macos.tar.gz
tar xzf inbrowse-x86_64-macos.tar.gz
./inbrowse --version
```

## Windows

Download the `.zip` from [Releases](https://github.com/incordai/Inbrowse/releases), extract, run `inbrowse.exe --version`.

## Docker

Build the image from the repository's `Dockerfile`, then run it:

```bash
docker build -t inbrowse .
docker run -d --name inbrowse -p 127.0.0.1:9222:9222 inbrowse
```

The image is built on `distroless/cc:nonroot`, with no shell or package manager in the runtime image, running as uid 65532. Note the `-p 127.0.0.1:...` above: it publishes the port to host loopback only. A mounted `--storage-dir` must be writable by uid 65532 — see [Run in production at scale](Run-in-production-at-scale.md#the-container-does-not-run-as-root).

Release archives and the Docker image include the rendering engine. Source
builders must pass `--features render`; see [Build from source](Build-from-source.md).

## From source

See [Build from source](Build-from-source.md).

## What's in the archive

- `inbrowse`: CLI and CDP server.
- `inbrowse-worker`: helper for the parallel `scrape` command. Keep both in the same directory.

Archive suffixes identify the feature set: no suffix includes rendering,
`-stealth` includes rendering and stealth, `-no-render` includes neither, and
`-no-render-stealth` includes stealth without rendering.

## Smoke test

```bash
./inbrowse fetch https://example.com --eval "document.title"
./inbrowse fetch https://example.com --screenshot smoke.png
```

Expected output: `"Example Domain"`, followed by a nonempty PNG at `smoke.png`.

## Troubleshooting

`cannot execute binary file`: wrong arch. Check `uname -m`.

`GLIBC_2.35 not found`: distro is older than Ubuntu 22.04. Use Docker or build from source.

macOS Gatekeeper warning: `xattr -d com.apple.quarantine ./inbrowse`.
