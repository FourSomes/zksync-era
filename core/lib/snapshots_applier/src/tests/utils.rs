//! Test utils.

use std::{collections::HashMap, fmt, sync::Arc};

use async_trait::async_trait;
use zksync_object_store::{Bucket, ObjectStore, ObjectStoreError, ObjectStoreFactory};
use zksync_types::{
    api::en::SyncBlock,
    block::L1BatchHeader,
    commitment::{L1BatchMetaParameters, L1BatchMetadata, L1BatchWithMetadata},
    snapshots::{
        SnapshotFactoryDependencies, SnapshotFactoryDependency, SnapshotHeader,
        SnapshotRecoveryStatus, SnapshotStorageLog, SnapshotStorageLogsChunk,
        SnapshotStorageLogsChunkMetadata, SnapshotStorageLogsStorageKey,
    },
    AccountTreeId, Bytes, L1BatchNumber, MiniblockNumber, ProtocolVersionId, StorageKey,
    StorageValue, H160, H256,
};
use zksync_web3_decl::jsonrpsee::core::ClientError as RpcError;

use crate::SnapshotsApplierMainNodeClient;

#[derive(Debug, Default)]
pub(super) struct MockMainNodeClient {
    pub fetch_l2_block_responses: HashMap<MiniblockNumber, SyncBlock>,
    pub fetch_newest_snapshot_response: Option<SnapshotHeader>,
}

#[async_trait]
impl SnapshotsApplierMainNodeClient for MockMainNodeClient {
    async fn fetch_l2_block(&self, number: MiniblockNumber) -> Result<Option<SyncBlock>, RpcError> {
        Ok(self.fetch_l2_block_responses.get(&number).cloned())
    }

    async fn fetch_newest_snapshot(&self) -> Result<Option<SnapshotHeader>, RpcError> {
        Ok(self.fetch_newest_snapshot_response.clone())
    }
}

type ValidateFn = dyn Fn(&str) -> Result<(), ObjectStoreError> + Send + Sync;

pub(super) struct ObjectStoreWithErrors {
    inner: Arc<dyn ObjectStore>,
    validate_fn: Box<ValidateFn>,
}

impl fmt::Debug for ObjectStoreWithErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.as_ref().fmt(formatter)
    }
}

impl ObjectStoreWithErrors {
    pub fn new(
        inner: Arc<dyn ObjectStore>,
        validate_fn: impl Fn(&str) -> Result<(), ObjectStoreError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            validate_fn: Box::new(validate_fn),
        }
    }
}

#[async_trait]
impl ObjectStore for ObjectStoreWithErrors {
    async fn get_raw(&self, bucket: Bucket, key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        (self.validate_fn)(key)?;
        self.inner.get_raw(bucket, key).await
    }

    async fn put_raw(
        &self,
        _bucket: Bucket,
        _key: &str,
        _value: Vec<u8>,
    ) -> Result<(), ObjectStoreError> {
        unreachable!("Should not be used in snapshot applier")
    }

    async fn remove_raw(&self, _bucket: Bucket, _key: &str) -> Result<(), ObjectStoreError> {
        unreachable!("Should not be used in snapshot applier")
    }

    fn storage_prefix_raw(&self, bucket: Bucket) -> String {
        self.inner.storage_prefix_raw(bucket)
    }
}

fn miniblock_metadata(
    number: MiniblockNumber,
    l1_batch_number: L1BatchNumber,
    hash: H256,
) -> SyncBlock {
    SyncBlock {
        number,
        l1_batch_number,
        last_in_batch: true,
        timestamp: 0,
        l1_gas_price: 0,
        l2_fair_gas_price: 0,
        fair_pubdata_price: None,
        base_system_contracts_hashes: Default::default(),
        operator_address: Default::default(),
        transactions: None,
        virtual_blocks: None,
        hash: Some(hash),
        protocol_version: Default::default(),
    }
}

