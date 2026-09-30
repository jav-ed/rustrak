//! Indexed exact identity lookup; identifiers never change event grouping.
use serde::{Deserialize, Serialize};

use crate::db::DbPool;
use crate::error::AppResult;
use crate::models::EventSummary;
use crate::pagination::{EventCursor, PAGE_SIZE};

/// The two supported identity sources in a stored Sentry event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LookupKind {
    User,
    Request,
}

impl LookupKind {
    fn sql_parts(self) -> (&'static str, &'static str) {
        // Keep these predicates identical to the partial-index migrations.
        // Non-string and oversized SDK values stay ingestible but unindexed.
        #[cfg(feature = "sqlite")]
        match self {
            Self::User => (
                "json_extract(data, '$.user.id')",
                "json_type(data, '$.user.id') = 'text' AND length(CAST(json_extract(data, '$.user.id') AS BLOB)) BETWEEN 1 AND 200",
            ),
            Self::Request => (
                r#"json_extract(data, '$.tags."request.id"')"#,
                r#"json_type(data, '$.tags."request.id"') = 'text' AND length(CAST(json_extract(data, '$.tags."request.id"') AS BLOB)) BETWEEN 1 AND 200"#,
            ),
        }
        #[cfg(feature = "postgres")]
        match self {
            Self::User => (
                "(data #>> '{user,id}')",
                "jsonb_typeof(data #> '{user,id}') = 'string' AND octet_length(data #>> '{user,id}') BETWEEN 1 AND 200",
            ),
            Self::Request => (
                "(data #>> '{tags,request.id}')",
                "jsonb_typeof(data #> '{tags,request.id}') = 'string' AND octet_length(data #>> '{tags,request.id}') BETWEEN 1 AND 200",
            ),
        }
    }
}

/// Return one bounded page without loading the full event JSON into Rust.
pub(crate) async fn list(
    pool: &DbPool,
    project_id: i32,
    kind: LookupKind,
    value: &str,
    cursor: Option<&EventCursor>,
) -> AppResult<(Vec<EventSummary>, bool)> {
    let (field, predicate) = kind.sql_parts();
    let boundary = if cursor.is_some() {
        "AND (timestamp, id) < ($4, $5)"
    } else {
        ""
    };
    // Only fixed SQL fragments are interpolated. All caller values are bound.
    let sql = format!(
        "SELECT id, event_id, issue_id, timestamp, calculated_type, calculated_value, \
         level, platform, release, environment, event_type FROM events \
         WHERE project_id = $1 AND {field} = $2 AND {predicate} {boundary} \
         ORDER BY timestamp DESC, id DESC LIMIT $3"
    );
    let query = sqlx::query_as::<_, EventSummary>(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(project_id)
        .bind(value)
        .bind(PAGE_SIZE + 1);
    let mut rows = if let Some(cursor) = cursor {
        query
            .bind(cursor.last_timestamp)
            .bind(cursor.last_id)
            .fetch_all(pool)
            .await?
    } else {
        query.fetch_all(pool).await?
    };
    let has_more = rows.len() > PAGE_SIZE as usize;
    rows.truncate(PAGE_SIZE as usize);
    Ok((rows, has_more))
}
