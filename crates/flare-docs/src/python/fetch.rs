//! PyPI registry access: resolve a project's JSON manifest, pick its pure-
//! Python wheel, and unpack whatever typed source it carries — `.pyi` stubs
//! or, for inline-typed packages, its own `.py` source.
//!
//! Wheels are fetched whole (like npm tarballs) rather than file-by-file, for
//! the same reason: it's one HTTP request per package instead of one per
//! module, and cross-file imports (`from .context import Foo`) resolve as a
//! local path probe instead of a network fetch per import.

use std::io::Read;

/// Ceiling on a decompressed wheel tree. PyPI's own upload limits are well
/// under this; the cap bounds memory against a hostile or corrupt wheel
/// rather than rejecting legitimate packages.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Largest single stub file worth parsing. Real `.pyi` files run to a few
/// hundred KB; anything past this is a generated/bundled artifact whose
/// extraction cost outweighs its value.
const MAX_PYI_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PythonFetchError {
    #[error("invalid PyPI manifest: {0}")]
    InvalidManifest(String),
    #[error("wheel error: {0}")]
    Wheel(String),
    #[error(
        "package \"{0}\" ships no type stubs, and no types-{0} package was found on PyPI"
    )]
    NoTypes(String),
}

/// The subset of a PyPI project manifest this crate needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub homepage: Option<String>,
    /// URL of a pure-Python wheel (`*-none-any.whl`), when the release
    /// publishes one. `None` means there's nothing safe to unpack for this
    /// release — a platform-specific-only release, or an empty `urls` list —
    /// and the caller should treat that like "no types shipped".
    pub wheel_url: Option<String>,
    /// The release's long description, when PyPI reports it as markdown
    /// (`description_content_type: "text/markdown"`). `None` for RST/plain
    /// releases -- out of scope for the fenced-code-block example extractor.
    pub readme: Option<String>,
}

/// Project manifest endpoint. `version` may be an exact release version or
/// `"latest"`; unlike npm's registry, PyPI's JSON API has no `latest` alias —
/// the version segment is simply omitted to get the current release.
pub fn manifest_url(package: &str, version: &str) -> String {
    if version == "latest" {
        format!("https://pypi.org/pypi/{package}/json")
    } else {
        format!("https://pypi.org/pypi/{package}/{version}/json")
    }
}

/// typeshed's PyPI naming convention for a package that ships no stubs of its
/// own: `requests` -> `types-requests`. Unlike DefinitelyTyped's separate npm
/// scope, these are published to the very same registry this module already
/// fetches from.
pub fn types_package_name(package: &str) -> String {
    format!("types-{package}")
}

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A release's homepage, tried in the couple of shapes PyPI manifests
/// actually use in practice: the older top-level `home_page` field, then
/// `project_urls` under its common key spellings.
fn homepage(info: &serde_json::Value) -> Option<String> {
    json_str(info, "home_page").or_else(|| {
        let urls = info.get("project_urls")?;
        ["Homepage", "homepage", "Home", "Source"]
            .iter()
            .find_map(|k| json_str(urls, k))
    })
}

/// The first pure-Python wheel (`*-none-any.whl`) in a release's `urls`
/// array. That shape is what type-stub packages and most pure-Python
/// packages publish; a release with only platform-specific wheels or a
/// source-only release yields `None` rather than guessing at a binary
/// artifact this crate has no business unpacking.
fn pure_python_wheel_url(urls: &serde_json::Value) -> Option<String> {
    urls.as_array()?.iter().find_map(|entry| {
        let packagetype = entry.get("packagetype")?.as_str()?;
        let filename = entry.get("filename")?.as_str()?;
        if packagetype == "bdist_wheel" && filename.ends_with("-none-any.whl") {
            json_str(entry, "url")
        } else {
            None
        }
    })
}

pub fn parse_manifest(bytes: &[u8]) -> Result<PackageManifest, PythonFetchError> {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| PythonFetchError::InvalidManifest(e.to_string()))?;
    let info = v
        .get("info")
        .ok_or_else(|| PythonFetchError::InvalidManifest("missing \"info\"".into()))?;
    let name = json_str(info, "name")
        .ok_or_else(|| PythonFetchError::InvalidManifest("missing \"info.name\"".into()))?;
    let version = json_str(info, "version")
        .ok_or_else(|| PythonFetchError::InvalidManifest("missing \"info.version\"".into()))?;
    let wheel_url = v.get("urls").and_then(pure_python_wheel_url);
    let readme = if json_str(info, "description_content_type").as_deref() == Some("text/markdown") {
        json_str(info, "description")
    } else {
        None
    };
    Ok(PackageManifest {
        name,
        version,
        summary: json_str(info, "summary").unwrap_or_default(),
        homepage: homepage(info),
        wheel_url,
        readme,
    })
}