fn l1_block_metadata(l1_batch_number: L1BatchNumber, root_hash: H256) -> L1BatchWithMetadata {
    L1BatchWithMetadata {
        header: L1BatchHeader::new(
            l1_batch_number,
            0,
            Default::default(),
            ProtocolVersionId::default(),
        ),
        metadata: L1BatchMetadata {
            root_hash,
            rollup_last_leaf_index: 0,
            merkle_root_hash: H256::zero(),
            initial_writes_compressed: Some(vec![]),
            repeated_writes_compressed: Some(vec![]),
            commitment: H256::zero(),
            l2_l1_merkle_root: H256::zero(),
            block_meta_params: L1BatchMetaParameters {
                zkporter_is_available: false,
                bootloader_code_hash: H256::zero(),
                default_aa_code_hash: H256::zero(),
            },
            aux_data_hash: H256::zero(),
            meta_parameters_hash: H256::zero(),
            pass_through_data_hash: H256::zero(),
            events_queue_commitment: None,
            bootloader_initial_content_commitment: None,
            state_diffs_compressed: vec![],
        },
        raw_published_factory_deps: vec![],
    }
}

fn random_storage_logs(
    l1_batch_number: L1BatchNumber,
    chunk_id: u64,
    logs_per_chunk: u64,
) -> Vec<SnapshotStorageLog> {
    (0..logs_per_chunk)
        .map(|x| SnapshotStorageLog {
            key: StorageKey::new(
                AccountTreeId::from_fixed_bytes(H160::random().to_fixed_bytes()),
                H256::random(),
            ),
            value: StorageValue::random(),
            l1_batch_number_of_initial_write: l1_batch_number,
            enumeration_index: x + chunk_id * logs_per_chunk,
        })
        .collect()
}

pub(super) fn mock_recovery_status() -> SnapshotRecoveryStatus {
    SnapshotRecoveryStatus {
        l1_batch_number: L1BatchNumber(123),
        l1_batch_root_hash: H256::random(),
        l1_batch_timestamp: 0,
        miniblock_number: MiniblockNumber(321),
        miniblock_hash: H256::random(),
        miniblock_timestamp: 0,
        protocol_version: ProtocolVersionId::default(),
        storage_logs_chunks_processed: vec![true, true],
    }
}

pub(super) async fn prepare_clients(
    status: &SnapshotRecoveryStatus,
) -> (
    Arc<dyn ObjectStore>,
    MockMainNodeClient,
    HashMap<H256, SnapshotStorageLog>,
) {
    let object_store_factory = ObjectStoreFactory::mock();
    let object_store = object_store_factory.create_store().await;
    let mut client = MockMainNodeClient::default();
    let factory_dep_bytes: Vec<u8> = (0..32).collect();
    let factory_deps = SnapshotFactoryDependencies {
        factory_deps: vec![SnapshotFactoryDependency {
            bytecode: Bytes::from(factory_dep_bytes),
        }],
    };
    object_store
        .put(status.l1_batch_number, &factory_deps)
        .await
        .unwrap();

    let mut all_snapshot_storage_logs = HashMap::<H256, SnapshotStorageLog>::new();
    for chunk_id in 0..status.storage_logs_chunks_processed.len() as u64 {
        let chunk_storage_logs = SnapshotStorageLogsChunk {
            storage_logs: random_storage_logs(status.l1_batch_number, chunk_id, 10),
        };
        let chunk_key = SnapshotStorageLogsStorageKey {
            l1_batch_number: status.l1_batch_number,
            chunk_id,
        };
        object_store
            .put(chunk_key, &chunk_storage_logs)
            .await
            .unwrap();

        all_snapshot_storage_logs.extend(
            chunk_storage_logs
                .storage_logs
                .into_iter()
                .map(|log| (log.key.hashed_key(), log)),
        );
    }

    let snapshot_header = SnapshotHeader {
        l1_batch_number: status.l1_batch_number,
        miniblock_number: status.miniblock_number,
        last_l1_batch_with_metadata: l1_block_metadata(
            status.l1_batch_number,
            status.l1_batch_root_hash,
        ),
        storage_logs_chunks: vec![
            SnapshotStorageLogsChunkMetadata {
                chunk_id: 0,
                filepath: "file0".to_string(),
            },
            SnapshotStorageLogsChunkMetadata {
                chunk_id: 1,
                filepath: "file1".to_string(),
            },
        ],
        factory_deps_filepath: "some_filepath".to_string(),
    };
    client.fetch_newest_snapshot_response = Some(snapshot_header);
    client.fetch_l2_block_responses.insert(
        status.miniblock_number,
        miniblock_metadata(
            status.miniblock_number,
            status.l1_batch_number,
            status.miniblock_hash,
        ),
    );
    (object_store, client, all_snapshot_storage_logs)
}
