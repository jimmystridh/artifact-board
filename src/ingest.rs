//! Bounded, in-memory ingestion. Limits are local prototype defaults, not product policy.
use pulldown_cmark::{Event, Options, Parser, Tag, html};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

pub const MAX_FILES: usize = 2048;
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ZIP_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct Prepared {
    pub files: BTreeMap<String, Vec<u8>>,
    pub entrypoint: String,
    pub warnings: Vec<String>,
}

fn path_ok(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.contains(['\\', ':', '%', '?', '#'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.ends_with([' ', '.']))
    {
        return Err(format!("Unsafe or ambiguous relative path: {path:?}"));
    }
    Ok(())
}

pub fn validate_files(
    files: BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if files.is_empty() {
        return Err("Upload contains no files".into());
    }
    if files.len() > MAX_FILES {
        return Err(format!("Too many files; local limit is {MAX_FILES}"));
    }
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    for (path, bytes) in &files {
        path_ok(path)?;
        if !paths.insert(path.to_lowercase()) {
            return Err(format!("Case-conflicting path: {path}"));
        }
        if bytes.len() > MAX_FILE_BYTES {
            return Err(format!("File exceeds local 16 MiB limit: {path}"));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or("Expanded size overflow")?;
        if total > MAX_TOTAL_BYTES {
            return Err("Upload exceeds local 64 MiB expanded limit".into());
        }
    }
    for path in &paths {
        let mut parent = path.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            if paths.contains(prefix) {
                return Err(format!("File/directory conflict: {prefix}"));
            }
            parent = prefix;
        }
    }
    Ok(files)
}

pub fn extract_zip(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if bytes.len() > MAX_ZIP_BYTES {
        return Err("ZIP exceeds local 32 MiB compressed limit".into());
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("Invalid ZIP: {e}"))?;
    if archive.len() > MAX_FILES {
        return Err(format!("ZIP exceeds local {MAX_FILES} entry limit"));
    }
    let mut files = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut total = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Unsupported ZIP entry: {e}"))?;
        let raw = std::str::from_utf8(entry.name_raw()).map_err(|_| "ZIP paths must be UTF-8")?;
        let directory = entry.is_dir();
        let name = if directory {
            raw.strip_suffix('/').unwrap_or(raw)
        } else {
            raw
        }
        .to_string();
        path_ok(&name)?;
        let key = name.to_lowercase();
        if !seen.insert(key.clone()) {
            return Err(format!("Duplicate or case-conflicting ZIP path: {name}"));
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err(format!("Links and special files are unsupported: {name}"));
            }
            if (kind == 0o040000 && !directory) || (kind == 0o100000 && directory) {
                return Err(format!("Inconsistent ZIP file type: {name}"));
            }
        }
        if directory {
            directories.insert(key);
            continue;
        }
        if entry.size() > MAX_FILE_BYTES as u64 {
            return Err(format!("ZIP entry exceeds local 16 MiB limit: {name}"));
        }
        if entry.size() > 1024 * 1024 && entry.size() / entry.compressed_size().max(1) > 200 {
            return Err(format!("Suspicious ZIP expansion ratio: {name}"));
        }
        let mut data = Vec::new();
        (&mut entry)
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|e| format!("Cannot read ZIP entry {name}: {e}"))?;
        if data.len() > MAX_FILE_BYTES {
            return Err(format!("ZIP entry expands beyond limit: {name}"));
        }
        total = total
            .checked_add(data.len())
            .ok_or("Expanded size overflow")?;
        if total > MAX_TOTAL_BYTES {
            return Err("ZIP expands beyond local 64 MiB limit".into());
        }
        files.insert(name, data);
    }
    let files = validate_files(files)?;
    let file_keys: BTreeSet<_> = files.keys().map(|p| p.to_lowercase()).collect();
    for dir in directories {
        let mut path = dir.as_str();
        loop {
            if file_keys.contains(path) {
                return Err(format!("File/directory conflict: {path}"));
            }
            match path.rsplit_once('/') {
                Some((prefix, _)) => path = prefix,
                None => break,
            }
        }
    }
    Ok(files)
}

