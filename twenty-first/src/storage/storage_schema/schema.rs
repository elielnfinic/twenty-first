use super::super::storage_vec::Index;
use super::{traits::*, DbtSingleton, DbtVec};
use crate::sync::{AtomicMutex, AtomicRw};
use std::sync::Arc;

/// Provides a virtual database schema.
///
/// `DbtSchema` can create any number of instances of types that
/// implement the trait [`DbTable`].  We refer to these instances as
/// `table`.  Examples are [`DbtVec`] and [`DbtSingleton`].
///
/// With proper usage (below), the application can perform writes
/// to any subset of the `table`s and then persist (write) the data
/// atomically to the database.
///
/// Thus we get something like relational DB transactions using
/// `LevelDB` key/val store.
///
/// ### Atomicity -- Single Table:
///
/// An individual `table` is atomic for all read and write
/// operations to itself.
///
/// ### Atomicity -- Multi Table:
///
/// Important!  Operations over multiple `table`s are NOT atomic
/// without additional locking by the application.
///
/// This can be achieved by placing the `table`s into a heterogenous
/// container such as a `struct` or `tuple`. Then place an
/// `Arc<Mutex<..>>` or `Arc<Mutex<RwLock<..>>` around the container.
///
/// # Example:
///
/// ```
/// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{SimpleRustyStorage, traits::*}};
/// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
/// # let mut rusty_storage = SimpleRustyStorage::new(db);
/// # rusty_storage.restore_or_new();
/// use std::sync::{Arc, RwLock};
///
/// let tables = (
///     rusty_storage.schema.new_vec::<u64, u16>("ages"),
///     rusty_storage.schema.new_vec::<u64, String>("names"),
///     rusty_storage.schema.new_singleton::<bool>(12u64.into())
/// );
///
/// let atomic_tables = Arc::new(RwLock::new(tables));
/// let mut lock = atomic_tables.write().unwrap();
/// lock.0.push(5);
/// lock.1.push("Sally".into());
/// lock.2.set(true);
/// ```
///
/// In the example, the `table` were placed in a `tuple` container.
/// It works equally well to put them in a `struct`.  If the tables
/// are all of the same type (including generics), they could be
/// placed in a collection type such as `Vec`, or `HashMap`.
///
/// This crate provides [`AtomicRw`] and [`AtomicMutex`]
/// which are simple wrappers around `Arc<RwLock<T>>` and `Arc<Mutex<T>>`.
/// `DbtSchema` provides helper methods for wrapping your `table`s with
/// these.
///
/// This is the recommended usage.
///
/// # Example:
///
/// ```rust
/// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{SimpleRustyStorage, traits::*}};
/// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
/// # let mut rusty_storage = SimpleRustyStorage::new(db);
/// # rusty_storage.restore_or_new();
/// let tables = (
///     rusty_storage.schema.new_vec::<u64, u16>("ages"),
///     rusty_storage.schema.new_vec::<u64, String>("names"),
///     rusty_storage.schema.new_singleton::<bool>(12u64.into())
/// );
///
/// let atomic_tables = rusty_storage.schema.atomic_rw(tables);
///
/// // these writes happen atomically.
/// atomic_tables.with_mut(|tables| {
///     tables.0.push(5);
///     tables.1.push("Sally".into());
///     tables.2.set(true);
/// });
/// ```
pub struct DbtSchema<
    ParentKey,
    ParentValue,
    Reader: StorageReader<ParentKey, ParentValue> + Send + Sync,
> {
    /// These are the tables known by this `DbtSchema` instance.
    ///
    /// Implementor(s) of [`StorageWriter`] will iterate over these
    /// tables, collect the pending operations, and write them
    /// atomically to the DB.
    pub tables: Vec<Box<dyn DbTable<ParentKey, ParentValue> + Send + Sync>>,

    /// Database Reader
    pub reader: Arc<Reader>,
}

