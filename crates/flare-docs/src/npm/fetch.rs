//! npm registry access: resolve a package manifest, fall back to
//! DefinitelyTyped when a package ships no types, and pull the whole package
//! as a single tarball.
//!
//! Fetching the tarball rather than individual files is deliberate. It is one
//! HTTP request per package instead of one per module, and — more importantly
//! — it turns cross-file resolution (`export * from './x'`) into a local path
//! probe. Resolving imports over the network is exactly the problem that makes
//! `deno_doc` unusable as a library without reimplementing Node resolution.

use std::io::Read;

/// Ceiling on a decompressed package tree. npm's own publish limit is well
/// under this; the cap bounds memory against a hostile or corrupt tarball
/// rather than rejecting legitimate packages.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Largest single declaration file worth parsing. Real `.d.ts` files run to a
/// few hundred KB; anything past this is a bundled artifact whose extraction
/// cost outweighs its value.
const MAX_DTS_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum NpmFetchError {
    #[error("invalid npm manifest: {0}")]
    InvalidManifest(String),
    #[error("tarball error: {0}")]
    Tarball(String),
    #[error("package \"{0}\" ships no TypeScript types, and no @types package was found")]
    NoTypes(String),
}

/// The subset of an npm version manifest this crate needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: Option<String>,
    /// True when the package declares `types`/`typings`, or any `exports.*.types`
    /// condition. Packages without it need the DefinitelyTyped fallback.
    pub has_types: bool,
    pub tarball_url: Option<String>,
}

/// Version manifest endpoint. `version` may be an exact semver or a dist-tag
/// such as `latest`; the registry resolves both.
pub fn manifest_url(package: &str, version: &str) -> String {
    format!("https://registry.npmjs.org/{package}/{version}")
}

/// DefinitelyTyped name for a package that ships no types of its own. Scoped
/// packages flatten the scope with a double underscore, which is the
/// convention DefinitelyTyped itself uses: `@babel/core` -> `@types/babel__core`.
pub fn types_package_name(package: &str) -> String {
    match package.strip_prefix('@') {
        Some(rest) => match rest.split_once('/') {
            Some((scope, name)) => format!("@types/{scope}__{name}"),
            None => format!("@types/{rest}"),
        },
        None => format!("@types/{package}"),
    }
}

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|s| s.as_str()).map(str::to_string)
}

/// Recursively looks for a `types` (or `typings`) condition anywhere in an
/// `exports` map. Modern packages declare types per entry point rather than at
/// the manifest root, so a root-only check reports false negatives.
fn exports_declare_types(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(map) => map.iter().any(|(k, val)| {
            (k == "types" || k == "typings") && val.is_string() || exports_declare_types(val)
        }),
        serde_json::Value::Array(items) => items.iter().any(exports_declare_types),
        _ => false,
    }
}

pub fn parse_manifest(bytes: &[u8]) -> Result<PackageManifest, NpmFetchError> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| NpmFetchError::InvalidManifest(e.to_string()))?;
    let name = json_str(&v, "name")
        .ok_or_else(|| NpmFetchError::InvalidManifest("missing \"name\"".into()))?;
    let version = json_str(&v, "version")
        .ok_or_else(|| NpmFetchError::InvalidManifest("missing \"version\"".into()))?;
    let has_types = v.get("types").and_then(|t| t.as_str()).is_some()
        || v.get("typings").and_then(|t| t.as_str()).is_some()
        || v.get("exports").is_some_and(exports_declare_types);
    Ok(PackageManifest {
        name,
        version,
        description: json_str(&v, "description").unwrap_or_default(),
        homepage: json_str(&v, "homepage"),
        has_types,
        tarball_url: v
            .get("dist")
            .and_then(|d| d.get("tarball"))
            .and_then(|t| t.as_str())
            .map(str::to_string),
    })
}

/// Canonical tarball URL, used when a manifest omits `dist.tarball`.
pub fn tarball_url(package: &str, version: &str) -> String {
    // Scoped packages keep the scope in the path but drop it from the filename:
    // @types/express -> /@types/express/-/express-4.17.21.tgz
    let bare = package.rsplit('/').next().unwrap_or(package);
    format!("https://registry.npmjs.org/{package}/-/{bare}-{version}.tgz")
}

