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

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub nostrid: String,
    pub ts: u64,
    pub name: String,
    pub pubkey: String,
    pub onetimekey: String,

}

impl LitePool {
    pub async fn insert_session(
        &self,
        id: &str,
        ts: u64,
        name: &str,
        pubkey: &str,
        onetimekey: &str,
    ) -> anyhow::Result<u64> {
        let sql = format!(
            "insert into sessions (id, ts, name, pubkey, onetimekey) values(?, ?, ?, ?, ?)
            ;",
        );

        let ts = ts as i64;
        let rows = sqlx::query(&sql)
            .bind(id)
            .bind(&ts)
            .bind(name)
            .bind(pubkey)
            .bind(onetimekey)
            .execute(&self.db)
            .await
            .map(|a| a.rows_affected())?;
        Ok(rows)
    }

    pub async fn get_session(&self, nostrid: &str) -> anyhow::Result<Option<Session>> {
        let sql = 
            "select id, ts, name, pubkey, onetimekey from sessions where id=? order by ts desc limit 1;";

       if let Some(it) = sqlx::query(&sql).fetch_optional(&self.db).await? {
        let se = Session {
            nostrid: it.get(0),
            ts: u64::try_from(it.get::<'_, i64, _>(1))?,
            name: it.get(2),
            pubkey: it.get(3),
            onetimekey: it.get(4),
        };

            return Ok(Some(se));
       }

       Ok(None)
    }

    pub async fn take_onetimekey(&self, session: &Session) -> anyhow::Result<u64> {
        let sql =
            "update sessions set onetimekey=? where id=? and onetimekey!=''
            ;";

        let rows = sqlx::query(&sql).bind(&session.nostrid).execute(&self.db).await?;
        Ok(rows.rows_affected())
    }
}

impl LitePool {
    pub async fn insert_receiver(
        &self,
        id: &str,
        ts: u64,
        pubkey: &str,
        address: &str,
    ) -> anyhow::Result<u64> {
        let sql = format!(
            "insert into receivers (id, ts, pubkey, address) values(?, ?, ?, ?)
            ;",
        );

        let ts = ts as i64;
        let rows = sqlx::query(&sql)
            .bind(id)
            .bind(&ts)
            .bind(pubkey)
            .bind(address)
            .execute(&self.db)
            .await
            .map(|a| a.rows_affected())?;

        let sql = "
        with logs as (select * from receivers where id = ? order by ts desc),
        old as (select id from logs where ts < (select min(ts) from (select * from logs limit 3)))
        delete from receivers where id in old;";
        let keep =  sqlx::query(&sql)
        .bind(id)
        .execute(&self.db)
        .await
        .map(|e|e.rows_affected());
        debug!("keep n signal receivers for {}: {:?}",id, keep);

        Ok(rows)
    }

    /// id, ts, pubkey
    pub async fn get_receiver(&self, address: &str) -> anyhow::Result<Option<(String, u64, String)>> {
        let sql = 
            "select id, ts, pubkey, address from receivers where address=? limit 1";

       if let Some(it) = sqlx::query(&sql)
       .bind(address).fetch_optional(&self.db).await? {
        let se =(
            it.get(0),
            u64::try_from(it.get::<'_, i64, _>(1))?,
            it.get(2),
        );

            return Ok(Some(se));
       }

       Ok(None)
    }

    pub async fn remove(&self, nostrid: &str) -> anyhow::Result<u64> {
        let sql =
        "delete from receivers where id=?
        ;";

        let rows = sqlx::query(&sql).bind(nostrid).execute(&self.db).await?;
        Ok(rows.rows_affected())
    }
}
