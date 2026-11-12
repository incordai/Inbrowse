use std::env;

fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_version(version: String) -> String {
    version.strip_prefix('v').unwrap_or(&version).to_string()
}

fn github_tag_version() -> Option<String> {
    if env_value("GITHUB_REF_TYPE").as_deref() == Some("tag") {
        return env_value("GITHUB_REF_NAME").map(normalize_version);
    }

    env_value("GITHUB_REF")
        .and_then(|value| value.strip_prefix("refs/tags/").map(str::to_owned))
        .map(normalize_version)
}

/// Source tarballs (GitHub's `/archive/refs/tags/vX.Y.Z.tar.gz`, which Homebrew
/// and nixpkgs build from) have neither git metadata nor GITHUB_* env, so the
/// tag is otherwise unknown. `.git_archival.txt` is marked `export-subst`, so
/// `git archive` (and GitHub's tarball endpoint) expands its placeholder to the
/// tag; in a git checkout the placeholder stays unexpanded and is ignored.
fn archive_version() -> Option<String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let path = std::path::Path::new(&manifest_dir).join("../../.git_archival.txt");
    if !path.exists() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    std::fs::read_to_string(&path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("describe-name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains("$Format"))
        .map(|value| normalize_version(value.to_string()))
}

/// Local builds are not cut from a tag, so the bare workspace version cannot
/// tell two checkouts apart. Append the commit when git is there to answer;
/// tarball and minimal-image builds without git keep the bare version.
fn local_version() -> String {
    let package = env::var("CARGO_PKG_VERSION").expect("Cargo sets CARGO_PKG_VERSION");
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty());
    match sha {
        Some(sha) => format!("{package}-dev+{sha}"),
        None => package,
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=INBROWSE_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-env-changed=GITHUB_REF");

    let version = env_value("INBROWSE_VERSION")
        .map(normalize_version)
        .or_else(github_tag_version)
        .or_else(archive_version)
        .unwrap_or_else(local_version);

    println!("cargo:rustc-env=INBROWSE_BUILD_VERSION={version}");

    delay_load_media_dlls();
    #[cfg(windows)]
    windows_resources(&version);
}

/// Windows shows a binary's icon in Explorer and its version resource in
/// Properties > Details and Task Manager. Without one, `inbrowse.exe` has a
/// generic icon and blank name, company and version fields.
#[cfg(windows)]
fn windows_resources(version: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let icon = std::path::Path::new(&manifest_dir).join("../../assets/inbrowse.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon = icon
        .canonicalize()
        .unwrap_or_else(|err| panic!("{}: {err}", icon.display()));
    let out_dir = std::path::PathBuf::from(env::var("OUT_DIR").expect("Cargo sets OUT_DIR"));

    for (bin, description) in [
        ("inbrowse", "Inbrowse"),
        ("inbrowse-worker", "Inbrowse scrape worker"),
    ] {
        let rc = out_dir.join(format!("{bin}.rc"));
        std::fs::write(&rc, resource_script(bin, description, version, &icon))
            .unwrap_or_else(|err| panic!("{}: {err}", rc.display()));
        match embed_resource::compile_for(&rc, [bin], embed_resource::NONE) {
            embed_resource::CompilationResult::NotAttempted(why) => {
                println!(
                    "cargo:warning=no Windows resource compiler, {bin}.exe gets no icon: {why}"
                );
            }
            embed_resource::CompilationResult::Failed(why) => {
                panic!("compiling {}: {why}", rc.display());
            }
            embed_resource::CompilationResult::Ok
            | embed_resource::CompilationResult::NotWindows => {}
        }
    }
}

/// A resource script with the icon and a VERSIONINFO block. The numeric
/// version is the leading `major.minor.patch` of `version` (a tag such as
/// `0.3.1`, or `0.3.1-dev+sha` for local builds); the string fields keep it whole.
#[cfg(windows)]
fn resource_script(bin: &str, description: &str, version: &str, icon: &std::path::Path) -> String {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .take(3)
        .map(|part| part.parse::<u16>().unwrap_or(0));
    let [major, minor, patch] = [(); 3].map(|_| parts.next().unwrap_or(0));
    // `canonicalize` gives a `\\?\` verbatim path; rc.exe wants a plain one,
    // with backslashes escaped inside the string literal.
    let icon = icon.to_string_lossy();
    let icon = icon
        .strip_prefix(r"\\?\")
        .unwrap_or(&icon)
        .replace('\\', r"\\");
    let text = |value: &str| value.replace('"', "\"\"");
    let (version, description) = (text(version), text(description));
    format!(
        r#"#include <winver.h>

1 ICON "{icon}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK VS_FFI_FILEFLAGSMASK
FILEFLAGS 0
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
FILESUBTYPE VFT2_UNKNOWN
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "Incord AI"
            VALUE "FileDescription", "{description}"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "{bin}"
            VALUE "LegalCopyright", "Copyright 2026 Incord AI"
            VALUE "OriginalFilename", "{bin}.exe"
            VALUE "ProductName", "Inbrowse"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
}

/// The video decoder crate (moq-video, via inbrowse-media) imports Media
/// Foundation and Direct3D 11 on Windows for its hardware decoder, which
/// Inbrowse never uses (inbrowse-media decodes in software). Loaded at
/// startup, `mfplat.dll` stops the whole binary from starting on Windows N
/// editions without the Media Feature Pack, and both cost startup time.
/// Delay-loading means they load only if ever called.
fn delay_load_media_dlls() {
    let windows_msvc = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows_msvc || env::var_os("CARGO_FEATURE_MEDIA").is_none() {
        return;
    }
    // Only `inbrowse` links the decoder; `inbrowse-worker` would warn that it
    // imports nothing from these.
    for dll in ["mfplat.dll", "d3d11.dll"] {
        println!("cargo:rustc-link-arg-bin=inbrowse=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bin=inbrowse=delayimp.lib");
}
