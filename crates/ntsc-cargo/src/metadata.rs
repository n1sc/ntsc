//! Versioned metadata shared by generated Rust API manifests.
//!
//! This module is the standard for the adaptor autogen contract: the
//! `<crate>.export.json` manifest emitted beside every generated adaptor
//! crate. The emitter (`adaptor`) serializes [`ExportManifest`] with
//! `serde_json`; every reader (`ntsc cargo doc`, `ntsc cargo coverage`,
//! codegen's export-type loader) parses the same shape tolerantly —
//! unknown fields are ignored, missing sections read as empty — so shape
//! additions never break older readers. The canonical byte layout is
//! whatever `serde_json::to_string_pretty` produces for these structs
//! (struct field order, no extra whitespace); freshness checks compare
//! bytes, so deterministic serialization means unchanged APIs never
//! rewrite the manifest and never invalidate downstream caches.
//!
//! Wire-shape history (see `API_METADATA_SCHEMA_VERSION`):
//! - v2 adds `callbacks` (generated `__NtscCb` impls and poll families).
//! - v3 records every discovered-but-unbridgeable item with its true
//!   status and reason, and adds `array[...]` collection labels.
//! - `types`/`enums` are reserved and empty in v3.

use serde::{Deserialize, Serialize};

/// Current version of the generated API metadata schema.
///
/// Version 2 adds the top-level `callbacks` section describing each
/// generated `__NtscCb` trait impl and its poll family; readers that only
/// consume `functions`/`types`/`enums` are unaffected.
/// Version 3 records every discovered-but-unbridgeable item with its true
/// status (`unsupported`, `conditional`, `probe_failed`) and reason instead
/// of collapsing failures, and adds `array[...]` collection labels.
pub const API_METADATA_SCHEMA_VERSION: u32 = 3;

/// Availability status recorded for every API item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiStatus {
    Typed,
    Opaque,
    Conditional,
    Unsupported,
    ProbeFailed,
}

/// Status and optional diagnostic details for one API item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiItemStatus {
    pub status: ApiStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl ApiItemStatus {
    /// A directly generated item.
    pub const fn typed() -> Self {
        Self {
            status: ApiStatus::Typed,
            reason: None,
            remedy: None,
        }
    }
}

impl ApiStatus {
    /// The stable wire name used in export manifests.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Opaque => "opaque",
            Self::Conditional => "conditional",
            Self::Unsupported => "unsupported",
            Self::ProbeFailed => "probe_failed",
        }
    }
}

/// Versioned top-level API metadata envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiMetadata {
    pub schema_version: u32,
    pub module: String,
}

/// The canonical `<crate>.export.json` document: every bridge the
/// generated adaptor exposes, every discovered-but-unbridgeable item,
/// and every callback trait the adaptor implements.
///
/// Field order is the wire order. `types`/`enums` are reserved and
/// always empty in v3; readers must tolerate their absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub schema_version: u32,
    pub module: String,
    pub functions: Vec<ExportEntry>,
    pub callbacks: Vec<ExportCallback>,
    #[serde(default)]
    pub types: Vec<serde_json::Value>,
    #[serde(default)]
    pub enums: Vec<serde_json::Value>,
}

/// One entry of `ExportManifest::functions`: either a generated bridge
/// or a recorded failure. The shapes differ, so the wire form is
/// untagged — readers distinguish entries by their `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExportEntry {
    Typed(ExportTyped),
    Untyped(ExportUntyped),
}

/// A generated bridge: its Neutron-visible name, crate-rooted call
/// path, tier-1 parameter/return labels, link symbol, and whether
/// Neutron must await it (`_async` offloads and native `async` fns).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTyped {
    pub name: String,
    pub path: String,
    pub status: ApiStatus,
    pub params: Vec<ExportParam>,
    #[serde(rename = "return")]
    pub ret: ExportReturn,
    pub symbol: String,
    #[serde(rename = "async")]
    pub is_async: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// One tier-1 bridge parameter: its `a{index}` name and manifest
/// label (`"string"`, `"array[int]"`, `"object[Widget]"`, ...).
/// `origin` names the owning dependency crate when the type lives in
/// another adaptor (`"serenity"` for `object[client::ClientBuilder]`
/// bridged by the `songbird` adaptor); readers resolve methods and drop
/// glue through the origin, since handles live in one shared space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportParam {
    pub name: String,
    pub tier: u32,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// One tier-1 bridge return: its manifest label. `tier` is always 1;
