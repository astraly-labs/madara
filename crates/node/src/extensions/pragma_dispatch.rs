//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.

use futures::StreamExt;
use mp_block::MadaraPendingBlock;
use mp_exex::{ExExContext, ExExEvent, ExExNotification};
use starknet_api::{block::BlockNumber, felt};
use starknet_core::types::Felt;

lazy_static::lazy_static! {
    pub static ref PRAGMA_DISPATCHER_ADDRESS: Felt = felt!("0x2a85bd616f912537c50a49a4076db02c00b29b2cdc8a197ce92ed1837fa875b");
}

pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    while let Some(notification) = ctx.notifications.next().await {
        let (_block, block_number): (Box<MadaraPendingBlock>, BlockNumber) = match notification {
            ExExNotification::BlockProduced { block, block_number } => (block, block_number),
            ExExNotification::BlockSynced { new } => {
                // This ExEx doesn't do anything for Synced blocks from the Full node
                ctx.events.send(ExExEvent::FinishedHeight(new))?;
                return Ok(());
            }
        };

        log::info!("👋 Hello from the ExEx (triggered at block #{})", block_number);
        ctx.events.send(ExExEvent::FinishedHeight(block_number))?;
    }
    Ok(())
}