impl<
        ParentKey,
        ParentValue,
        Reader: StorageReader<ParentKey, ParentValue> + 'static + Sync + Send,
    > DbtSchema<ParentKey, ParentValue, Reader>
{
    /// Create a new DbtVec
    ///
    /// The `DbtSchema` will keep a reference to the `DbtVec`. In this way,
    /// the Schema becomes aware of any write operations and later
    /// a [`StorageWriter`] impl can write them all out.
    ///
    /// Atomicity: see [`DbtSchema`]
    #[inline]
    pub fn new_vec<I, T>(&mut self, name: &str) -> DbtVec<ParentKey, ParentValue, Index, T>
    where
        ParentKey: From<Index> + 'static,
        ParentValue: From<T> + 'static,
        T: Clone + From<ParentValue> + 'static,
        ParentKey: From<(ParentKey, ParentKey)>,
        ParentKey: From<u8>,
        Index: From<ParentValue>,
        ParentValue: From<Index>,
        Index: From<u64> + 'static,
        DbtVec<ParentKey, ParentValue, Index, T>: DbTable<ParentKey, ParentValue> + Send + Sync,
    {
        assert!(self.tables.len() < 255);
        let reader = self.reader.clone();
        let key_prefix = self.tables.len() as u8;
        let vector = DbtVec::<ParentKey, ParentValue, Index, T>::new(reader, key_prefix, name);

        self.tables.push(Box::new(vector.clone()));
        vector
    }

    // possible future extension
    // fn new_hashmap<K, V>(&self) -> Arc<RefCell<DbtHashMap<K, V>>> { }

    /// Create a new DbtSingleton
    ///
    /// The `DbtSchema` will keep a reference to the `DbtSingleton`.
    /// In this way, the Schema becomes aware of any write operations
    /// and later a [`StorageWriter`] impl can write them all out.
    ///
    /// Atomicity: see [`DbtSchema`]
    #[inline]
    pub fn new_singleton<S>(&mut self, key: ParentKey) -> DbtSingleton<ParentKey, ParentValue, S>
    where
        S: Default + Eq + Clone + 'static,
        ParentKey: 'static,
        ParentValue: From<S> + 'static,
        ParentKey: From<(ParentKey, ParentKey)> + From<u8>,
        DbtSingleton<ParentKey, ParentValue, S>: DbTable<ParentKey, ParentValue> + Send + Sync,
    {
        let singleton = DbtSingleton::<ParentKey, ParentValue, S>::new(key, self.reader.clone());
        self.tables.push(Box::new(singleton.clone()));
        singleton
    }

    // note: it would be nice to have a `create_tables()` method
    // that takes a list of table definition enums and returns
    // some kind of dynamic heterogenous list (maybe frunk::Hlist?)
    // of tables.  However, specifying the tables to be created
    // as a parameter seems to require variadic generics and/or
    // higher-kinded-types, which do not exist in rust yet.
    //
    // So for now, we make do with the pattern that callers
    // invoke `new_singleton()` and `new_vec()` to populate a
    // container such as a `tuple` or `struct` and then they
    // should pass the container to `atomic_rw()` to make the
    // tables atomic.

    /// Wraps input of type `T` with a [`AtomicRw`]
    ///
    /// # Example:
    ///
    /// ```
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{DbtSchema, SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// # let mut rusty_storage = SimpleRustyStorage::new(db);
    /// # rusty_storage.restore_or_new();
    ///
    /// let ages = rusty_storage.schema.new_vec::<u64, u16>("ages");
    /// let names = rusty_storage.schema.new_vec::<u64, String>("names");
    /// let proceed = rusty_storage.schema.new_singleton::<bool>(12u64.into());
    ///
    /// let tables = (ages, names, proceed);
    /// let atomic_tables = rusty_storage.schema.atomic_rw(tables);
    ///
    /// // these writes happen atomically.
    /// atomic_tables.with_mut(|tables| {
    ///     tables.0.push(5);
    ///     tables.1.push("Sally".into());
    ///     tables.2.set(true);
    /// });
    /// ```
    pub fn atomic_rw<T>(&self, data: T) -> AtomicRw<T> {
        AtomicRw::from(data)
    }

    /// Wraps input of type `T` with a [`AtomicMutex`]
    ///
    /// # Example:
    ///
    /// ```
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{DbtSchema, SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// # let mut rusty_storage = SimpleRustyStorage::new(db);
    /// # rusty_storage.restore_or_new();
    ///
    /// let ages = rusty_storage.schema.new_vec::<u64, u16>("ages");
    /// let names = rusty_storage.schema.new_vec::<u64, String>("names");
    /// let proceed = rusty_storage.schema.new_singleton::<bool>(12u64.into());
    ///
    /// let tables = (ages, names, proceed);
    /// let atomic_tables = rusty_storage.schema.atomic_mutex(tables);
    ///
    /// // these writes happen atomically.
    /// atomic_tables.with_mut(|tables| {
    ///     tables.0.push(5);
    ///     tables.1.push("Sally".into());
    ///     tables.2.set(true);
    /// });
    /// ```
    pub fn atomic_mutex<T>(&self, data: T) -> AtomicMutex<T> {
        AtomicMutex::from(data)
    }
}
