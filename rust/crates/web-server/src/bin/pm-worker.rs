//! Standalone PM background worker process.

use clap::Parser;
use std::path::PathBuf;
use web_server::{
    configured_tokio_worker_stack_size_bytes, init_pm_worker_state, run_pm_worker_loop,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "AOS PM background worker")]
struct Args {
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Default model used when restoring sessions without an explicit model.
    #[arg(long)]
    model: Option<String>,
}

fn main() {
    let args = Args::parse();
    std::env::set_var("AOS_PM_WORKER_PROCESS", "true");

    let data_dir = args.data_dir.unwrap_or_else(|| {
        directories::ProjectDirs::from("com", "aos", "enterprise")
            .map_or_else(|| PathBuf::from("."), |d| d.data_dir().to_path_buf())
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("aos-pm-process-worker")
        .thread_stack_size(configured_tokio_worker_stack_size_bytes())
        .build()
        .expect("failed to build PM worker Tokio runtime");

    runtime.block_on(async move {
        let state = init_pm_worker_state(data_dir, args.model)
            .await
            .expect("failed to init PM worker state");
        run_pm_worker_loop(state).await;
    });
}
