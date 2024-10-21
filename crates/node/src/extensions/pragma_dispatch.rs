//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.
use std::sync::Arc;

use futures::StreamExt;
use mp_block::MadaraPendingBlock;
use mp_rpc::Starknet;
use starknet_api::felt;
use starknet_core::types::{
    BlockId, BlockTag, BroadcastedInvokeTransaction, BroadcastedInvokeTransactionV1, BroadcastedTransaction, Felt,
    FunctionCall, InvokeTransactionResult,
};
use starknet_signers::SigningKey;

use mc_devnet::{Call, Multicall, Selector};
use mc_mempool::transaction_hash;
use mc_rpc::versions::v0_7_1::{StarknetReadRpcApiV0_7_1Server, StarknetWriteRpcApiV0_7_1Server};
use mp_convert::ToFelt;
use mp_exex::{ExExContext, ExExEvent, ExExNotification};
use mp_transactions::broadcasted_to_blockifier;

const PENDING_BLOCK: BlockId = BlockId::Tag(BlockTag::Pending);

lazy_static::lazy_static! {
    // TODO: Keystore path?
    pub static ref ACCOUNT_ADDRESS: Felt = felt!("0x029aed70d5adfa054273b9f7b76a4c4bc3a0120fd5fd36a2e5f9e59f033a2330");
    pub static ref PRIVATE_KEY: SigningKey = SigningKey::from_secret_scalar(felt!("0x0410c6eadd73918ea90b6658d24f5f2c828e39773819c1443d8602a3c72344c2"));

    pub static ref PRAGMA_FEEDS_REGISTRY_ADDRESS: Felt = felt!("0x4bdad51395192c74da761af49ecd9443514ff01dbfd15b490c798f57a3e75d8");
    pub static ref PRAGMA_DISPATCHER_ADDRESS: Felt = felt!("0x60240f2bccef7e64f920eec05c5bfffbc48c6ceaa4efca8748772b60cbafc3");

    pub static ref MAX_FEE: Felt = felt!("2386F26FC10000"); // 0.01 eth

    // NewFeedId event selector
    pub static ref NEW_FEED_ID_SELECTOR: Felt = felt!("0x012eaeb62184f1ca53999ece2d2273b81f9c64bc057a93dad05e09f970b030f9");
    // RemovedFeedId event selector
    pub static ref REMOVED_FEED_ID_SELECTOR: Felt = felt!("0x02a45c5a3b53e7afa46712156f544cec1b9d4679804036a16ec9521389117be4");

    // Empty feed list. Used instead of [`Vec::is_empty`].
    // The first element is the length of the vec & after are the elements.
    pub static ref EMPTY_FEEDS: Vec<Felt> = vec![Felt::ZERO];
}

/// 🧩 Pragma main ExEx.
/// At the end of each produced block by the node, adds a new dispatch transaction
/// using the Pragma Dispatcher contract.
pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    // Feed ids that will be dispatched.
    // The first element is the length of the vec & after are the elements.
    let mut feed_ids: Vec<Felt> = get_feed_ids_from_registry(&ctx.starknet).await.unwrap_or(vec![Felt::ZERO]);
    log::info!("🧩 Pragma's ExEx: Initialized feed IDs from Registry. Total feeds: {}", feed_ids[0]);

    while let Some(notification) = ctx.notifications.next().await {
        let (block, block_number) = match notification {
            ExExNotification::BlockProduced { block, block_number } => (block, block_number),
            ExExNotification::BlockSynced { block_number } => {
                ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                continue;
            }
        };

        // Will update in-place the feed ids vec
        if let Err(e) = update_feed_ids_if_necessary(&ctx.starknet, &block, block_number.0, &mut feed_ids).await {
            log::error!("🧩 [#{}] Pragma's ExEx: Error while updating feed IDs: {:?}", block_number, e);
            ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
            continue;
        }

        if feed_ids == *EMPTY_FEEDS {
            log::warn!("🧩 [#{}] Pragma's ExEx: No feed IDs available, skipping dispatch", block_number);
            ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
            continue;
        }

        // Don't dispatch if we're too late in the blocks.
        match ctx.starknet.current_block_number() {
            Ok(current_head) => {
                if current_head - 1 > block_number.0 {
                    ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                    continue;
                }
            }
            Err(_) => {
                ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                continue;
            }
        }

        if let Err(e) = process_dispatch_transaction(&ctx, block_number.0, &feed_ids).await {
            log::error!("🧩 [#{}] Pragma's ExEx: Error while processing dispatch transaction: {:?}", block_number, e);
        }

        ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
    }
    Ok(())
}

