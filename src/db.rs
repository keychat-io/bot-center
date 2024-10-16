use futures_util::StreamExt;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::Row;
use sqlx::SqlitePool;

use crate::EventMsg;

#[derive(Debug, Clone)]
pub struct LitePool {
    db: SqlitePool,
}

impl LitePool {
    /// https://docs.rs/sqlx-sqlite/0.7.1/sqlx_sqlite/struct.SqliteConnectOptions.html#impl-FromStr-for-SqliteConnectOptions
    pub async fn open(dbpath: &str) -> anyhow::Result<LitePool> {
        let opts = dbpath
            .parse::<SqliteConnectOptions>()?
            .create_if_missing(true)
            // .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // prevent other thread open it
            .locking_mode(sqlx::sqlite::SqliteLockingMode::Normal)
            // or normal
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        info!("SqlitePool open: {:?}", opts);
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            // .max_connections(1)
            .connect_with(opts)
            .await?;

        let it = Self { db };
        it.init().await?;

        Ok(it)
    }

    pub fn database(&self) -> &SqlitePool {
        &self.db
    }

    pub async fn init(&self) -> Result<(), anyhow::Error> {
        sqlx::migrate!("./migrations")
            .run(&self.db)
            .await
            .map_err(|e| format_err!("run sqlite migrations failed: {}", e))?;

        Ok(())
    }

    pub async fn insert_event(&self, event: &EventMsg) -> anyhow::Result<u64> {
        let sql = format!(
            "insert into events (id, ts, kind, src, dest, comfirmed, content) values(?, ?, ?, ?, ?, ?, ?)
            ;",
        );

        let ts = event.ts as i64;
        let rows = sqlx::query(&sql)
            .bind(&event.id)
            .bind(&ts)
            .bind(&event.kind)
            .bind(&event.from)
            .bind(&event.to)
            .bind(&event.comfirmed)
            .bind(&event.content)
            .execute(&self.db)
            .await
            .map(|a| a.rows_affected())?;
        Ok(rows)
    }

    pub async fn get_events_ts_max(&self, dest: &str) -> anyhow::Result<u64> {
        let sql = format!("select max(ts) from events where dest=?;",);

        let rows = sqlx::query(&sql)
            .bind(dest)
            .fetch_optional(&self.db)
            .await?;

        let ts = rows
            .map(|s| s.get::<'_, i64, _>(0) as u64)
            .unwrap_or_default();

        Ok(ts)
    }

    pub async fn get_events(&self, dests: &[&str]) -> anyhow::Result<Vec<EventMsg>> {
        let slice = dests
            .iter()
            .map(|s| format!("'{}'", s))
            .collect::<Vec<_>>()
            .join(",");

        let sql = format!(
            "select id, ts, kind, src, dest, comfirmed, content from events where comfirmed=false and dest in ({}) order by ts;",
            slice
        );

        let mut rows = sqlx::query(&sql).fetch(&self.db);

        let mut txs = vec![];
        while let Some(it) = rows.next().await {
            let it = it?;
            let tx = EventMsg {
                id: it.get(0),
                ts: u64::try_from(it.get::<'_, i64, _>(1))?,
                kind: u16::try_from(it.get::<'_, i32, _>(2))?,
                from: it.get(3),
                to: it.get(4),
                comfirmed: it.get(5),
                content: it.get(6),
            };
            txs.push(tx);
        }

        Ok(txs)
    }

    pub async fn update_event(&self, ids: &[&str], comfirmed: bool) -> anyhow::Result<u64> {
        let slice = ids
            .iter()
            .map(|s| format!("'{}'", s))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "update events set comfirmed=? where id in ({})
            ;",
            slice,
        );

        let rows = sqlx::query(&sql).bind(&comfirmed).execute(&self.db).await?;
        Ok(rows.rows_affected())
    }
}
