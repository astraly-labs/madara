pub mod errors;

use std::{fmt, sync::Arc};

use errors::{StarknetRpcApiError, StarknetRpcResult};
use jsonrpsee::core::{async_trait, RpcResult};
use mc_db::{db_block_id::DbBlockIdResolvable, MadaraBackend};
use mp_block::{MadaraMaybePendingBlock, MadaraMaybePendingBlockInfo};
use mp_chain_config::{ChainConfig, RpcVersion};
use mp_convert::ToFelt;
use starknet_core::types::{
    BroadcastedDeclareTransaction, BroadcastedDeployAccountTransaction, BroadcastedInvokeTransaction,
    DeclareTransactionResult, DeployAccountTransactionResult, Felt, InvokeTransactionResult,
};

#[async_trait]
pub trait AddTransactionProvider: Send + Sync {
    async fn add_declare_transaction(
        &self,
        declare_transaction: BroadcastedDeclareTransaction,
    ) -> RpcResult<DeclareTransactionResult>;

    async fn add_deploy_account_transaction(
        &self,
        deploy_account_transaction: BroadcastedDeployAccountTransaction,
    ) -> RpcResult<DeployAccountTransactionResult>;

    async fn add_invoke_transaction(
        &self,
        invoke_transaction: BroadcastedInvokeTransaction,
    ) -> RpcResult<InvokeTransactionResult>;
}

/// A Starknet RPC server for Madara
#[derive(Clone)]
pub struct Starknet {
    pub backend: Arc<MadaraBackend>,
    pub chain_config: Arc<ChainConfig>,
    pub add_transaction_provider: Arc<dyn AddTransactionProvider>,
}

impl Starknet {
    pub fn new(
        backend: Arc<MadaraBackend>,
        chain_config: Arc<ChainConfig>,
        add_transaction_provider: Arc<dyn AddTransactionProvider>,
    ) -> Self {
        Self { backend, add_transaction_provider, chain_config }
    }

    pub fn clone_backend(&self) -> Arc<MadaraBackend> {
        Arc::clone(&self.backend)
    }

    pub fn get_block_info(
        &self,
        block_id: &impl DbBlockIdResolvable,
    ) -> StarknetRpcResult<MadaraMaybePendingBlockInfo> {
        self.backend
            .get_block_info(block_id)
            .or_internal_server_error("Error getting block from storage")?
            .ok_or(StarknetRpcApiError::BlockNotFound)
    }

    pub fn get_block_n(&self, block_id: &impl DbBlockIdResolvable) -> StarknetRpcResult<u64> {
        self.backend
            .get_block_n(block_id)
            .or_internal_server_error("Error getting block from storage")?
            .ok_or(StarknetRpcApiError::BlockNotFound)
    }

    pub fn get_block(&self, block_id: &impl DbBlockIdResolvable) -> StarknetRpcResult<MadaraMaybePendingBlock> {
        self.backend
            .get_block(block_id)
            .or_internal_server_error("Error getting block from storage")?
            .ok_or(StarknetRpcApiError::BlockNotFound)
    }

    pub fn chain_id(&self) -> Felt {
        self.chain_config.chain_id.clone().to_felt()
    }

    pub fn current_block_number(&self) -> StarknetRpcResult<u64> {
        self.get_block_n(&mp_block::BlockId::Tag(mp_block::BlockTag::Latest))
    }

    pub fn current_spec_version(&self) -> RpcVersion {
        RpcVersion::RPC_VERSION_LATEST
    }

    pub fn get_l1_last_confirmed_block(&self) -> StarknetRpcResult<u64> {
        Ok(self
            .backend
            .get_l1_last_confirmed_block()
            .or_internal_server_error("Error getting L1 last confirmed block")?
            .unwrap_or_default())
    }
}

pub fn display_internal_server_error(err: impl fmt::Display) {
    log::error!(target: "rpc_errors", "{:#}", err);
}

#[macro_export]
macro_rules! bail_internal_server_error {
    ($msg:literal $(,)?) => {{
        $crate::utils::display_internal_server_error(anyhow::anyhow!($msg));
        return ::core::result::Result::Err($crate::StarknetRpcApiError::InternalServerError.into())
    }};
    ($err:expr $(,)?) => {
        $crate::utils::display_internal_server_error(anyhow::anyhow!($err));
        return ::core::result::Result::Err($crate::StarknetRpcApiError::InternalServerError.into())
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::utils::display_internal_server_error(anyhow::anyhow!($fmt, $($arg)*));
        return ::core::result::Result::Err($crate::StarknetRpcApiError::InternalServerError.into())
    };
}

pub trait ResultExt<T, E> {
    fn or_internal_server_error<C: fmt::Display>(self, context: C) -> Result<T, StarknetRpcApiError>;
    fn or_else_internal_server_error<C: fmt::Display, F: FnOnce() -> C>(
        self,
        context_fn: F,
    ) -> Result<T, StarknetRpcApiError>;
    fn or_contract_error<C: fmt::Display>(self, context: C) -> Result<T, StarknetRpcApiError>;
}

impl<T, E: Into<anyhow::Error>> ResultExt<T, E> for Result<T, E> {
    #[inline]
    fn or_internal_server_error<C: fmt::Display>(self, context: C) -> Result<T, StarknetRpcApiError> {
        match self {
            Ok(val) => Ok(val),
            Err(err) => {
                display_internal_server_error(format!("{}: {:#}", context, E::into(err)));
                Err(StarknetRpcApiError::InternalServerError)
            }
        }
    }

    #[inline]
    fn or_else_internal_server_error<C: fmt::Display, F: FnOnce() -> C>(
        self,
        context_fn: F,
    ) -> Result<T, StarknetRpcApiError> {
        match self {
            Ok(val) => Ok(val),
            Err(err) => {
                display_internal_server_error(format!("{}: {:#}", context_fn(), E::into(err)));
                Err(StarknetRpcApiError::InternalServerError)
            }
        }
    }

    // TODO: should this be a thing?
    #[inline]
    fn or_contract_error<C: fmt::Display>(self, context: C) -> Result<T, StarknetRpcApiError> {
        match self {
            Ok(val) => Ok(val),
            Err(err) => {
                log::error!(target: "rpc_errors", "Contract storage error: {context}: {:#}", E::into(err));
                Err(StarknetRpcApiError::ContractError)
            }
        }
    }
}
