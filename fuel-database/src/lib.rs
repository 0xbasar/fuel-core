use crate::{
    in_memory::transaction::MemoryTransactionView,
    transactional::DatabaseTransaction,
};
use fuel_core_interfaces::common::error::InterpreterError;
use serde::{
    de::DeserializeOwned,
    Serialize,
};
use std::{
    fmt::{
        self,
        Debug,
        Formatter,
    },
    io::ErrorKind,
    marker::{
        PhantomData,
        Send,
    },
    sync::Arc,
};
use thiserror::Error;

pub mod in_memory;
#[cfg(feature = "rocksdb")]
pub mod rocks_db;
pub mod tables;
pub mod transactional;

/// Database tables column ids.
#[repr(u32)]
#[derive(
    Copy, Clone, Debug, strum_macros::EnumCount, PartialEq, Eq, enum_iterator::Sequence,
)]
pub enum Column {
    /// The column id of metadata about the blockchain
    Metadata = 0,
    /// See [`ContractsRawCode`](fuel_core_interfaces::db::ContractsRawCode)
    ContractsRawCode = 1,
    /// See [`ContractsRawCode`](fuel_core_interfaces::db::ContractsRawCode)
    ContractsInfo = 2,
    /// See [`ContractsState`](fuel_core_interfaces::db::ContractsState)
    ContractsState = 3,
    /// See [`ContractsLatestUtxo`](fuel_core_interfaces::db::ContractsLatestUtxo)
    ContractsLatestUtxo = 4,
    /// See [`ContractsAssets`](fuel_vm::storage::ContractsAssets)
    ContractsAssets = 5,
    /// See [`Coins`](fuel_core_interfaces::db::Coins)
    Coins = 6,
    /// The column of the table that stores `true` if `owner` owns `Coin` with `coin_id`
    OwnedCoins = 7,
    /// See [`Transactions`](fuel_core_interfaces::db::Transactions)
    Transactions = 8,
    /// Transaction id to current status
    TransactionStatus = 9,
    /// The column of the table of all `owner`'s transactions
    TransactionsByOwnerBlockIdx = 10,
    /// See [`Receipts`](fuel_core_interfaces::db::Receipts)
    Receipts = 11,
    /// See [`FuelBlocks`](fuel_core_interfaces::db::FuelBlocks)
    FuelBlocks = 12,
    /// Maps fuel block id to fuel block hash
    FuelBlockIds = 13,
    /// See [`Messages`](fuel_core_interfaces::db::Messages)
    Messages = 14,
    /// The column of the table that stores `true` if `owner` owns `Message` with `message_id`
    OwnedMessageIds = 15,
    /// The column that stores the consensus metadata associated with a finalized fuel block
    FuelBlockConsensus = 16,
}

pub type Result<T> = core::result::Result<T, Error>;
pub type DataSource = Arc<dyn TransactableStorage>;
pub type ColumnId = u32;

#[derive(Clone, Debug, Default)]
pub struct MultiKey<K1: AsRef<[u8]>, K2: AsRef<[u8]>> {
    _marker_1: PhantomData<K1>,
    _marker_2: PhantomData<K2>,
    inner: Vec<u8>,
}

impl<K1: AsRef<[u8]>, K2: AsRef<[u8]>> MultiKey<K1, K2> {
    pub fn new(key: &(K1, K2)) -> Self {
        Self {
            _marker_1: Default::default(),
            _marker_2: Default::default(),
            inner: key
                .0
                .as_ref()
                .iter()
                .chain(key.1.as_ref().iter())
                .copied()
                .collect(),
        }
    }
}

impl<K1: AsRef<[u8]>, K2: AsRef<[u8]>> AsRef<[u8]> for MultiKey<K1, K2> {
    fn as_ref(&self) -> &[u8] {
        self.inner.as_slice()
    }
}

impl<K1: AsRef<[u8]>, K2: AsRef<[u8]>> From<MultiKey<K1, K2>> for Vec<u8> {
    fn from(key: MultiKey<K1, K2>) -> Vec<u8> {
        key.inner
    }
}

pub type KVItem = Result<(Vec<u8>, Vec<u8>)>;

#[derive(Copy, Clone, Debug, PartialOrd, Eq, PartialEq)]
pub enum IterDirection {
    Forward,
    Reverse,
}

impl Default for IterDirection {
    fn default() -> Self {
        Self::Forward
    }
}

