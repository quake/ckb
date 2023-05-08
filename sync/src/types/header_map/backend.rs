use std::path;

use ckb_types::packed::Byte32;

use crate::types::HeaderIndexView;

pub(crate) trait KeyValueBackend {
    fn new<P>(tmpdir: Option<P>) -> Self
    where
        P: AsRef<path::Path>;

    fn is_empty(&self) -> bool;
    fn update_empty_flag(&self);

    fn contains_key(&self, key: &Byte32) -> bool;
    fn get(&self, key: &Byte32) -> Option<HeaderIndexView>;
    fn insert(&self, values: &[HeaderIndexView]);
    fn remove(&self, key: &Byte32);
}
