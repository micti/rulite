//! Database threads: one writer and 1–2 readers, each owning its own
//! `rusqlite::Connection` and fed by a bounded channel. No pool.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{mpsc, oneshot};

use crate::schema::{self, Schema, SchemaCache};

/// Pending jobs per thread before callers wait (backpressure).
const QUEUE_DEPTH: usize = 32;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

pub struct Db {
    writer: mpsc::Sender<Job>,
    readers: Vec<mpsc::Sender<Job>>,
    next_reader: AtomicUsize,
    schema: Arc<SchemaCache>,
}

/// Handles of the DB threads. They exit once the [`Db`] is dropped.
pub struct Threads(Vec<JoinHandle<()>>);

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    /// The DB thread is gone (shutting down).
    Closed,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Sqlite(e) => e.fmt(f),
            DbError::Closed => f.write_str("database is shutting down"),
        }
    }
}

/// Opens the writer (which also switches the file to WAL), loads the schema,
/// then opens `readers` read-only connections.
pub fn open(path: &Path, readers: usize) -> rusqlite::Result<(Db, Threads)> {
    let writer =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    writer.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
    configure(&writer)?;
    let schema = Arc::new(SchemaCache::new(schema::introspect(&writer)?));

    let mut threads = Vec::with_capacity(readers + 1);
    let writer = spawn("rulite-writer", writer, &mut threads)?;
    let readers = (0..readers)
        .map(|i| {
            let conn =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            configure(&conn)?;
            spawn(&format!("rulite-reader-{i}"), conn, &mut threads)
        })
        .collect::<rusqlite::Result<_>>()?;

    let db = Db { writer, readers, next_reader: AtomicUsize::new(0), schema };
    Ok((db, Threads(threads)))
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -2000;
         PRAGMA temp_store = FILE;",
    )
}

fn spawn(name: &str, mut conn: Connection, threads: &mut Vec<JoinHandle<()>>) -> rusqlite::Result<mpsc::Sender<Job>> {
    let (tx, mut rx) = mpsc::channel::<Job>(QUEUE_DEPTH);
    let handle = std::thread::Builder::new()
        .name(name.into())
        .stack_size(256 * 1024)
        .spawn(move || {
            while let Some(job) = rx.blocking_recv() {
                job(&mut conn);
            }
        })
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    threads.push(handle);
    Ok(tx)
}

impl Db {
    /// Runs `f` on a reader thread (round robin) with an up-to-date schema.
    pub async fn read<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection, &Schema) -> rusqlite::Result<T> + Send + 'static,
    {
        let i = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.run(&self.readers[i], move |conn, schema| f(conn, schema)).await
    }

    /// Runs `f` on the writer thread with an up-to-date schema.
    pub async fn write<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Schema) -> rusqlite::Result<T> + Send + 'static,
    {
        self.run(&self.writer, f).await
    }

    async fn run<T, F>(&self, queue: &mpsc::Sender<Job>, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Schema) -> rusqlite::Result<T> + Send + 'static,
    {
        let (done, result) = oneshot::channel();
        let cache = self.schema.clone();
        let job: Job = Box::new(move |conn| {
            let r = cache.refresh(conn).and_then(|schema| f(conn, &schema));
            let _ = done.send(r);
        });
        queue.send(job).await.map_err(|_| DbError::Closed)?;
        result.await.map_err(|_| DbError::Closed)?.map_err(DbError::Sqlite)
    }
}

impl Threads {
    pub fn join(self) {
        for handle in self.0 {
            let _ = handle.join();
        }
    }
}
