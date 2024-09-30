//! ExEx of Pragma Dispatcher
//! Adds a new TX at the end of each block, dispatching a message through
//! Hyperlane.

use futures::StreamExt;
use mp_exex::{ExExContext, ExExEvent};
use starknet_api::block::BlockNumber;

pub async fn exex_pragma_dispatch(mut ctx: ExExContext) -> anyhow::Result<()> {
    while let Some(_notification) = ctx.notifications.next().await {
        log::info!("👋 Hello from the ExEx");
        ctx.events.send(ExExEvent::FinishedHeight(BlockNumber(0)))?;
    }
    Ok(())
}
