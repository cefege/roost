//! Durable cursor-ordered chat batches and their per-replica bus relay.
//! The database is the source of truth; notices only advance local delivery,
//! and `chat_delivery` keeps the relay and the direct path from repeating a
//! batch.

use std::sync::Arc;

use super::chat_delivery::ChatDelivery;
use crate::cluster::notify::{CHAT_CHANNEL, ClusterNotifier, Notice};
use crate::db::{CoordDb, DbBackend};

#[derive(Debug, Clone)]
pub struct ChatEventRow {
    pub cursor: u64,
    pub events_json: String,
}

pub async fn append(
    db: &CoordDb,
    conversation_id: &str,
    events_json: &str,
    run_start: bool,
) -> Result<u64, sqlx::Error> {
    let mut transaction = match db.backend() {
        DbBackend::Sqlite => db.begin_write().await?,
        DbBackend::Postgres => db.pool().begin().await?,
    };
    if db.backend() == DbBackend::Postgres {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat:' || $1, 0))")
            .bind(conversation_id)
            .execute(&mut *transaction)
            .await?;
    }
    let cursor: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(cursor), 0) + 1 FROM agent_chat_events WHERE conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *transaction)
    .await?;
    sqlx::query("INSERT INTO agent_chat_events(conversation_id, cursor, events_json, run_start, created_ms) VALUES($1,$2,$3,$4,$5)")
        .bind(conversation_id)
        .bind(cursor)
        .bind(events_json)
        .bind(i64::from(run_start))
        .bind(now_ms())
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(u64::try_from(cursor).unwrap_or_default())
}

pub async fn read_after(
    db: &CoordDb,
    conversation_id: &str,
    after_cursor: u64,
    limit: u32,
) -> Result<(Vec<ChatEventRow>, bool), sqlx::Error> {
    let mut transaction = db.pool().begin().await?;
    if db.backend() == DbBackend::Postgres {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *transaction)
            .await?;
    }
    let minimum: Option<i64> =
        sqlx::query_scalar("SELECT MIN(cursor) FROM agent_chat_events WHERE conversation_id=$1")
            .bind(conversation_id)
            .fetch_one(&mut *transaction)
            .await?;
    let truncated =
        minimum.is_some_and(|minimum| i128::from(after_cursor) + 1 < i128::from(minimum));
    let rows = sqlx::query_as::<_, (i64, String)>("SELECT cursor, events_json FROM agent_chat_events WHERE conversation_id=$1 AND cursor>$2 ORDER BY cursor LIMIT $3")
        .bind(conversation_id).bind(i64::try_from(after_cursor).unwrap_or(i64::MAX)).bind(i64::from(limit.min(500)))
        .fetch_all(&mut *transaction).await?;
    transaction.commit().await?;
    Ok((
        rows.into_iter()
            .map(|(cursor, events_json)| ChatEventRow {
                cursor: u64::try_from(cursor).unwrap_or_default(),
                events_json,
            })
            .collect(),
        truncated,
    ))
}

pub async fn snapshot_rows(
    db: &CoordDb,
    conversation_id: &str,
) -> Result<(u64, Vec<ChatEventRow>), sqlx::Error> {
    let mut transaction = db.pool().begin().await?;
    if db.backend() == DbBackend::Postgres {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *transaction)
            .await?;
    }
    let cursor: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(cursor), 0) FROM agent_chat_events WHERE conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *transaction)
    .await?;
    let start = sqlx::query_scalar::<_, i64>("SELECT cursor FROM agent_chat_events WHERE conversation_id=$1 AND cursor <= $2 AND run_start=1 ORDER BY cursor DESC LIMIT 1")
        .bind(conversation_id).bind(cursor).fetch_optional(&mut *transaction).await?;
    let batches = if let Some(start) = start {
        sqlx::query_as::<_, (i64, String)>("SELECT cursor, events_json FROM agent_chat_events WHERE conversation_id=$1 AND cursor >= $2 AND cursor <= $3 ORDER BY cursor")
            .bind(conversation_id).bind(start).bind(cursor).fetch_all(&mut *transaction).await?
    } else {
        Vec::new()
    };
    transaction.commit().await?;
    Ok((
        u64::try_from(cursor).unwrap_or_default(),
        batches
            .into_iter()
            .map(|(cursor, events_json)| ChatEventRow {
                cursor: u64::try_from(cursor).unwrap_or_default(),
                events_json,
            })
            .collect(),
    ))
}

pub async fn prune(db: &CoordDb) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM agent_chat_events WHERE created_ms < $1 AND cursor < (SELECT MAX(latest.cursor) FROM agent_chat_events latest WHERE latest.conversation_id=agent_chat_events.conversation_id)")
        .bind(now_ms().saturating_sub(24 * 60 * 60 * 1000)).execute(db.pool()).await?;
    Ok(result.rows_affected())
}

/// Relays durable chat batches in cursor order, serializing concurrent notices.
/// A conversation is relayed from the first cursor a notice names; history
/// before that reaches a client through its snapshot, never through the bus.
/// A batch the writer already published directly is not published again.
pub async fn relay(notifier: Arc<ClusterNotifier>, db: CoordDb, delivery: Arc<ChatDelivery>) {
    let mut notices = notifier.subscribe();
    loop {
        let notice = match notices.recv().await {
            Ok(notice) if notice.channel == CHAT_CHANNEL => notice,
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(
                    missed,
                    "chat event notices lagged; resynchronizing relayed conversations"
                );
                Notice {
                    channel: CHAT_CHANNEL.to_owned(),
                    payload: String::new(),
                    resync: true,
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        let mut targets = Vec::new();
        if notice.resync {
            targets.extend(delivery.conversations());
        } else if let Some((id, first)) = notice.payload.rsplit_once(':') {
            let Ok(first) = first.parse::<u64>() else {
                continue;
            };
            delivery.start_at(id, first);
            targets.push(id.to_owned());
        }
        for id in targets {
            loop {
                let after = delivery.last_delivered(&id).unwrap_or(0);
                match read_after(&db, &id, after, 500).await {
                    Ok((rows, _)) => {
                        let batch_count = rows.len();
                        if batch_count == 0 {
                            break;
                        }
                        let mut expected = after.saturating_add(1);
                        for row in rows {
                            if row.cursor != expected {
                                tracing::warn!(conversation_id = %id, expected, actual = row.cursor, "chat event cursor gap while relaying");
                            }
                            expected = row.cursor.saturating_add(1);
                            delivery.deliver(&id, row.cursor, row.events_json);
                        }
                        if batch_count < 500 {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(conversation_id = %id, %error, "chat event relay read failed");
                        break;
                    }
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
