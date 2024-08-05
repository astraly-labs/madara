//! Contract history values are stored using a fixed prefix extractor in rocksdb.
//!
//! This means that we can access the last value of a history column (e.g. the last class hash of a contract)
//! from a any block in the blockchain by seeking to it using a rocksdb iterator, setting iteration to reverse mode,
//! and getting the next value.
//!
//! Insertion is batched and done in parallel using rayon: this is not intended for use in the RPCs.
use std::sync::Arc;

use rayon::{iter::ParallelIterator, slice::ParallelSlice};
use rocksdb::{BoundColumnFamily, IteratorMode, ReadOptions, WriteOptions};
use starknet_core::types::Felt;

use crate::{
    db_block_id::{DbBlockId, DbBlockIdResolvable},
    Column, DatabaseExt, DeoxysBackend, DeoxysStorageError, WriteBatchWithTransaction, DB, DB_UPDATES_BATCH_SIZE,
};

// NB: Columns cf needs prefix extractor of these length during creation
pub(crate) const CONTRACT_STORAGE_PREFIX_EXTRACTOR: usize = 64;
pub(crate) const CONTRACT_CLASS_HASH_PREFIX_EXTRACTOR: usize = 32;
pub(crate) const CONTRACT_NONCES_PREFIX_EXTRACTOR: usize = 32;

const LAST_KEY: &[u8] = &[0xFF; 64];

fn make_storage_key_prefix(contract_address: Felt, storage_key: Felt) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(contract_address.to_bytes_be().as_ref());
    key[32..].copy_from_slice(storage_key.to_bytes_be().as_ref());
    key
}

impl DeoxysBackend {
    fn resolve_history_kv<K: serde::Serialize, V: serde::de::DeserializeOwned, B: AsRef<[u8]>>(
        &self,
        id: &impl DbBlockIdResolvable,
        pending_col: Column,
        nonpending_col: Column,
        k: &K,
        make_bin_prefix: impl FnOnce(&K) -> B,
    ) -> Result<Option<V>, DeoxysStorageError> {
        let Some(id) = id.resolve_db_block_id(self)? else { return Ok(None) };

        let block_n = match id {
            DbBlockId::Pending => {
                // Get pending or fallback to latest block_n
                let col = self.db.get_column(pending_col);
                // todo: smallint here to avoid alloc
                if let Some(res) = self.db.get_pinned_cf(&col, bincode::serialize(k)?)? {
                    return Ok(Some(bincode::deserialize(&res)?)); // found in pending
                }

                let Some(block_n) = self.get_latest_block_n()? else { return Ok(None) };
                block_n
            }
            DbBlockId::BlockN(block_n) => block_n,
        };

        // We try to find history values.

        let block_n = u32::try_from(block_n).map_err(|_| DeoxysStorageError::InvalidBlockNumber)?;
        let bin_prefix = make_bin_prefix(k);
        let start_at = [bin_prefix.as_ref(), &block_n.to_be_bytes() as &[u8]].concat();

        let mut options = ReadOptions::default();
        options.set_prefix_same_as_start(true);
        // We don't need ot set an iteration range as we have set up a prefix extractor for the column.
        // We are doing prefix iteration
        // options.set_iterate_range(PrefixRange(&prefix as &[u8]));
        let mode = IteratorMode::From(&start_at, rocksdb::Direction::Reverse);
        // TODO(perf): It is possible to iterate in a pinned way, using raw iter
        let mut iter = self.db.iterator_cf_opt(&self.db.get_column(nonpending_col), options, mode);

        match iter.next() {
            Some(res) => {
                #[allow(unused_variables)]
                let (k, v) = res?;
                #[cfg(debug_assertions)]
                assert!(k.starts_with(bin_prefix.as_ref())); // This should fail if we forgot to set up a prefix iterator for the column.

                Ok(Some(bincode::deserialize(&v)?))
            }
            None => Ok(None),
        }
    }

    pub fn get_contract_class_hash_at(
        &self,
        id: &impl DbBlockIdResolvable,
        contract_addr: &Felt,
    ) -> Result<Option<Felt>, DeoxysStorageError> {
        self.resolve_history_kv(
            id,
            Column::PendingContractToClassHashes,
            Column::ContractToClassHashes,
            contract_addr,
            |k| k.to_bytes_be(),
        )
    }

    pub fn get_contract_nonce_at(
        &self,
        id: &impl DbBlockIdResolvable,
        contract_addr: &Felt,
    ) -> Result<Option<Felt>, DeoxysStorageError> {
        self.resolve_history_kv(id, Column::PendingContractToNonces, Column::ContractToNonces, contract_addr, |k| {
            k.to_bytes_be()
        })
    }

    pub fn get_contract_storage_at(
        &self,
        id: &impl DbBlockIdResolvable,
        contract_addr: &Felt,
        key: &Felt,
    ) -> Result<Option<Felt>, DeoxysStorageError> {
        self.resolve_history_kv(
            id,
            Column::PendingContractStorage,
            Column::ContractStorage,
            &(*contract_addr, *key),
            |(k1, k2)| make_storage_key_prefix(*k1, *k2),
        )
    }

