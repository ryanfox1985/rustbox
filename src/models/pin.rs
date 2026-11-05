use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Serialize, sqlx::FromRow)]
pub struct Pin {
    pub id: i64,
    pub name: Option<String>,
    pub pin_pi: Option<i64>,
    pub value: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Pin {
    pub fn is_on(&self) -> bool {
        self.value == Some(true)
    }

    pub async fn all(db: &SqlitePool) -> sqlx::Result<Vec<Pin>> {
        sqlx::query_as("SELECT * FROM pins ORDER BY id")
            .fetch_all(db)
            .await
    }

    pub async fn find(db: &SqlitePool, id: i64) -> sqlx::Result<Option<Pin>> {
        sqlx::query_as("SELECT * FROM pins WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
    }

    pub async fn create(
        db: &SqlitePool,
        name: Option<String>,
        pin_pi: Option<i64>,
        value: Option<bool>,
    ) -> sqlx::Result<Pin> {
        let now = Utc::now();
        sqlx::query_as(
            "INSERT INTO pins (name, pin_pi, value, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?) RETURNING *",
        )
        .bind(name)
        .bind(pin_pi)
        .bind(value)
        .bind(now)
        .bind(now)
        .fetch_one(db)
        .await
    }

    pub async fn update(
        db: &SqlitePool,
        id: i64,
        name: Option<String>,
        pin_pi: Option<i64>,
        value: Option<bool>,
    ) -> sqlx::Result<Option<Pin>> {
        sqlx::query_as(
            "UPDATE pins SET name = ?, pin_pi = ?, value = ?, updated_at = ? \
             WHERE id = ? RETURNING *",
        )
        .bind(name)
        .bind(pin_pi)
        .bind(value)
        .bind(Utc::now())
        .bind(id)
        .fetch_optional(db)
        .await
    }

    // Returns false when there was no such pin.
    pub async fn delete(db: &SqlitePool, id: i64) -> sqlx::Result<bool> {
        let result = sqlx::query("DELETE FROM pins WHERE id = ?")
            .bind(id)
            .execute(db)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
