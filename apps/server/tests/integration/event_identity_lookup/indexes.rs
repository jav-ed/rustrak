use super::support::{timestamp, Fixture};
use serde_json::json;
use sqlx::Row;
use std::time::Instant;
use uuid::Uuid;

async fn drop_indexes(f: &Fixture) {
    #[cfg(feature = "sqlite")]
    let migrations = [include_str!(
        "../../../migrations/sqlite/20260930000000_event_identity_lookup.down.sql"
    )]
    .as_slice();
    #[cfg(feature = "postgres")]
    let migrations = [
        include_str!("../../../migrations/postgres/20260930000001_event_request_lookup.down.sql"),
        include_str!("../../../migrations/postgres/20260930000000_event_user_lookup.down.sql"),
    ]
    .as_slice();
    for sql in migrations {
        sqlx::raw_sql(*sql).execute(&f.db.pool).await.unwrap();
    }
}

async fn create_indexes(f: &Fixture) {
    #[cfg(feature = "sqlite")]
    let migrations = [include_str!(
        "../../../migrations/sqlite/20260930000000_event_identity_lookup.up.sql"
    )]
    .as_slice();
    #[cfg(feature = "postgres")]
    let migrations = [
        include_str!("../../../migrations/postgres/20260930000000_event_user_lookup.up.sql"),
        include_str!("../../../migrations/postgres/20260930000001_event_request_lookup.up.sql"),
    ]
    .as_slice();
    for sql in migrations {
        sqlx::raw_sql(*sql).execute(&f.db.pool).await.unwrap();
    }
}

#[actix_web::test]
async fn historical_sdk_values_survive_index_creation_and_rollback() {
    let f = Fixture::new().await;
    drop_indexes(&f).await;
    let large = (0..10000).map(|n| format!("{n:08x}")).collect::<String>();
    for value in [
        json!(large),
        json!("é".repeat(100)),
        json!("é".repeat(101)),
        json!(55),
        json!(true),
        json!(null),
        json!({"id":"object"}),
        json!(["array"]),
    ] {
        f.insert(
            f.project,
            None,
            json!({"user":{"id":value},"tags":{"request.id":value}}),
        )
        .await;
    }
    create_indexes(&f).await;
    #[cfg(feature = "postgres")]
    {
        let valid: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_index WHERE indexrelid IN ('idx_events_project_user_identity'::regclass, 'idx_events_project_request_identity'::regclass) AND indisvalid")
            .fetch_one(&f.db.pool).await.unwrap();
        assert_eq!(valid, 2);
    }
    // Reversible migrations must not alter stored payloads, including unindexed IDs.
    drop_indexes(&f).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE project_id = $1")
        .bind(f.project)
        .fetch_one(&f.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 8);
    create_indexes(&f).await;
}

