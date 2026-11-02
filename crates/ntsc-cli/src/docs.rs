use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::docs_data::EMBEDDED;

/// A documentation page, either read from a checkout's docs directory or
/// from the copy embedded at build time.
pub(crate) struct DocPage {
    pub title: String,
    pub path: String,
    pub body: String,
}

/// Load docs from disk when a checkout is available so edits show up without
/// rebuilding. Installed binaries fall back to the embedded copy.
pub(crate) fn load_docs() -> Vec<DocPage> {
    if let Some(root) = find_docs_dir()
        && let Ok(pages) = read_pages(&root)
        && !pages.is_empty()
    {
        return pages;
    }

    EMBEDDED
        .iter()
        .map(|doc| DocPage {
            title: doc.title.to_string(),
            path: doc.path.to_string(),
            body: doc.body.to_string(),
        })
        .collect()
}

fn find_docs_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("NTSC_DOCS_DIR") {
        let dir = PathBuf::from(dir);
        if is_docs_dir(&dir) {
            return Some(dir);
        }
    }

    if let Ok(cwd) = env::current_dir()
        && let Some(dir) = find_upwards(&cwd)
    {
        return Some(dir);
    }

    if let Ok(exe) = env::current_exe()
        && let Some(parent) = exe.parent()
    {
        for candidate in [
            parent.join("docs"),
            parent.join("../share/ntsc/docs"),
            parent.join("../share/doc/ntsc"),
        ] {
            if is_docs_dir(&candidate) {
                return Some(candidate);
            }
        }
        if let Some(dir) = find_upwards(parent) {
            return Some(dir);
        }
    }

    None
}

fn find_upwards(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join("docs");
        if is_docs_dir(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_docs_dir(dir: &Path) -> bool {
    dir.join("README.md").is_file() && dir.join("guide").is_dir() && dir.join("reference").is_dir()
}

fn read_pages(root: &Path) -> io::Result<Vec<DocPage>> {
    let mut files = Vec::new();
    collect_markdown(root, &mut files)?;
    files.sort();

    let mut pages = Vec::with_capacity(files.len());
    for path in files {
        let body = fs::read_to_string(&path)?;
        let title = title_for(&path, &body);
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        pages.push(DocPage {
            title,
            path: relative,
            body,
        });
    }
    Ok(pages)
}

fn collect_markdown(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

fn title_for(path: &Path, body: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn read_pages_orders_files_and_uses_headings() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("ntsc-docs-test-{}-{stamp}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("guide")).unwrap();
        fs::create_dir_all(root.join("reference")).unwrap();
        fs::write(root.join("README.md"), "# Top\n").unwrap();
        fs::write(root.join("guide/b.md"), "# Second\n").unwrap();
        fs::write(root.join("guide/a.md"), "# First\n").unwrap();

        let pages = read_pages(&root).unwrap();
        let titles: Vec<_> = pages.iter().map(|page| page.title.as_str()).collect();
        assert_eq!(titles, ["Top", "First", "Second"]);
        assert_eq!(pages[1].path, "guide/a.md");

        fs::remove_dir_all(&root).unwrap();
    }
}
