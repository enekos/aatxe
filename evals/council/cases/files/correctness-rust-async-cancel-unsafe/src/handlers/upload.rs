use crate::state::AppState;
use anyhow::Result;
use tokio::io::AsyncWriteExt;

pub struct UploadedFile {
    pub bytes: Vec<u8>,
    pub size_bytes: u64,
}

pub async fn upload(state: &AppState, file: UploadedFile) -> Result<u64> {
    // Hold the index lock across the whole reserve→write→commit cycle
    // so concurrent uploads observe a consistent next_id.
    let mut idx = state.file_index.lock().await;

    let id = idx.reserve_id(file.size_bytes);

    // Bump the in-flight-upload counter so the dashboard shows live
    // throughput.
    state.metrics.write_started(file.size_bytes);

    let path = state.upload_dir.join(format!("{id}.blob"));
    let mut f = tokio::fs::File::create(&path).await?;
    f.write_all(&file.bytes).await?;
    f.sync_all().await?;

    idx.commit(id);
    state.metrics.write_completed();
    Ok(id)
}
