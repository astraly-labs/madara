use dp_block::{BlockId, BlockTag};
use starknet_core::types::BlockHashAndNumber;

use crate::errors::StarknetRpcResult;
use crate::{utils::OptionExt, Starknet};

/// Get the Most Recent Accepted Block Hash and Number
///
/// ### Arguments
///
/// This function does not take any arguments.
///
/// ### Returns
///
/// * `block_hash_and_number` - A tuple containing the latest block hash and number of the current
///   network.
pub fn block_hash_and_number(starknet: &Starknet) -> StarknetRpcResult<BlockHashAndNumber> {
    let block_info = starknet.get_block_info(&BlockId::Tag(BlockTag::Latest))?;
    let block_info = block_info.as_nonpending().ok_or_internal_server_error("Latest block is pending")?;

    Ok(BlockHashAndNumber { block_hash: block_info.block_hash, block_number: block_info.header.block_number })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dc_db::{block_db::ChainInfo, DeoxysBackend};
    use dp_block::{DeoxysBlockInfo, DeoxysBlockInner, DeoxysMaybePendingBlock, DeoxysMaybePendingBlockInfo, Header};
    use dp_state_update::StateDiff;
    use rstest::rstest;
    use starknet_core::types::Felt;
    use std::sync::Arc;

    #[rstest]
    fn test_block_hash_and_number() {
        let chain_id = Felt::from_bytes_be_slice(b"MADARA_TEST");
        let backend =
            DeoxysBackend::open_for_testing(Arc::new(ChainInfo { chain_id, chain_name: "integration test".into() }));
        let starknet = Starknet::new(
            Arc::clone(&backend),
            0,
            crate::ChainConfig {
                chain_id,
                feeder_gateway: "http://example.com".parse().unwrap(),
                gateway: "http://example.com".parse().unwrap(),
            },
        );

        backend
            .store_block(
                DeoxysMaybePendingBlock {
                    info: DeoxysMaybePendingBlockInfo::NotPending(DeoxysBlockInfo {
                        header: Header { parent_block_hash: Felt::ZERO, block_number: 0, ..Default::default() },
                        block_hash: Felt::ONE,
                        tx_hashes: vec![],
                    }),
                    inner: DeoxysBlockInner { transactions: vec![], receipts: vec![] },
                },
                StateDiff::default(),
                vec![],
            )
            .unwrap();

        assert_eq!(
            block_hash_and_number(&starknet).unwrap(),
            BlockHashAndNumber { block_hash: Felt::ONE, block_number: 0 }
        );

        backend
            .store_block(
                DeoxysMaybePendingBlock {
                    info: DeoxysMaybePendingBlockInfo::NotPending(DeoxysBlockInfo {
                        header: Header { parent_block_hash: Felt::ONE, block_number: 1, ..Default::default() },
                        block_hash: Felt::from_hex_unchecked("0x12345"),
                        tx_hashes: vec![],
                    }),
                    inner: DeoxysBlockInner { transactions: vec![], receipts: vec![] },
                },
                StateDiff::default(),
                vec![],
            )
            .unwrap();

        assert_eq!(
            block_hash_and_number(&starknet).unwrap(),
            BlockHashAndNumber { block_hash: Felt::from_hex_unchecked("0x12345"), block_number: 1 }
        );
    }
}
