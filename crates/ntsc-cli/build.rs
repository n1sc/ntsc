use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let suffix = match (profile.as_str(), env::var_os("NTSC_PACKAGED_BUILD")) {
        ("debug", _) => "d",
        ("release", Some(_)) => "r",
        ("release", None) => "t",
        _ => "t",
    };
    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = build_time.as_secs();
    let unique_stamp = build_time.as_nanos();
    let commit = env::var("VERGEN_GIT_SHA").unwrap_or_default();
    // Compact base-36 encoding keeps the build ID short while remaining
    // unique per build. A two-build-per-nanosecond collision is impossible
    // within the build-time window.
    let id = if commit.is_empty() {
        format!("{}{suffix}", encode_base36(unique_stamp))
    } else {
        format!(
            "{}-{}{suffix}",
            &commit[..commit.len().min(7)],
            encode_base36(unique_stamp)
        )
    };
    println!("cargo:rustc-env=NTSC_BUILD_ID={id}");
    let date = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|date| date.trim().to_string())
        .filter(|date| !date.is_empty())
        .unwrap_or_else(|| seconds.to_string());
    println!("cargo:rustc-env=NTSC_BUILD_DATE={date}");
    println!("cargo:rerun-if-env-changed=NTSC_PACKAGED_BUILD");
    println!("cargo:rerun-if-env-changed=VERGEN_GIT_SHA");

    if let Err(error) = generate_docs() {
        println!("cargo:warning=could not embed documentation: {error}");
    }
}

fn encode_base36(mut value: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "0".into())
}

/// Embed every Markdown file under docs/ so the docs browser works from an
/// installed binary with no checkout on disk.
fn generate_docs() -> std::io::Result<()> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR"))?;
    let docs_root = std::path::PathBuf::from(manifest).join("../../docs");
    let mut pages = Vec::new();
    collect_pages(&docs_root, &docs_root, &mut pages)?;
    pages.sort_by(|left, right| left.0.cmp(&right.0));

    let mut generated = String::from(
        "pub(crate) struct EmbeddedDoc {\n    pub title: &'static str,\n    pub path: &'static str,\n    pub body: &'static str,\n}\n\npub(crate) const EMBEDDED: &[EmbeddedDoc] = &[\n",
    );
    for (relative, title) in &pages {
        generated.push_str(&format!(
            "    EmbeddedDoc {{ title: {title:?}, path: {relative:?}, body: include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../docs/{relative}\")) }},\n"
        ));
    }
    generated.push_str("];\n");

    let out_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR"))?;
    std::fs::write(
        std::path::PathBuf::from(out_dir).join("docs_data.rs"),
        generated,
    )?;
    println!("cargo:rerun-if-changed={}", docs_root.display());
    Ok(())
}

fn collect_pages(
    root: &std::path::Path,
    dir: &std::path::Path,
    pages: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_pages(root, &path, pages)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            let body = std::fs::read_to_string(&path)?;
            let title = title_for(&path, &body);
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            pages.push((relative, title));
        }
    }
    Ok(())
}

fn title_for(path: &std::path::Path, body: &str) -> String {
    body.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Documentation")
                .replace('-', " ")
        })
}