fn safe_url(url: &str) -> bool {
    let clean: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    if clean.starts_with("//") || clean.contains('\\') {
        return false;
    }
    match clean.split_once(':') {
        Some((scheme, _)) => matches!(
            scheme.to_ascii_lowercase().as_str(),
            "https" | "http" | "mailto"
        ),
        None => true,
    }
}

pub trait ReactBuildAdapter {
    /// Implementations must provide disposable resource-bounded isolation before executing any source.
    fn build(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        locked: bool,
    ) -> Result<BTreeMap<String, Vec<u8>>, String>;
}
struct UnavailableBuild;
impl ReactBuildAdapter for UnavailableBuild {
    fn build(
        &self,
        _: &BTreeMap<String, Vec<u8>>,
        _: bool,
    ) -> Result<BTreeMap<String, Vec<u8>>, String> {
        Err("React source is valid, but isolated builds are unavailable on this computer. No Docker/Podman runtime or writable resource-control boundary is available. No source was executed. Configure a separate bounded container worker, or upload prebuilt static dist files.".into())
    }
}

pub fn prepare(
    kind: &str,
    files: BTreeMap<String, Vec<u8>>,
    entrypoint: &str,
) -> Result<Prepared, String> {
    prepare_with_mode(kind, files, entrypoint, "container")
}

/// Server/operator selects mode; never copy this value from an upload request.
pub fn prepare_with_mode(
    kind: &str,
    files: BTreeMap<String, Vec<u8>>,
    entrypoint: &str,
    mode: &str,
) -> Result<Prepared, String> {
    match mode {
        "container" => prepare_with_adapter(kind, files, entrypoint, &UnavailableBuild),
        "trusted-host" => {
            let mut prepared = prepare_with_adapter(kind, files, entrypoint, &TrustedHostBuild)?;
            if kind == "react" {
                prepared.warnings.push("TRUSTED-HOST MODE: uploaded install/build scripts executed as the server user without isolation. Use only sources you trust.".into());
            }
            Ok(prepared)
        }
        _ => Err("Unknown operator build mode; use container or trusted-host".into()),
    }
}

static BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct TrustedHostBuild;
impl ReactBuildAdapter for TrustedHostBuild {
    fn build(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        locked: bool,
    ) -> Result<BTreeMap<String, Vec<u8>>, String> {
        use std::{
            fs,
            time::{Duration, Instant},
        };
        let _guard = BUILD_LOCK
            .try_lock()
            .map_err(|_| "A trusted-host build is already running; retry later")?;
        let dir = tempfile::Builder::new()
            .prefix("artifact-build-")
            .tempdir()
            .map_err(|e| format!("Cannot create private build directory: {e}"))?;
        let work = dir.path().join("source");
        let home = dir.path().join("home");
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        for (name, bytes) in files {
            let path = work.join(name);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        // Operator-only absolute directory, never sourced from the upload manifest.
        let node_dir = std::env::var("ARTIFACT_NODE_BIN_DIR").unwrap_or_else(|_| {
            "/opt/codex/runtimes/codex-primary-runtime/dependencies/node/bin".into()
        });
        if !std::path::Path::new(&node_dir).is_absolute() {
            return Err("ARTIFACT_NODE_BIN_DIR must be absolute".into());
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        run_npm(
            &work,
            &home,
            &node_dir,
            if locked {
                &["ci", "--no-audit", "--no-fund"]
            } else {
                &["install", "--no-audit", "--no-fund"]
            },
            deadline,
        )?;
        run_npm(&work, &home, &node_dir, &["run", "build"], deadline)?;
        let dist = work.join("dist");
        let meta = fs::symlink_metadata(&dist).map_err(|_| "Build did not produce dist/")?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("dist must be a real directory".into());
        }
        let mut output = BTreeMap::new();
        let mut total = 0;
        collect_output(&dist, &dist, &mut output, &mut total, deadline, 0)?;
        validate_files(output)
    }
}

fn collect_output(
    root: &std::path::Path,
    directory: &std::path::Path,
    output: &mut BTreeMap<String, Vec<u8>>,
    total: &mut usize,
    deadline: std::time::Instant,
    depth: usize,
) -> Result<(), String> {
    use std::fs;
    if depth > 64 {
        return Err("Build output directory nesting exceeds local limit".into());
    }
    for item in fs::read_dir(directory).map_err(|e| e.to_string())? {
        if std::time::Instant::now() > deadline {
            return Err("Build timed out collecting output".into());
        }
        let path = item.map_err(|e| e.to_string())?.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err("Build output symlinks are unsupported".into());
        }
        if meta.is_dir() {
            collect_output(root, &path, output, total, deadline, depth + 1)?;
        } else if meta.is_file() {
            if output.len() >= MAX_FILES || meta.len() > MAX_FILE_BYTES as u64 {
                return Err("Build output exceeds file limits".into());
            }
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .ok_or("Build paths must be UTF-8")?
                .to_string();
            path_ok(&name)?;
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .map_err(|e| e.to_string())?
                .take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            *total += bytes.len();
            if bytes.len() > MAX_FILE_BYTES || *total > MAX_TOTAL_BYTES {
                return Err("Build output exceeds expanded size limits".into());
            }
            output.insert(name, bytes);
        } else {
            return Err("Build output contains a special file".into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn run_npm(
    work: &std::path::Path,
    home: &std::path::Path,
    node_dir: &str,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<(), String> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut command = Command::new(std::path::Path::new(node_dir).join("npm"));
    command
        .args(args)
        .current_dir(work)
        .env_clear()
        .env("PATH", format!("{node_dir}:/usr/bin:/bin"))
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("CI", "true")
        .env("npm_config_cache", home.join("npm-cache"))
        .env("npm_config_userconfig", home.join("empty-npmrc"))
        .env("npm_config_registry", "https://registry.npmjs.org")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // Optional operator-configured routing only, never arbitrary inherited environment.
    for (setting, keys) in [
        (
            "ARTIFACT_BUILD_HTTP_PROXY",
            ["HTTP_PROXY", "http_proxy", "npm_config_proxy"],
        ),
        (
            "ARTIFACT_BUILD_HTTPS_PROXY",
            ["HTTPS_PROXY", "https_proxy", "npm_config_https_proxy"],
        ),
    ] {
        if let Ok(value) = std::env::var(setting) {
            let proxy = url::Url::parse(&value)
                .map_err(|_| format!("Invalid operator proxy URL in {setting}"))?;
            if !matches!(proxy.scheme(), "http" | "https")
                || proxy.host_str().is_none()
                || !proxy.username().is_empty()
                || proxy.password().is_some()
            {
                return Err(format!(
                    "{setting} requires an HTTP(S) proxy URL without credentials"
                ));
            }
            for key in keys {
                command.env(key, &value);
            }
        }
    }
    if let Ok(path) = std::env::var("ARTIFACT_BUILD_CA_FILE") {
        let certificate =
            std::fs::read_to_string(&path).map_err(|_| "Cannot read operator build CA file")?;
        if certificate.len() > 1024 * 1024
            || !certificate.contains("-----BEGIN CERTIFICATE-----")
            || certificate.contains("PRIVATE KEY")
        {
            return Err("Operator build CA file must contain public PEM certificates only".into());
        }
        command.env("NODE_EXTRA_CA_CERTS", path);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot launch trusted-host npm: {e}"))?;
    let pid = child.id() as i32;
    struct Cleanup(i32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    let cleanup = Cleanup(pid);
    fn collect(mut input: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            let mut buf = [0; 8192];
            loop {
                match input.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let keep = n.min((512 * 1024usize).saturating_sub(captured.len()));
                        captured.extend_from_slice(&buf[..keep]);
                    }
                }
            }
            String::from_utf8_lossy(&captured)
                .chars()
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .collect()
        })
    }
    let stdout = collect(child.stdout.take().unwrap());
    let stderr = collect(child.stderr.take().unwrap());
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(e) => break Err(format!("Cannot wait for npm: {e}")),
            Ok(None) if Instant::now() >= deadline => {
                break Err("Trusted-host build exceeded the local 120-second deadline".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
        }
    };
    drop(cleanup); // Kill only this build's process group, including leftover descendants.
    let _ = child.wait();
    // In trusted-host mode malicious code can escape its process group; never wait forever on a pipe.
    let drain_deadline = Instant::now() + Duration::from_millis(250);
    while (!stdout.is_finished() || !stderr.is_finished()) && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = if stdout.is_finished() {
        stdout.join().unwrap_or_default()
    } else {
        String::new()
    };
    let err = if stderr.is_finished() {
        stderr.join().unwrap_or_default()
    } else {
        String::new()
    };
    let status = result?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "npm {} failed ({status}).\n{out}\n{err}",
            args.join(" ")
        ))
    }
}
#[cfg(not(unix))]
fn run_npm(
    _: &std::path::Path,
    _: &std::path::Path,
    _: &str,
    _: &[&str],
    _: std::time::Instant,
) -> Result<(), String> {
    Err("Trusted-host builds currently require Unix process-group support".into())
}

