//! `ntsc doc` — an HTML reference generated from `##` doc comments.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use ntsc_ast::expr::FunctionParam;
use ntsc_ast::stmt::Stmt;
use ntsc_ast::types::TypeAnnotation;

/// One documented declaration, with the members of a class or enum nested
/// beneath it.
struct Item {
    kind: &'static str,
    signature: String,
    docs: Vec<String>,
    members: Vec<Item>,
}

/// Generate the reference under `<root>/build/doc` and return the index path.
pub(crate) fn generate(root: &Path) -> Result<PathBuf, String> {
    let sources = collect_sources(root)?;
    if sources.is_empty() {
        return Err(format!("no Neutron sources found under {}", root.display()));
    }

    let out_dir = root.join("build").join("doc");
    fs::create_dir_all(&out_dir)
        .map_err(|error| format!("cannot create {}: {error}", out_dir.display()))?;

    let mut pages = Vec::new();
    for path in &sources {
        let source = fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let items = extract(&source);
        if items.is_empty() {
            continue;
        }
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "page".to_string());
        let page = out_dir.join(format!("{}.html", slug(&title)));
        fs::write(&page, render_page(&title, &items))
            .map_err(|error| format!("cannot write {}: {error}", page.display()))?;
        pages.push((title, page));
    }

    if pages.is_empty() {
        return Err("no documented declarations were found".to_string());
    }

    let index = out_dir.join("index.html");
    fs::write(&index, render_index(&pages))
        .map_err(|error| format!("cannot write {}: {error}", index.display()))?;
    Ok(index)
}

/// Every `.nt` file under `root/src`, or under `root` when it has no `src`.
fn collect_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let start = if root.join("src").is_dir() {
        root.join("src")
    } else {
        root.to_path_buf()
    };
    let mut sources = Vec::new();
    let mut pending = vec![start];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !matches!(name.as_ref(), "build" | "target" | ".ntsc" | ".git") {
                    pending.push(path);
                }
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "nt") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    Ok(sources)
}

fn extract(source: &str) -> Vec<Item> {
    let tokens = ntsc_lexer::tokenize(source);
    let Ok(program) = ntsc_parser::parse(&tokens) else {
        return Vec::new();
    };
    let lines: Vec<&str> = source.lines().collect();
    program
        .statements
        .iter()
        .filter_map(|statement| item_from(statement, &lines))
        .collect()
}

