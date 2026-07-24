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
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Fetch (or read from cache) docs for a package, printing the result.
    Get {
        package: String,
        #[arg(long, default_value = "latest")]
        version: String,
    },
    /// List every cached document.
    List,
    /// Force a fresh fetch for a package, bypassing the cache.
    Refresh {
        package: String,
        #[arg(long, default_value = "latest")]
        version: String,
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
        DocsCmd::Get { package, version } => {
            let cached = match store.get_by_path(&flare_docs::docs_id_path(&package, &version)) {
                Ok(cached) => cached,
                Err(e) => {
                    eprintln!("flare-docs: cache lookup failed: {e}");
                    std::process::exit(1);
                }
            };
            match cached {
                Some(doc) => println!("{}", serde_json::to_string_pretty(&doc).unwrap()),
                None => fetch_and_print(&store, &package, &version),
            }
        }
        DocsCmd::Refresh { package, version } => fetch_and_print(&store, &package, &version),
    }
}

fn fetch_and_print(store: &flare_docs::DocsStore, package: &str, version: &str) {
    let fetcher = flare_docs::UreqFetcher::new();
    match flare_docs::fetch_and_store(&fetcher, store, package, version) {
        Ok(doc) => println!("{}", serde_json::to_string_pretty(&doc).unwrap()),
        Err(e) => {
            eprintln!("flare-docs: fetch failed: {e}");
            std::process::exit(1);
        }
    }
}
