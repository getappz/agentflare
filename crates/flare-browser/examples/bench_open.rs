//! `cargo run -p flare-browser --example bench_open -- [agent-browser path]`
//! Five cold opens, warm opens, snapshots and URL reads in an isolated session.
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), String> {
    let backend = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(flare_browser::find_backend)?;
    let session = format!("af-bench-{}", std::process::id());
    let run = |action: &str, args: &[String]| {
        let argv = flare_browser::build_argv(&session, action, args, &[])?;
        flare_browser::run_blocking(&backend, &argv, &[], None, std::process::Stdio::null())
    };
    let result = (|| -> Result<(), String> {
        println!("phase,run,elapsed_ms");
        for phase in ["cold_open", "warm_open", "snapshot", "get_url"] {
            for sample in 1..=5 {
                if phase == "cold_open" {
                    run("close", &[])?;
                }
                let (action, args) = match phase {
                    "cold_open" | "warm_open" => ("open", vec!["about:blank".into()]),
                    "snapshot" => ("snapshot", vec![]),
                    _ => ("get", vec!["url".into()]),
                };
                let start = Instant::now();
                let output = run(action, &args)?;
                println!(
                    "{phase},{sample},{:.3}",
                    start.elapsed().as_secs_f64() * 1000.0
                );
                if action != "snapshot" && !output.contains("about:blank") {
                    return Err(format!("{phase}: unexpected output: {output}"));
                }
            }
        }
        Ok(())
    })();
    let cleanup = run("close", &[]);
    result?;
    cleanup?;
    Ok(())
}
