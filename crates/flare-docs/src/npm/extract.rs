//! Extracts an npm package's public API surface from its TypeScript
//! declaration files.
//!
//! A `.d.ts` is already fully explicit — every type is written out, nothing is
//! inferred — so a syntactic tree-sitter pass recovers the same API surface a
//! full type-checker would, at a fraction of the dependency cost.
//!
//! The node types below are the reason this module exists rather than reusing
//! an off-the-shelf signature extractor: declaration files spell members as
//! `method_signature` / `property_signature` / `public_field_definition`,
//! whereas implementation files use `method_definition`. A query written for
//! `.ts` matches nothing inside a `.d.ts` class body, which silently collapses
//! every class to one opaque node.

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Covers both declaration files (`method_signature`, `property_signature`,
/// `public_field_definition`) and implementation sources (`method_definition`,
/// `function_declaration`), so the same query serves npm `.d.ts` and JSR `.ts`.
const QUERY_TS: &str = r"
(class_declaration name: (type_identifier) @name) @def
(abstract_class_declaration name: (type_identifier) @name) @def
(interface_declaration name: (type_identifier) @name) @def
(type_alias_declaration name: (type_identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(function_declaration name: (identifier) @name) @def
(function_signature name: (identifier) @name) @def
(method_signature name: (property_identifier) @name) @def
(abstract_method_signature name: (property_identifier) @name) @def
(property_signature name: (property_identifier) @name) @def
(public_field_definition name: (property_identifier) @name) @def
(method_definition name: (property_identifier) @name) @def
(internal_module name: (identifier) @name) @def
(variable_declarator name: (identifier) @name) @def
";

/// Longest single line kept for an item's rendered signature. Declaration
/// files contain occasional pathological one-liners (giant string-literal
/// unions such as `ResponseHeader`); storing them whole would dominate the
/// index for no retrieval benefit.
const MAX_SIGNATURE_CHARS: usize = 400;

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("tree-sitter language error: {0}")]
    Language(String),
    #[error("tree-sitter query error: {0}")]
    Query(String),
    #[error("source is not valid utf-8: {0}")]
    Utf8(String),
}

/// One documented item from a declaration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiItem {
    /// Dotted path within the module, e.g. `Context.json`.
    pub fq_path: String,
    /// Bare name, used as the stored document title.
    pub name: String,
    /// `class` | `interface` | `method` | `property` | ...
    pub kind: String,
    /// The declaration's own source text, first line, trimmed.
    pub signature: String,
    /// Doc comment immediately preceding the declaration, `/** */` markers
    /// stripped. Empty when the declaration is undocumented — which is common
    /// and not an error; the signature alone is still worth indexing.
    pub docs: String,
    /// 1-based line of the declaration within its file.
    pub line: usize,
}

impl ApiItem {
    /// Stored document body: signature first, then prose. The signature is the
    /// part that prevents hallucinated calls, so it leads even when a
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
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

/// Maps a tree-sitter node kind onto the short kind stored on the document.
fn item_kind(node_kind: &str) -> &'static str {
    match node_kind {
        "class_declaration" | "abstract_class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type",
        "enum_declaration" => "enum",
        "function_declaration" | "function_signature" => "function",
        "method_signature" | "abstract_method_signature" | "method_definition" => "method",
        "property_signature" | "public_field_definition" => "property",
        _ => "item",
    }
}

/// A declaration's doc comment is its immediately preceding sibling, when that
/// sibling is a `/** */` block. Anything else (a line comment, another
/// declaration) means the item is undocumented.
fn leading_docs(node: Node, src: &str) -> String {
    // A doc comment sits before the whole statement, but the captured node is
    // often nested inside it — `export declare class Foo` wraps the class in an
    // export_statement, and `var json: T` wraps the declarator in a
    // variable_statement. Climb through those wrappers so the comment is found
    // relative to the statement the author actually annotated.
    let mut anchor = node;
    while let Some(parent) = anchor.parent() {
        if !matches!(
            parent.kind(),
            "export_statement"
                | "ambient_declaration"
                | "variable_statement"
                | "variable_declaration"
                | "lexical_declaration"
        ) {
            break;
        }
        anchor = parent;
    }
    // Named-sibling lookup skips punctuation tokens; a preceding declaration
    // (rather than a comment) correctly yields "undocumented".
    let Some(prev) = anchor.prev_named_sibling() else {
        return String::new();
    };
    if prev.kind() != "comment" {
        return String::new();
    }
    let raw = &src[prev.byte_range()];
    if !raw.starts_with("/**") {
        return String::new();
    }
    let mut lines: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let cleaned = line
            .trim_start()
            .trim_start_matches("/**")
            .trim_start_matches("*/")
            .trim_start_matches('*')
            .trim_end_matches("*/");
        let cleaned = cleaned.strip_prefix(' ').unwrap_or(cleaned);
        lines.push(cleaned);
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim_end().to_string()
}

