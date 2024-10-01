//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.

use futures::StreamExt;
use mp_exex::{ExExContext, ExExEvent, ExExNotification};

pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    while let Some(notification) = ctx.notifications.next().await {
        let block_number = match notification {
            ExExNotification::BlockProduced { new } => new,
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
