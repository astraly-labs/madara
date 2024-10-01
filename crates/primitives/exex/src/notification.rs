use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use mp_block::Header;
use starknet_api::block::BlockNumber;
use tokio::sync::mpsc::Receiver;

/// Notifications sent to an `ExEx`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExExNotification {
    /// Chain got committed without a reorg, and only the new chain is returned.
    BlockClosed {
        /// The new chain after commit.
        new: BlockNumber,
    },
}

impl ExExNotification {
    /// Returns the committed chain.
    pub fn closed_block(&self) -> BlockNumber {
        match self {
            Self::BlockClosed { new } => *new,
        }
    }
}

/// A stream of [`ExExNotification`]s. The stream will emit notifications for all blocks.
#[derive(Debug)]
pub struct ExExNotifications {
    #[allow(unused)]
    node_head: Header,
    notifications: Receiver<ExExNotification>,
}

impl ExExNotifications {
    /// Creates a new instance of [`ExExNotifications`].
    pub const fn new(node_head: Header, notifications: Receiver<ExExNotification>) -> Self {
        Self { node_head, notifications }
    }
}

impl Stream for ExExNotifications {
    type Item = ExExNotification;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().notifications.poll_recv(cx)
    }
}
