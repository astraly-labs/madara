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

lazy_static::lazy_static! {
    // TODO: Keystore path?
    pub static ref ACCOUNT_ADDRESS: Felt = felt!("0x4a2b383d808b7285cc98b2309f974f5111633c84fd82c9375c118485d2d57ba");
    pub static ref PRIVATE_KEY: SigningKey = SigningKey::from_secret_scalar(felt!("0x7a9779748888c95d96bbbce041b5109c6ffc0c4f30561c0170384a5922d9e91"));

    // TODO: Replace by the correct addresses
    pub static ref PRAGMA_FEEDS_REGISTRY_ADDRESS: Felt = felt!("0x2a85bd616f912537c50a49a4076db02c00b29b2cdc8a197ce92ed1837fa875b");
    pub static ref PRAGMA_DISPATCHER_ADDRESS: Felt = felt!("0x2a85bd616f912537c50a49a4076db02c00b29b2cdc8a197ce92ed1837fa875b");

    pub static ref MAX_FEE: Felt = felt!("2386F26FC10000"); // 0.01 eth
}

/// 🧩 Pragma main ExEx.
/// At the end of each produced block by the node, adds a new dispatch transaction
/// using the Pragma Dispatcher contract.
pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    let feed_ids = get_feed_ids_from_registry(&ctx.starknet).await?;

    while let Some(notification) = ctx.notifications.next().await {
        let block_number = match notification {
            ExExNotification::BlockProduced { block: _, block_number } => block_number,
            ExExNotification::BlockSynced { block_number } => {
                // This ExEx doesn't do anything for Synced blocks from the Full node
                ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
                continue;
            }
        };

        let dispatch_tx = create_dispatch_tx(&ctx.starknet, &feed_ids)?;
        log::info!("🧩 [#{}] Pragma's ExEx: Adding dispatch transaction...", block_number);
        ctx.starknet.add_invoke_transaction(dispatch_tx).await?;

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