#[actix_web::test]
async fn first_and_cursor_pages_use_identity_indexes_without_a_sort() {
    let f = Fixture::new().await;
    let mut tx = f.db.pool.begin().await.unwrap();
    for n in 0..2000 {
        let value = format!("identity-{}", n % 200);
        sqlx::query("INSERT INTO events(id,event_id,project_id,data,timestamp,ingested_at) VALUES($1,$2,$3,$4,$5,$5)")
            .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(f.project)
            .bind(json!({"user":{"id":value},"tags":{"request.id":value}}))
            .bind(timestamp()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE events")
        .execute(&f.db.pool)
        .await
        .unwrap();
    #[cfg(feature = "sqlite")]
    let selectors = [
        ("json_extract(data, '$.user.id')", "json_type(data, '$.user.id') = 'text' AND length(CAST(json_extract(data, '$.user.id') AS BLOB)) BETWEEN 1 AND 200", "idx_events_project_user_identity"),
        (r#"json_extract(data, '$.tags."request.id"')"#, r#"json_type(data, '$.tags."request.id"') = 'text' AND length(CAST(json_extract(data, '$.tags."request.id"') AS BLOB)) BETWEEN 1 AND 200"#, "idx_events_project_request_identity"),
    ];
    #[cfg(feature = "postgres")]
    let selectors = [
        ("(data #>> '{user,id}')", "jsonb_typeof(data #> '{user,id}') = 'string' AND octet_length(data #>> '{user,id}') BETWEEN 1 AND 200", "idx_events_project_user_identity"),
        ("(data #>> '{tags,request.id}')", "jsonb_typeof(data #> '{tags,request.id}') = 'string' AND octet_length(data #>> '{tags,request.id}') BETWEEN 1 AND 200", "idx_events_project_request_identity"),
    ];
    for (field, guard, index) in selectors {
        for with_cursor in [false, true] {
            #[cfg(feature = "sqlite")]
            let explain = "EXPLAIN QUERY PLAN";
            #[cfg(feature = "postgres")]
            let explain = "EXPLAIN (ANALYZE, BUFFERS)";
            let boundary = if with_cursor {
                "AND (timestamp, id) < ($4, $5)"
            } else {
                ""
            };
            // Match the HTTP projection: a narrow SELECT id can hide sort/read costs.
            let sql = format!("{explain} SELECT id,event_id,issue_id,timestamp,calculated_type,calculated_value,level,platform,release,environment,event_type FROM events WHERE project_id=$1 AND {field}=$2 AND {guard} {boundary} ORDER BY timestamp DESC,id DESC LIMIT $3");
            let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(f.project)
                .bind("identity-0")
                .bind(21_i64);
            let rows = if with_cursor {
                query
                    .bind(timestamp())
                    .bind(Uuid::max())
                    .fetch_all(&f.db.pool)
                    .await
                    .unwrap()
            } else {
                query.fetch_all(&f.db.pool).await.unwrap()
            };
            #[cfg(feature = "sqlite")]
            let column = "detail";
            #[cfg(feature = "postgres")]
            let column = "QUERY PLAN";
            let plan = rows
                .iter()
                .map(|row| row.get::<String, _>(column))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(plan.contains(index), "{plan}");
            assert!(
                !plan.contains("Sort") && !plan.contains("TEMP B-TREE"),
                "{plan}"
            );
            println!("{index}, cursor={with_cursor}: {plan}");
        }
    }
}

#[actix_web::test]
#[ignore = "Explicit synthetic write/storage measurement; no timing threshold in CI"]
async fn synthetic_write_and_storage_cost() {
    let f = Fixture::new().await;
    drop_indexes(&f).await;
    let mut elapsed = Vec::new();
    let mut extra_bytes: i64 = 0;
    for indexed in [false, true] {
        let start = Instant::now();
        let mut tx = f.db.pool.begin().await.unwrap();
        for n in 0..5000 {
            let data = json!({"user":{"id":format!("user-{}", n % 100)},"tags":{"request.id":format!("request-{n}")},"message":"synthetic lookup benchmark"});
            sqlx::query("INSERT INTO events(id,event_id,project_id,data,timestamp,ingested_at) VALUES($1,$2,$3,$4,$5,$5)")
                .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(f.project).bind(data)
                .bind(timestamp()).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        elapsed.push(start.elapsed().as_secs_f64());
        if !indexed {
            #[cfg(feature = "sqlite")]
            let before: i64 = sqlx::query_scalar("PRAGMA page_count")
                .fetch_one(&f.db.pool)
                .await
                .unwrap();
            create_indexes(&f).await;
            #[cfg(feature = "sqlite")]
            {
                let after: i64 = sqlx::query_scalar("PRAGMA page_count")
                    .fetch_one(&f.db.pool)
                    .await
                    .unwrap();
                let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
                    .fetch_one(&f.db.pool)
                    .await
                    .unwrap();
                extra_bytes = (after - before) * page_size;
            }
            #[cfg(feature = "postgres")]
            {
                extra_bytes = sqlx::query_scalar("SELECT pg_relation_size('idx_events_project_user_identity') + pg_relation_size('idx_events_project_request_identity')")
                    .fetch_one(&f.db.pool).await.unwrap();
            }
        }
    }
    assert!(extra_bytes > 0);
    println!("Synthetic 5000-row batches: no lookup indexes={:.3}s, with indexes={:.3}s; index bytes after initial 5000 rows={extra_bytes}. Single ordered trial, not a production capacity claim.", elapsed[0], elapsed[1]);
}