fn item_from(statement: &Stmt, lines: &[&str]) -> Option<Item> {
    match statement {
        Stmt::Function {
            name,
            params,
            return_type,
            ..
        } => Some(Item {
            kind: "fun",
            signature: format!(
                "{}({}){}",
                name.lexeme(),
                render_params(params),
                render_return(return_type)
            ),
            docs: docs_above(lines, name.span.line),
            members: Vec::new(),
        }),
        Stmt::AsyncFunction {
            name,
            params,
            return_type,
            ..
        } => Some(Item {
            kind: "async fun",
            signature: format!(
                "{}({}){}",
                name.lexeme(),
                render_params(params),
                render_return(return_type)
            ),
            docs: docs_above(lines, name.span.line),
            members: Vec::new(),
        }),
        Stmt::Class {
            name, parent, body, ..
        } => Some(Item {
            kind: "class",
            signature: match parent {
                Some(parent) => format!("{} extends {}", name.lexeme(), parent.lexeme()),
                None => name.lexeme().to_string(),
            },
            docs: docs_above(lines, name.span.line),
            members: body
                .iter()
                .filter_map(|member| item_from(member, lines))
                .collect(),
        }),
        Stmt::Enum { name, members, .. } => Some(Item {
            kind: "enum",
            signature: name.lexeme().to_string(),
            docs: docs_above(lines, name.span.line),
            members: members
                .iter()
                .map(|member| Item {
                    kind: "case",
                    signature: if member.data_types.is_empty() {
                        member.name.lexeme().to_string()
                    } else {
                        format!(
                            "{}({})",
                            member.name.lexeme(),
                            member
                                .data_types
                                .iter()
                                .map(render_type)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
                    docs: docs_above(lines, member.name.span.line),
                    members: Vec::new(),
                })
                .collect(),
        }),
        Stmt::Test { name, .. } => Some(Item {
            kind: "test",
            signature: name.lexeme().to_string(),
            docs: docs_above(lines, name.span.line),
            members: Vec::new(),
        }),
        Stmt::TypeAlias { name, target, .. } => Some(Item {
            kind: "type",
            signature: format!("{} = {}", name.lexeme(), render_type(target)),
            docs: docs_above(lines, name.span.line),
            members: Vec::new(),
        }),
        Stmt::Var {
            name,
            type_annotation,
            ..
        } if type_annotation.is_some() => Some(Item {
            kind: "var",
            signature: format!(
                "{}: {}",
                name.lexeme(),
                render_type(type_annotation.as_ref()?)
            ),
            docs: docs_above(lines, name.span.line),
            members: Vec::new(),
        }),
        _ => None,
    }
}

/// Contiguous `##` lines directly above `line` (1-based).
fn docs_above(lines: &[&str], line: u32) -> Vec<String> {
    let mut docs = Vec::new();
    let mut index = line.saturating_sub(1) as usize;
    while index > 0 {
        index -= 1;
        let Some(text) = lines.get(index) else {
            break;
        };
        let Some(rest) = text.trim().strip_prefix("##") else {
            break;
        };
        docs.push(rest.trim().to_string());
    }
    docs.reverse();
    docs
}

fn render_params(params: &[FunctionParam]) -> String {
    params
        .iter()
        .map(|param| match &param.type_annotation {
            Some(annotation) => format!("{} {}", render_type(annotation), param.name.lexeme()),
            None => param.name.lexeme().to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_return(return_type: &Option<ntsc_ast::types::ReturnType>) -> String {
    match return_type {
        Some(annotation) => format!(" -> {}", render_type(&annotation.ty)),
        None => String::new(),
    }
}

fn render_type(annotation: &TypeAnnotation) -> String {
    match annotation {
        TypeAnnotation::Int => "int".to_string(),
        TypeAnnotation::Float => "float".to_string(),
        TypeAnnotation::String => "string".to_string(),
        TypeAnnotation::Bool => "bool".to_string(),
        TypeAnnotation::Object => "object".to_string(),
        TypeAnnotation::Any => "any".to_string(),
        TypeAnnotation::Pointer => "pointer".to_string(),
        TypeAnnotation::Array(inner) => render_bracketed("array", inner.as_deref()),
        TypeAnnotation::Slice(inner) => render_bracketed("slice", inner.as_deref()),
        TypeAnnotation::Option(inner) => format!("option[{}]", render_type(inner)),
        TypeAnnotation::Result { ok, err } => {
            format!("result[{}, {}]", render_type(ok), render_type(err))
        }
        TypeAnnotation::View(inner, mutable) => {
            let prefix = if *mutable { "view mut" } else { "view" };
            format!("{prefix} {}", render_type(inner))
        }
        TypeAnnotation::Shared(inner) => format!("shared {}", render_type(inner)),
        TypeAnnotation::Own(inner) => format!("own {}", render_type(inner)),
        TypeAnnotation::Ref(inner, mutable) => {
            let prefix = if *mutable { "&mut" } else { "&" };
            format!("{prefix}{}", render_type(inner))
        }
        TypeAnnotation::RawPointer(inner, mutable) => {
            let prefix = if *mutable { "*mut" } else { "*const" };
            format!("{prefix} {}", render_type(inner))
        }
        TypeAnnotation::Named(token) => token.lexeme().to_string(),
        TypeAnnotation::ImplTrait(token) => format!("impl {}", token.lexeme()),
        TypeAnnotation::Dyn(token) => format!("dyn {}", token.lexeme()),
        TypeAnnotation::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(render_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeAnnotation::Chan(inner) => format!("chan[{}]", render_type(inner)),
    }
}

fn render_bracketed(name: &str, inner: Option<&TypeAnnotation>) -> String {
    match inner {
        Some(inner) => format!("{name}[{}]", render_type(inner)),
        None => name.to_string(),
    }
}

fn slug(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

const STYLE: &str = "
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; background: #0b0d15; color: #e2e8f0;
  font: 15px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; }
header { padding: 28px 32px 18px; border-bottom: 1px solid #3a425e; }
header .brand { color: #c084fc; font-weight: 700; letter-spacing: .18em;
  text-transform: uppercase; font-size: 12px; }
header h1 { margin: 6px 0 0; font-size: 26px; }
header .path { color: #7a86a0; font-size: 13px; }
main { max-width: 980px; margin: 0 auto; padding: 24px 32px 80px; }
section.item { margin: 0 0 22px; padding: 16px 18px; background: #111420;
  border: 1px solid #3a425e; border-radius: 12px; }
section.item.depth-1 { margin: 12px 0 0 18px; background: #0f121d;
  border-color: #2a3147; }
h2.sig, h3.sig { margin: 0 0 8px; font-size: 16px; font-weight: 600; }
h3.sig { font-size: 14px; }
.kind { color: #5eead4; font-weight: 700; margin-right: 8px; }
.depth-1 .kind { color: #c084fc; }
p { margin: 6px 0 0; color: #cbd5e1; }
a { color: #5eead4; text-decoration: none; }
a:hover { text-decoration: underline; }
ul.pages { list-style: none; padding: 0; display: grid; gap: 10px; }
ul.pages li { padding: 14px 16px; background: #111420; border: 1px solid #3a425e;
  border-radius: 10px; }
";

fn render_page(title: &str, items: &[Item]) -> String {
    let mut body = String::new();
    for item in items {
        render_item(&mut body, item, 0);
    }
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} — NTSC</title><style>{STYLE}</style></head><body>\
         <header><div class=\"brand\">NTSC reference</div><h1>{title}</h1>\
         <div class=\"path\"><a href=\"index.html\">all pages</a></div></header>\
         <main>{body}</main></body></html>",
        title = escape(title),
    )
}

fn render_item(out: &mut String, item: &Item, depth: usize) {
    let tag = if depth == 0 { "h2" } else { "h3" };
    let _ = write!(
        out,
        "<section class=\"item depth-{depth}\"><{tag} class=\"sig\">\
         <span class=\"kind\">{}</span>{}</{tag}>",
        escape(item.kind),
        escape(&item.signature)
    );
    for doc in &item.docs {
        let _ = write!(out, "<p>{}</p>", escape(doc));
    }
    for member in &item.members {
        render_item(out, member, depth + 1);
    }
    out.push_str("</section>");
}

fn render_index(pages: &[(String, PathBuf)]) -> String {
    let mut list = String::new();
    for (title, path) in pages {
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = write!(
            list,
            "<li><a href=\"{}\" class=\"kind\">{}</a></li>",
            escape(&file),
            escape(title)
        );
    }
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>NTSC reference</title><style>{STYLE}</style></head><body>\
         <header><div class=\"brand\">NTSC reference</div><h1>Project documentation</h1>\
         <div class=\"path\">generated by ntsc doc</div></header>\
         <main><ul class=\"pages\">{list}</ul></main></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_comments_attach_to_the_following_declaration() {
        let source = "## Adds two numbers.\n## Used by the examples.\nfun add(int a, int b) -> int {\n    return a + b\n}\n";
        let items = extract(source);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "fun");
        assert_eq!(items[0].signature, "add(int a, int b) -> int");
        assert_eq!(
            items[0].docs,
            vec!["Adds two numbers.", "Used by the examples."]
        );
    }

    #[test]
    fn a_blank_line_detaches_the_comment() {
        let source = "## Not attached.\n\nfun lonely() {\n}\n";
        let items = extract(source);
        assert_eq!(items.len(), 1);
        assert!(items[0].docs.is_empty());
    }

    #[test]
    fn classes_list_their_members() {
        let source = "## A shape.\nclass Circle {\n    ## Radius.\n    var float radius\n    ## Area.\n    fun area() -> float {\n        return 3.0\n    }\n}\n";
        let items = extract(source);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].signature, "Circle");
        assert_eq!(items[0].members.len(), 2);
        assert_eq!(items[0].members[0].signature, "radius: float");
        assert_eq!(items[0].members[1].signature, "area() -> float");
        assert_eq!(items[0].members[1].docs, vec!["Area."]);
    }

    #[test]
    fn enum_cases_render_their_payloads() {
        let source = "enum Shape {\n    Circle(float),\n    Empty\n}\n";
        let items = extract(source);
        assert_eq!(items[0].members[0].signature, "Circle(float)");
        assert_eq!(items[0].members[1].signature, "Empty");
    }

    #[test]
    fn html_escapes_signatures() {
        assert_eq!(escape("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert_eq!(slug("My Page!"), "my-page");
    }
}