/// Walks up to the nearest enclosing named declaration so members get a
/// qualified path (`Context.json`) rather than colliding on bare names — many
/// classes in one package declare a `json` or `get` member.
fn enclosing_path(node: Node, src: &str) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        let is_container = matches!(
            n.kind(),
            "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                // Namespaces qualify their members too, so `e.json` and
                // `other.json` stay distinct documents.
                | "internal_module"
                | "module"
        );
        if is_container && let Some(name) = n.child_by_field_name("name") {
            return Some(src[name.byte_range()].to_string());
        }
        cur = n.parent();
    }
    None
}

/// True when a declaration forms part of the package's public surface.
///
/// Two spellings both count, because real declaration files use both:
///
/// * ESM — an `export` keyword somewhere up the chain (`export declare class`).
/// * CommonJS/ambient — a bare `declare` plus a trailing `export = x`, which is
///   what almost every DefinitelyTyped package uses. There is no `export`
///   keyword on those declarations at all, so requiring one drops the entire
///   API of exactly the packages the `@types` fallback exists to serve.
///
/// Members of a namespace inherit their namespace's reachability, and anything
/// declared inside a function body is local regardless of spelling.
fn is_exported(node: Node) -> bool {
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "export_statement" | "ambient_declaration" => return true,
            // `statement_block` is the body of BOTH a function and a
            // namespace. A namespace body is a declaration scope whose members
            // are as reachable as the namespace itself (keep walking up); a
            // function body is an execution scope whose contents are locals.
            "statement_block" => {
                let in_namespace = n
                    .parent()
                    .is_some_and(|p| matches!(p.kind(), "internal_module" | "module"));
                if !in_namespace {
                    return false;
                }
            }
            "arrow_function" | "function_expression" => return false,
            _ => {}
        }
        cur = n.parent();
    }
    false
}

/// Parses one declaration file and returns its exported API items.
pub fn extract(source: &str) -> Result<Vec<ApiItem>, ExtractError> {
    let lang = language();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| ExtractError::Language(e.to_string()))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(Vec::new());
    };
    let query = Query::new(&lang, QUERY_TS).map_err(|e| ExtractError::Query(e.to_string()))?;
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
        if !is_exported(def) {
            continue;
        }
        let name = source[name_node.byte_range()].to_string();
        let fq_path = match enclosing_path(def, source) {
            Some(parent) if parent != name => format!("{parent}.{name}"),
            _ => name.clone(),
        };
        let mut signature = source[def.byte_range()]
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .trim_end_matches(['{', ';'])
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
            kind: item_kind(def.kind()).to_string(),
            signature,
            docs: leading_docs(def, source),
            line: def.start_position().row + 1,
        });
    }
    Ok(items)
}