/// One declaration file recovered from a package tarball.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtsFile {
    /// Path within the package, leading `package/` stripped.
    pub path: String,
    pub source: String,
}

/// Unpacks a gzipped npm tarball and returns just its `.d.ts` entries.
///
/// Everything else (JS, maps, assets) is skipped without being read into
/// memory — a package's runtime code is typically an order of magnitude larger
/// than its declarations and contributes nothing to the API surface.
pub fn extract_dts(tarball_gz: &[u8]) -> Result<Vec<DtsFile>, NpmFetchError> {
    let decoder = flate2::read::GzDecoder::new(tarball_gz);
    let mut archive = tar::Archive::new(decoder.take(MAX_UNPACKED_BYTES));
    let mut out = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| NpmFetchError::Tarball(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| NpmFetchError::Tarball(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| NpmFetchError::Tarball(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !path.ends_with(".d.ts") && !path.ends_with(".d.mts") && !path.ends_with(".d.cts") {
            continue;
        }
        let size = entry.header().size().unwrap_or(0);
        if size as usize > MAX_DTS_BYTES {
            continue;
        }
        let mut buf = String::new();
        // A non-UTF-8 declaration file is malformed; skip it rather than
        // failing the package, so one bad entry can't lose the whole API.
        if entry.read_to_string(&mut buf).is_err() {
            continue;
        }
        // npm tarballs root every entry under `package/`.
        let rel = path.strip_prefix("package/").unwrap_or(&path).to_string();
        out.push(DtsFile {
            path: rel,
            source: buf,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// One markdown file recovered from a package tarball.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFile {
    /// Path within the package, e.g. `README.md` or `docs/quickstart.md`.
    pub path: String,
    pub source: String,
}

/// Markdown files that are real files in a package but not usage
/// documentation — release notes, process docs, licensing, or bundled test
/// fixtures.
fn is_excluded_markdown(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.starts_with("node_modules/")
        || lower.starts_with(".github/")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/node_modules/")
        || lower.contains("/.github/")
        || base.starts_with("changelog")
        || base.starts_with("contributing")
        || base.starts_with("code_of_conduct")
        || base.starts_with("license")
        || base.starts_with("security")
}

/// Unpacks a gzipped npm tarball and returns every markdown file worth
/// scanning for usage examples — the README and any bundled guides, but not
/// changelogs, licenses, or process docs.
pub fn extract_markdown(tarball_gz: &[u8]) -> Result<Vec<MarkdownFile>, NpmFetchError> {
    let decoder = flate2::read::GzDecoder::new(tarball_gz);
    let mut archive = tar::Archive::new(decoder.take(MAX_UNPACKED_BYTES));
    let mut out = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| NpmFetchError::Tarball(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| NpmFetchError::Tarball(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| NpmFetchError::Tarball(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !path.ends_with(".md") && !path.ends_with(".markdown") {
            continue;
        }
        let rel = path.strip_prefix("package/").unwrap_or(&path).to_string();
        if is_excluded_markdown(&rel) {
            continue;
        }
        let size = entry.header().size().unwrap_or(0);
        if size as usize > MAX_DTS_BYTES {
            continue;
        }
        let mut buf = String::new();
        if entry.read_to_string(&mut buf).is_err() {
            continue;
        }
        out.push(MarkdownFile { path: rel, source: buf });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_verified_registry_urls() {
        // Shapes verified live against registry.npmjs.org.
        assert_eq!(
            manifest_url("express", "4.18.2"),
            "https://registry.npmjs.org/express/4.18.2"
        );
        assert_eq!(
            manifest_url("@types/node", "latest"),
            "https://registry.npmjs.org/@types/node/latest"
        );
        assert_eq!(
            tarball_url("hono", "4.6.3"),
            "https://registry.npmjs.org/hono/-/hono-4.6.3.tgz"
        );
        assert_eq!(
            tarball_url("@types/express", "4.17.21"),
            "https://registry.npmjs.org/@types/express/-/express-4.17.21.tgz"
        );
    }

    #[test]
    fn maps_scoped_names_onto_definitelytyped_convention() {
        assert_eq!(types_package_name("express"), "@types/express");
        assert_eq!(types_package_name("@babel/core"), "@types/babel__core");
        assert_eq!(types_package_name("@types/node"), "@types/types__node");
    }

    #[test]
    fn detects_types_declared_only_under_exports_conditions() {
        // zod 3.23.8 declares types per entry point, not at the manifest root.
        // A root-only check would wrongly send it to DefinitelyTyped.
        let manifest = br#"{
            "name": "zod", "version": "3.23.8",
            "exports": { ".": { "types": "./index.d.cts" }, "./v4": { "types": "./v4/index.d.cts" } }
        }"#;
        let m = parse_manifest(manifest).unwrap();
        assert!(m.has_types, "exports.*.types must count as shipping types");
    }

    #[test]
    fn detects_missing_types_so_the_types_fallback_can_fire() {
        // express 4.18.2 genuinely ships no types — verified live.
        let manifest =
            br#"{"name":"express","version":"4.18.2","description":"Fast web framework"}"#;
        let m = parse_manifest(manifest).unwrap();
        assert!(!m.has_types);
        assert_eq!(m.description, "Fast web framework");
        assert_eq!(types_package_name(&m.name), "@types/express");
    }

    #[test]
    fn reads_root_types_field_and_dist_tarball() {
        let manifest = br#"{
            "name": "hono", "version": "4.6.3", "types": "./dist/types/index.d.ts",
            "dist": { "tarball": "https://registry.npmjs.org/hono/-/hono-4.6.3.tgz" }
        }"#;
        let m = parse_manifest(manifest).unwrap();
        assert!(m.has_types);
        assert_eq!(
            m.tarball_url.as_deref(),
            Some("https://registry.npmjs.org/hono/-/hono-4.6.3.tgz")
        );
    }

    #[test]
    fn manifest_without_name_or_version_is_rejected() {
        assert!(parse_manifest(b"{\"name\":\"x\"}").is_err());
        assert!(parse_manifest(b"not json").is_err());
    }

    /// Builds a tiny gzipped tar in the shape npm publishes.
    fn fake_tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("package/{path}"), body.as_bytes())
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_only_declaration_files_and_strips_the_package_prefix() {
        let tgz = fake_tarball(&[
            ("dist/index.d.ts", "export declare const a: number;"),
            ("dist/index.js", "module.exports = {}"),
            ("dist/index.d.mts", "export declare const b: string;"),
            ("README.md", "# hi"),
        ]);
        let files = extract_dts(&tgz).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["dist/index.d.mts", "dist/index.d.ts"]);
        assert!(files[1].source.contains("declare const a"));
    }

    #[test]
    fn a_package_with_no_declaration_files_yields_an_empty_set_not_an_error() {
        let tgz = fake_tarball(&[("index.js", "module.exports = {}")]);
        assert!(extract_dts(&tgz).unwrap().is_empty());
    }

    #[test]
    fn corrupt_tarball_is_an_error_not_a_panic() {
        assert!(extract_dts(b"definitely not a gzip stream").is_err());
    }

    #[test]
    fn extracts_markdown_and_strips_the_package_prefix() {
        let tgz = fake_tarball(&[
            ("README.md", "# hono\n\n## Usage\n\n```js\nconst app = new Hono()\n```\n"),
            ("docs/quickstart.md", "## Quickstart\n\n```js\napp.get('/', c => c.text('hi'))\n```\n"),
            ("CHANGELOG.md", "## 4.6.3\n\n```js\nthis example must not be indexed\n```\n"),
            ("index.js", "module.exports = {}"),
        ]);
        let files = extract_markdown(&tgz).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["README.md", "docs/quickstart.md"], "{paths:?}");
    }

    #[test]
    fn excludes_changelog_contributing_and_license_files() {
        let tgz = fake_tarball(&[
            ("CHANGELOG.md", "# Changelog\n\n```js\nx\n```\n"),
            ("CONTRIBUTING.md", "# Contributing\n\n```js\nx\n```\n"),
            ("LICENSE.md", "MIT License\n\n```js\nx\n```\n"),
            ("test/fixtures.md", "```js\nx\n```\n"),
        ]);
        assert!(extract_markdown(&tgz).unwrap().is_empty());
    }

    #[test]
    fn a_tarball_with_no_markdown_yields_an_empty_set_not_an_error() {
        let tgz = fake_tarball(&[("index.js", "module.exports = {}")]);
        assert!(extract_markdown(&tgz).unwrap().is_empty());
    }
}