/// One `.pyi` stub file recovered from a wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyiFile {
    /// Path within the wheel, e.g. `requests/api.pyi`.
    pub path: String,
    pub source: String,
}

/// True for a top-level PEP 561 marker file (`py.typed`), present when a
/// package is typed via inline annotations rather than separate `.pyi`
/// stubs — e.g. `click/py.typed`.
fn is_py_typed_marker(path: &str) -> bool {
    path.ends_with("/py.typed") || path == "py.typed"
}

/// True for a `.py` file worth treating as typed source when a package ships
/// no `.pyi` stubs of its own. Filters out files that are real parts of a
/// wheel but not part of its public API: tests, vendored dependencies, and
/// build/fixture scripts.
fn is_relevant_py_source(path: &str) -> bool {
    if !path.ends_with(".py") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    !(lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || base.starts_with("test_")
        || lower.contains("/_vendor")
        || lower.contains("/__pycache__/")
        || base == "setup.py"
        || base == "conftest.py")
}

/// Unpacks a wheel (a zip archive) and returns the typed source it carries:
/// `.pyi` stub files when the package ships them, or its own `.py` source
/// when it's PEP 561 "inline typed" instead — a `py.typed` marker with no
/// separate stubs, the shape modern packages like click and pandas actually
/// use. Neither present means the caller should fall back to typeshed.
pub fn extract_pyi(wheel_zip: &[u8]) -> Result<Vec<PyiFile>, PythonFetchError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(wheel_zip))
        .map_err(|e| PythonFetchError::Wheel(e.to_string()))?;

    // A cheap first pass over just the central directory's names decides
    // which shape this wheel takes, before spending any decompression work.
    let mut has_pyi = false;
    let mut has_marker = false;
    for name in archive.file_names() {
        if name.ends_with(".pyi") {
            has_pyi = true;
        }
        if is_py_typed_marker(name) {
            has_marker = true;
        }
    }
    if !has_pyi && !has_marker {
        return Ok(Vec::new());
    }

    let mut total_bytes: u64 = 0;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| PythonFetchError::Wheel(e.to_string()))?;
        let path = entry.name().to_string();
        // `.pyi` stubs are authored specifically as an interface — smaller
        // and cleaner than the runtime source — so they win whenever both
        // exist for a file.
        let wanted = if has_pyi {
            path.ends_with(".pyi")
        } else {
            is_relevant_py_source(&path)
        };
        if !wanted {
            continue;
        }
        let size = entry.size();
        if size as usize > MAX_PYI_BYTES {
            continue;
        }
        total_bytes += size;
        if total_bytes > MAX_UNPACKED_BYTES {
            break;
        }
        let mut buf = String::new();
        // A non-UTF-8 file is malformed; skip it rather than failing the
        // whole package, so one bad entry can't lose the rest of the API.
        if entry.read_to_string(&mut buf).is_err() {
            continue;
        }
        out.push(PyiFile { path, source: buf });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_verified_registry_urls() {
        // Shapes verified live against pypi.org/pypi.
        assert_eq!(
            manifest_url("requests", "2.32.3"),
            "https://pypi.org/pypi/requests/2.32.3/json"
        );
        // PyPI has no "latest" version alias — the segment is dropped instead.
        assert_eq!(
            manifest_url("requests", "latest"),
            "https://pypi.org/pypi/requests/json"
        );
    }

    #[test]
    fn typeshed_naming_matches_pypi_convention() {
        // Verified live: types-requests exists on PyPI for exactly this reason.
        assert_eq!(types_package_name("requests"), "types-requests");
    }

    #[test]
    fn parses_a_release_with_a_pure_python_wheel() {
        let manifest = br#"{
            "info": {
                "name": "requests", "version": "2.32.3",
                "summary": "Python HTTP for Humans.",
                "home_page": "https://requests.readthedocs.io"
            },
            "urls": [
                {"packagetype": "sdist", "filename": "requests-2.32.3.tar.gz", "url": "https://files.pythonhosted.org/.../requests-2.32.3.tar.gz"},
                {"packagetype": "bdist_wheel", "filename": "requests-2.32.3-py3-none-any.whl", "url": "https://files.pythonhosted.org/.../requests-2.32.3-py3-none-any.whl"}
            ]
        }"#;
        let m = parse_manifest(manifest).unwrap();
        assert_eq!(m.name, "requests");
        assert_eq!(m.summary, "Python HTTP for Humans.");
        assert_eq!(
            m.homepage.as_deref(),
            Some("https://requests.readthedocs.io")
        );
        assert_eq!(
            m.wheel_url.as_deref(),
            Some("https://files.pythonhosted.org/.../requests-2.32.3-py3-none-any.whl")
        );
    }

    #[test]
    fn a_release_with_only_platform_wheels_has_no_wheel_url() {
        // e.g. a C-extension package with no universal wheel — treated the
        // same as "nothing to unpack", not an error at this layer.
        let manifest = br#"{
            "info": {"name": "cryptography", "version": "43.0.0", "summary": "crypto"},
            "urls": [
                {"packagetype": "bdist_wheel", "filename": "cryptography-43.0.0-cp39-abi3-manylinux_2_17_x86_64.whl", "url": "https://example/x.whl"},
                {"packagetype": "sdist", "filename": "cryptography-43.0.0.tar.gz", "url": "https://example/x.tar.gz"}
            ]
        }"#;
        let m = parse_manifest(manifest).unwrap();
        assert_eq!(m.wheel_url, None);
    }

    #[test]
    fn falls_back_to_project_urls_homepage_when_home_page_is_absent() {
        let manifest = br#"{
            "info": {
                "name": "p", "version": "1.0.0", "summary": "d",
                "project_urls": {"Homepage": "https://example.com/p"}
            },
            "urls": []
        }"#;
        let m = parse_manifest(manifest).unwrap();
        assert_eq!(m.homepage.as_deref(), Some("https://example.com/p"));
    }

    #[test]
    fn readme_is_kept_only_when_pypi_reports_it_as_markdown() {
        let markdown = br##"{
            "info": {
                "name": "p", "version": "1.0.0", "summary": "d",
                "description": "# p\n\n```python\nimport p\n```\n",
                "description_content_type": "text/markdown"
            },
            "urls": []
        }"##;
        let m = parse_manifest(markdown).unwrap();
        assert!(m.readme.as_deref().unwrap().contains("import p"));

        let rst = br##"{
            "info": {
                "name": "p", "version": "1.0.0", "summary": "d",
                "description": "p\n=\n\n.. code-block:: python\n\n    import p\n",
                "description_content_type": "text/x-rst"
            },
            "urls": []
        }"##;
        let m = parse_manifest(rst).unwrap();
        assert_eq!(
            m.readme, None,
            "RST isn't parsed by the fenced-code-block extractor -- keeping it would silently yield nothing"
        );
    }

    #[test]
    fn manifest_without_info_is_rejected() {
        assert!(parse_manifest(b"{}").is_err());
        assert!(parse_manifest(b"not json").is_err());
    }

    /// Builds a tiny zip in the shape a wheel takes.
    fn fake_wheel(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (path, body) in files {
                writer.start_file(*path, opts).unwrap();
                std::io::Write::write_all(&mut writer, body.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extracts_only_stub_files() {
        let zip = fake_wheel(&[
            ("requests/api.pyi", "def get(url: str) -> Response: ..."),
            ("requests/api.py", "def get(url): ..."),
            ("requests-2.32.3.dist-info/METADATA", "Metadata-Version: 2.1"),
        ]);
        let files = extract_pyi(&zip).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["requests/api.pyi"]);
        assert!(files[0].source.contains("def get"));
    }

    #[test]
    fn a_wheel_with_no_stub_files_yields_an_empty_set_not_an_error() {
        let zip = fake_wheel(&[("requests/api.py", "def get(url): ...")]);
        assert!(extract_pyi(&zip).unwrap().is_empty());
    }

    #[test]
    fn extracts_py_source_for_inline_typed_packages_with_no_pyi_stubs() {
        // The shape click, pandas, and most modern packages actually use:
        // a `py.typed` marker plus annotated `.py` source, no separate
        // `.pyi` stubs at all. Verified live: click 8.4.2 has this exact
        // shape and previously (incorrectly) fell back to typeshed for it.
        let zip = fake_wheel(&[
            ("click/py.typed", ""),
            ("click/core.py", "def command() -> None: ..."),
            ("click/tests/test_core.py", "def test_command(): ..."),
        ]);
        let files = extract_pyi(&zip).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["click/core.py"],
            "the py.typed marker itself and the test file must not be treated as API source"
        );
    }

    #[test]
    fn pyi_stubs_are_preferred_over_py_source_when_both_exist() {
        let zip = fake_wheel(&[
            ("pkg/py.typed", ""),
            ("pkg/mod.py", "def real(): pass"),
            ("pkg/mod.pyi", "def real() -> None: ..."),
        ]);
        let files = extract_pyi(&zip).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["pkg/mod.pyi"]);
    }

    #[test]
    fn a_package_with_neither_stubs_nor_a_py_typed_marker_yields_nothing() {
        // Distinguishes "untyped" from "inline typed" -- a bare .py file with
        // no py.typed marker is not a PEP 561-compliant typed package.
        let zip = fake_wheel(&[("pkg/mod.py", "def f(): pass")]);
        assert!(extract_pyi(&zip).unwrap().is_empty());
    }

    #[test]
    fn corrupt_wheel_is_an_error_not_a_panic() {
        assert!(extract_pyi(b"definitely not a zip").is_err());
    }
}
