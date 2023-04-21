use crate::types::HeaderView;
use ckb_async_runtime::Handle;
use ckb_types::packed::{Byte32, Byte32Reader};
use ckb_types::prelude::{Entity, FromSliceShouldBeOk, Reader};
use heed::{BoxedError, BytesDecode, BytesEncode, Database, Env, EnvOpenOptions, Flags};
use std::{borrow::Cow, path};

pub struct HeaderMap {
    env: Env,
    db: Database<HeaderMapKey, HeaderMapValue>,
    _tmpdir: tempfile::TempDir,
}

pub struct HeaderMapKey;

pub struct HeaderMapValue;

impl BytesEncode<'_> for HeaderMapKey {
    type EItem = Byte32;

    fn bytes_encode(item: &Self::EItem) -> Result<Cow<[u8]>, BoxedError> {
        Ok(Cow::from(item.as_slice()))
    }
}

impl<'a> BytesDecode<'a> for HeaderMapKey {
    type DItem = Byte32;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, BoxedError> {
        Ok(Byte32Reader::from_slice_should_be_ok(bytes).to_entity())
    }
}

impl BytesEncode<'_> for HeaderMapValue {
    type EItem = HeaderView;

    fn bytes_encode(item: &Self::EItem) -> Result<Cow<[u8]>, BoxedError> {
        Ok(Cow::from(item.to_vec()))
    }
}

impl<'a> BytesDecode<'a> for HeaderMapValue {
    type DItem = HeaderView;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, BoxedError> {
        Ok(HeaderView::from_slice_should_be_ok(bytes))
    }
}

impl HeaderMap {
    pub(crate) fn new<P>(tmpdir: Option<P>, _memory_limit: usize, _async_handle: &Handle) -> Self
    where
        P: AsRef<path::Path>,
    {
        let mut builder = tempfile::Builder::new();
        builder.prefix("ckb-tmp-");
        let tmpdir = if let Some(ref path) = tmpdir {
            builder.tempdir_in(path)
        } else {
            builder.tempdir()
        }
        .expect("failed to create a tempdir to save header map into disk");

        let mut env_builder = EnvOpenOptions::new();
        // 6GB, around 18,000,000 headers
        // TODO: make this configurable or increase it dynamically
        env_builder.map_size(6 * 1024 * 1024 * 1024);
        env_builder.max_dbs(1);
        // setup flags for better write performance
        unsafe {
            env_builder.flag(Flags::MdbNoSync);
            env_builder.flag(Flags::MdbNoMetaSync);
            env_builder.flag(Flags::MdbMapAsync);
            env_builder.flag(Flags::MdbWriteMap);
        }
        let env = env_builder
            .open(&tmpdir)
            .expect("failed to open lmdb database to save header map into disk");

        let mut wtxn = env.write_txn().expect("failed to create write transaction");
        let db: Database<HeaderMapKey, HeaderMapValue> = env
            .create_database(&mut wtxn, Some("HeaderMap"))
            .expect("failed to create header map database");
        wtxn.commit().expect("failed to commit write transaction");

        Self {
            env,
            db,
            _tmpdir: tmpdir,
        }
    }

    pub(crate) fn contains_key(&self, hash: &Byte32) -> bool {
        let txn = self
            .env
            .read_txn()
            .expect("failed to create read transaction");
        self.db
            .get(&txn, hash)
            .expect("failed to get header from header map")
            .is_some()
    }

    pub(crate) fn get(&self, hash: &Byte32) -> Option<HeaderView> {
        let txn = self
            .env
            .read_txn()
            .expect("failed to create read transaction");
        self.db
            .get(&txn, hash)
            .expect("failed to get header from header map")
    }

    pub(crate) fn insert(&self, view: HeaderView) {
        let mut txn = self
            .env
            .write_txn()
            .expect("failed to create write transaction");
        self.db
            .put(&mut txn, &view.hash(), &view)
            .expect("failed to insert header into header map");
        txn.commit().expect("failed to commit write transaction");
    }

    pub(crate) fn remove(&self, hash: &Byte32) {
        let mut txn = self
            .env
            .write_txn()
            .expect("failed to create write transaction");
        self.db
            .delete(&mut txn, hash)
            .expect("failed to remove header from header map");
        txn.commit().expect("failed to commit write transaction");
    }
}
