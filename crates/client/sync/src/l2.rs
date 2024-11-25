//! Contains the code required to sync data from the feeder efficiently.
use crate::fetch::fetchers::fetch_pending_block_and_updates;
use crate::fetch::l2_fetch_task;
use crate::utils::trim_hash;
use anyhow::Context;
use futures::{stream, StreamExt};
use mc_block_import::{
    BlockImportResult, BlockImporter, BlockValidationContext, PreValidatedBlock, UnverifiedFullBlock,
};
use mc_db::MadaraBackend;
use mc_db::MadaraStorageError;
use mc_gateway_client::GatewayProvider;
use mc_telemetry::{TelemetryHandle, VerbosityLevel};
use mp_block::BlockId;
use mp_block::BlockTag;
use mp_exex::ExExManagerHandle;
use mp_exex::ExExNotification;
use mp_gateway::error::SequencerError;
use mp_utils::{channel_wait_or_graceful_shutdown, wait_or_graceful_shutdown, PerfStopwatch};
use starknet_api::block::BlockNumber;
use starknet_api::core::ChainId;
use starknet_types_core::felt::Felt;
use std::pin::pin;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::Duration;

// TODO: add more explicit error variants
#[derive(thiserror::Error, Debug)]
pub enum L2SyncError {
    #[error("Provider error: {0:#}")]
    SequencerError(#[from] SequencerError),
    #[error("Database error: {0:#}")]
    Db(#[from] MadaraStorageError),
    #[error(transparent)]
    BlockImport(#[from] mc_block_import::BlockImportError),
    #[error("Unexpected class type for class hash {class_hash:#x}")]
    UnexpectedClassType { class_hash: Felt },
}

/// Contains the latest Starknet verified state on L2
#[derive(Debug, Clone)]
pub struct L2StateUpdate {
    pub block_number: u64,
    pub global_root: Felt,
    pub block_hash: Felt,
}

/// Sends a notification to the ExExs that a block has been imported.
fn notify_exexs(exex_manager: &Option<ExExManagerHandle>, block_n: u64) -> anyhow::Result<()> {
    let Some(manager) = exex_manager.as_ref() else {
        return Ok(());
    };

    let notification = ExExNotification::BlockSynced { block_number: BlockNumber(block_n) };
    manager.send(notification).map_err(|e| anyhow::anyhow!("Could not send ExEx notification: {}", e))
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(backend, updates_receiver, block_import, validation), fields(module = "Sync"))]
async fn l2_verify_and_apply_task(
    backend: Arc<MadaraBackend>,
    mut updates_receiver: mpsc::Receiver<PreValidatedBlock>,
    block_import: Arc<BlockImporter>,
    validation: BlockValidationContext,
    backup_every_n_blocks: Option<u64>,
    telemetry: TelemetryHandle,
    stop_on_sync: bool,
    cancellation_token: tokio_util::sync::CancellationToken,
    exex_manager: Option<ExExManagerHandle>,
) -> anyhow::Result<()> {
    while let Some(block) = channel_wait_or_graceful_shutdown(pin!(updates_receiver.recv()), &cancellation_token).await
    {
        let BlockImportResult { header, block_hash } = block_import.verify_apply(block, validation.clone()).await?;

        tracing::info!(
            "✨ Imported #{} ({}) and updated state root ({})",
            header.block_number,
            trim_hash(&block_hash),
            trim_hash(&header.global_state_root)
        );
        tracing::debug!(
            "Block import #{} ({:#x}) has state root {:#x}",
            header.block_number,
            block_hash,
            header.global_state_root
        );

        notify_exexs(&exex_manager, header.block_number)?;

        telemetry.send(
            VerbosityLevel::Info,
            serde_json::json!({
                "best": block_hash.to_fixed_hex_string(),
                "height": header.block_number,
                "origin": "Own",
                "msg": "block.import",
            }),
        );

        if backup_every_n_blocks.is_some_and(|backup_every_n_blocks| header.block_number % backup_every_n_blocks == 0) {
            tracing::info!("⏳ Backing up database at block {}...", header.block_number);
            let sw = PerfStopwatch::new();
            backend.backup().await.context("backing up database")?;
            tracing::info!("✅ Database backup is done ({:?})", sw.elapsed());
        }
    }

    if stop_on_sync {
        cancellation_token.cancel()
    }

    Ok(())
}

async fn l2_block_conversion_task(
    updates_receiver: mpsc::Receiver<UnverifiedFullBlock>,
    output: mpsc::Sender<PreValidatedBlock>,
    block_import: Arc<BlockImporter>,
    validation: BlockValidationContext,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // Items of this stream are futures that resolve to blocks, which becomes a regular stream of blocks
    // using futures buffered.
    let conversion_stream = stream::unfold(
        (updates_receiver, block_import, validation.clone(), cancellation_token.clone()),
        |(mut updates_recv, block_import, validation, cancellation_token)| async move {
            channel_wait_or_graceful_shutdown(updates_recv.recv(), &cancellation_token).await.map(|block| {
                let block_import_ = Arc::clone(&block_import);
                let validation_ = validation.clone();
                (
                    async move { block_import_.pre_validate(block, validation_).await },
                    (updates_recv, block_import, validation, cancellation_token),
                )
            })
        },
    );

    let mut stream = pin!(conversion_stream.buffered(10));
    while let Some(block) = channel_wait_or_graceful_shutdown(stream.next(), &cancellation_token).await {
        if output.send(block?).await.is_err() {
            // channel closed
            break;
        }
    }
    Ok(())
}

async fn l2_pending_block_task(
    backend: Arc<MadaraBackend>,
    block_import: Arc<BlockImporter>,
    validation: BlockValidationContext,
    sync_finished_cb: oneshot::Receiver<()>,
    provider: Arc<GatewayProvider>,
    pending_block_poll_interval: Duration,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // clear pending status
    {
        backend.clear_pending_block().context("Clearing pending block")?;
        tracing::debug!("l2_pending_block_task: startup: wrote no pending");
    }

    // we start the pending block task only once the node has been fully sync
    if sync_finished_cb.await.is_err() {
        // channel closed
        return Ok(());
    }

    tracing::debug!("Start pending block poll");

    let mut interval = tokio::time::interval(pending_block_poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while wait_or_graceful_shutdown(interval.tick(), &cancellation_token).await.is_some() {
        tracing::debug!("Getting pending block...");

        let current_block_hash = backend
            .get_block_hash(&BlockId::Tag(BlockTag::Latest))
            .context("Getting latest block hash")?
            .unwrap_or(/* genesis parent block hash */ Felt::ZERO);
        let Some(block) = fetch_pending_block_and_updates(
            current_block_hash,
            &backend.chain_config().chain_id,
            &provider,
            &cancellation_token,
        )
        .await
        .context("Getting pending block from FGW")?
        else {
            continue;
        };

        // HACK(see issue #239): The latest block in db may not match the pending parent block hash
        // Just silently ignore it for now and move along.
        let import_block = || async {
            let block = block_import.pre_validate_pending(block, validation.clone()).await?;
            block_import.verify_apply_pending(block, validation.clone()).await?;
            anyhow::Ok(())
        };

        if let Err(err) = import_block().await {
            tracing::debug!("Error while importing pending block: {err:#}");
        }
    }

    Ok(())
}

pub struct L2SyncConfig {
    pub first_block: u64,
    pub n_blocks_to_sync: Option<u64>,
    pub stop_on_sync: bool,
    pub verify: bool,
    pub sync_polling_interval: Option<Duration>,
    pub backup_every_n_blocks: Option<u64>,
    pub pending_block_poll_interval: Duration,
    pub ignore_block_order: bool,
}

/// Spawns workers to fetch blocks and state updates from the feeder.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip(backend, provider, config, chain_id, telemetry, block_importer), fields(module = "Sync"))]
pub async fn sync(
    backend: &Arc<MadaraBackend>,
    provider: GatewayProvider,
    config: L2SyncConfig,
    chain_id: ChainId,
    telemetry: TelemetryHandle,
    block_importer: Arc<BlockImporter>,
    cancellation_token: tokio_util::sync::CancellationToken,
    exex_manager: Option<ExExManagerHandle>,
) -> anyhow::Result<()> {
    let (fetch_stream_sender, fetch_stream_receiver) = mpsc::channel(8);
    let (block_conv_sender, block_conv_receiver) = mpsc::channel(4);
    let provider = Arc::new(provider);
    let (once_caught_up_cb_sender, once_caught_up_cb_receiver) = oneshot::channel();

    // [Fetch task] ==new blocks and updates=> [Block conversion task] ======> [Verification and apply
    // task]
    // - Fetch task does parallel fetching
    // - Block conversion is compute heavy and parallel wrt. the next few blocks,
    // - Verification is sequential and does a lot of compute when state root verification is enabled.
    //   DB updates happen here too.

    // we are using separate tasks so that fetches don't get clogged up if by any chance the verify task
    // starves the tokio worker
    let validation = BlockValidationContext {
        trust_transaction_hashes: false,
        trust_global_tries: !config.verify,
        chain_id,
        trust_class_hashes: false,
        ignore_block_order: config.ignore_block_order,
    };

    let mut join_set = JoinSet::new();
    join_set.spawn(l2_fetch_task(
        Arc::clone(backend),
        config.first_block,
        config.n_blocks_to_sync,
        config.stop_on_sync,
        fetch_stream_sender,
        Arc::clone(&provider),
        config.sync_polling_interval,
        once_caught_up_cb_sender,
        cancellation_token.clone(),
    ));
    join_set.spawn(l2_block_conversion_task(
        fetch_stream_receiver,
        block_conv_sender,
        Arc::clone(&block_importer),
        validation.clone(),
        cancellation_token.clone(),
    ));
    join_set.spawn(l2_verify_and_apply_task(
        Arc::clone(backend),
        block_conv_receiver,
        Arc::clone(&block_importer),
        validation.clone(),
        config.backup_every_n_blocks,
        telemetry,
        config.stop_on_sync,
        cancellation_token.clone(),
        exex_manager,
    ));
    join_set.spawn(l2_pending_block_task(
        Arc::clone(backend),
        Arc::clone(&block_importer),
        validation.clone(),
        once_caught_up_cb_receiver,
        provider,
        config.pending_block_poll_interval,
        cancellation_token.clone(),
    ));

    while let Some(res) = join_set.join_next().await {
        res.context("task was dropped")??;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::utils::gateway::{test_setup, TestContext};
    use mc_block_import::tests::block_import_utils::create_dummy_unverified_full_block;
    use mc_block_import::BlockImporter;
    use mc_db::{db_block_id::DbBlockId, MadaraBackend};

    use mc_telemetry::TelemetryService;
    use mp_block::header::L1DataAvailabilityMode;
    use mp_block::MadaraBlock;
    use mp_chain_config::StarknetVersion;
    use rstest::rstest;
    use starknet_types_core::felt::Felt;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Test the `l2_verify_and_apply_task` function.
    ///
    ///
    /// This test verifies the behavior of the `l2_verify_and_apply_task` by simulating
    /// a block verification and application process.
    ///
    /// # Test Steps
    /// 1. Initialize the backend and necessary components.
    /// 2. Create a mock block.
    /// 3. Spawn the `l2_verify_and_apply_task` in a new thread.
    /// 4. Send the mock block for verification and application.
    /// 5. Wait for the task to complete or for a timeout to occur.
    /// 6. Verify that the block has been correctly applied to the backend.
    ///
    /// # Panics
    /// - If the task fails or if the waiting timeout is exceeded.
    /// - If the block is not correctly applied to the backend.
    #[rstest]
    #[tokio::test]
    async fn test_l2_verify_and_apply_task(test_setup: Arc<MadaraBackend>) {
        let backend = test_setup;
        let (block_conv_sender, block_conv_receiver) = mpsc::channel(100);
        let block_importer = Arc::new(BlockImporter::new(backend.clone(), None, true).unwrap());
        let validation = BlockValidationContext::new(backend.chain_config().chain_id.clone());
        let telemetry = TelemetryService::new(true, vec![]).unwrap().new_handle();

        let mock_block = create_dummy_unverified_full_block();

        let task_handle = tokio::spawn(l2_verify_and_apply_task(
            backend.clone(),
            block_conv_receiver,
            block_importer.clone(),
            validation.clone(),
            Some(1),
            telemetry,
            false,
            tokio_util::sync::CancellationToken::new(),
            None,
        ));

        let mock_pre_validated_block = block_importer.pre_validate(mock_block, validation.clone()).await.unwrap();
        block_conv_sender.send(mock_pre_validated_block).await.unwrap();

        drop(block_conv_sender);

        match tokio::time::timeout(std::time::Duration::from_secs(120), task_handle).await {
            Ok(Ok(_)) => (),
            Ok(Err(e)) => panic!("Task failed: {:?}", e),
            Err(_) => panic!("Timeout reached while waiting for task completion"),
        }

        let applied_block = backend.get_block(&DbBlockId::Number(0)).unwrap();
        assert!(applied_block.is_some(), "The block was not applied correctly");
        let applied_block = MadaraBlock::try_from(applied_block.unwrap()).unwrap();

        assert_eq!(applied_block.info.header.block_number, 0, "Block number does not match");
        assert_eq!(applied_block.info.header.block_timestamp, 0, "Block timestamp does not match");
        assert_eq!(applied_block.info.header.parent_block_hash, Felt::ZERO, "Parent block hash does not match");
        assert!(applied_block.inner.transactions.is_empty(), "Block should not contain any transactions");
        assert_eq!(
            applied_block.info.header.protocol_version,
            StarknetVersion::default(),
            "Protocol version does not match"
        );
        assert_eq!(applied_block.info.header.sequencer_address, Felt::ZERO, "Sequencer address does not match");
        assert_eq!(applied_block.info.header.l1_gas_price.eth_l1_gas_price, 0, "L1 gas price (ETH) does not match");
        assert_eq!(applied_block.info.header.l1_gas_price.strk_l1_gas_price, 0, "L1 gas price (STRK) does not match");
        assert_eq!(applied_block.info.header.l1_da_mode, L1DataAvailabilityMode::Blob, "L1 DA mode does not match");
    }

    /// Test the `l2_block_conversion_task` function.
    ///
    /// Steps:
    /// 1. Initialize necessary components.
    /// 2. Create a mock block.
    /// 3. Send the mock block to updates_sender
    /// 4. Call the `l2_block_conversion_task` function with the mock data.
    /// 5. Verify the results and ensure the function behaves as expected.
    #[rstest]
    #[tokio::test]
    async fn test_l2_block_conversion_task(test_setup: Arc<MadaraBackend>) {
        let backend = test_setup;
        let (updates_sender, updates_receiver) = mpsc::channel(100);
        let (output_sender, mut output_receiver) = mpsc::channel(100);
        let block_import = Arc::new(BlockImporter::new(backend.clone(), None, true).unwrap());
        let validation = BlockValidationContext::new(backend.chain_config().chain_id.clone());

        let mock_block = create_dummy_unverified_full_block();

        updates_sender.send(mock_block).await.unwrap();

        let task_handle = tokio::spawn(l2_block_conversion_task(
            updates_receiver,
            output_sender,
            block_import,
            validation,
            tokio_util::sync::CancellationToken::new(),
        ));

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), output_receiver.recv()).await;
        match result {
            Ok(Some(b)) => {
                assert_eq!(b.unverified_block_number, Some(0), "Block number does not match");
            }
            Ok(None) => panic!("Channel closed without receiving a result"),
            Err(_) => panic!("Timeout reached while waiting for result"),
        }

        // Close the updates_sender channel to allow the task to complete
        drop(updates_sender);

        match tokio::time::timeout(std::time::Duration::from_secs(5), task_handle).await {
            Ok(Ok(_)) => (),
            Ok(Err(e)) => panic!("Task failed: {:?}", e),
            Err(_) => panic!("Timeout reached while waiting for task completion"),
        }
    }

    /// Test the `l2_pending_block_task` function.
    ///
    /// This test function verifies the behavior of the `l2_pending_block_task`.
    /// It simulates the necessary environment and checks that the task executes correctly
    /// within a specified timeout.
    ///
    /// # Test Steps
    /// 1. Initialize the backend and test context.
    /// 2. Create a `BlockImporter` and a `BlockValidationContext`.
    /// 3. Spawn the `l2_pending_block_task` in a new thread.
    /// 4. Simulate the "once_caught_up" signal.
    /// 5. Wait for the task to complete or for a timeout to occur.
    ///
    /// # Panics
    /// - If the task fails or if the waiting timeout is exceeded.
    #[rstest]
    #[tokio::test]
    async fn test_l2_pending_block_task(test_setup: Arc<MadaraBackend>) {
        let backend = test_setup;
        let ctx = TestContext::new(backend.clone());
        let block_import = Arc::new(BlockImporter::new(backend.clone(), None, true).unwrap());
        let validation = BlockValidationContext::new(backend.chain_config().chain_id.clone());

        let task_handle = tokio::spawn(l2_pending_block_task(
            backend.clone(),
            block_import.clone(),
            validation.clone(),
            ctx.once_caught_up_receiver,
            ctx.provider.clone(),
            std::time::Duration::from_secs(5),
            tokio_util::sync::CancellationToken::new(),
        ));

        // Simulate the "once_caught_up" signal
        ctx.once_caught_up_sender.send(()).unwrap();

        // Wait for the task to complete
        match tokio::time::timeout(std::time::Duration::from_secs(120), task_handle).await {
            Ok(Ok(_)) => (),
            Ok(Err(e)) => panic!("Task failed: {:?}", e),
            Err(_) => panic!("Timeout reached while waiting for task completion"),
        }
    }
}
