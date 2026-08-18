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

    /// Rebuild Memory search projections from canonical structured facts for
    /// this tenant, then exit. Use with --data-dir; this never reads the old
    /// projection as an authority.
    #[arg(long, value_name = "TENANT")]
    rebuild_memory_projection: Option<String>,

    /// Limit a Memory projection rebuild to one user. Defaults to every user
    /// with canonical facts in the tenant.
    #[arg(long, requires = "rebuild_memory_projection")]
    memory_user: Option<String>,

    /// Verify the durable projection hash after each rebuilt user scope.
    #[arg(long, requires = "rebuild_memory_projection")]
    verify_memory_projection: bool,

    /// Default model used for chat requests (e.g. "anthropic/claude-3-5-sonnet-4-20250514").
    /// Defaults to Sonnet 4 if not specified.
    #[arg(long)]
    model: Option<String>,

    /// Load and validate the already-downloaded built-in embedding model, then exit.
    #[cfg(feature = "nl2sql")]
    #[arg(long, value_name = "CACHE_DIR")]
    warm_local_embedding: Option<PathBuf>,

    /// Internal black-box persistence TCK. Debug builds only; never expose in
    /// release help or use this mode for normal server startup.
    #[cfg(debug_assertions)]
    #[arg(long, hide = true, value_name = "DATA_DIR")]
    semantic_kernel_process_tck: Option<PathBuf>,

    #[cfg(debug_assertions)]
    #[arg(long, hide = true, requires = "semantic_kernel_process_tck")]
    semantic_kernel_tck_case: Option<String>,

    #[cfg(debug_assertions)]
    #[arg(long, hide = true, requires = "semantic_kernel_process_tck")]
    semantic_kernel_tck_mode: Option<String>,
}

fn main() {
    let args = Args::parse();

    if let Some(tenant_id) = args.rebuild_memory_projection.as_deref() {
        let data_dir = args.data_dir.clone().unwrap_or_else(|| {
            directories::ProjectDirs::from("com", "aos", "enterprise")
                .map_or_else(|| PathBuf::from("."), |d| d.data_dir().to_path_buf())
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build Memory projection rebuild runtime");
        runtime
            .block_on(web_server::rebuild_memory_projection(
                data_dir,
                tenant_id,
                args.memory_user.as_deref(),
                args.verify_memory_projection,
            ))
            .expect("failed to rebuild Memory projection");
        return;
    }

    #[cfg(feature = "nl2sql")]
    if let Some(cache_dir) = args.warm_local_embedding {
        let result = web_server::warm_local_embedding_model(cache_dir);
        web_server::shutdown_local_embedding_model();
        result.expect("failed to warm built-in local embedding model");
        return;
    }

    #[cfg(debug_assertions)]
    if let Some(data_dir) = args.semantic_kernel_process_tck {
        std::env::set_var("AOS_INTERNAL_PROCESS_TCK", "1");
        let case = args
            .semantic_kernel_tck_case
            .expect("semantic kernel TCK case is required");
        let mode = args
            .semantic_kernel_tck_mode
            .unwrap_or_else(|| "prepare".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build semantic kernel TCK runtime");
        runtime
            .block_on(web_server::run_semantic_kernel_process_tck(
                data_dir, case, mode,
            ))
            .expect("semantic kernel process TCK failed");
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
