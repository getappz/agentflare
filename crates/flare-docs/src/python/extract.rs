//! Extracts a Python package's public API surface from its `.pyi` type
//! stub files.
//!
//! A `.pyi` stub is ordinary Python syntax — a `def`/`class` header plus a
//! body of just `...` — so a syntactic tree-sitter pass over the same grammar
//! used for real `.py` source recovers the API surface directly, no separate
//! "declaration file" grammar needed the way TypeScript's `.d.ts` required.

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Grammar verified against tree-sitter-python's own `queries/tags.scm`:
/// both node types carry a `name: (identifier)` field.
const QUERY_PY: &str = r"
(class_definition name: (identifier) @name) @def
(function_definition name: (identifier) @name) @def
";

/// Longest single signature kept. Stub files occasionally carry pathological
/// one-liners (huge `Literal[...]` unions); storing them whole would dominate
/// the index for no retrieval benefit.
const MAX_SIGNATURE_CHARS: usize = 400;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("tree-sitter language error: {0}")]
    Language(String),
    #[error("tree-sitter query error: {0}")]
    Query(String),
}

/// One documented item from a stub file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiItem {
    /// Dotted path within the module, e.g. `Session.request`.
    pub fq_path: String,
    /// Bare name, used as the stored document title.
    pub name: String,
    /// `class` | `function` | `method`.
    pub kind: String,
    /// The declaration's own header (decorators, `def`/`class` line(s), up
    /// to but excluding the body), trimmed.
    pub signature: String,
    /// The docstring immediately inside the declaration's body, when present.
    /// Empty when undocumented — common in stubs, and not an error; the
    /// signature alone is still worth indexing.
    pub docs: String,
    /// 1-based line of the declaration within its file.
    pub line: usize,
}

impl ApiItem {
    /// Stored document body: signature first, then prose. The signature is
    /// the part that prevents hallucinated calls, so it leads even when a
    /// docstring exists.
    pub fn content(&self) -> String {
        if self.docs.is_empty() {
            self.signature.clone()
        } else {
            format!("{}\n\n{}", self.signature, self.docs)
        }
    }
}

fn language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

/// True for a name that's part of the public API by Python's naming
/// convention: no leading underscore, or a dunder like `__init__`/`__repr__`
/// (name-mangled/truly-private single/double-leading-underscore names are
/// the only ones excluded). Stubs have no `export` keyword the way `.d.ts`
/// does — this convention is the only signal there is.
fn is_public(name: &str) -> bool {
    !name.starts_with('_') || (name.starts_with("__") && name.ends_with("__") && name.len() > 4)
}

/// Walks up to the nearest enclosing class so methods get a qualified path
/// (`Session.request`) rather than colliding on bare names — many classes in
/// one module declare a `close` or `get` member.
fn enclosing_path(node: Node, src: &str) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "class_definition"
            && let Some(name) = n.child_by_field_name("name")
        {
            return Some(src[name.byte_range()].to_string());
        }
        cur = n.parent();
    }
    None
}

/// The docstring inside a `def`/`class` body, when its first statement (skip
/// past any stray leading comments) is a bare string literal — the standard
/// Python docstring position. Returns the string's already-unquoted content
/// via the `string_content` child tree-sitter-python's string node wraps its
/// text in, so no manual quote-stripping is needed.
fn docstring(body: Node, src: &str) -> String {
    let mut cursor = body.walk();
    let Some(first) = body
        .named_children(&mut cursor)
        .find(|n| n.kind() != "comment")
    else {
        return String::new();
    };
    if first.kind() != "expression_statement" {
        return String::new();
    }
    let Some(expr) = first.named_child(0) else {
        return String::new();
    };
    if expr.kind() != "string" {
        return String::new();
    }
    let mut c = expr.walk();
    let Some(content) = expr
        .named_children(&mut c)
        .find(|n| n.kind() == "string_content")
    else {
        // An empty string literal ("" / '') is valid but has no content
        // child at all — that's an empty docstring, not a missing one, and
        // either way there is nothing to index.
        return String::new();
    };
    src[content.byte_range()].trim().to_string()
}