/// Relative module specifiers this file re-exports or imports from.
///
/// Resolution stays inside the extracted tarball, so this is a local file
/// probe rather than a network fetch per import — the reason the whole package
/// is downloaded as one tarball instead of file-by-file.
pub fn relative_imports(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    // A named import whose specifier list wraps puts `from` on a different
    // line than the `import`/`export` keyword, so scanning line by line drops
    // it. Accumulate wrapped lines into one logical statement instead, keeping
    // the keyword anchor -- matching a bare `from` anywhere would also catch
    // prose in doc comments.
    let mut pending: Option<String> = None;
    for line in source.lines() {
        let line = line.trim();
        let stmt = match pending.take() {
            Some(mut acc) => {
                acc.push(' ');
                acc.push_str(line);
                acc
            }
            None if line.starts_with("export") || line.starts_with("import") => line.to_string(),
            None => continue,
        };
        let Some(from_idx) = stmt.find("from ") else {
            // No specifier yet: either the statement already ended (a
            // side-effect `import './x';`) or its list is still open across
            // lines. Only the latter is worth carrying forward, and the length
            // bound keeps a file with no terminators from accumulating without
            // limit.
            if !stmt.contains(';') && stmt.len() < 8192 {
                pending = Some(stmt);
            }
            continue;
        };
        let rest = &stmt[from_idx + 5..];
        let quote = match rest.chars().next() {
            Some(c @ ('\'' | '"')) => c,
            _ => continue,
        };
        let rest = &rest[1..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        let spec = &rest[..end];
        if spec.starts_with('.') && !out.iter().any(|s| s == spec) {
            out.push(spec.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_class_members_from_a_declaration_file() {
        // The regression this whole module exists for: in a .d.ts, members are
        // `method_signature`/`public_field_definition`, NOT `method_definition`.
        // A query written for implementation files yields the class and nothing
        // inside it.
        let src = r#"
/**
 * The request context.
 */
export declare class Context {
    /**
     * `.json()` renders JSON as `Content-Type:application/json`.
     */
    json(object: unknown, status?: number): Response;
    /** Bindings for the environment. */
    env: string;
}
"#;
        let items = extract(src).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.fq_path.as_str()).collect();
        assert!(paths.contains(&"Context"), "{paths:?}");
        assert!(paths.contains(&"Context.json"), "{paths:?}");
        assert!(paths.contains(&"Context.env"), "{paths:?}");

        let json = items.iter().find(|i| i.fq_path == "Context.json").unwrap();
        assert_eq!(json.kind, "method");
        assert_eq!(
            json.signature,
            "json(object: unknown, status?: number): Response"
        );
        assert!(json.docs.contains("renders JSON"), "{:?}", json.docs);
    }

    #[test]
    fn interface_members_are_qualified_and_documented() {
        let src = r#"
export interface ExecutionContext {
    /**
     * Extends the lifetime of the event callback.
     *
     * @param promise - A promise to wait for.
     */
    waitUntil(promise: Promise<unknown>): void;
}
"#;
        let items = extract(src).unwrap();
        let wait = items
            .iter()
            .find(|i| i.fq_path == "ExecutionContext.waitUntil")
            .expect("member should be qualified by its interface");
        assert_eq!(wait.kind, "method");
        assert!(wait.docs.contains("@param promise"), "{:?}", wait.docs);
        assert_eq!(wait.signature, "waitUntil(promise: Promise<unknown>): void");
    }

    #[test]
    fn ambient_commonjs_declarations_are_public_api() {
        // The shape almost every DefinitelyTyped package uses: no `export`
        // keyword anywhere, just ambient `declare` plus a trailing
        // `export = e`. Requiring an export_statement ancestor drops the
        // entire API — and this is exactly the path untyped packages like
        // express and react take.
        let src = r#"
/**
 * Creates an Express application.
 */
declare function e(): core.Express;

declare namespace e {
    /**
     * Parses incoming requests with JSON payloads.
     * @since 4.16.0
     */
    var json: typeof bodyParser.json;

    interface Router {
        use(handler: RequestHandler): Router;
    }
}

export = e;
"#;
        let items = extract(src).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.fq_path.as_str()).collect();
        assert!(paths.contains(&"e"), "ambient function missing: {paths:?}");
        assert!(
            paths.iter().any(|p| p.ends_with("json")),
            "namespace member missing: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("Router")),
            "namespace interface missing: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("Router.use")),
            "nested interface member missing: {paths:?}"
        );

        let json = items.iter().find(|i| i.fq_path.ends_with("json")).unwrap();
        assert!(json.docs.contains("JSON payloads"), "{:?}", json.docs);
    }

    #[test]
    fn module_private_declarations_are_skipped() {
        // Un-exported helpers are real declarations but not callable API;
        // indexing them would surface methods consumers cannot reach.
        let src = r#"
interface Hidden {
    secret(): void;
}
export interface Visible {
    shown(): void;
}
"#;
        let items = extract(src).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.fq_path.as_str()).collect();
        assert!(paths.contains(&"Visible"), "{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("Hidden")), "{paths:?}");
    }

    #[test]
    fn undocumented_items_still_yield_their_signature() {
        // Most real packages ship few docstrings (zod: ~0%), so a missing
        // comment must not drop the item — the signature is the payload.
        let src = "export declare function parse(input: string): number;\n";
        let items = extract(src).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].docs, "");
        // The `export declare` prefix belongs to the enclosing export_statement,
        // not the declaration node — so the stored signature is the useful part
        // without the module-plumbing noise.
        assert_eq!(items[0].signature, "function parse(input: string): number");
        assert_eq!(items[0].content(), items[0].signature);
    }

    #[test]
    fn pathological_one_line_unions_are_truncated() {
        let long = (0..300)
            .map(|i| format!("'header-{i}'"))
            .collect::<Vec<_>>()
            .join(" | ");
        let src = format!("export type ResponseHeader = {long};\n");
        let items = extract(&src).unwrap();
        assert_eq!(items.len(), 1);
        assert!(
            items[0].signature.chars().count() <= MAX_SIGNATURE_CHARS + 2,
            "signature was {} chars",
            items[0].signature.chars().count()
        );
    }

    #[test]
    fn collects_relative_reexports_for_local_resolution() {
        let src = r#"
export * from './context';
export { Hono } from "./hono";
import type { Env } from './types';
import { external } from 'some-package';
"#;
        let imports = relative_imports(src);
        assert_eq!(imports, vec!["./context", "./hono", "./types"]);
    }

    #[test]
    fn collects_relative_imports_whose_specifier_list_wraps() {
        // Formatters wrap long named imports, which puts `from` on its own
        // line -- the common shape in real .d.ts bundles.
        let src = r#"
import {
  Context,
  Handler,
} from './context';
export {
  Hono,
} from "./hono";
import './side-effect';
import { after } from './after';
"#;
        assert_eq!(
            relative_imports(src),
            vec!["./context", "./hono", "./after"],
            "a wrapped specifier list must not hide its module, and a \
             side-effect import must not swallow the statement after it"
        );
    }

    #[test]
    fn malformed_source_does_not_panic() {
        // tree-sitter is error-tolerant; a truncated file must degrade to
        // "fewer items", never a crash, or one bad file fails a whole package.
        // Recovery is best-effort — yielding nothing is acceptable, panicking
        // or erroring is not.
        let items = extract("export declare class Broken {\n  json(").unwrap();
        assert!(items.len() <= 2, "{items:?}");
        assert!(extract("").unwrap().is_empty());
        assert!(extract("<<< not typescript >>>").is_ok());
    }
}
