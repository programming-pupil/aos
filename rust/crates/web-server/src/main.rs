//! Binary entry point for the web-server.

use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use web_server::{configured_tokio_worker_stack_size_bytes, serve_with_options};

#[derive(Parser, Debug)]
#[command(author, version, about = "Enterprise WebUI Server")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:3001")]
    addr: SocketAddr,

    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Serve the built Web UI from this directory and fall back to index.html
    /// for client-side routes.
    #[arg(long)]
    web_dir: Option<PathBuf>,

    /// Directory where `aos` CLI writes `.aos/telemetry.jsonl`.
    /// Defaults to the current working directory. Must match the directory
    /// where the CLI is run (e.g. your project root).
    #[arg(long)]
    telemetry_dir: Option<PathBuf>,

    /// Default model used for chat requests (e.g. "anthropic/claude-3-5-sonnet-4-20250514").
    /// Defaults to Sonnet 4 if not specified.
    #[arg(long)]
    model: Option<String>,

    /// Load and validate the already-downloaded built-in embedding model, then exit.
    #[cfg(feature = "nl2sql")]
    #[arg(long, value_name = "CACHE_DIR")]
    warm_local_embedding: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    #[cfg(feature = "nl2sql")]
    if let Some(cache_dir) = args.warm_local_embedding {
        let result = web_server::warm_local_embedding_model(cache_dir);
        web_server::shutdown_local_embedding_model();
        result.expect("failed to warm built-in local embedding model");
        return;
    }

    let data_dir = args.data_dir.unwrap_or_else(|| {
        directories::ProjectDirs::from("com", "aos", "enterprise")
            .map_or_else(|| PathBuf::from("."), |d| d.data_dir().to_path_buf())
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("aos-tokio-worker")
        .thread_stack_size(configured_tokio_worker_stack_size_bytes())
        .build()
        .expect("failed to build Tokio runtime");

    runtime.block_on(serve_with_options(
        args.addr,
        data_dir,
        args.telemetry_dir,
        args.model,
        args.web_dir,
    ));
    drop(runtime);

    #[cfg(feature = "nl2sql")]
    web_server::shutdown_local_embedding_model();
}
