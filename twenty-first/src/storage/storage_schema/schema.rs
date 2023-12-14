// use super::super::storage_vec::Index;
use super::{traits::*, DbtSingleton, DbtVec, RustyKey};
use crate::sync::{AtomicMutex, AtomicRw};
use serde::{de::DeserializeOwned, Serialize};
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
/// use std::sync::{Arc, RwLock};
/// let mut storage = SimpleRustyStorage::new(db);
/// storage.restore_or_new();
///
/// let tables = (
///     storage.schema.new_vec::<u16>("ages"),
///     storage.schema.new_vec::<String>("names"),
///     storage.schema.new_singleton::<bool>(12u64.into())
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
/// let mut storage = SimpleRustyStorage::new(db);
/// storage.restore_or_new();
///
/// let atomic_tables = storage.schema.create_tables_rw(|s| {
///     (
///         s.new_vec::<u16>("ages"),
///         s.new_vec::<String>("names"),
///         s.new_singleton::<bool>(12u64.into())
///     )
/// });
///
/// // these writes happen atomically.
/// atomic_tables.lock_mut(|tables| {
///     tables.0.push(5);
///     tables.1.push("Sally".into());
///     tables.2.set(true);
/// });
/// ```
pub struct DbtSchema<Reader: StorageReader + Send + Sync> {
    /// These are the tables known by this `DbtSchema` instance.
    ///
    /// Implementor(s) of [`StorageWriter`] will iterate over these
    /// tables, collect the pending operations, and write them
    /// atomically to the DB.
    pub tables: AtomicRw<Vec<Box<dyn DbTable + Send + Sync>>>,

    /// Database Reader
    pub reader: Arc<Reader>,
}

impl<Reader: StorageReader + 'static + Sync + Send> DbtSchema<Reader> {
    /// Create a new DbtVec
    ///
    /// The `DbtSchema` will keep a reference to the `DbtVec`. In this way,
    /// the Schema becomes aware of any write operations and later
    /// a [`StorageWriter`] impl can write them all out.
    ///
    /// Atomicity: see [`DbtSchema`]
    #[inline]
    pub fn new_vec<V>(&mut self, name: &str) -> DbtVec<V>
    where
        V: Clone + 'static,
        V: Serialize + DeserializeOwned,
        DbtVec<V>: DbTable + Send + Sync,
    {
        self.tables.lock_mut(|tables| {
            assert!(tables.len() < 255);
            let reader = self.reader.clone();
            let key_prefix = tables.len() as u8;
            let vector = DbtVec::<V>::new(reader, key_prefix, name);

            tables.push(Box::new(vector.clone()));
            vector
        })
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
    pub fn new_singleton<V>(&mut self, key: RustyKey) -> DbtSingleton<V>
    where
        V: Default + Clone + 'static,
        V: Serialize + DeserializeOwned,
        DbtSingleton<V>: DbTable + Send + Sync,
    {
        let singleton = DbtSingleton::<V>::new(key, self.reader.clone());
        self.tables
            .lock_mut(|t| t.push(Box::new(singleton.clone())));
        singleton
    }

    /// create tables and wrap in an [`AtomicRw<T>`]
    ///
    /// This is the recommended way to create a group of tables
    /// that are atomic for reads and writes across tables.
    ///
    /// Atomicity is guaranteed by an [`RwLock`](std::sync::RwLock).
    ///
    /// # Example:
    ///
    /// ```rust
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// let mut storage = SimpleRustyStorage::new(db);
    /// storage.restore_or_new();
    ///
    /// let atomic_tables = storage.schema.create_tables_rw(|s| {
    ///     (
    ///         s.new_vec::<u16>("ages"),
    ///         s.new_vec::<String>("names"),
    ///         s.new_singleton::<bool>(12u64.into())
    ///     )
    /// });
    ///
    /// // these writes happen atomically.
    /// atomic_tables.lock_mut(|tables| {
    ///     tables.0.push(5);
    ///     tables.1.push("Sally".into());
    ///     tables.2.set(true);
    /// });
    /// ```
    pub fn create_tables_rw<D, F>(&mut self, f: F) -> AtomicRw<D>
    where
        F: Fn(&mut Self) -> D,
    {
        let data = f(self);
        AtomicRw::<D>::from(data)
    }

    /// create tables and wrap in an [`AtomicMutex<T>`]
    ///
    /// This is a simple way to create a group of tables
    /// that are atomic for reads and writes across tables.
    ///
    /// Atomicity is guaranteed by a [`Mutex`](std::sync::Mutex).
    ///
    /// # Example:
    ///
    /// ```rust
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// let mut storage = SimpleRustyStorage::new(db);
    /// storage.restore_or_new();
    ///
    /// let atomic_tables = storage.schema.create_tables_mutex(|s| {
    ///     (
    ///         s.new_vec::<u16>("ages"),
    ///         s.new_vec::<String>("names"),
    ///         s.new_singleton::<bool>(12u64.into())
    ///     )
    /// });
    ///
    /// // these writes happen atomically.
    /// atomic_tables.lock_mut(|tables| {
    ///     tables.0.push(5);
    ///     tables.1.push("Sally".into());
    ///     tables.2.set(true);
    /// });
    /// ```
    pub fn create_tables_mutex<D, F>(&mut self, f: F) -> AtomicMutex<D>
    where
        F: Fn(&mut Self) -> D,
    {
        let data = f(self);
        AtomicMutex::<D>::from(data)
    }

    /// Wraps input of type `T` with a [`AtomicRw`]
    ///
    /// note: method [`create_tables_rw()`](Self::create_tables_rw()) is a simpler alternative.
    ///
    /// # Example:
    ///
    /// ```
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{DbtSchema, SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// let mut storage = SimpleRustyStorage::new(db);
    /// storage.restore_or_new();
    ///
    /// let ages = storage.schema.new_vec::<u16>("ages");
    /// let names = storage.schema.new_vec::<String>("names");
    /// let proceed = storage.schema.new_singleton::<bool>(12u64.into());
    ///
    /// let tables = (ages, names, proceed);
    /// let atomic_tables = storage.schema.atomic_rw(tables);
    ///
    /// // these writes happen atomically.
    /// atomic_tables.lock_mut(|tables| {
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
    /// note: method [`create_tables_mutex()`](Self::create_tables_mutex()) is a simpler alternative.
    ///
    /// # Example:
    ///
    /// ```
    /// # use twenty_first::storage::{level_db, storage_vec::traits::*, storage_schema::{DbtSchema, SimpleRustyStorage, traits::*}};
    /// # let db = level_db::DB::open_new_test_database(true, None, None, None).unwrap();
    /// let mut storage = SimpleRustyStorage::new(db);
    /// storage.restore_or_new();
    ///
    /// let ages = storage.schema.new_vec::<u16>("ages");
    /// let names = storage.schema.new_vec::<String>("names");
    /// let proceed = storage.schema.new_singleton::<bool>(12u64.into());
    ///
    /// let tables = (ages, names, proceed);
    /// let atomic_tables = storage.schema.atomic_mutex(tables);
    ///
    /// // these writes happen atomically.
    /// atomic_tables.lock_mut(|tables| {
    ///     tables.0.push(5);
    ///     tables.1.push("Sally".into());
    ///     tables.2.set(true);
    /// });
    /// ```
    pub fn atomic_mutex<T>(&self, data: T) -> AtomicMutex<T> {
        AtomicMutex::from(data)
    }
}
