use clap::{Args, Subcommand};

#[derive(Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub cmd: DocsCmd,
}

#[derive(Subcommand)]
pub enum DocsCmd {
    /// Search cached third-party documentation.
    Search {
        query: String,
        /// Max results to return; capped at 50.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Fetch (or read from cache) docs for a package, printing the result.
    Get {
        package: String,
        #[arg(long, default_value = "latest")]
        version: String,
        /// Registry to look the package up in: rust (docs.rs), npm, or python (PyPI).
        /// Defaults to rust; scoped names (@scope/pkg) imply npm.
        #[arg(long, short = 'e')]
        ecosystem: Option<String>,
    },
    /// List every cached document.
    List,
    /// Force a fresh fetch for a package, bypassing the cache.
    Refresh {
        package: String,
        #[arg(long, default_value = "latest")]
        version: String,
        /// Registry to look the package up in: rust (docs.rs), npm, or python (PyPI).
        #[arg(long, short = 'e')]
        ecosystem: Option<String>,
    },
}

pub fn run(args: DocsArgs) {
    let store = match flare_docs::DocsStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("flare-docs: failed to open store: {e}");
            std::process::exit(1);
        }
    };

    match args.cmd {
        DocsCmd::Search { query, limit } => match store.search(&query, limit) {
            Ok(hits) => println!("{}", serde_json::to_string_pretty(&hits).unwrap()),
            Err(e) => {
                eprintln!("flare-docs: search failed: {e}");
                std::process::exit(1);
            }
        },
        DocsCmd::List => match store.list() {
            Ok(docs) => println!("{}", serde_json::to_string_pretty(&docs).unwrap()),
            Err(e) => {
                eprintln!("flare-docs: list failed: {e}");
                std::process::exit(1);
            }
        },
        DocsCmd::Get {
            package,
            version,
            ecosystem,
        } => {
            let eco = resolve_ecosystem(ecosystem.as_deref(), &package);
            let cached = match store.get_by_path(&eco.docs_id_path(&package, &version)) {
                Ok(cached) => cached,
                Err(e) => {
                    eprintln!("flare-docs: cache lookup failed: {e}");
                    std::process::exit(1);
                }
            };
            match cached {
                Some(doc) => println!("{}", serde_json::to_string_pretty(&doc).unwrap()),
                // A cache-miss "get" is still just a document lookup from the
                // caller's point of view -- print only the doc, matching the
                // cache-hit shape above, rather than inventing fetch-outcome
                // telemetry a plain "get" never asked for.
                None => fetch_and_print(&store, eco, &package, &version, false),
            }
        }
        DocsCmd::Refresh {
            package,
            version,
            ecosystem,
        } => {
            let eco = resolve_ecosystem(ecosystem.as_deref(), &package);
            fetch_and_print(&store, eco, &package, &version, true)
        }
    }
}

fn resolve_ecosystem(explicit: Option<&str>, package: &str) -> flare_docs::Ecosystem {
    match flare_docs::Ecosystem::resolve(explicit, package) {
        Ok(eco) => eco,
        Err(e) => {
            eprintln!("flare-docs: {e}");
            std::process::exit(2);
        }
    }
}

fn fetch_and_print(
    store: &flare_docs::DocsStore,
    eco: flare_docs::Ecosystem,
    package: &str,
    version: &str,
    verbose: bool,
) {
    let fetcher = flare_docs::UreqFetcher::new();
    let fetched = match eco {
        flare_docs::Ecosystem::Rust => {
            flare_docs::fetch_and_store(&fetcher, store, package, version)
                .map_err(|e| e.to_string())
        }
        flare_docs::Ecosystem::Npm => {
            flare_docs::npm::fetch_and_store(&fetcher, store, package, version)
                .map_err(|e| e.to_string())
        }
        flare_docs::Ecosystem::Python => {
            flare_docs::python::fetch_and_store(&fetcher, store, package, version)
                .map_err(|e| e.to_string())
        }
    };
    match fetched {
        Ok(outcome) => {
            if verbose {
                println!("{}", serde_json::to_string_pretty(&outcome).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&outcome.doc).unwrap());
            }
            if let Some(err) = &outcome.items_error {
                eprintln!("flare-docs: per-item indexing failed: {err}");
            }
        }
        Err(e) => {
            eprintln!(
                "flare-docs: fetch failed: {e} — {}",
                eco.other_ecosystem_hint(package)
            );
            std::process::exit(1);
        }
    }
}