pub trait KeyValueStore {
    fn get(&self, key: &[u8], column: Column) -> Result<Option<Vec<u8>>>;
    fn put(&self, key: &[u8], column: Column, value: Vec<u8>) -> Result<Option<Vec<u8>>>;
    fn delete(&self, key: &[u8], column: Column) -> Result<Option<Vec<u8>>>;
    fn exists(&self, key: &[u8], column: Column) -> Result<bool>;
    // TODO: Use `Option<&[u8]>` instead of `Option<Vec<u8>>`. Also decide, do we really need usage
    //  of `Option`? If `len` is zero it is the same as `None`. Apply the same change for all upper
    //  functions.
    //  https://github.com/FuelLabs/fuel-core/issues/622
    fn iter_all(
        &self,
        column: Column,
        prefix: Option<Vec<u8>>,
        start: Option<Vec<u8>>,
        direction: IterDirection,
    ) -> Box<dyn Iterator<Item = KVItem> + '_>;
}

pub trait BatchOperations: KeyValueStore {
    fn batch_write(
        &self,
        entries: &mut dyn Iterator<Item = WriteOperation>,
    ) -> Result<()> {
        for entry in entries {
            match entry {
                // TODO: error handling
                WriteOperation::Insert(key, column, value) => {
                    let _ = self.put(&key, column, value);
                }
                WriteOperation::Remove(key, column) => {
                    let _ = self.delete(&key, column);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum WriteOperation {
    Insert(Vec<u8>, Column, Vec<u8>),
    Remove(Vec<u8>, Column),
}

pub trait Transaction {
    fn transaction<F, R>(&mut self, f: F) -> TransactionResult<R>
    where
        F: FnOnce(&mut MemoryTransactionView) -> TransactionResult<R> + Copy;
}

pub type TransactionResult<T> = core::result::Result<T, TransactionError>;

pub trait TransactableStorage:
    KeyValueStore + BatchOperations + Debug + Send + Sync
{
}

#[derive(Clone, Debug)]
pub enum TransactionError {
    Aborted,
}

#[derive(Clone, Debug)]
pub struct Database {
    data: DataSource,
    // used for RAII
    _drop: Arc<DropResources>,
}

trait DropFnTrait: FnOnce() {}
impl<F> DropFnTrait for F where F: FnOnce() {}
type DropFn = Box<dyn DropFnTrait>;

impl fmt::Debug for DropFn {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "DropFn")
    }
}

#[derive(Debug, Default)]
struct DropResources {
    // move resources into this closure to have them dropped when db drops
    drop: Option<DropFn>,
}

impl<F: 'static + FnOnce()> From<F> for DropResources {
    fn from(closure: F) -> Self {
        Self {
            drop: Option::Some(Box::new(closure)),
        }
    }
}

impl Drop for DropResources {
    fn drop(&mut self) {
        if let Some(drop) = self.drop.take() {
            (drop)()
        }
    }
}

/// * SAFETY: we are safe to do it because DataSource is Send+Sync and there is nowhere it is overwritten
/// it is not Send+Sync by default because Storage insert fn takes &mut self
unsafe impl Send for Database {}
unsafe impl Sync for Database {}

impl Database {
    #[cfg(feature = "rocksdb")]
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let db = crate::rocks_db::RocksDb::default_open(path)?;

        Ok(Database {
            data: Arc::new(db),
            _drop: Default::default(),
        })
    }

    pub fn in_memory() -> Self {
        Self {
            data: Arc::new(in_memory::memory_store::MemoryStore::default()),
            _drop: Default::default(),
        }
    }

    // TODO: Get `K` and `V` by reference to force compilation error for the current
    //  code(we have many `Copy`).
    //  https://github.com/FuelLabs/fuel-core/issues/622
    pub fn insert<K: AsRef<[u8]>, V: Serialize, R: DeserializeOwned>(
        &self,
        key: K,
        column: Column,
        value: V,
    ) -> Result<Option<R>> {
        let result = self.data.put(
            key.as_ref(),
            column,
            bincode::serialize(&value).map_err(|_| Error::Codec)?,
        )?;
        if let Some(previous) = result {
            Ok(Some(
                bincode::deserialize(&previous).map_err(|_| Error::Codec)?,
            ))
        } else {
            Ok(None)
        }
    }

    pub fn remove<V: DeserializeOwned>(
        &self,
        key: &[u8],
        column: Column,
    ) -> Result<Option<V>> {
        self.data
            .delete(key, column)?
            .map(|val| bincode::deserialize(&val).map_err(|_| Error::Codec))
            .transpose()
    }

    pub fn get<V: DeserializeOwned>(
        &self,
        key: &[u8],
        column: Column,
    ) -> Result<Option<V>> {
        self.data
            .get(key, column)?
            .map(|val| bincode::deserialize(&val).map_err(|_| Error::Codec))
            .transpose()
    }

    // TODO: Rename to `contains_key` to be the same as `StorageInspect`
    //  https://github.com/FuelLabs/fuel-core/issues/622
    pub fn exists(&self, key: &[u8], column: Column) -> Result<bool> {
        self.data.exists(key, column)
    }

    pub fn iter_all<K, V>(
        &self,
        column: Column,
        prefix: Option<Vec<u8>>,
        start: Option<Vec<u8>>,
        direction: Option<IterDirection>,
    ) -> impl Iterator<Item = Result<(K, V)>> + '_
    where
        K: From<Vec<u8>>,
        V: DeserializeOwned,
    {
        self.data
            .iter_all(column, prefix, start, direction.unwrap_or_default())
            .map(|val| {
                val.and_then(|(key, value)| {
                    let key = K::from(key);
                    let value: V =
                        bincode::deserialize(&value).map_err(|_| Error::Codec)?;
                    Ok((key, value))
                })
            })
    }

    pub fn transaction(&self) -> DatabaseTransaction {
        self.into()
    }
}

impl AsRef<Database> for Database {
    fn as_ref(&self) -> &Database {
        self
    }
}

/// Construct an ephemeral database
/// uses rocksdb when rocksdb features are enabled
/// uses in-memory when rocksdb features are disabled
impl Default for Database {
    fn default() -> Self {
        #[cfg(not(feature = "rocksdb"))]
        {
            Self {
                data: Arc::new(in_memory::memory_store::MemoryStore::default()),
                _drop: Default::default(),
            }
        }
        #[cfg(feature = "rocksdb")]
        {
            let tmp_dir = tempfile::TempDir::new().unwrap();
            Self {
                data: Arc::new(
                    crate::rocks_db::RocksDb::default_open(tmp_dir.path()).unwrap(),
                ),
                _drop: Arc::new(
                    {
                        move || {
                            // cleanup temp dir
                            drop(tmp_dir);
                        }
                    }
                    .into(),
                ),
            }
        }
    }
}

// TODO: Use either `KvStoreError` or `Error`

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("error performing binary serialization")]
    Codec,
    #[error("Failed to initialize chain")]
    ChainAlreadyInitialized,
    #[error("Chain is not yet initialized")]
    ChainUninitialized,
    #[error("Invalid database version")]
    InvalidDatabaseVersion,
    #[error("error occurred in the underlying datastore `{0}`")]
    DatabaseError(Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
pub enum KvStoreError {
    #[error("generic error occurred")]
    Error(Box<dyn std::error::Error + Send + Sync>),
    /// This error should be created with `not_found` macro.
    #[error("resource of type `{0}` was not found at the: {1}")]
    NotFound(&'static str, &'static str),
}

/// Creates `KvStoreError::NotFound` error with file and line information inside.
///
/// # Examples
///
/// ```
/// use fuel_database::not_found;
/// use fuel_database::tables::Messages;
///
/// let string_type = not_found!("BlockId");
/// let mappable_type = not_found!(Messages);
/// let mappable_path = not_found!(fuel_database::tables::Coins);
/// ```
#[macro_export]
macro_rules! not_found {
    ($name: literal) => {
        $crate::KvStoreError::NotFound($name, concat!(file!(), ":", line!()))
    };
    ($ty: path) => {
        $crate::KvStoreError::NotFound(
            ::core::any::type_name::<<$ty as $crate::tables::Mappable>::GetValue>(),
            concat!(file!(), ":", line!()),
        )
    };
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        std::io::Error::new(ErrorKind::Other, e)
    }
}

