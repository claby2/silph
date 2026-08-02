//! All storage contact lives here: SQLite (rusqlite) for persistence and
//! indexing, Gorilla delta-delta encoding (tsz) for at-rest compression.
//!
//! Layout: each insert lands as raw rows in `samples`, committed through WAL
//! with synchronous=NORMAL — an acked insert survives a process crash, but an
//! OS crash or power loss may drop the WAL tail written since the last
//! checkpoint. A dedicated maintenance task ([`Store::maintenance`]) compacts
//! completed hourly windows into one tsz-encoded blob per series in `chunks`,
//! deletes the raw rows they covered, and enforces retention with plain
//! DELETEs. Timestamps are stored in seconds (the scrape grid is 15s; tsz
//! assumes seconds precision) and within one second the newest write wins;
//! the public API stays in milliseconds.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, TransactionBehavior};
use silph_core::Point;
use tsz::stream::{BufferedReader, BufferedWriter};
use tsz::{DataPoint, Decode, Encode, StdDecoder, StdEncoder};

/// Seconds per compacted chunk window, aligned to the epoch.
const CHUNK_WINDOW_S: i64 = 3600;
/// How often the maintenance task compacts windows and enforces retention.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub enum StoreError {
    Db(rusqlite::Error),
    Io(std::io::Error),
    /// A chunk blob failed to decode; includes the tsz decoder error.
    Codec(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Db(e) => write!(f, "{e}"),
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::Codec(e) => write!(f, "chunk decode: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<Inner>>,
    db_path: PathBuf,
    retention_s: i64,
}

struct Inner {
    conn: Connection,
    /// (metric, host, instance) -> series id, to skip the upsert per insert.
    series_ids: HashMap<(String, String, String), i64>,
}

fn open_conn(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // The app and maintenance connections briefly contend for the write lock;
    // wait it out instead of surfacing SQLITE_BUSY.
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

impl Store {
    pub fn open(data_dir: &Path, retention: Duration) -> Result<Store> {
        std::fs::create_dir_all(data_dir).map_err(StoreError::Io)?;
        let db_path = data_dir.join("silph.db");
        let conn = open_conn(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS series (
               id INTEGER PRIMARY KEY,
               metric TEXT NOT NULL,
               host TEXT NOT NULL,
               instance TEXT NOT NULL DEFAULT '',
               UNIQUE(metric, host, instance)
             );
             CREATE TABLE IF NOT EXISTS samples (
               series_id INTEGER NOT NULL,
               ts_s INTEGER NOT NULL,
               value REAL NOT NULL,
               PRIMARY KEY (series_id, ts_s)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS chunks (
               series_id INTEGER NOT NULL,
               start_s INTEGER NOT NULL,
               end_s INTEGER NOT NULL,
               data BLOB NOT NULL,
               PRIMARY KEY (series_id, start_s)
             ) WITHOUT ROWID;",
        )?;
        Ok(Store {
            inner: Arc::new(Mutex::new(Inner {
                conn,
                series_ids: HashMap::new(),
            })),
            db_path,
            retention_s: retention.as_secs() as i64,
        })
    }

    /// Open a maintenance handle on its own connection, so compaction never
    /// holds the mutex that inserts and queries share.
    pub fn maintenance(&self) -> Result<Maintenance> {
        Ok(Maintenance {
            conn: Arc::new(Mutex::new(open_conn(&self.db_path)?)),
            retention_s: self.retention_s,
        })
    }

    /// Insert one host's processed points at the given scrape timestamp.
    /// Series are labeled `host`, plus `instance` for per-resource metrics.
    pub async fn insert(&self, host: &str, ts_ms: i64, points: Vec<Point>) -> Result<()> {
        let inner = self.inner.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || inner.lock().unwrap().insert(&host, ts_ms, &points))
            .await
            .expect("storage insert task panicked")
    }

    /// Range query for one series, downsampled into `step_ms` buckets
    /// (average). Returns (bucket timestamp ms, value) pairs.
    pub async fn query(
        &self,
        metric: &str,
        host: &str,
        instance: Option<&str>,
        start_ms: i64,
        end_ms: i64,
        step_ms: i64,
    ) -> Result<Vec<(i64, f64)>> {
        let inner = self.inner.clone();
        let metric = metric.to_string();
        let host = host.to_string();
        let instance = instance.unwrap_or("").to_string();
        tokio::task::spawn_blocking(move || {
            inner
                .lock()
                .unwrap()
                .query(&metric, &host, &instance, start_ms, end_ms, step_ms)
        })
        .await
        .expect("storage query task panicked")
    }

    /// Flush and close the engine; call once on shutdown.
    pub fn close(&self) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        // Fold the WAL back into the main database file; best-effort.
        let _ = inner
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    #[cfg(test)]
    fn compact_at(&self, now_s: i64) -> Result<()> {
        compact_and_prune(
            &mut self.inner.lock().unwrap().conn,
            self.retention_s,
            now_s,
        )
    }
}

/// Background compaction + retention, running on a dedicated connection.
pub struct Maintenance {
    conn: Arc<Mutex<Connection>>,
    retention_s: i64,
}

impl Maintenance {
    /// Compact completed windows and enforce retention every `interval`,
    /// forever. The first pass runs immediately, so retention holds even
    /// under restart churn or when no inserts arrive. Errors are logged and
    /// never kill the loop.
    pub async fn run(self, interval: Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let conn = self.conn.clone();
            let retention_s = self.retention_s;
            let result = tokio::task::spawn_blocking(move || {
                compact_and_prune(&mut conn.lock().unwrap(), retention_s, now_s())
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "storage maintenance pass failed"),
                Err(e) => tracing::error!(error = %e, "storage maintenance task panicked"),
            }
        }
    }

    #[cfg(test)]
    fn compact_at(&self, now_s: i64) -> Result<()> {
        compact_and_prune(&mut self.conn.lock().unwrap(), self.retention_s, now_s)
    }
}

impl Inner {
    fn insert(&mut self, host: &str, ts_ms: i64, points: &[Point]) -> Result<()> {
        let ts_s = ts_ms.div_euclid(1000);
        // New series ids stay local until the commit succeeds: a rolled-back
        // transaction must not leave cache entries for rows it never created.
        let mut pending: HashMap<(String, String, String), i64> = HashMap::new();
        let tx = self.conn.transaction()?;
        for point in points {
            let instance = point.instance.as_deref().unwrap_or("");
            let key = (
                point.name.to_string(),
                host.to_string(),
                instance.to_string(),
            );
            let id = match self.series_ids.get(&key).or_else(|| pending.get(&key)) {
                Some(id) => *id,
                None => {
                    // Two portable statements instead of RETURNING (SQLite >= 3.35),
                    // since we link whatever libsqlite3 the system provides.
                    tx.prepare_cached(
                        "INSERT OR IGNORE INTO series (metric, host, instance) VALUES (?1, ?2, ?3)",
                    )?
                    .execute((point.name, host, instance))?;
                    let id = tx
                        .prepare_cached(
                            "SELECT id FROM series WHERE metric = ?1 AND host = ?2 AND instance = ?3",
                        )?
                        .query_row((point.name, host, instance), |row| row.get(0))?;
                    pending.insert(key, id);
                    id
                }
            };
            tx.prepare_cached(
                "INSERT OR REPLACE INTO samples (series_id, ts_s, value) VALUES (?1, ?2, ?3)",
            )?
            .execute((id, ts_s, point.value))?;
        }
        tx.commit()?;
        self.series_ids.extend(pending);
        Ok(())
    }

    fn query(
        &self,
        metric: &str,
        host: &str,
        instance: &str,
        start_ms: i64,
        end_ms: i64,
        step_ms: i64,
    ) -> Result<Vec<(i64, f64)>> {
        let id: i64 = match self
            .conn
            .prepare_cached(
                "SELECT id FROM series WHERE metric = ?1 AND host = ?2 AND instance = ?3",
            )?
            .query_row((metric, host, instance), |row| row.get(0))
        {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };

        // Ceil the start so a mid-second start never admits an earlier sample.
        let start_s = (start_ms + 999).div_euclid(1000);
        let end_s = end_ms.div_euclid(1000);
        // Keyed by timestamp so raw samples override chunk points, mirroring
        // compact_and_prune's merge semantics.
        let mut points: BTreeMap<i64, f64> = BTreeMap::new();

        let blobs: Vec<Vec<u8>> = self
            .conn
            .prepare_cached(
                "SELECT data FROM chunks
                 WHERE series_id = ?1 AND end_s >= ?2 AND start_s <= ?3",
            )?
            .query_map((id, start_s, end_s), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for blob in blobs {
            points.extend(
                decode_chunk(&blob)?
                    .into_iter()
                    .filter(|(ts, _)| (start_s..=end_s).contains(ts)),
            );
        }

        self.conn
            .prepare_cached(
                "SELECT ts_s, value FROM samples
                 WHERE series_id = ?1 AND ts_s >= ?2 AND ts_s <= ?3",
            )?
            .query_map((id, start_s, end_s), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?
            .try_for_each(|row| {
                row.map(|(ts, value)| {
                    points.insert(ts, value);
                })
            })?;

        // Average into step_ms buckets aligned to epoch multiples of step,
        // which is the axis api.rs builds.
        let step_ms = step_ms.max(1);
        let mut buckets: BTreeMap<i64, (f64, u32)> = BTreeMap::new();
        for (ts_s, value) in points {
            let ts_ms = ts_s * 1000;
            let bucket = ts_ms.div_euclid(step_ms) * step_ms;
            let entry = buckets.entry(bucket).or_insert((0.0, 0));
            entry.0 += value;
            entry.1 += 1;
        }
        Ok(buckets
            .into_iter()
            .map(|(ts, (sum, n))| (ts, sum / n as f64))
            .collect())
    }
}

/// Encode every hourly window that ended before the current one into a tsz
/// chunk, drop the raw rows it covered, then apply retention. Each window is
/// its own short write transaction, so inserts on the app connection only
/// ever wait for one window, not the whole pass; a window that fails (e.g. a
/// corrupt chunk blob) is logged and skipped, leaving its raw rows for the
/// next pass.
fn compact_and_prune(conn: &mut Connection, retention_s: i64, now_s: i64) -> Result<()> {
    let cutoff = now_s.div_euclid(CHUNK_WINDOW_S) * CHUNK_WINDOW_S;
    let windows: Vec<(i64, i64)> = conn
        .prepare_cached("SELECT DISTINCT series_id, ts_s / ?1 FROM samples WHERE ts_s < ?2")?
        .query_map((CHUNK_WINDOW_S, cutoff), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    for (series_id, window) in windows {
        if let Err(e) = compact_window(conn, series_id, window) {
            tracing::warn!(series_id, window, error = %e, "window compaction failed; skipping");
        }
    }

    let retention_cutoff = now_s - retention_s;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.prepare_cached("DELETE FROM chunks WHERE end_s < ?1")?
        .execute([retention_cutoff])?;
    tx.prepare_cached("DELETE FROM samples WHERE ts_s < ?1")?
        .execute([retention_cutoff])?;
    tx.commit()?;
    Ok(())
}

fn compact_window(conn: &mut Connection, series_id: i64, window: i64) -> Result<()> {
    let (start_s, end_s) = (window * CHUNK_WINDOW_S, (window + 1) * CHUNK_WINDOW_S);
    // Immediate: this transaction reads then writes, and a deferred
    // read-to-write upgrade can fail with SQLITE_BUSY_SNAPSHOT, which
    // busy_timeout does not retry.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut samples: BTreeMap<i64, f64> = BTreeMap::new();
    // Late samples for an already-compacted window: merge, don't clobber.
    let existing: Option<Vec<u8>> = tx
        .prepare_cached("SELECT data FROM chunks WHERE series_id = ?1 AND start_s = ?2")?
        .query_row((series_id, start_s), |row| row.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    if let Some(blob) = existing {
        for (ts, value) in decode_chunk(&blob)? {
            samples.insert(ts, value);
        }
    }
    tx.prepare_cached(
        "SELECT ts_s, value FROM samples
         WHERE series_id = ?1 AND ts_s >= ?2 AND ts_s < ?3",
    )?
    .query_map((series_id, start_s, end_s), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    })?
    .try_for_each(|row| {
        row.map(|(ts, value)| {
            samples.insert(ts, value);
        })
    })?;
    if samples.is_empty() {
        return Ok(());
    }

    let blob = encode_chunk(start_s, &samples);
    tx.prepare_cached(
        "INSERT OR REPLACE INTO chunks (series_id, start_s, end_s, data)
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute((series_id, start_s, end_s, blob))?;
    tx.prepare_cached("DELETE FROM samples WHERE series_id = ?1 AND ts_s >= ?2 AND ts_s < ?3")?
        .execute((series_id, start_s, end_s))?;
    tx.commit()?;
    Ok(())
}

fn encode_chunk(start_s: i64, samples: &BTreeMap<i64, f64>) -> Vec<u8> {
    let mut encoder = StdEncoder::new(start_s.max(0) as u64, BufferedWriter::new());
    for (&ts, &value) in samples {
        encoder.encode(DataPoint::new(ts.max(0) as u64, value));
    }
    encoder.close().to_vec()
}

fn decode_chunk(blob: &[u8]) -> Result<Vec<(i64, f64)>> {
    let mut decoder = StdDecoder::new(BufferedReader::new(blob.to_vec().into_boxed_slice()));
    let mut points = Vec::new();
    loop {
        match decoder.next() {
            Ok(dp) => points.push((dp.get_time() as i64, dp.get_value())),
            Err(tsz::decode::Error::EndOfStream) => break,
            Err(e) => return Err(StoreError::Codec(format!("{e:?}"))),
        }
    }
    Ok(points)
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(retention: Duration) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), retention).unwrap();
        (dir, store)
    }

    fn point(name: &'static str, value: f64) -> Point {
        Point::new(name, value)
    }

    #[tokio::test]
    async fn insert_query_round_trip() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        for i in 0..10i64 {
            let pts = vec![
                point("cpu", i as f64),
                Point::with_instance("disk_used", "/", 100.0 + i as f64),
            ];
            store.insert("web-1", i * 15_000, pts).await.unwrap();
        }

        let cpu = store
            .query("cpu", "web-1", None, 0, 200_000, 15_000)
            .await
            .unwrap();
        assert_eq!(cpu.len(), 10);
        assert_eq!(cpu[0], (0, 0.0));
        assert_eq!(cpu[9], (135_000, 9.0));

        let disk = store
            .query("disk_used", "web-1", Some("/"), 0, 200_000, 15_000)
            .await
            .unwrap();
        assert_eq!(disk.len(), 10);
        assert_eq!(disk[3].1, 103.0);

        // Wrong host / missing instance finds nothing.
        assert!(
            store
                .query("cpu", "web-2", None, 0, 200_000, 15_000)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .query("disk_used", "web-1", None, 0, 200_000, 15_000)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failed_commit_does_not_poison_series_cache() {
        let (_dir, store) = store(Duration::from_secs(86400));
        // Abort the next commit: the series row rolls back, and the cache must
        // not keep an id pointing at it.
        store
            .inner
            .lock()
            .unwrap()
            .conn
            .commit_hook(Some(|| true))
            .unwrap();
        assert!(
            store
                .insert("web-1", 1000, vec![point("cpu", 1.0)])
                .await
                .is_err()
        );
        store
            .inner
            .lock()
            .unwrap()
            .conn
            .commit_hook(None::<fn() -> bool>)
            .unwrap();
        store
            .insert("web-1", 2000, vec![point("cpu", 2.0)])
            .await
            .unwrap();
        let res = store
            .query("cpu", "web-1", None, 0, 60_000, 1_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(2000, 2.0)]);
    }

    #[tokio::test]
    async fn downsampling_averages_buckets() {
        let (_dir, store) = store(Duration::from_secs(86400));
        for i in 0..4i64 {
            store
                .insert("web-1", i * 15_000, vec![point("cpu", i as f64)])
                .await
                .unwrap();
        }
        // One 60s bucket averaging 0,1,2,3.
        let res = store
            .query("cpu", "web-1", None, 0, 59_000, 60_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(0, 1.5)]);
    }

    #[tokio::test]
    async fn query_start_excludes_partial_leading_second() {
        let (_dir, store) = store(Duration::from_secs(86400));
        store
            .insert("web-1", 0, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        // A start inside second 0 must not pull in the sample at 0 ms.
        let res = store
            .query("cpu", "web-1", None, 500, 10_000, 1_000)
            .await
            .unwrap();
        assert!(
            res.is_empty(),
            "sample before requested start leaked in: {res:?}"
        );
        // Exact-boundary start still includes the sample.
        store
            .insert("web-1", 1000, vec![point("cpu", 2.0)])
            .await
            .unwrap();
        let res = store
            .query("cpu", "web-1", None, 1000, 10_000, 1_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(1000, 2.0)]);
    }

    #[tokio::test]
    async fn raw_sample_overrides_compacted_point() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        store.compact_at(2 * 3600).unwrap();
        // A corrected value for an already-compacted second, before the next
        // compaction pass merges it: the raw value must win, not average in.
        store
            .insert("web-1", 10_000, vec![point("cpu", 2.0)])
            .await
            .unwrap();
        let res = store
            .query("cpu", "web-1", None, 0, 3_600_000, 15_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(0, 2.0)]);
    }

    #[tokio::test]
    async fn same_second_insert_is_last_write_wins() {
        let (_dir, store) = store(Duration::from_secs(86400));
        store
            .insert("web-1", 1000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        store
            .insert("web-1", 1400, vec![point("cpu", 2.0)])
            .await
            .unwrap();
        // Second-precision storage: within one second the newest write wins.
        let res = store
            .query("cpu", "web-1", None, 0, 60_000, 1_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(1000, 2.0)]);
    }

    #[tokio::test]
    async fn compaction_preserves_query_results() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        // Two full hours of data on the 15s grid.
        let n = 2 * 240;
        for i in 0..n {
            store
                .insert("web-1", i * 15_000, vec![point("cpu", (i % 7) as f64)])
                .await
                .unwrap();
        }
        let before = store
            .query("cpu", "web-1", None, 0, n * 15_000, 60_000)
            .await
            .unwrap();

        // Compact as if "now" is one hour past the data: both hours complete.
        store.compact_at(3 * 3600).unwrap();
        {
            let inner = store.inner.lock().unwrap();
            let raw: i64 = inner
                .conn
                .query_row("SELECT COUNT(*) FROM samples", [], |r| r.get(0))
                .unwrap();
            let chunks: i64 = inner
                .conn
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
                .unwrap();
            assert_eq!(raw, 0, "all raw rows compacted");
            assert_eq!(chunks, 2, "one chunk per hour window");
        }

        let after = store
            .query("cpu", "web-1", None, 0, n * 15_000, 60_000)
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn late_samples_merge_into_existing_chunk() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        store.compact_at(2 * 3600).unwrap();
        // A late sample for the already-compacted first hour.
        store
            .insert("web-1", 40_000, vec![point("cpu", 2.0)])
            .await
            .unwrap();
        store.compact_at(2 * 3600).unwrap();

        let res = store
            .query("cpu", "web-1", None, 0, 3_600_000, 15_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(0, 1.0), (30_000, 2.0)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_loop_prunes_without_inserts() {
        let (_dir, store) = store(Duration::from_secs(3600));
        // Epoch-1970 data is ancient relative to the real clock the loop uses.
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        tokio::spawn(store.maintenance().unwrap().run(Duration::from_millis(30)));
        // Retention must kick in with no further inserts arriving.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let res = store
                .query("cpu", "web-1", None, 0, 60_000, 1_000)
                .await
                .unwrap();
            if res.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "maintenance loop never pruned: {res:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn insert_survives_corrupt_chunk() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0), point("mem", 5.0)])
            .await
            .unwrap();
        store.compact_at(2 * 3600).unwrap();
        store
            .inner
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE chunks SET data = X'00' WHERE series_id =
                   (SELECT id FROM series WHERE metric = 'cpu')",
                [],
            )
            .unwrap();
        // Late samples for the compacted hour: cpu's window now fails to
        // merge, but the pass keeps going and mem's window must still compact.
        store
            .insert("web-1", 40_000, vec![point("cpu", 2.0), point("mem", 6.0)])
            .await
            .unwrap();
        store.compact_at(2 * 3600).unwrap();
        store
            .insert("web-1", 50_000, vec![point("cpu", 3.0)])
            .await
            .unwrap();

        let inner = store.inner.lock().unwrap();
        let count = |metric: &str| -> i64 {
            inner
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM samples WHERE series_id =
                       (SELECT id FROM series WHERE metric = ?1)",
                    [metric],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count("mem"), 0, "healthy series still compacts");
        assert_eq!(count("cpu"), 2, "failed window's raw rows survive");
    }

    #[tokio::test]
    async fn compaction_ignores_app_mutex() {
        let (_dir, store) = store(Duration::from_secs(86400 * 30));
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        let maintenance = store.maintenance().unwrap();
        {
            // Holding the app mutex must not block compaction: it runs on its
            // own connection. (This deadlocks if compaction shares `inner`.)
            let _guard = store.inner.lock().unwrap();
            maintenance.compact_at(2 * 3600).unwrap();
        }
        let res = store
            .query("cpu", "web-1", None, 0, 3_600_000, 15_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(0, 1.0)]);
    }

    #[tokio::test]
    async fn retention_prunes_old_data() {
        let (_dir, store) = store(Duration::from_secs(3600));
        store
            .insert("web-1", 10_000, vec![point("cpu", 1.0)])
            .await
            .unwrap();
        // "now" far past the retention window: chunk is built, then pruned.
        store.compact_at(10 * 3600).unwrap();
        let res = store
            .query("cpu", "web-1", None, 0, 20 * 3_600_000, 15_000)
            .await
            .unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn chunk_format_is_stable() {
        // Frozen bytes: tsz 0.1.4 output. If this test breaks after a dep
        // bump, existing chunks on disk are unreadable — keep the exact pin
        // in Cargo.toml or write a migration; do not just update the bytes.
        const GOLDEN_BLOB: &[u8] = &[
            0, 0, 0, 0, 0, 0, 14, 16, 0, 0, 127, 224, 0, 0, 0, 0, 0, 1, 15, 193, 51, 255, 172, 3,
            96, 3, 240, 0, 0, 0, 0,
        ];
        let points = [(3600, 1.0), (3615, 2.5), (3630, -3.0)];
        let samples: BTreeMap<i64, f64> = points.iter().copied().collect();
        assert_eq!(encode_chunk(3600, &samples), GOLDEN_BLOB);
        assert_eq!(decode_chunk(GOLDEN_BLOB).unwrap(), points);
    }

    #[tokio::test]
    async fn reopen_persists_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path(), Duration::from_secs(86400)).unwrap();
            store
                .insert("web-1", 15_000, vec![point("cpu", 4.2)])
                .await
                .unwrap();
            store.close().unwrap();
        }
        let store = Store::open(dir.path(), Duration::from_secs(86400)).unwrap();
        let res = store
            .query("cpu", "web-1", None, 0, 60_000, 15_000)
            .await
            .unwrap();
        assert_eq!(res, vec![(15_000, 4.2)]);
    }
}
