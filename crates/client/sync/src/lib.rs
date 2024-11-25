use crate::l2::L2SyncConfig;
use anyhow::Context;
use fetch::fetchers::FetchConfig;
use hyper::header::{HeaderName, HeaderValue};
use mc_block_import::BlockImporter;
use mc_db::MadaraBackend;
use mc_gateway_client::GatewayProvider;
use mc_telemetry::TelemetryHandle;
use mp_block::{BlockId, BlockTag};
use mp_exex::ExExManagerHandle;
use std::{sync::Arc, time::Duration};

pub mod fetch;
pub mod l2;
pub mod metrics;
#[cfg(test)]
pub mod tests;
pub mod utils;

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(backend, block_importer, fetch_config, telemetry))]
pub async fn sync(
    backend: &Arc<MadaraBackend>,
    block_importer: Arc<BlockImporter>,
    fetch_config: FetchConfig,
    starting_block: Option<u64>,
    backup_every_n_blocks: Option<u64>,
    telemetry: TelemetryHandle,
    pending_block_poll_interval: Duration,
    cancellation_token: tokio_util::sync::CancellationToken,
    exex_manager: Option<ExExManagerHandle>,
) -> anyhow::Result<()> {
    let (starting_block, ignore_block_order) = if let Some(starting_block) = starting_block {
        tracing::warn!("Forcing unordered state. This will most probably break your database.");
        (starting_block, true)
    } else {
        (
            backend
                .get_block_n(&BlockId::Tag(BlockTag::Latest))
                .context("getting sync tip")?
                .map(|block_id| block_id + 1) // next block after the tip
                .unwrap_or_default() as _, // or genesis
            false,
        )
    };

    tracing::info!("⛓️  Starting L2 sync from block {}", starting_block);

    let mut provider = GatewayProvider::new(fetch_config.gateway, fetch_config.feeder_gateway);
    if let Some(api_key) = fetch_config.api_key {
        provider.add_header(
            HeaderName::from_static("x-throttling-bypass"),
            HeaderValue::from_str(&api_key).with_context(|| "Invalid API key format")?,
        )
    }

    l2::sync(
        backend,
        provider,
        L2SyncConfig {
            first_block: starting_block,
            n_blocks_to_sync: fetch_config.n_blocks_to_sync,
            stop_on_sync: fetch_config.stop_on_sync,
            verify: fetch_config.verify,
            sync_polling_interval: fetch_config.sync_polling_interval,
            backup_every_n_blocks,
            pending_block_poll_interval,
            ignore_block_order,
        },
        backend.chain_config().chain_id.clone(),
        telemetry,
        block_importer,
        cancellation_token,
        exex_manager,
    )
    .await?;

    Ok(())
}
