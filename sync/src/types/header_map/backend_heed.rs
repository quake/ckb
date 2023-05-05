use super::{HeaderIndexViewInner, KeyValueBackend};
use crate::types::HeaderIndexView;
use ckb_types::packed::Byte32Reader;
use ckb_types::{packed::Byte32, prelude::*};
use heed::{BoxedError, BytesDecode, BytesEncode, Database, Env, EnvOpenOptions, Flags};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{borrow::Cow, path};
use tempfile::TempDir;

pub(crate) struct HeedBackend {
    env: Env,
    db: Database<HeaderMapKey, HeaderMapValue>,
    empty_flag: AtomicBool,
    _tmpdir: TempDir,
}

struct HeaderMapKey;

struct HeaderMapValue;

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
    type EItem = HeaderIndexViewInner;

    fn bytes_encode(item: &Self::EItem) -> Result<Cow<[u8]>, BoxedError> {
        Ok(Cow::from(item.to_vec()))
    }
}

impl<'a> BytesDecode<'a> for HeaderMapValue {
    type DItem = HeaderIndexViewInner;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, BoxedError> {
        Ok(HeaderIndexViewInner::from_slice_should_be_ok(bytes))
    }
}

impl KeyValueBackend for HeedBackend {
    fn new<P>(tmp_path: Option<P>) -> Self
    where
        P: AsRef<path::Path>,
    {
        let mut builder = tempfile::Builder::new();
        builder.prefix("ckb-tmp-");
        let tmpdir = if let Some(ref path) = tmp_path {
            builder.tempdir_in(path)
        } else {
            builder.tempdir()
        }
        .expect("failed to create a tempdir to save header map into disk");

        let mut env_builder = EnvOpenOptions::new();
        // 3GB, around 20,000,000 headers
        // TODO: make this configurable or increase it dynamically
        env_builder.map_size(3 * 1024 * 1024 * 1024);
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
            empty_flag: AtomicBool::new(true),
            _tmpdir: tmpdir,
        }
    }

    fn is_empty(&self) -> bool {
        self.empty_flag.load(Ordering::SeqCst)
    }

    fn contains_key(&self, key: &Byte32) -> bool {
        let txn = self
            .env
            .read_txn()
            .expect("failed to create read transaction");
        self.db
            .get(&txn, key)
            .expect("failed to get header from disk headermap")
            .is_some()
    }

    fn get(&self, key: &Byte32) -> Option<HeaderIndexView> {
        let txn = self
            .env
            .read_txn()
            .expect("failed to create read transaction");
        self.db
            .get(&txn, key)
            .expect("failed to get header from disk headermap")
            .map(|inner| (key.clone(), inner).into())
    }

    fn insert(&self, values: &[HeaderIndexView]) {
        let mut txn = self
            .env
            .write_txn()
            .expect("failed to create write transaction");
        for value in values {
            let (hash, inner): (Byte32, HeaderIndexViewInner) = value.clone().into();
            self.db
                .put(&mut txn, &hash, &inner)
                .expect("failed to insert header into header map");
        }
        txn.commit().expect("failed to commit write transaction");
        self.empty_flag.store(false, Ordering::SeqCst);
    }

    fn remove(&self, key: &Byte32) {
        let mut txn = self
            .env
            .write_txn()
            .expect("failed to create write transaction");
        self.db
            .delete(&mut txn, key)
            .expect("failed to remove header from disk headermap");
        if self
            .db
            .is_empty(&txn)
            .expect("failed to check if db is empty")
        {
            self.empty_flag.store(true, Ordering::SeqCst);
        }
        txn.commit().expect("failed to commit write transaction");
    }
}