/// Update the feed ids list if necessary.
/// It means:
///   * if the feed id list is empty,
///   * if we find the event [NewFeedId] or [RemovedFeedId] in the block's events.
async fn update_feed_ids_if_necessary(
    starknet: &Arc<Starknet>,
    block: &MadaraPendingBlock,
    block_number: u64,
    feed_ids: &mut Vec<Felt>,
) -> anyhow::Result<()> {
    // If the list is empty, it may be because the contract wasn't deployed before.
    // Requery.
    if *feed_ids == *EMPTY_FEEDS {
        *feed_ids = get_feed_ids_from_registry(starknet).await?;
        log::info!("🧩 [#{}] Pragma's ExEx: Refreshed all feeds. Total feeds: {}", block_number, feed_ids[0]);
        return Ok(());
    }

    for receipt in &block.inner.receipts {
        if let mp_receipt::TransactionReceipt::Invoke(invoke_receipt) = receipt {
            for event in &invoke_receipt.events {
                if event.from_address != *PRAGMA_FEEDS_REGISTRY_ADDRESS {
                    continue;
                }
                if event.keys.is_empty() || event.data.len() != 2 {
                    continue;
                }
                let selector = event.keys[0];
                let feed_id = event.data[1];
                if selector == *NEW_FEED_ID_SELECTOR {
                    if !feed_ids.contains(&feed_id) {
                        feed_ids.push(feed_id);
                        feed_ids[0] += Felt::ONE;
                        log::info!(
                            "🧩 [#{}] Pragma's ExEx: Added new feed ID \"0x{:x}\". Total feeds: {}",
                            block_number,
                            feed_id,
                            feed_ids[0]
                        );
                    }
                } else if selector == *REMOVED_FEED_ID_SELECTOR {
                    if let Some(pos) = feed_ids.iter().position(|x| *x == feed_id) {
                        feed_ids.remove(pos);
                        feed_ids[0] -= Felt::ONE;
                        log::info!(
                            "🧩 [#{}] Pragma's ExEx: Removed feed ID \"0x{:x}\". Total feeds: {}",
                            block_number,
                            feed_id,
                            feed_ids[0]
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Create a Dispatch tx and sends it.
/// Logs info about the tx status.
async fn process_dispatch_transaction(ctx: &ExExContext, block_number: u64, feed_ids: &[Felt]) -> anyhow::Result<()> {
    let invoke_result = create_and_add_dispatch_tx(&ctx.starknet, feed_ids, block_number).await?;
    log::info!("🧩 [#{}] Pragma's ExEx: Transaction sent, hash: {}", block_number, &invoke_result.transaction_hash);
    Ok(())
}

/// Creates & Invoke the Dispatch TX.
async fn create_and_add_dispatch_tx(
    starknet: &Arc<Starknet>,
    feed_ids: &[Felt],
    block_number: u64,
) -> anyhow::Result<InvokeTransactionResult> {
    let dispatch_tx = create_dispatch_tx(starknet, feed_ids)?;
    log::info!("🧩 [#{}] Pragma's ExEx: Adding dispatch transaction...", block_number);
    let invoke_result = starknet.add_invoke_transaction(dispatch_tx).await?;
    Ok(invoke_result)
}

/// Creates a new Dispatch transaction.
/// The transaction will be signed using the `ACCOUNT_ADDRESS` and `PRIVATE_KEY` constants.
fn create_dispatch_tx(starknet: &Arc<Starknet>, feed_ids: &[Felt]) -> anyhow::Result<BroadcastedInvokeTransaction> {
    let mut tx = BroadcastedInvokeTransaction::V1(BroadcastedInvokeTransactionV1 {
        sender_address: *ACCOUNT_ADDRESS,
        calldata: Multicall::default()
            .with(Call {
                to: *PRAGMA_DISPATCHER_ADDRESS,
                selector: Selector::from("dispatch"),
                calldata: feed_ids.to_vec(),
            })
            .flatten()
            .collect(),
        max_fee: *MAX_FEE,
        signature: vec![], // This will get filled below
        nonce: starknet.get_nonce(PENDING_BLOCK, *ACCOUNT_ADDRESS)?,
        is_query: false,
    });
    tx = sign_tx(starknet, tx)?;
    Ok(tx)
}

/// Sign a transaction using the constants.
fn sign_tx(
    starknet: &Arc<Starknet>,
    mut tx: BroadcastedInvokeTransaction,
) -> anyhow::Result<BroadcastedInvokeTransaction> {
    let (blockifier_tx, _) = broadcasted_to_blockifier(
        BroadcastedTransaction::Invoke(tx.clone()),
        starknet.chain_config.chain_id.to_felt(),
        starknet.chain_config.latest_protocol_version,
    )?;

    let signature = PRIVATE_KEY.sign(&transaction_hash(&blockifier_tx))?;
    let tx_signature = match &mut tx {
        BroadcastedInvokeTransaction::V1(tx) => &mut tx.signature,
        BroadcastedInvokeTransaction::V3(tx) => &mut tx.signature,
    };
    *tx_signature = vec![signature.r, signature.s];
    Ok(tx)
}

/// Retrieves the available feed ids from the Pragma Feeds Registry.
async fn get_feed_ids_from_registry(starknet: &Arc<Starknet>) -> anyhow::Result<Vec<Felt>> {
    let call = FunctionCall {
        contract_address: *PRAGMA_FEEDS_REGISTRY_ADDRESS,
        entry_point_selector: Selector::from("get_all_feeds").into(),
        calldata: vec![],
    };
    let feed_ids = starknet.call(call, PENDING_BLOCK)?;
    Ok(feed_ids)
}
