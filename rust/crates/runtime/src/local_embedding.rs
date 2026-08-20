use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

pub const MODEL: &str = "local/paraphrase-multilingual-minilm-l12-v2-q";
pub const DIMENSIONS: usize = 384;
pub const MODEL_VERSION: &str = "faf4aa4225822f3bc6376869cb1164e8e3feedd0";
pub const VECTOR_SIGNATURE: &str =
    "sha256:634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99";
const MAX_INFERENCE_BATCH_SIZE: usize = 8;

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static MODEL_INSTANCE: OnceLock<Mutex<Option<fastembed::TextEmbedding>>> = OnceLock::new();
static INTERACTIVE_WAITERS: AtomicUsize = AtomicUsize::new(0);

struct InteractiveWaiterGuard;

impl InteractiveWaiterGuard {
    fn new() -> Self {
        INTERACTIVE_WAITERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for InteractiveWaiterGuard {
    fn drop(&mut self) {
        INTERACTIVE_WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn configure_cache_for_data_dir(data_dir: &Path) -> anyhow::Result<()> {
    let cache_dir = std::env::var_os("AOS_LOCAL_EMBEDDING_CACHE_DIR")
        .map_or_else(|| data_dir.join("models").join("fastembed"), PathBuf::from);
    configure_cache_dir(cache_dir)
}

pub fn configure_cache_dir(cache_dir: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cache_dir)?;
    if let Some(existing) = CACHE_DIR.get() {
        if existing != &cache_dir {
            anyhow::bail!(
                "local embedding cache already configured at {}; cannot change it to {}",
                existing.display(),
                cache_dir.display()
            );
        }
        return Ok(());
    }
    CACHE_DIR.set(cache_dir).map_err(|path| {
        anyhow::anyhow!(
            "failed to configure local embedding cache at {}",
            path.display()
        )
    })
}

fn build_model() -> anyhow::Result<fastembed::TextEmbedding> {
    let cache_dir = CACHE_DIR
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("local embedding cache directory is not configured"))?;
    let snapshot_dir = cache_dir
        .join("models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q")
        .join("snapshots")
        .join(MODEL_VERSION);
    let model_path = snapshot_dir.join("model_optimized.onnx");
    let required_files = [
        model_path.clone(),
        snapshot_dir.join("tokenizer.json"),
        snapshot_dir.join("config.json"),
        snapshot_dir.join("special_tokens_map.json"),
        snapshot_dir.join("tokenizer_config.json"),
    ];
    if !required_files.iter().all(|path| path.is_file()) {
        let missing = required_files
            .iter()
            .filter(|path| !path.is_file())
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "bundled local embedding model is incomplete; missing: {missing}. AOS never downloads model files from the server process"
        );
    }

    let model_bytes = std::fs::read(&model_path)?;
    let actual_signature = format!("sha256:{}", hex::encode(Sha256::digest(&model_bytes)));
    if actual_signature != VECTOR_SIGNATURE {
        anyhow::bail!(
            "bundled local embedding model checksum mismatch at {}; expected {}, got {}",
            model_path.display(),
            VECTOR_SIGNATURE,
            actual_signature
        );
    }
    let tokenizer_files = fastembed::TokenizerFiles {
        tokenizer_file: std::fs::read(&required_files[1])?,
        config_file: std::fs::read(&required_files[2])?,
        special_tokens_map_file: std::fs::read(&required_files[3])?,
        tokenizer_config_file: std::fs::read(&required_files[4])?,
    };
    let model = fastembed::UserDefinedEmbeddingModel::new(model_bytes, tokenizer_files)
        .with_pooling(fastembed::Pooling::Mean)
        .with_quantization(fastembed::QuantizationMode::Static);
    fastembed::TextEmbedding::try_new_from_user_defined(
        model,
        fastembed::InitOptionsUserDefined::new(),
    )
    .map_err(|error| anyhow::anyhow!("failed to load bundled local embedding model: {error}"))
}

fn with_model<T>(
    operation: impl FnOnce(&mut fastembed::TextEmbedding) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let model_slot = MODEL_INSTANCE.get_or_init(|| Mutex::new(None));
    let mut model = model_slot.lock();
    if model.is_none() {
        *model = Some(build_model()?);
    }
    operation(
        model
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("built-in embedding model was not initialized"))?,
    )
}

pub fn warm() -> anyhow::Result<()> {
    let texts = vec!["AOS local embedding readiness check".to_string()];
    let vectors = embed(&texts)?;
    let dimensions = vectors.first().map_or(0, Vec::len);
    if dimensions != DIMENSIONS {
        anyhow::bail!(
            "built-in embedding model returned {dimensions} dimensions; expected {DIMENSIONS}"
        );
    }
    Ok(())
}

pub fn shutdown() {
    if let Some(model_slot) = MODEL_INSTANCE.get() {
        drop(model_slot.lock().take());
    }
}

pub fn embed(texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
    let _interactive_waiter = InteractiveWaiterGuard::new();
    embed_with_priority(texts, false)
}

/// Run a lower-priority indexing batch without starving interactive queries.
///
/// ONNX inference is serialized because the bundled model is a singleton. A
/// repository import can enqueue thousands of chunks, so background callers
/// wait between batches whenever a user-facing query is ready to run.
pub fn embed_background(texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
    embed_with_priority(texts, true)
}

fn embed_with_priority(texts: &[String], background: bool) -> anyhow::Result<Vec<Vec<f32>>> {
    let mut vectors = Vec::with_capacity(texts.len());
    for batch in texts.chunks(MAX_INFERENCE_BATCH_SIZE) {
        if background {
            while INTERACTIVE_WAITERS.load(Ordering::Acquire) > 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let mut batch_vectors = with_model(|model| {
            model
                .embed(batch, None)
                .map_err(|error| anyhow::anyhow!("built-in embedding inference failed: {error}"))
        })?;
        vectors.append(&mut batch_vectors);
    }
    Ok(vectors)
}

#[cfg(test)]
mod tests {
    use super::{configure_cache_dir, embed, shutdown, DIMENSIONS};
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires AOS_TEST_LOCAL_EMBEDDING_CACHE_DIR and ONNX Runtime"]
    fn bundled_model_loads_and_produces_distinct_finite_vectors() {
        let cache_dir = std::env::var_os("AOS_TEST_LOCAL_EMBEDDING_CACHE_DIR")
            .map(PathBuf::from)
            .expect("AOS_TEST_LOCAL_EMBEDDING_CACHE_DIR must point to the bundled model cache");
        configure_cache_dir(cache_dir).expect("configure local embedding cache");

        let texts = vec![
            "AOS local embedding readiness check".to_string(),
            "完全不同的业务指标查询".to_string(),
        ];
        let vectors = embed(&texts).expect("run local embedding inference");
        assert_eq!(vectors.len(), 2);
        assert!(vectors.iter().all(
            |vector| vector.len() == DIMENSIONS && vector.iter().all(|value| value.is_finite())
        ));
        assert_ne!(vectors[0], vectors[1]);
        shutdown();
    }
}
