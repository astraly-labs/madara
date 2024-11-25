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
pub fn rpc_api_user(starknet: &Starknet) -> anyhow::Result<RpcModule<()>> {
    let mut rpc_api = RpcModule::new(());

    rpc_api.merge(versions::user::v0_7_1::StarknetReadRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    rpc_api.merge(versions::user::v0_8_0::StarknetReadRpcApiV0_8_0Server::into_rpc(starknet.clone()))?;
    rpc_api.merge(versions::user::v0_7_1::StarknetWriteRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    rpc_api.merge(versions::user::v0_7_1::StarknetTraceRpcApiV0_7_1Server::into_rpc(starknet.clone()))?;
    rpc_api.merge(versions::user::v0_8_0::StarknetWsRpcApiV0_8_0Server::into_rpc(starknet.clone()))?;

    Ok(rpc_api)
}

pub fn rpc_api_admin(starknet: &Starknet) -> anyhow::Result<RpcModule<()>> {
    let mut rpc_api = RpcModule::new(());

    rpc_api.merge(versions::admin::v0_1_0::MadaraWriteRpcApiV0_1_0Server::into_rpc(starknet.clone()))?;

    Ok(rpc_api)
}
