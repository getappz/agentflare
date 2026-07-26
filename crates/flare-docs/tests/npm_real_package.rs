//! End-to-end check against real, unmodified npm package tarballs.
//!
//! Unit fixtures are hand-written and can quietly encode the same wrong
//! assumption as the code under test. These run the extractor over declaration
//! files as actually published to npm, so a grammar or packaging change that
//! breaks real packages fails here even when the synthetic fixtures still pass.
//!
//! Fixtures live in `.refs/npm/<pkg>` (gitignored, populated by unpacking the
//! published tarball). The tests skip when absent so a fresh clone still passes
//! `cargo test` without network access.

use std::path::{Path, PathBuf};

fn refs_dir(package: &str) -> Option<PathBuf> {
    // crates/flare-docs -> repo root
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    let dir = root.join(".refs").join("npm").join(package);
    dir.is_dir().then_some(dir)
}

fn read_dts(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            read_dts(&path, out);
        } else if path.to_string_lossy().ends_with(".d.ts")
            && let Ok(src) = std::fs::read_to_string(&path)
        {
            out.push(src);
        }
    }
}

fn all_items(package: &str) -> Option<Vec<flare_docs::npm::ApiItem>> {
    let dir = refs_dir(package)?;
    let mut sources = Vec::new();
    read_dts(&dir, &mut sources);
    let mut items = Vec::new();
    for src in &sources {
        items.extend(flare_docs::npm::extract(src).expect("real .d.ts must parse"));
    }
    Some(items)
}

#[test]
fn hono_class_members_are_extracted_with_signatures_and_docs() {
    let Some(items) = all_items("hono") else {
        eprintln!("skipping: .refs/npm/hono not present");
        return;
    };

    // `Context.json` is the canonical case: a documented method inside an
    // exported class in a declaration file. An implementation-file query
    // (`method_definition`) finds the class but never this member.
    let json = items
        .iter()
        .find(|i| i.fq_path == "Context.json")
        .unwrap_or_else(|| panic!("Context.json missing; got {} items", items.len()));
    // hono declares this as `json: JSONRespond` — a property whose type is a
    // callable interface — rather than a method. Both spellings are ordinary
    // in real declaration files, so assert on what callers actually need
    // (a signature and its prose) instead of pinning the node kind.
    assert!(
        json.kind == "property" || json.kind == "method",
        "unexpected kind {:?}",
        json.kind
    );
    assert!(
        json.signature.contains("json"),
        "signature not captured: {:?}",
        json.signature
    );
    assert!(
        json.docs.contains("JSON"),
        "docstring not attached: {:?}",
        json.docs
    );

    // Members must outnumber containers by a wide margin — that ratio is what
    // distinguishes real member extraction from top-level-only parsing.
    let members = items
        .iter()
        .filter(|i| i.kind == "method" || i.kind == "property")
        .count();
    assert!(
        members > 100,
        "only {members} members from hono's declarations"
    );
}

#[test]
fn zod_yields_signatures_even_though_it_ships_almost_no_docstrings() {
    let Some(items) = all_items("zod") else {
        eprintln!("skipping: .refs/npm/zod not present");
        return;
    };
    // zod ships ~0% TSDoc; the signature is the entire value here, so an
    // extractor that only kept documented items would return nothing useful.
    let documented = items.iter().filter(|i| !i.docs.is_empty()).count();
    assert!(
        items.len() > 200,
        "expected a large API surface, got {}",
        items.len()
    );
    assert!(
        items.iter().all(|i| !i.signature.is_empty()),
        "every item must carry a signature even when undocumented"
    );
    eprintln!(
        "zod: {} items, {documented} documented ({}%)",
        items.len(),
        documented * 100 / items.len().max(1)
    );

    // The concrete regression: `.email()` on the string schema must be
    // recoverable, since that is exactly the kind of call a model invents.
    assert!(
        items.iter().any(|i| i.name == "email"),
        "ZodString.email not found in zod's declarations"
    );
}
