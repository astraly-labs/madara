//! Starknet RPC server API implementation
//!
//! It uses the madara client and backend in order to answer queries.

mod constants;
pub mod providers;
#[cfg(test)]
pub mod test_utils;
mod types;
pub mod utils;
pub mod versions;

use jsonrpsee::RpcModule;

use mp_rpc::Starknet;

/// Returns the RpcModule merged with all the supported RPC versions.
pub fn versioned_rpc_api(
    starknet: &Starknet,
    read: bool,
    write: bool,
    trace: bool,
    internal: bool,
    ws: bool,
) -> anyhow::Result<RpcModule<()>> {
    let mut rpc_api = RpcModule::new(());

    if read {
        rpc_api.merge(versions::v0_7_1::StarknetReadRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    }
    if write {
        rpc_api.merge(versions::v0_7_1::StarknetWriteRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    }
    if trace {
        rpc_api.merge(versions::v0_7_1::StarknetTraceRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    }
    if internal {
        rpc_api.merge(versions::v0_7_1::MadaraWriteRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    }
    if ws {
        // V0.8.0 ...
    }

    Ok(rpc_api)
}
