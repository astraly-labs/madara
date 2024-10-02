//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.
use std::sync::Arc;

use futures::StreamExt;
use mp_rpc::Starknet;
use starknet_api::felt;
use starknet_core::types::{
    BlockId, BlockTag, BroadcastedInvokeTransaction, BroadcastedInvokeTransactionV1, BroadcastedTransaction, Felt,
    FunctionCall,
};
use starknet_signers::SigningKey;

use mc_devnet::{Call, Multicall, Selector};
use mc_mempool::transaction_hash;
use mc_rpc::versions::v0_7_1::{StarknetReadRpcApiV0_7_1Server, StarknetWriteRpcApiV0_7_1Server};
use mp_convert::ToFelt;
use mp_exex::{ExExContext, ExExEvent, ExExNotification};
use mp_transactions::broadcasted_to_blockifier;

const PENDING_BLOCK: BlockId = BlockId::Tag(BlockTag::Pending);

// Update the feed ids from the Feed Registry every 500 blocks. (500s)
const UPDATE_FEEDS_INTERVAL: u64 = 500;

lazy_static::lazy_static! {
    // TODO: Keystore path?
    pub static ref ACCOUNT_ADDRESS: Felt = felt!("0x4a2b383d808b7285cc98b2309f974f5111633c84fd82c9375c118485d2d57ba");
    pub static ref PRIVATE_KEY: SigningKey = SigningKey::from_secret_scalar(felt!("0x7a9779748888c95d96bbbce041b5109c6ffc0c4f30561c0170384a5922d9e91"));

    // TODO: Replace by the correct addresses
    pub static ref PRAGMA_FEEDS_REGISTRY_ADDRESS: Felt = felt!("0x6c05a18cb507fdbb2049047538f2824116e118e5699ae163c7473da38df2bb");
    pub static ref PRAGMA_DISPATCHER_ADDRESS: Felt = felt!("0x25a70290a333bc22397a6dac4c44d3af50c3adfe7f397d504422fd72cb9858a");

    pub static ref MAX_FEE: Felt = felt!("2386F26FC10000"); // 0.01 eth
}

/// 🧩 Pragma main ExEx.
/// At the end of each produced block by the node, adds a new dispatch transaction
/// using the Pragma Dispatcher contract.
pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    let mut feed_ids: Vec<Felt> = Vec::new();
    let mut last_fetch_block = 0;

    while let Some(notification) = ctx.notifications.next().await {
        let block_number = match notification {
            ExExNotification::BlockProduced { block: _, block_number } => block_number,
            ExExNotification::BlockSynced { block_number } => {
                // This ExEx doesn't do anything for Synced blocks from the Full node
                ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                continue;
            }
        };

        // Fetch feed IDs every 500 blocks or on the first iteration
        if block_number.0.saturating_sub(last_fetch_block) >= UPDATE_FEEDS_INTERVAL || feed_ids.is_empty() {
            match get_feed_ids_from_registry(&ctx.starknet).await {
                Ok(new_feed_ids) => {
                    feed_ids = new_feed_ids;
                    last_fetch_block = block_number.0;
                    log::info!("🧩 [#{}] Pragma's ExEx: 📜 Updated feed IDs: {:?}", block_number, feed_ids);
                }
                Err(e) => {
                    log::warn!("🧩 [#{}] Pragma's ExEx: Failed to fetch feed IDs: {:?}", block_number, e);
                    // Don't crash the app and just stop the ExEx here
                    ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                    continue;
                }
            }
        }

        // Skip creating a dispatch transaction if we don't have any feed IDs
        if feed_ids.is_empty() {
            log::warn!("🧩 [#{}] Pragma's ExEx: No feed IDs available, skipping dispatch", block_number);
            ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
            continue;
        }

        match create_dispatch_tx(&ctx.starknet, &feed_ids) {
            Ok(dispatch_tx) => {
                log::info!("🧩 [#{}] Pragma's ExEx: Adding dispatch transaction...", block_number);
                if let Err(e) = ctx.starknet.add_invoke_transaction(dispatch_tx).await {
                    log::error!("🧩 [#{}] Pragma's ExEx: Failed to add dispatch transaction: {:?}", block_number, e);
                }
            }
            Err(e) => {
                log::error!("🧩 [#{}] Pragma's ExEx: Failed to create dispatch transaction: {:?}", block_number, e);
            }
        }

        ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
    }
    Ok(())
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