impl From<Error> for KvStoreError {
    fn from(e: Error) -> Self {
        KvStoreError::Error(Box::new(e))
    }
}

impl From<KvStoreError> for Error {
    fn from(e: KvStoreError) -> Self {
        Error::DatabaseError(Box::new(e))
    }
}

impl From<KvStoreError> for std::io::Error {
    fn from(e: KvStoreError) -> Self {
        std::io::Error::new(ErrorKind::Other, e)
    }
}

impl From<Error> for InterpreterError {
    fn from(e: Error) -> Self {
        InterpreterError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

impl From<KvStoreError> for InterpreterError {
    fn from(e: KvStoreError) -> Self {
        InterpreterError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

impl From<KvStoreError> for fuel_core_interfaces::executor::Error {
    fn from(e: KvStoreError) -> Self {
        fuel_core_interfaces::executor::Error::CorruptedBlockState(Box::new(e))
    }
}

impl From<Error> for fuel_core_interfaces::executor::Error {
    fn from(e: Error) -> Self {
        fuel_core_interfaces::executor::Error::CorruptedBlockState(Box::new(e))
    }
}

impl From<KvStoreError> for fuel_core_interfaces::executor::TransactionValidityError {
    fn from(e: KvStoreError) -> Self {
        Self::DataStoreError(Box::new(e))
    }
}