/// only tier-1 signatures are ever exported. `origin` mirrors
/// [`ExportParam::origin`] for foreign-typed returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReturn {
    pub tier: u32,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A discovered-but-unbridgeable item: its true status, why it has
/// no bridge, and the compact Rust signature when one exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportUntyped {
    pub name: String,
    pub path: String,
    pub status: ApiStatus,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// One callback trait the adaptor implements: the generated
/// `__NtscCb_<Short>` struct, its event queue, and the poll family
/// Neutron drains (`<Type>_poll`, `<Type>_arg_count`, `<Type>_arg`,
/// `<Type>_arg_str`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCallback {
    #[serde(rename = "trait")]
    pub trait_path: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub methods: Vec<ExportCallbackMethod>,
}

/// One overridden callback method: its queued argument labels in
/// order (`_arg` for handles/ints, `_arg_str` for strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCallbackMethod {
    pub name: String,
    pub params: Vec<String>,
    #[serde(rename = "async")]
    pub is_async: bool,
}

/// Parse one export manifest tolerantly: unknown fields are ignored
/// and missing sections default to empty, so newer producers never
/// break older readers. A malformed document is still an error.
pub fn parse_export_manifest(content: &str) -> Result<ExportManifest, serde_json::Error> {
    serde_json::from_str(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_use_stable_wire_names() {
        let json = serde_json::to_string(&ApiStatus::ProbeFailed).unwrap();
        assert_eq!(json, "\"probe_failed\"");
    }

    #[test]
    fn optional_diagnostics_are_omitted() {
        let json = serde_json::to_value(ApiItemStatus::typed()).unwrap();
        assert_eq!(json, serde_json::json!({"status": "typed"}));
    }

    #[test]
    fn manifest_round_trips_hostile_strings() {
        let manifest = ExportManifest {
            schema_version: API_METADATA_SCHEMA_VERSION,
            module: "demo".into(),
            functions: vec![
                ExportEntry::Typed(ExportTyped {
                    name: "greet".into(),
                    path: "greet".into(),
                    status: ApiStatus::Typed,
                    params: vec![ExportParam {
                        name: "a0".into(),
                        tier: 1,
                        ty: "string".into(),
                        origin: None,
                    }],
                    ret: ExportReturn {
                        tier: 1,
                        ty: "string".into(),
                        origin: None,
                    },
                    symbol: "ntsc_adaptor_demo_greet".into(),
                    is_async: false,
                    doc: Some("says \"hi\"\nand bye \\ all".into()),
                }),
                ExportEntry::Untyped(ExportUntyped {
                    name: "bad".into(),
                    path: "bad".into(),
                    status: ApiStatus::Unsupported,
                    reason: "quote \" and newline\ninside".into(),
                    signature: None,
                }),
            ],
            callbacks: vec![ExportCallback {
                trait_path: "Handler".into(),
                ty: "__NtscCb_Handler".into(),
                methods: vec![ExportCallbackMethod {
                    name: "done".into(),
                    params: vec!["int".into()],
                    is_async: false,
                }],
            }],
            types: Vec::new(),
            enums: Vec::new(),
        };
        let text = serde_json::to_string_pretty(&manifest).unwrap();
        let back = parse_export_manifest(&text).unwrap();
        assert_eq!(back, manifest);
        // serde owns escaping now: the raw bytes stay valid JSON even
        // with quotes, backslashes, and newlines inside strings.
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], 3);
        assert_eq!(
            value["functions"][1]["reason"],
            "quote \" and newline\ninside"
        );
    }

    #[test]
    fn manifest_reader_ignores_unknown_fields() {
        let text = r#"{
            "schema_version": 99,
            "module": "demo",
            "functions": [{"name": "f", "path": "f", "status": "typed",
                "params": [], "return": {"tier": 1, "type": "void"},
                "symbol": "s", "async": false, "future_opt_in": true}],
            "callbacks": [],
            "types": [], "enums": [], "next_version_field": [1, 2]
        }"#;
        let manifest = parse_export_manifest(text).unwrap();
        assert_eq!(manifest.functions.len(), 1);
    }
}
