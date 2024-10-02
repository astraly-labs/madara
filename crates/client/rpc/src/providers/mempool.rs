use jsonrpsee::core::{async_trait, RpcResult};
use mc_mempool::Mempool;
use mc_mempool::MempoolProvider;
use mp_rpc::errors::StarknetRpcApiError;
use mp_rpc::AddTransactionProvider;
use starknet_core::types::{
    BroadcastedDeclareTransaction, BroadcastedDeployAccountTransaction, BroadcastedInvokeTransaction,
    DeclareTransactionResult, DeployAccountTransactionResult, InvokeTransactionResult,
};
use std::sync::Arc;

/// This [`AddTransactionProvider`] adds the received transactions to a mempool.
pub struct MempoolAddTxProvider {
    mempool: Arc<Mempool>,
}

impl MempoolAddTxProvider {
    pub fn new(mempool: Arc<Mempool>) -> Self {
        Self { mempool }
    }
}

#[async_trait]
impl AddTransactionProvider for MempoolAddTxProvider {
    async fn add_declare_transaction(
        &self,
        declare_transaction: BroadcastedDeclareTransaction,
    ) -> RpcResult<DeclareTransactionResult> {
        Ok(self
            .mempool
            .accept_declare_tx(declare_transaction)
            .map_err(<mc_mempool::Error as Into<StarknetRpcApiError>>::into)?)
    }
    async fn add_deploy_account_transaction(
        &self,
        deploy_account_transaction: BroadcastedDeployAccountTransaction,
    ) -> RpcResult<DeployAccountTransactionResult> {
        Ok(self
            .mempool
            .accept_deploy_account_tx(deploy_account_transaction)
            .map_err(<mc_mempool::Error as Into<StarknetRpcApiError>>::into)?)
    }
    async fn add_invoke_transaction(
        &self,
        invoke_transaction: BroadcastedInvokeTransaction,
    ) -> RpcResult<InvokeTransactionResult> {
        Ok(self
            .mempool
            .accept_invoke_tx(invoke_transaction)
            .map_err(<mc_mempool::Error as Into<StarknetRpcApiError>>::into)?)
    }
}