    /// NB: This functions needs to run on the rayon thread pool
    pub(crate) fn contract_db_store_block(
        &self,
        block_number: u64,
        contract_class_updates: &[(Felt, Felt)],
        contract_nonces_updates: &[(Felt, Felt)],
        contract_kv_updates: &[((Felt, Felt), Felt)],
    ) -> Result<(), DeoxysStorageError> {
        let block_number = u32::try_from(block_number).map_err(|_| DeoxysStorageError::InvalidBlockNumber)?;

        let mut writeopts = WriteOptions::new();
        writeopts.disable_wal(true);

        fn write_chunk(
            db: &DB,
            writeopts: &WriteOptions,
            col: &Arc<BoundColumnFamily>,
            block_number: u32,
            chunk: impl IntoIterator<Item = (impl AsRef<[u8]>, Felt)>,
        ) -> Result<(), DeoxysStorageError> {
            let mut batch = WriteBatchWithTransaction::default();
            for (key, value) in chunk {
                // TODO: find a way to avoid this allocation
                let key = [key.as_ref(), &block_number.to_be_bytes() as &[u8]].concat();
                batch.put_cf(col, key, bincode::serialize(&value)?);
            }
            db.write_opt(batch, writeopts)?;
            Ok(())
        }

        contract_class_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractToClassHashes),
            |col, chunk| {
                write_chunk(&self.db, &writeopts, col, block_number, chunk.iter().map(|(k, v)| (k.to_bytes_be(), *v)))
            },
        )?;
        contract_nonces_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractToNonces),
            |col, chunk| {
                write_chunk(&self.db, &writeopts, col, block_number, chunk.iter().map(|(k, v)| (k.to_bytes_be(), *v)))
            },
        )?;
        contract_kv_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractStorage),
            |col, chunk| {
                write_chunk(
                    &self.db,
                    &writeopts,
                    col,
                    block_number,
                    chunk.iter().map(|((k1, k2), v)| {
                        let mut key = [0u8; 64];
                        key[..32].copy_from_slice(k1.to_bytes_be().as_ref());
                        key[32..].copy_from_slice(k2.to_bytes_be().as_ref());
                        (key, *v)
                    }),
                )
            },
        )?;

        Ok(())
    }

    /// NB: This functions needs to run on the rayon thread pool
    pub(crate) fn contract_db_store_pending(
        &self,
        contract_class_updates: &[(Felt, Felt)],
        contract_nonces_updates: &[(Felt, Felt)],
        contract_kv_updates: &[((Felt, Felt), Felt)],
    ) -> Result<(), DeoxysStorageError> {
        let mut writeopts = WriteOptions::new();
        writeopts.disable_wal(true);

        fn write_chunk(
            db: &DB,
            writeopts: &WriteOptions,
            col: &Arc<BoundColumnFamily>,
            chunk: impl IntoIterator<Item = (impl AsRef<[u8]>, Felt)>,
        ) -> Result<(), DeoxysStorageError> {
            let mut batch = WriteBatchWithTransaction::default();
            for (key, value) in chunk {
                // TODO: find a way to avoid this allocation
                batch.put_cf(col, key.as_ref(), bincode::serialize(&value)?);
            }
            db.write_opt(batch, writeopts)?;
            Ok(())
        }

        contract_class_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractToClassHashes),
            |col, chunk| write_chunk(&self.db, &writeopts, col, chunk.iter().map(|(k, v)| (k.to_bytes_be(), *v))),
        )?;
        contract_nonces_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractToNonces),
            |col, chunk| write_chunk(&self.db, &writeopts, col, chunk.iter().map(|(k, v)| (k.to_bytes_be(), *v))),
        )?;
        contract_kv_updates.par_chunks(DB_UPDATES_BATCH_SIZE).try_for_each_init(
            || self.db.get_column(Column::ContractStorage),
            |col, chunk| {
                write_chunk(
                    &self.db,
                    &writeopts,
                    col,
                    chunk.iter().map(|((k1, k2), v)| {
                        let mut key = [0u8; 64];
                        key[..32].copy_from_slice(k1.to_bytes_be().as_ref());
                        key[32..].copy_from_slice(k2.to_bytes_be().as_ref());
                        (key, *v)
                    }),
                )
            },
        )?;

        Ok(())
    }

    pub(crate) fn contract_db_clear_pending(&self) -> Result<(), DeoxysStorageError> {
        let mut writeopts = WriteOptions::new();
        writeopts.disable_wal(true);

        self.db.delete_range_cf_opt(
            &self.db.get_column(Column::PendingContractToNonces),
            &[] as _,
            LAST_KEY,
            &writeopts,
        )?;
        self.db.delete_range_cf_opt(
            &self.db.get_column(Column::PendingContractToClassHashes),
            &[] as _,
            LAST_KEY,
            &writeopts,
        )?;
        self.db.delete_range_cf_opt(
            &self.db.get_column(Column::PendingContractStorage),
            &[] as _,
            LAST_KEY,
            &writeopts,
        )?;

        Ok(())
    }
}