/// Parses one `.pyi` stub file and returns its public API items.
pub fn extract(source: &str) -> Result<Vec<ApiItem>, ExtractError> {
    let lang = language();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| ExtractError::Language(e.to_string()))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(Vec::new());
    };
    let query = Query::new(&lang, QUERY_PY).map_err(|e| ExtractError::Query(e.to_string()))?;
    let def_idx = query
        .capture_index_for_name("def")
        .ok_or_else(|| ExtractError::Query("missing @def capture".into()))?;
    let name_idx = query
        .capture_index_for_name("name")
        .ok_or_else(|| ExtractError::Query("missing @name capture".into()))?;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut items = Vec::new();

    while let Some(m) = matches.next() {
        let def = m
            .captures
            .iter()
            .find(|c| c.index == def_idx)
            .map(|c| c.node);
        let name_node = m
            .captures
            .iter()
            .find(|c| c.index == name_idx)
            .map(|c| c.node);
        let (Some(def), Some(name_node)) = (def, name_node) else {
            continue;
        };
        let name = source[name_node.byte_range()].to_string();
        if !is_public(&name) {
            continue;
        }
        let parent_path = enclosing_path(def, source);
        let fq_path = match &parent_path {
            Some(parent) if parent != &name => format!("{parent}.{name}"),
            _ => name.clone(),
        };
        let kind = match def.kind() {
            "class_definition" => "class",
            _ if parent_path.is_some() => "method",
            _ => "function",
        };

        // Decorators (`@property`, `@overload`, ...) live in a wrapping
        // `decorated_definition` sibling, outside the captured node's own
        // range. Including them is a best-effort widen — if this grammar
        // node name is ever wrong, the fallback is simply "no decorator
        // line", never a panic or lost item.
        let sig_start = def
            .parent()
            .filter(|p| p.kind() == "decorated_definition")
            .map_or(def.start_byte(), |p| p.start_byte());
        let body = def.child_by_field_name("body");
        let sig_end = body.map_or(def.end_byte(), |b| b.start_byte());
        let mut signature = source[sig_start..sig_end]
            .trim()
            .trim_end_matches(':')
            .trim_end()
            .to_string();
        if signature.chars().count() > MAX_SIGNATURE_CHARS {
            signature = signature
                .chars()
                .take(MAX_SIGNATURE_CHARS)
                .collect::<String>()
                + " …";
        }

        items.push(ApiItem {
            fq_path,
            name,
            kind: kind.to_string(),
            signature,
            docs: body.map(|b| docstring(b, source)).unwrap_or_default(),
            line: def.start_position().row + 1,
        });
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_class_and_methods_with_docstrings() {
        let src = r#"
class Session:
    """A persistent HTTP session."""
    def request(self, method: str, url: str) -> Response:
        """Sends a request and returns its response."""
        ...
    def close(self) -> None: ...
"#;
        let items = extract(src).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.fq_path.as_str()).collect();
        assert!(paths.contains(&"Session"), "{paths:?}");
        assert!(paths.contains(&"Session.request"), "{paths:?}");
        assert!(paths.contains(&"Session.close"), "{paths:?}");

        let session = items.iter().find(|i| i.fq_path == "Session").unwrap();
        assert_eq!(session.kind, "class");
        assert_eq!(session.docs, "A persistent HTTP session.");

        let request = items
            .iter()
            .find(|i| i.fq_path == "Session.request")
            .unwrap();
        assert_eq!(request.kind, "method");
        assert_eq!(
            request.signature,
            "def request(self, method: str, url: str) -> Response"
        );
        assert_eq!(request.docs, "Sends a request and returns its response.");
    }

    #[test]
    fn module_level_functions_are_kind_function_not_method() {
        let src = "def get(url: str) -> Response: ...\n";
        let items = extract(src).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "function");
        assert_eq!(items[0].fq_path, "get");
    }

    #[test]
    fn private_names_are_skipped_but_dunders_are_kept() {
        let src = r#"
class C:
    def __init__(self) -> None: ...
    def _internal(self) -> None: ...
    def public(self) -> None: ...
"#;
        let items = extract(src).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.fq_path.as_str()).collect();
        assert!(paths.contains(&"C.__init__"), "{paths:?}");
        assert!(paths.contains(&"C.public"), "{paths:?}");
        assert!(!paths.contains(&"C._internal"), "{paths:?}");
    }

    #[test]
    fn decorators_are_included_in_the_signature() {
        let src = r#"
class C:
    @property
    def name(self) -> str: ...
"#;
        let items = extract(src).unwrap();
        let name = items.iter().find(|i| i.fq_path == "C.name").unwrap();
        assert!(
            name.signature.starts_with("@property"),
            "{:?}",
            name.signature
        );
        assert!(name.signature.contains("def name(self) -> str"));
    }

    #[test]
    fn wrapped_multi_line_signatures_are_captured_whole() {
        let src = "def f(\n    a: int,\n    b: str,\n) -> bool: ...\n";
        let items = extract(src).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].signature.contains("a: int"));
        assert!(items[0].signature.contains("b: str"));
        assert!(items[0].signature.ends_with("-> bool"));
    }

    #[test]
    fn undocumented_items_still_yield_their_signature() {
        let src = "def parse(input: str) -> int: ...\n";
        let items = extract(src).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].docs, "");
        assert_eq!(items[0].content(), items[0].signature);
    }

    #[test]
    fn a_leading_comment_does_not_hide_the_docstring() {
        let src = "def f() -> None:\n    # type: ignore\n    \"\"\"Docs.\"\"\"\n    ...\n";
        let items = extract(src).unwrap();
        assert_eq!(items[0].docs, "Docs.");
    }

    #[test]
    fn malformed_source_does_not_panic() {
        let items = extract("def broken(\n").unwrap();
        assert!(items.len() <= 1, "{items:?}");
        assert!(extract("").unwrap().is_empty());
        assert!(extract("<<< not python >>>").is_ok());
    }
}
