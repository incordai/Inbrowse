## Requirements

- Rust 1.75+ ([rustup.rs](https://rustup.rs))
- C compiler (gcc or clang)
- ~5 GB free disk space (V8 compiles from source on first build)

Build time depends on the machine and cache state. Cargo reuses unchanged
dependencies, but a release source edit can still require substantial compilation
and linking. For the scoped incremental runtime workflow, see
[Fast edit/test loop](Testing-and-debugging.md#fast-edittest-loop).

## Build

```bash
git clone https://github.com/incordai/Inbrowse.git
cd inbrowse
cargo build --release -p inbrowse-cli --bins --features render
```

Binary is at `./target/release/inbrowse`.

This produces the release binary with geometry, screenshots, screencasting,
and PDF export.

## Rendering and stealth

```bash
cargo build --release -p inbrowse-cli --bins --features render,stealth
```

This is the complete rendering build with the stealth wreq/BoringSSL transport,
TLS fingerprint randomization, browser-identity protections, and tracker
blocklist. See [Configure stealth and proxies](Configure-stealth-and-proxies.md).

## Without rendering

```bash
cargo build --release -p inbrowse-cli --bins --no-default-features
cargo build --release -p inbrowse-cli --bins --no-default-features --features stealth
```

The second command keeps stealth while excluding layout, screenshots,
screencasting, and PDF export.

The stealth feature builds BoringSSL and generates Rust bindings. In addition
to the default requirements, install CMake, Clang, and the libclang/LLVM
development libraries. On Ubuntu/Debian:

```bash
sudo apt-get install build-essential cmake clang libclang-dev llvm-dev
```

On macOS, install the Xcode Command Line Tools and CMake. On Windows, install
the Visual Studio C++ Build Tools, CMake, and LLVM/Clang. Ensure the directory
containing `libclang` is available through `LIBCLANG_PATH` if bindgen cannot
locate it automatically.

On macOS 26 with the standalone Command Line Tools, Apple Clang may not find
libc++ while compiling BoringSSL. Use the active SDK for that build:

```bash
SDK_PATH="$(xcrun --show-sdk-path)"
SDKROOT="$SDK_PATH" CXXFLAGS="-isystem $SDK_PATH/usr/include/c++/v1" \
  cargo build --release -p inbrowse-cli --bins --features render,stealth
```

## Video and audio (`media` feature)

The `media` feature adds frame, audio and caption extraction from video and
audio files. Most of it is Rust or C that cargo compiles from bundled source
(H.264 through OpenH264, Opus, AAC). VP8 and VP9 use libvpx, which you install
once on the build machine.

Link libvpx **statically**. A static build carries libvpx inside the
`inbrowse` binary, so people who run a release need nothing installed. Without
`VPX_STATIC=1` the binary links libvpx dynamically and fails to start on
machines without it.

All three platforms use the same variables:

| Variable | Value |
|---|---|
| `VPX_LIB_DIR` | directory holding `libvpx.a` (`vpx.lib` on Windows) |
| `VPX_INCLUDE_DIR` | directory holding `vpx/vpx_decoder.h` |
| `VPX_VERSION` | installed libvpx version, for example `1.16.0` |
| `VPX_STATIC` | `1` |

Set `VPX_LIB_DIR` even when pkg-config could find libvpx. In pkg-config mode,
a library under `/usr` is linked dynamically even with `VPX_STATIC=1`.

NASM is optional. With NASM on `PATH`, OpenH264 builds its x86 assembly and
decodes faster. Without it, the build falls back to plain C. Set
`OPENH264_NO_ASM=1` to skip the assembly on purpose.

### Linux (Debian, Ubuntu)

```bash
sudo apt-get install build-essential pkg-config libvpx-dev nasm

# Ask the package where libvpx.a is: Debian and Ubuntu's vpx.pc reports
# libdir=/usr/lib, but the library is in /usr/lib/<arch>-linux-gnu.
export VPX_LIB_DIR="$(dirname "$(dpkg -L libvpx-dev | grep '/libvpx\.a$')")"
export VPX_INCLUDE_DIR="$(pkg-config --variable=includedir vpx)"
export VPX_VERSION="$(pkg-config --modversion vpx)"
export VPX_STATIC=1
ls "$VPX_LIB_DIR/libvpx.a"   # must exist

cargo build --release -p inbrowse-cli --bins --features render,media
ldd target/release/inbrowse | grep vpx   # must print nothing
```

On Fedora, install `libvpx-devel` and `nasm`. Check that `libvpx.a` exists;
if your distribution ships only the shared library, build libvpx from source
with `--enable-static --disable-shared --enable-pic` and point `VPX_LIB_DIR`
at the result.

### macOS

```bash
brew install libvpx nasm pkg-config

export VPX_LIB_DIR="$(brew --prefix libvpx)/lib"
export VPX_INCLUDE_DIR="$(brew --prefix libvpx)/include"
export VPX_VERSION="$(pkg-config --modversion vpx)"
export VPX_STATIC=1
ls "$VPX_LIB_DIR/libvpx.a"   # must exist

cargo build --release -p inbrowse-cli --bins --features render,media
otool -L target/release/inbrowse | grep vpx   # must print nothing
```

Homebrew builds for the macOS version and CPU it runs on. To build a binary for
older macOS releases, build libvpx yourself with the same
`MACOSX_DEPLOYMENT_TARGET` you use for Inbrowse.

### Windows

Install libvpx with [vcpkg](https://github.com/microsoft/vcpkg), using the
`x64-windows-static` triplet. Windows builds of Inbrowse use the static C
runtime (`+crt-static` in `.cargo/config.toml`, because V8's prebuilt library
is built against it), and that triplet makes a static library built against
the same runtime. Run this in PowerShell:

```powershell
git clone https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat
C:\vcpkg\vcpkg install libvpx:x64-windows-static

$env:VPX_LIB_DIR     = "C:\vcpkg\installed\x64-windows-static\lib"
$env:VPX_INCLUDE_DIR = "C:\vcpkg\installed\x64-windows-static\include"
$env:VPX_VERSION     = "1.16.0"   # Version: line of lib\pkgconfig\vpx.pc
$env:VPX_STATIC      = "1"

cargo build --release -p inbrowse-cli --bins --features render,media
```

Check the binary from a Developer PowerShell for Visual Studio. The list of
DLLs it loads must not include libvpx:

```powershell
dumpbin /dependents target\release\inbrowse.exe
```

For NASM on Windows, install it from [nasm.us](https://www.nasm.us) and add its
folder to `PATH`.

## OpenSSL on older systems

If the build fails on the vendored OpenSSL with an AVX-512 assembler error (common on older VPS hosts):

```bash
OPENSSL_NO_VENDOR=1 cargo build --release -p inbrowse-cli --bins --features render
```

Uses the system OpenSSL instead.

## Run from the build

```bash
./target/release/inbrowse --version
./target/release/inbrowse fetch https://example.com --eval "document.title"
```

Install system-wide:

```bash
cargo install --path crates/inbrowse-cli --features render
```

## Tests

```bash
cargo nextest run --release --features render --no-fail-fast
```

Integration suite:

```bash
python3 tests/test_all.py
```

Use `cargo nextest`, not `cargo test`: runtime tests require process isolation
because the engine owns a single V8 isolate per process.
