use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct RustModule {
    pub module: String,
    #[serde(default, deserialize_with = "read_functions")]
    pub functions: Vec<RustFunction>,
}

fn read_functions<'de, D>(deserializer: D) -> Result<Vec<RustFunction>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut functions = Vec::new();
    for value in values {
        if value.get("status").and_then(serde_json::Value::as_str) != Some("typed") {
            continue;
        }
        match serde_json::from_value::<RustFunction>(value) {
            Ok(function) => functions.push(function),
            Err(error) => eprintln!("ntsc-lsp: invalid Rust function metadata: {error}"),
        }
    }
    Ok(functions)
}

#[derive(Debug, Deserialize)]
pub struct RustFunction {
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub params: Vec<RustParameter>,
    #[serde(rename = "return", default)]
    pub result: RustType,
    #[serde(rename = "async", default)]
    pub is_async: bool,
    #[serde(default)]
    pub doc: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct RustType {
    #[serde(rename = "type", default)]
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct RustParameter {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub label: String,
}

impl RustFunction {
    pub fn callable(&self) -> bool {
        self.status == "typed"
            && self
                .symbol
                .as_ref()
                .is_some_and(|symbol| !symbol.is_empty())
            && !self.result.label.is_empty()
            && self
                .params
                .iter()
                .all(|parameter| !parameter.label.is_empty())
    }

    pub fn signature(&self, module: &str) -> String {
        let parameters = self
            .params
            .iter()
            .map(|parameter| format!("{} {}", parameter.label, parameter.name))
            .collect::<Vec<_>>()
            .join(", ");
        let prefix = if self.is_async { "async " } else { "" };
        format!(
            "{prefix}@{module}.{}({parameters}) -> {}",
            self.name, self.result.label
        )
    }
}

struct CachedModule {
    modified: Option<SystemTime>,
    bytes: u64,
    value: RustModule,
}

#[derive(Default)]
pub struct MetadataCache {
    files: HashMap<PathBuf, CachedModule>,
}

impl MetadataCache {
    pub fn refresh(&mut self, roots: &[PathBuf]) {
        let mut retained = HashMap::new();
        for root in roots {
            let directory = root.join(".ntsc");
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    eprintln!("ntsc-lsp: cannot read {}: {error}", directory.display());
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        eprintln!("ntsc-lsp: cannot read metadata directory entry: {error}");
                        continue;
                    }
                };
                let name = entry.file_name();
                match fs::metadata(entry.path()) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => continue,
                    Err(error) => {
                        eprintln!("ntsc-lsp: cannot inspect metadata directory: {error}");
                        continue;
                    }
                }
                let path = entry
                    .path()
                    .join(format!("{}.export.json", name.to_string_lossy()));
                let metadata = match fs::metadata(&path) {
                    Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_MANIFEST_BYTES => {
                        metadata
                    }
                    Ok(_) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        eprintln!("ntsc-lsp: cannot read {}: {error}", path.display());
                        continue;
                    }
                };
                let modified = match metadata.modified() {
                    Ok(modified) => Some(modified),
                    Err(error) => {
                        eprintln!("ntsc-lsp: cannot inspect {}: {error}", path.display());
                        None
                    }
                };
                if let Some(cached) = self.files.remove(&path)
                    && modified.is_some()
                    && cached.modified == modified
                    && cached.bytes == metadata.len()
                {
                    retained.insert(path, cached);
                    continue;
                }
                match fs::read(&path).and_then(|bytes| {
                    serde_json::from_slice::<RustModule>(&bytes).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    })
                }) {
                    Ok(value) => {
                        retained.insert(
                            path,
                            CachedModule {
                                modified,
                                bytes: metadata.len(),
                                value,
                            },
                        );
                    }
                    Err(error) => {
                        eprintln!("ntsc-lsp: invalid metadata {}: {error}", path.display())
                    }
                }
            }
        }
        self.files = retained;
    }

    pub fn modules<'a>(&'a self, root: &Path) -> impl Iterator<Item = &'a RustModule> {
        let directory = root.join(".ntsc");
        self.files
            .iter()
            .filter_map(move |(path, cached)| path.starts_with(&directory).then_some(&cached.value))
    }

    pub fn module(&self, root: &Path, name: &str) -> Option<&RustModule> {
        let name = name.trim_start_matches('@').replace('-', "_");
        self.modules(root)
            .find(|module| module.module.replace('-', "_") == name)
    }
}

pub fn project_root(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(file);
    parent
        .ancestors()
        .find(|directory| directory.join("neutron.toml").is_file())
        .unwrap_or(parent)
        .to_path_buf()
}
