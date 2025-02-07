use std::{cell::Cell, fmt::Formatter, path::Path, string::FromUtf8Error};

use bytes::{BufMut, BytesMut};
use rocksdb::{
    Direction, Error as RocksDBError, IteratorMode, Options as RocksDBOptions, ReadOptions, DB,
};

use crate::codec::*;

/// Database instance.
pub struct Database {
    pub path: String,
    pub rocksdb: DB,
    pub options: Options,
    next_key_id: Cell<u64>,
}

unsafe impl Send for Database {}

unsafe impl Sync for Database {}

/// Options for open a database.
pub struct Options {
    /// RocksDB options.
    pub rocksdb_options: RocksDBOptions,
    /// For `sorted list` data type, run RocksDB `compact` operation when every specific deletes count.
    /// This is a performance optimization strategy.
    pub sorted_list_compact_deletes_count: u32,
    /// Auto delete the key meta when items count is 0, the key ID will be different for the next time when reuse the same key.
    pub delete_meta_when_empty: bool,
}

impl Default for Options {
    fn default() -> Self {
        let mut rocksdb_options = RocksDBOptions::default();
        rocksdb_options.create_if_missing(true);
        Options {
            rocksdb_options,
            sorted_list_compact_deletes_count: 300,
            delete_meta_when_empty: true,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub enum Error {
    FromUtf8(FromUtf8Error),
    RocksDB(RocksDBError),
    Message(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::FromUtf8(err) => write!(f, "FromUtf8Error: {}", err),
            Error::RocksDB(err) => write!(f, "RocksDBError: {}", err),
            Error::Message(err) => write!(f, "Error: {}", err),
        }
    }
}

impl std::error::Error for Error {}

impl From<FromUtf8Error> for Error {
    fn from(e: FromUtf8Error) -> Self {
        Error::FromUtf8(e)
    }
}

impl From<RocksDBError> for Error {
    fn from(e: RocksDBError) -> Self {
        Error::RocksDB(e)
    }
}

impl Database {
    /// Open database with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Database> {
        Database::open_with_options(path, Options::default())
    }

    /// Open database with specific options.
    pub fn open_with_options(path: impl AsRef<Path>, options: Options) -> Result<Database> {
        let path = path.as_ref();
        let db = DB::open(&options.rocksdb_options, path)?;
        let mut db = Database {
            path: path.display().to_string(),
            rocksdb: db,
            options,
            next_key_id: Cell::new(1),
        };
        db.after_open()?;
        Ok(db)
    }

    /// Destroy database.
    pub fn destroy(path: impl AsRef<Path>) -> Result<()> {
        Ok(DB::destroy(&RocksDBOptions::default(), path)?)
    }

    fn after_open(&mut self) -> Result<()> {
        let mut last_key_id: u64 = 0;
        self.for_each_key(|_, m| {
            last_key_id = m.id;
            true
        })?;
        self.next_key_id.set(last_key_id + 1);
        Ok(())
    }

    fn prefix_iterator<F>(&self, prefix: &[u8], mut f: F)
    where
        F: FnMut(Box<[u8]>, Box<[u8]>) -> bool,
    {
        let iter = self
            .rocksdb
            .iterator(IteratorMode::From(prefix, Direction::Forward));
        for (k, v) in iter {
            if !has_prefix(prefix, k.as_ref()) {
                break;
            }
            if !f(k, v) {
                break;
            }
        }
    }

    pub fn save_meta(
        &self,
        key: impl AsRef<[u8]>,
        meta: &KeyMeta,
        delete_if_empty: bool,
    ) -> Result<()> {
        if self.options.delete_meta_when_empty && delete_if_empty && meta.count < 1 {
            Ok(self.rocksdb.delete(encode_meta_key(key))?)
        } else {
            Ok(self.rocksdb.put(encode_meta_key(key), meta.get_bytes())?)
        }
    }

    pub fn get_meta(&self, key: impl AsRef<[u8]>) -> Result<Option<KeyMeta>> {
        Ok(self
            .rocksdb
            .get(encode_meta_key(key))
            .map(|v| v.map(|v| KeyMeta::from_bytes(v.as_slice())))?)
    }

    pub fn get_or_create_meta(&self, key: impl AsRef<[u8]>, key_type: KeyType) -> Result<KeyMeta> {
        let key = key.as_ref();
        let m = self.get_meta(key)?;
        match m {
            Some(m) => Ok(m),
            None => {
                let m = KeyMeta::new(self.next_key_id.get(), key_type);
                self.next_key_id.set(self.next_key_id.get() + 1);
                self.save_meta(key, &m, false)?;
                Ok(m)
            }
        }
    }

    pub fn for_each_key<F>(&self, mut f: F) -> Result<usize>
    where
        F: FnMut(&str, &KeyMeta) -> bool,
    {
        let mut counter: usize = 0;
        let mut has_error = None;
        self.prefix_iterator(PREFIX_META, |k, v| {
            counter += 1;
            match decode_meta_key(k.as_ref()) {
                Ok(key) => f(key.as_str(), &KeyMeta::from_bytes(v.as_ref())),
                Err(err) => {
                    has_error = Some(err);
                    false
                }
            }
        });
        match has_error {
            None => Ok(counter),
            Some(err) => Err(err.into()),
        }
    }

    pub fn for_each_key_with_limit<F>(&self, limit: usize, mut f: F) -> Result<usize>
    where
        F: FnMut(&str, &KeyMeta) -> bool,
    {
        let mut counter: usize = 0;
        let mut has_error = None;
        self.prefix_iterator(PREFIX_META, |k, v| {
            counter += 1;
            if counter > limit {
                false
            } else {
                match decode_meta_key(k.as_ref()) {
                    Ok(key) => f(key.as_str(), &KeyMeta::from_bytes(v.as_ref())),
                    Err(err) => {
                        has_error = Some(err);
                        false
                    }
                }
            }
        });
        match has_error {
            None => Ok(counter),
            Some(err) => Err(err.into()),
        }
    }

    pub fn for_each_key_with_prefix<F>(&self, prefix: &str, mut f: F) -> Result<usize>
    where
        F: FnMut(&str, &KeyMeta) -> bool,
    {
        let mut counter: usize = 0;
        let mut has_error = None;
        let k = {
            let p = prefix.as_bytes();
            let mut buf = BytesMut::with_capacity(PREFIX_META.len() + p.len());
            buf.put_slice(PREFIX_META);
            buf.put_slice(p);
            buf
        };
        self.prefix_iterator(k.as_ref(), |k, v| {
            counter += 1;
            match decode_meta_key(k.as_ref()) {
                Ok(key) => f(key.as_str(), &KeyMeta::from_bytes(v.as_ref())),
                Err(err) => {
                    has_error = Some(err);
                    false
                }
            }
        });
        match has_error {
            None => Ok(counter),
            Some(err) => Err(err.into()),
        }
    }

    pub fn keys(&self) -> Result<Vec<(String, KeyMeta)>> {
        let mut vec = Vec::new();
        self.for_each_key(|k, meta| {
            vec.push((k.to_string(), meta.clone()));
            true
        })?;
        Ok(vec)
    }

    pub fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<(String, KeyMeta)>> {
        let mut vec = Vec::new();
        self.for_each_key_with_prefix(prefix, |k, meta| {
            vec.push((k.to_string(), meta.clone()));
            true
        })?;
        Ok(vec)
    }

    pub fn for_each_data<F>(&self, key: &str, prefix: Option<&str>, mut f: F) -> Result<u64>
    where
        F: FnMut(Box<[u8]>, Box<[u8]>) -> bool,
    {
        let meta = self.get_meta(key)?;
        match meta {
            Some(meta) => {
                if meta.count > 0 {
                    let mut counter = 0;
                    let k = match meta.key_type {
                        KeyType::SortedSet => encode_data_key_sorted_set_prefix(meta.id),
                        _ => encode_data_key(meta.id),
                    };
                    let k = match prefix {
                        None => k,
                        Some(prefix) => {
                            let p = prefix.as_bytes();
                            let mut buf = BytesMut::with_capacity(k.len() + p.len());
                            buf.put_slice(k.as_ref());
                            buf.put_slice(p);
                            buf
                        }
                    };
                    self.prefix_iterator(k.as_ref(), |k, v| {
                        counter += 1;
                        f(k, v)
                    });
                    Ok(counter)
                } else {
                    Ok(0)
                }
            }
            None => Ok(0),
        }
    }

    pub fn get_count(&self, key: impl AsRef<[u8]>) -> Result<u64> {
        let meta = self.get_meta(key)?;
        Ok(match meta {
            Some(m) => m.count,
            _ => 0,
        })
    }

    pub fn delete_all(&self, key: &str) -> Result<u64> {
        let meta = self.get_meta(key)?;
        let mut deletes_count = 0;
        if let Some(meta) = meta {
            let mut has_error = None;
            self.for_each_data(key, None, |k, _| {
                deletes_count += 1;
                match self.rocksdb.delete(k) {
                    Ok(_) => true,
                    Err(err) => {
                        has_error = Some(err);
                        false
                    }
                }
            })?;
            if let Some(err) = has_error {
                return Err(err.into());
            }
            self.rocksdb.delete(encode_meta_key(key))?;
            self.rocksdb.compact_range(
                Some(encode_data_key(meta.id).as_ref()),
                Some(encode_data_key(meta.id + 1).as_ref()),
            );
        }
        Ok(deletes_count)
    }

    pub fn map_count(&self, key: impl AsRef<[u8]>) -> Result<u64> {
        self.get_count(key)
    }

    pub fn map_get(
        &self,
        key: impl AsRef<[u8]>,
        field: impl AsRef<[u8]>,
    ) -> Result<Option<Vec<u8>>> {
        let meta = self.get_or_create_meta(key, KeyType::Map)?;
        let full_key = encode_data_key_map_item(meta.id, field);
        Ok(self.rocksdb.get(full_key)?)
    }

    pub fn map_put(
        &self,
        key: impl AsRef<[u8]>,
        field: impl AsRef<[u8]>,
        value: impl AsRef<[u8]>,
    ) -> Result<()> {
        let key = key.as_ref();
        let mut meta = self.get_or_create_meta(key, KeyType::Map)?;
        let full_key = encode_data_key_map_item(meta.id, field);
        if self.rocksdb.get(&full_key)?.is_none() {
            meta.count += 1;
        }
        self.rocksdb.put(&full_key, value)?;
        self.save_meta(key, &meta, false)
    }

    pub fn map_delete(&self, key: impl AsRef<[u8]>, field: impl AsRef<[u8]>) -> Result<bool> {
        let key = key.as_ref();
        match self.get_meta(key)? {
            None => Ok(false),
            Some(mut meta) => {
                let full_key = encode_data_key_map_item(meta.id, field);
                if self.rocksdb.get(&full_key)?.is_some() {
                    meta.count -= 1;
                    self.rocksdb.delete(&full_key)?;
                    self.save_meta(key, &meta, true)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }

    pub fn map_for_each<F>(&self, key: &str, mut f: F) -> Result<u64>
    where
        F: FnMut(&str, Box<[u8]>) -> bool,
    {
        let mut has_error = None;
        let count = self.for_each_data(key, None, |k, v| {
            match decode_data_key_map_item(k.as_ref()) {
                Ok(k) => f(&k, v),
                Err(err) => {
                    has_error = Some(err);
                    false
                }
            }
        })?;
        match has_error {
            None => Ok(count),
            Some(err) => Err(err.into()),
        }
    }

    pub fn map_items(&self, key: &str) -> Result<Vec<(String, Box<[u8]>)>> {
        let count = self.get_count(key)?;
        let mut vec = Vec::with_capacity(count as u64 as usize);
        self.map_for_each(key, |f, v| {
            vec.push((String::from(f), v));
            true
        })?;
        Ok(vec)
    }

    pub fn map_for_each_with_prefix<F>(&self, key: &str, prefix: &str, mut f: F) -> Result<u64>
    where
        F: FnMut(&str, Box<[u8]>) -> bool,
    {
        let mut has_error = None;
        let count =
            self.for_each_data(key, Some(prefix), |k, v| {
                match decode_data_key_map_item(k.as_ref()) {
                    Ok(k) => f(&k, v),
                    Err(err) => {
                        has_error = Some(err);
                        false
                    }
                }
            })?;
        match has_error {
            None => Ok(count),
            Some(err) => Err(err.into()),
        }
    }

    pub fn map_items_with_prefix(
        &self,
        key: &str,
        prefix: &str,
    ) -> Result<Vec<(String, Box<[u8]>)>> {
        let mut vec = Vec::new();
        self.map_for_each_with_prefix(key, prefix, |f, v| {
            vec.push((String::from(f), v));
            true
        })?;
        Ok(vec)
    }

    pub fn set_count(&self, key: &str) -> Result<u64> {
        self.get_count(key)
    }

    pub fn set_add(&self, key: &str, value: &[u8]) -> Result<bool> {
        let mut meta = self.get_or_create_meta(key, KeyType::Set)?;
        let full_key = encode_data_key_set_item(meta.id, value);
        let mut is_new_item = false;
        if self.rocksdb.get(&full_key)?.is_none() {
            meta.count += 1;
            is_new_item = true;
        }
        self.rocksdb.put(&full_key, FILL_EMPTY_DATA)?;
        if is_new_item {
            self.save_meta(key, &meta, false)?;
        }
        Ok(is_new_item)
    }

    pub fn set_is_member(&self, key: &str, value: &[u8]) -> Result<bool> {
        match self.get_meta(key)? {
            None => Ok(false),
            Some(meta) => {
                let full_key = encode_data_key_set_item(meta.id, value);
                Ok(self.rocksdb.get(&full_key)?.is_some())
            }
        }
    }

    pub fn set_delete(&self, key: &str, value: &[u8]) -> Result<bool> {
        match self.get_meta(key)? {
            None => Ok(false),
            Some(mut meta) => {
                let full_key = encode_data_key_set_item(meta.id, value);
                if self.rocksdb.get(&full_key)?.is_some() {
                    meta.count -= 1;
                    self.rocksdb.delete(full_key)?;
                    self.save_meta(key, &meta, true)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }

    pub fn set_for_each<F>(&self, key: &str, mut f: F) -> Result<u64>
    where
        F: FnMut(Box<[u8]>) -> bool,
    {
        self.for_each_data(key, None, |k, _| {
            let value = decode_data_key_set_item(k.as_ref());
            f(Box::from(value))
        })
    }

    pub fn set_items(&self, key: &str) -> Result<Vec<Box<[u8]>>> {
        let count = self.get_count(key)?;
        let mut vec = Vec::with_capacity(count as u64 as usize);
        self.set_for_each(key, |v| {
            vec.push(v);
            true
        })?;
        Ok(vec)
    }

    pub fn list_count(&self, key: &str) -> Result<u64> {
        self.get_count(key)
    }

    pub fn list_left_push(&self, key: &str, value: &[u8]) -> Result<u64> {
        let mut meta = self.get_or_create_meta(key, KeyType::List)?;
        let (left, right) = meta.decode_list_extra();
        let full_key = encode_data_key_list_item(meta.id, left);
        self.rocksdb.put(full_key, value)?;
        meta.encode_list_extra(left - 1, right);
        meta.count += 1;
        self.save_meta(key, &meta, false)?;
        Ok(meta.count)
    }

    pub fn list_right_push(&self, key: &str, value: &[u8]) -> Result<u64> {
        let mut meta = self.get_or_create_meta(key, KeyType::List)?;
        let (left, right) = meta.decode_list_extra();
        let full_key = encode_data_key_list_item(meta.id, right);
        self.rocksdb.put(full_key, value)?;
        meta.encode_list_extra(left, right + 1);
        meta.count += 1;
        self.save_meta(key, &meta, false)?;
        Ok(meta.count)
    }

    pub fn list_left_pop(&self, key: &str) -> Result<Option<Box<[u8]>>> {
        match self.get_meta(key)? {
            None => Ok(None),
            Some(mut meta) => {
                let (left, right) = meta.decode_list_extra();
                let full_key = encode_data_key_list_item(meta.id, left + 1);
                match self.rocksdb.get(full_key.as_ref())? {
                    Some(value) => {
                        meta.encode_list_extra(left + 1, right);
                        meta.count -= 1;
                        self.save_meta(key, &meta, true)?;
                        self.rocksdb.delete(full_key.as_ref())?;
                        Ok(Some(Box::from(value)))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    pub fn list_right_pop(&self, key: &str) -> Result<Option<Box<[u8]>>> {
        match self.get_meta(key)? {
            None => Ok(None),
            Some(mut meta) => {
                let (left, right) = meta.decode_list_extra();
                let full_key = encode_data_key_list_item(meta.id, right - 1);
                match self.rocksdb.get(full_key.as_ref())? {
                    Some(value) => {
                        meta.encode_list_extra(left, right - 1);
                        meta.count -= 1;
                        self.save_meta(key, &meta, true)?;
                        self.rocksdb.delete(full_key.as_ref())?;
                        Ok(Some(Box::from(value)))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    pub fn list_for_each<F>(&self, key: &str, mut f: F) -> Result<u64>
    where
        F: FnMut(Box<[u8]>) -> bool,
    {
        self.for_each_data(key, None, |_, v| f(v))
    }

    pub fn list_items(&self, key: &str) -> Result<Vec<Box<[u8]>>> {
        let count = self.get_count(key)?;
        let mut vec = Vec::with_capacity(count as u64 as usize);
        self.list_for_each(key, |v| {
            vec.push(v);
            true
        })?;
        Ok(vec)
    }

    pub fn sorted_list_count(&self, key: &str) -> Result<u64> {
        self.get_count(key)
    }

    pub fn sorted_list_add(&self, key: &str, score: &[u8], value: &[u8]) -> Result<u64> {
        let mut meta = self.get_or_create_meta(key, KeyType::SortedList)?;
        let (sequence, left_deleted_count, right_deleted_count) = meta.decode_sorted_list_extra();
        let full_key = encode_data_key_sorted_list_item(meta.id, score, sequence);
        meta.encode_sorted_list_extra(sequence + 1, left_deleted_count, right_deleted_count);
        meta.count += 1;
        self.rocksdb.put(full_key, value)?;
        self.save_meta(key, &meta, false)?;
        Ok(meta.count)
    }

    pub fn sorted_list_left_pop(
        &self,
        key: &str,
        max_score: Option<&[u8]>,
    ) -> Result<Option<ScoreVal>> {
        let meta = self.get_meta(key)?;
        if let Some(mut meta) = meta {
            let (sequence, left_deleted_count, right_deleted_count) =
                meta.decode_sorted_list_extra();
            let prefix = encode_data_key(meta.id);
            let mut opts = ReadOptions::default();
            opts.set_prefix_same_as_start(true);
            let mut iter = self
                .rocksdb
                .iterator_opt(IteratorMode::From(&prefix, Direction::Forward), opts);
            if let Some((k, v)) = iter.next() {
                if !has_prefix(&prefix, k.as_ref()) {
                    return Ok(None);
                }
                let score = decode_data_key_sorted_list_item(k.as_ref());
                if let Some(max_score) = max_score {
                    if compare_score_bytes(score, max_score) > 0 {
                        return Ok(None);
                    }
                }
                self.rocksdb.delete(k.as_ref())?;
                meta.count -= 1;
                if left_deleted_count > 0
                    && left_deleted_count % self.options.sorted_list_compact_deletes_count == 0
                {
                    self.rocksdb
                        .compact_range(Some(encode_data_key(meta.id).as_ref()), Some(k.as_ref()));
                    meta.encode_sorted_list_extra(sequence, 0, right_deleted_count);
                } else {
                    meta.encode_sorted_list_extra(
                        sequence,
                        left_deleted_count + 1,
                        right_deleted_count,
                    );
                }
                self.save_meta(key, &meta, true)?;
                return Ok(Some((Box::from(score), v)));
            }
        }
        Ok(None)
    }

    pub fn sorted_list_right_pop(
        &self,
        key: &str,
        min_score: Option<&[u8]>,
    ) -> Result<Option<ScoreVal>> {
        let meta = self.get_meta(key)?;
        if let Some(mut meta) = meta {
            let (sequence, left_deleted_count, right_deleted_count) =
                meta.decode_sorted_list_extra();
            let prefix = encode_data_key(meta.id);
            let next_prefix = encode_data_key(meta.id + 1);
            let opts = ReadOptions::default();
            let mut iter = self
                .rocksdb
                .iterator_opt(IteratorMode::From(&next_prefix, Direction::Reverse), opts);
            if let Some((k, v)) = iter.next() {
                if !has_prefix(&prefix, k.as_ref()) {
                    return Ok(None);
                }
                let score = decode_data_key_sorted_list_item(k.as_ref());
                if let Some(min_score) = min_score {
                    if compare_score_bytes(score, min_score) < 0 {
                        return Ok(None);
                    }
                }
                self.rocksdb.delete(k.as_ref())?;
                meta.count -= 1;
                if right_deleted_count > 0
                    && right_deleted_count % self.options.sorted_list_compact_deletes_count == 0
                {
                    self.rocksdb
                        .compact_range(Some(k.as_ref()), Some(next_prefix.as_ref()));
                    meta.encode_sorted_list_extra(sequence, left_deleted_count, 0);
                } else {
                    meta.encode_sorted_list_extra(
                        sequence,
                        left_deleted_count,
                        right_deleted_count + 1,
                    );
                }
                self.save_meta(key, &meta, true)?;
                return Ok(Some((Box::from(score), v)));
            }
        }
        Ok(None)
    }

    pub fn sorted_list_for_each<F>(&self, key: &str, mut f: F) -> Result<u64>
    where
        F: FnMut((Box<[u8]>, Box<[u8]>)) -> bool,
    {
        self.for_each_data(key, None, |k, v| {
            let score = decode_data_key_sorted_list_item(k.as_ref());
            f((Box::from(score), v))
        })
    }

    pub fn sorted_list_items(&self, key: &str) -> Result<VecScoreVal> {
        let count = self.get_count(key)?;
        let mut vec = Vec::with_capacity(count as u64 as usize);
        self.sorted_list_for_each(key, |item| {
            vec.push(item);
            true
        })?;
        Ok(vec)
    }

    pub fn sorted_set_count(&self, key: &str) -> Result<u64> {
        self.get_count(key)
    }

    pub fn sorted_set_for_each<F>(&self, key: &str, mut f: F) -> Result<u64>
    where
        F: FnMut((Box<[u8]>, Box<[u8]>)) -> bool,
    {
        let score_len = self
            .get_meta(key)?
            .map(|m| m.decode_sorted_set_extra().1)
            .unwrap_or(0);
        self.for_each_data(key, None, |k, _| {
            f(decode_data_key_sorted_set_item_with_score(
                k.as_ref(),
                score_len,
            ))
        })
    }

    pub fn sorted_set_items(&self, key: &str) -> Result<VecScoreVal> {
        let count = self.get_count(key)?;
        let mut vec = Vec::with_capacity(count as u64 as usize);
        self.sorted_set_for_each(key, |v| {
            vec.push(v);
            true
        })?;
        Ok(vec)
    }

    pub fn sorted_set_add(&self, key: &str, score: &[u8], value: &[u8]) -> Result<u64> {
        let mut meta = self.get_or_create_meta(key, KeyType::SortedSet)?;
        let (deleted_count, score_len) = meta.decode_sorted_set_extra();
        let full_key1 = encode_data_key_sorted_set_item_with_score(meta.id, score, value);
        let full_key2 = encode_data_key_sorted_set_item_without_score(meta.id, value);
        if score_len < 1 {
            meta.encode_sorted_set_extra(deleted_count, score.len() as u8);
        } else {
            let actual_len = score.len() as u8;
            if score_len != actual_len {
                return Err(Error::Message(format!(
                    "invalid score length, expected {} bytes but got {} bytes",
                    score_len, actual_len
                )));
            }
        }
        meta.count += 1;
        self.rocksdb.put(full_key1, FILL_EMPTY_DATA)?;
        self.rocksdb.put(full_key2, score)?;
        self.save_meta(key, &meta, false)?;
        Ok(meta.count)
    }

    pub fn sorted_set_is_member(&self, key: &str, value: &[u8]) -> Result<bool> {
        match self.get_meta(key)? {
            None => Ok(false),
            Some(meta) => {
                let full_key = encode_data_key_sorted_set_item_without_score(meta.id, value);
                match self.rocksdb.get(full_key)? {
                    None => Ok(false),
                    Some(_) => Ok(true),
                }
            }
        }
    }

    pub fn sorted_set_delete(&self, key: &str, value: &[u8]) -> Result<bool> {
        match self.get_meta(key)? {
            None => Ok(false),
            Some(mut meta) => {
                let (deleted_count, score_len) = meta.decode_sorted_set_extra();
                let full_key1 = encode_data_key_sorted_set_item_without_score(meta.id, value);
                match self.rocksdb.get(full_key1.as_ref())? {
                    None => Ok(false),
                    Some(score) => {
                        let score = score.as_ref();
                        let full_key2 =
                            encode_data_key_sorted_set_item_with_score(meta.id, score, value);
                        self.rocksdb.delete(full_key2)?;
                        self.rocksdb.delete(full_key1)?;
                        meta.count -= 1;
                        if deleted_count > 0
                            && deleted_count % self.options.sorted_list_compact_deletes_count == 0
                        {
                            self.rocksdb.compact_range(
                                Some(encode_data_key(meta.id).as_ref()),
                                Some(encode_data_key(meta.id + 1).as_ref()),
                            );
                            meta.encode_sorted_set_extra(0, score_len);
                        } else {
                            meta.encode_sorted_set_extra(deleted_count + 1, score_len);
                        }
                        self.save_meta(key, &meta, true)?;
                        Ok(true)
                    }
                }
            }
        }
    }

    pub fn sorted_set_left(
        &self,
        key: &str,
        max_score: Option<&[u8]>,
        limit: usize,
    ) -> Result<VecScoreVal> {
        match self.get_meta(key)? {
            None => Ok(vec![]),
            Some(meta) => {
                let (_, score_len) = meta.decode_sorted_set_extra();
                let mut list = vec![];
                let prefix = encode_data_key_sorted_set_prefix(meta.id);
                let mut opts = ReadOptions::default();
                opts.set_prefix_same_as_start(true);
                let iter = self
                    .rocksdb
                    .iterator_opt(IteratorMode::From(&prefix, Direction::Forward), opts);
                for (k, _) in iter {
                    if !has_prefix(&prefix, k.as_ref()) {
                        break;
                    }
                    let (score, value) =
                        decode_data_key_sorted_set_item_with_score(k.as_ref(), score_len);
                    if let Some(max_score) = max_score {
                        if compare_score_bytes(score.as_ref(), max_score) > 0 {
                            break;
                        }
                    }
                    list.push((score, value));
                    if list.len() >= limit {
                        break;
                    }
                }
                Ok(list)
            }
        }
    }

    pub fn sorted_set_right(
        &self,
        key: &str,
        min_score: Option<&[u8]>,
        limit: usize,
    ) -> Result<VecScoreVal> {
        match self.get_meta(key)? {
            None => Ok(vec![]),
            Some(meta) => {
                let (_, score_len) = meta.decode_sorted_set_extra();
                let mut list = vec![];
                let prefix = encode_data_key_sorted_set_prefix(meta.id);
                let next_prefix = encode_data_key_sorted_set_prefix(meta.id + 1);
                let opts = ReadOptions::default();
                let iter = self
                    .rocksdb
                    .iterator_opt(IteratorMode::From(&next_prefix, Direction::Reverse), opts);
                for (k, _) in iter {
                    if !has_prefix(&prefix, k.as_ref()) {
                        break;
                    }
                    let (score, value) =
                        decode_data_key_sorted_set_item_with_score(k.as_ref(), score_len);
                    if let Some(min_score) = min_score {
                        if compare_score_bytes(score.as_ref(), min_score) < 0 {
                            break;
                        }
                    }
                    list.push((score, value));
                    if list.len() >= limit {
                        break;
                    }
                }
                Ok(list)
            }
        }
    }
}