pub fn prepare_with_adapter(
    kind: &str,
    files: BTreeMap<String, Vec<u8>>,
    entrypoint: &str,
    adapter: &dyn ReactBuildAdapter,
) -> Result<Prepared, String> {
    let mut files = validate_files(files)?;
    let mut warnings = Vec::new();
    match kind {
        "static" | "html" => {
            path_ok(entrypoint)?;
            if !entrypoint.to_ascii_lowercase().ends_with(".html")
                && !entrypoint.to_ascii_lowercase().ends_with(".htm")
            {
                return Err("Static entrypoint must be an HTML file".into());
            }
            if !files.contains_key(entrypoint) {
                return Err(format!("Entrypoint is missing: {entrypoint}"));
            }
            Ok(Prepared {
                files,
                entrypoint: entrypoint.into(),
                warnings,
            })
        }
        "markdown" | "md" => {
            path_ok(entrypoint)?;
            let source = files
                .get(entrypoint)
                .ok_or_else(|| format!("Markdown entrypoint is missing: {entrypoint}"))?;
            let source = std::str::from_utf8(source).map_err(|_| "Markdown must be UTF-8")?;
            let parser = Parser::new_ext(
                source,
                Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
            )
            .map(|e| match e {
                Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => Event::Start(Tag::Link {
                    link_type,
                    dest_url: if safe_url(&dest_url) {
                        dest_url
                    } else {
                        "#".into()
                    },
                    title,
                    id,
                }),
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => Event::Start(Tag::Image {
                    link_type,
                    dest_url: if safe_url(&dest_url) {
                        dest_url
                    } else {
                        "#".into()
                    },
                    title,
                    id,
                }),
                e => e,
            });
            let mut rendered = String::new();
            html::push_html(&mut rendered, parser);
            // Keep the generated entry in the same directory so local links/images retain their base.
            let output = match entrypoint.rsplit_once('/') {
                Some((dir, _)) => format!("{dir}/index.html"),
                None => "index.html".into(),
            };
            if output != entrypoint && files.contains_key(&output) {
                return Err(format!("Generated Markdown entry would overwrite {output}"));
            }
            let document = format!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Document</title><style>body{{margin:0;background:#fbfaf7;color:#252522;font:18px/1.75 system-ui,sans-serif}}article{{max-width:760px;margin:64px auto;padding:0 28px 80px}}h1,h2,h3{{line-height:1.2;letter-spacing:-.025em}}h1{{font-size:2.6em}}h2{{margin-top:2em}}a{{color:#326752}}img{{max-width:100%}}pre{{overflow:auto;padding:20px;background:#eeeee8;border-radius:8px}}code{{font-size:.88em}}blockquote{{margin-left:0;padding-left:24px;border-left:3px solid #93ad9a;color:#555}}table{{border-collapse:collapse;width:100%}}th,td{{border-bottom:1px solid #ddd;padding:10px;text-align:left}}@media(max-width:600px){{article{{margin-top:28px}}h1{{font-size:2em}}}}</style></head><body><article>{rendered}</article></body></html>"
            );
            files.insert(output.clone(), document.into_bytes());
            warnings.push("Markdown uses CommonMark with tables, strikethrough, and task lists. Embedded HTML is escaped; unsafe URL schemes are disabled. Relative assets resolve beside the source document.".into());
            Ok(Prepared {
                files: validate_files(files)?,
                entrypoint: output,
                warnings,
            })
        }
        "react" => {
            let package = files
                .get("package.json")
                .ok_or("React projects require package.json at the upload root")?;
            let package: serde_json::Value = serde_json::from_slice(package)
                .map_err(|e| format!("Invalid package.json: {e}"))?;
            let script = package
                .get("scripts")
                .and_then(|s| s.get("build"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty());
            if script.is_none() {
                return Err("React projects require a nonempty scripts.build; the build contract is npm run build producing dist/index.html".into());
            }
            let locked = files.contains_key("package-lock.json")
                || files.contains_key("npm-shrinkwrap.json");
            if !locked {
                warnings.push("No npm lockfile supplied. npm install would resolve dependencies at build time; reproducibility is not guaranteed. A lockfile is preferred, not required.".into());
            }
            if files
                .keys()
                .any(|p| p == "node_modules" || p.starts_with("node_modules/"))
            {
                return Err("Upload sources and a lockfile, not node_modules".into());
            }
            let result = adapter.build(&files, locked).map_err(|error| {
                if warnings.is_empty() {
                    error
                } else {
                    format!("{error} Warning: {}", warnings.join(" "))
                }
            })?;
            let result = validate_files(result)?;
            if !result.contains_key("index.html") {
                return Err("React build must produce dist/index.html (worker returns paths relative to dist)".into());
            }
            Ok(Prepared {
                files: result,
                entrypoint: "index.html".into(),
                warnings,
            })
        }
        _ => Err("Unsupported content kind; use static, markdown, or react".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn files(entries: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        entries
            .iter()
            .map(|(p, s)| (p.to_string(), s.as_bytes().to_vec()))
            .collect()
    }
    #[test]
    fn traversal_and_conflicts() {
        for p in [
            "../x", "/x", "C:/x", "a\\x", "a//x", "a/./x", "a/%2e/x", "a/x.",
        ] {
            assert!(validate_files(files(&[(p, "x")])).is_err(), "{p}");
        }
        assert!(validate_files(files(&[("A", "x"), ("a", "x")])).is_err());
        assert!(validate_files(files(&[("a", "x"), ("a/b", "x")])).is_err());
    }
    #[test]
    fn static_requires_entry() {
        assert!(prepare("static", files(&[("a.html", "hi")]), "index.html").is_err());
    }
    #[test]
    fn markdown_escapes_html_and_unsafe_links() {
        let p = prepare("markdown", files(&[("docs/readme.md", "# Hello\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1))\n\n![ok](image.png)")]), "docs/readme.md").unwrap();
        let html = String::from_utf8(p.files[&p.entrypoint].clone()).unwrap();
        assert_eq!(p.entrypoint, "docs/index.html");
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(html.contains("src=\"image.png\""));
    }
    #[test]
    fn markdown_wont_overwrite_asset() {
        assert!(
            prepare(
                "markdown",
                files(&[("a.md", "hi"), ("index.html", "keep")]),
                "a.md"
            )
            .is_err()
        );
    }
    #[test]
    fn react_fails_closed() {
        let e = prepare(
            "react",
            files(&[("package.json", r#"{"scripts":{"build":"vite build"}}"#)]),
            "",
        )
        .unwrap_err();
        assert!(e.contains("No source was executed"));
        assert!(e.contains("No npm lockfile"));
    }
    struct Fake;
    impl ReactBuildAdapter for Fake {
        fn build(
            &self,
            _: &BTreeMap<String, Vec<u8>>,
            _: bool,
        ) -> Result<BTreeMap<String, Vec<u8>>, String> {
            Ok(files(&[("index.html", "fixture")]))
        }
    }
    #[test]
    fn fake_react_contract_warns_without_lock() {
        let p = prepare_with_adapter(
            "react",
            files(&[("package.json", r#"{"scripts":{"build":"vite build"}}"#)]),
            "",
            &Fake,
        )
        .unwrap();
        assert_eq!(p.entrypoint, "index.html");
        assert_eq!(p.warnings.len(), 1);
    }
    #[test]
    fn react_requires_build_script() {
        assert!(
            prepare_with_adapter("react", files(&[("package.json", "{}")]), "", &Fake).is_err()
        );
    }
    #[test]
    fn zip_rejects_traversal_duplicate_case_symlink_and_bomb() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let archive = |names: &[&str], payload: &[u8]| {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for name in names {
                writer
                    .start_file(
                        *name,
                        SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .unwrap();
                writer.write_all(payload).unwrap();
            }
            writer.finish().unwrap().into_inner()
        };
        assert!(extract_zip(&archive(&["../index.html"], b"x")).is_err());
        assert!(extract_zip(&archive(&["A", "a"], b"x")).is_err());
        assert!(extract_zip(&archive(&["huge"], &vec![0; 2 * 1024 * 1024])).is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("link", "../outside", SimpleFileOptions::default())
            .unwrap();
        assert!(extract_zip(&writer.finish().unwrap().into_inner()).is_err());
        assert_eq!(
            extract_zip(&archive(&["index.html"], b"ok")).unwrap()["index.html"],
            b"ok"
        );
    }
    #[test]
    fn limits_enforced() {
        let mut f = files(&[("large", "")]);
        f.insert("large".into(), vec![0; MAX_FILE_BYTES + 1]);
        assert!(validate_files(f).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn owned_process_timeout_is_bounded_and_environment_scrubbed() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{Duration, Instant},
        };
        let dir = tempfile::tempdir().unwrap();
        let npm = dir.path().join("npm");
        fs::write(
            &npm,
            "#!/bin/sh\n[ -z \"$CARGO_HOME\" ] || exit 87\nsleep 10\n",
        )
        .unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        let error = run_npm(
            dir.path(),
            dir.path(),
            dir.path().to_str().unwrap(),
            &["run", "build"],
            start + Duration::from_millis(80),
        )
        .unwrap_err();
        assert!(error.contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[cfg(unix)]
    #[test]
    fn output_symlinks_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("index.html")).unwrap();
        let error = collect_output(
            dir.path(),
            dir.path(),
            &mut BTreeMap::new(),
            &mut 0,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            0,
        )
        .unwrap_err();
        assert!(error.contains("symlink"));
    }
    #[test]
    fn react_adapter_output_contract_and_limits() {
        struct Bad(bool);
        impl ReactBuildAdapter for Bad {
            fn build(
                &self,
                _: &BTreeMap<String, Vec<u8>>,
                _: bool,
            ) -> Result<BTreeMap<String, Vec<u8>>, String> {
                if self.0 {
                    Ok(BTreeMap::from([(
                        "index.html".into(),
                        vec![0; MAX_FILE_BYTES + 1],
                    )]))
                } else {
                    Ok(files(&[("other.html", "no entry")]))
                }
            }
        }
        let source = files(&[("package.json", r#"{"scripts":{"build":"anything"}}"#)]);
        assert!(
            prepare_with_adapter("react", source.clone(), "", &Bad(false))
                .unwrap_err()
                .contains("dist/index.html")
        );
        assert!(
            prepare_with_adapter("react", source, "", &Bad(true))
                .unwrap_err()
                .contains("limit")
        );
    }
    #[cfg(unix)]
    #[test]
    fn build_failure_logs_are_bounded() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{Duration, Instant},
        };
        let dir = tempfile::tempdir().unwrap();
        let npm = dir.path().join("npm");
        fs::write(
            &npm,
            "#!/bin/sh\nhead -c 2000000 /dev/zero | tr '\\000' x >&2\nexit 9\n",
        )
        .unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
        let error = run_npm(
            dir.path(),
            dir.path(),
            dir.path().to_str().unwrap(),
            &["run", "build"],
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.contains("failed"));
        assert!(error.len() < 1024 * 1024);
        assert!(error.len() >= 512 * 1024);
    }
}
