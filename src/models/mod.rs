use bigdecimal::BigDecimal;
use futures::future::try_join_all;
use near_indexer_primitives::views::ExecutionStatusView;

use num_traits::{ToPrimitive, Zero};
use sqlx::{Arguments, Row};

pub(crate) use indexer_balances::FieldCount;
pub(crate) mod balance_changes;
mod serializers;

pub trait FieldCount {
    /// Get the number of fields on a struct.
    fn field_count() -> usize;
}

pub trait SqlxMethods {
    fn add_to_args(&self, args: &mut sqlx::postgres::PgArguments);

    fn insert_query(count: usize) -> anyhow::Result<String>;

    fn name() -> String;
}

pub async fn chunked_insert<T: SqlxMethods + std::fmt::Debug>(
    pool: &sqlx::Pool<sqlx::Postgres>,
    items: &[T],
    retry_count: usize,
) -> anyhow::Result<()> {
    let futures = items
        .chunks(crate::db_adapters::CHUNK_SIZE_FOR_BATCH_INSERT)
        .map(|items_part| insert_retry_or_panic(pool, items_part, retry_count));
    try_join_all(futures).await.map(|_| ())
}

async fn insert_retry_or_panic<T: SqlxMethods + std::fmt::Debug>(
    pool: &sqlx::Pool<sqlx::Postgres>,
    items: &[T],
    retry_count: usize,
) -> anyhow::Result<()> {
    let mut interval = crate::INTERVAL;
    let mut retry_attempt = 0usize;
    let query = T::insert_query(items.len())?;

    loop {
        if retry_attempt == retry_count {
            return Err(anyhow::anyhow!(
                "Failed to perform query to database after {} attempts. Stop trying.",
                retry_count
            ));
        }
        retry_attempt += 1;

        let mut args = sqlx::postgres::PgArguments::default();
        for item in items {
            item.add_to_args(&mut args);
        }

        match sqlx::query_with(&query, args).execute(pool).await {
            Ok(_) => break,
            Err(async_error) => {
                tracing::error!(
                         target: crate::INDEXER,
                         "Error occurred during {}:\n{} were not stored. \n{:#?} \n Retrying in {} milliseconds...",
                         async_error,
                         &T::name(),
                         &items,
                         interval.as_millis(),
                     );
                tokio::time::sleep(interval).await;
                if interval < crate::MAX_DELAY_TIME {
                    interval *= 2;
                }
            }
        }
    }
    Ok(())
}

pub async fn select_retry_or_panic(
    pool: &sqlx::Pool<sqlx::Postgres>,
    query: &str,
    substitution_items: &[String],
    retry_count: usize,
) -> anyhow::Result<Vec<sqlx::postgres::PgRow>> {
    let mut interval = crate::INTERVAL;
    let mut retry_attempt = 0usize;

    loop {
        if retry_attempt == retry_count {
            return Err(anyhow::anyhow!(
                "Failed to perform query to database after {} attempts. Stop trying.",
                retry_count
            ));
        }
        retry_attempt += 1;

        let mut args = sqlx::postgres::PgArguments::default();
        for item in substitution_items {
            args.add(item);
        }

        match sqlx::query_with(query, args).fetch_all(pool).await {
            Ok(res) => return Ok(res),
            Err(async_error) => {
                // todo we print here select with non-filled placeholders. It would be better to get the final select statement here
                tracing::error!(
                         target: crate::INDEXER,
                         "Error occurred during {}:\nFailed SELECT:\n{}\n Retrying in {} milliseconds...",
                         async_error,
                    query,
                         interval.as_millis(),
                     );
                tokio::time::sleep(interval).await;
                if interval < crate::MAX_DELAY_TIME {
                    interval *= 2;
                }
            }
        }
    }
}

pub(crate) async fn start_after_interruption(
    pool: &sqlx::Pool<sqlx::Postgres>,
) -> anyhow::Result<u64> {
    let query = "SELECT block_height
                        FROM blocks
                        ORDER BY block_timestamp desc
                        LIMIT 1";

    let res = select_retry_or_panic(pool, query, &[], 10).await?;
    Ok(res
        .first()
        .map(|value| value.get(0))
        .unwrap_or_else(BigDecimal::zero)
        .to_u64()
        .expect("height should be positive"))
}

// Generates `($1, $2, $3), ($4, $5, $6)`
fn create_placeholders_chain(
    mut items_count: usize,
    fields_count: usize,
) -> anyhow::Result<String> {
    if items_count < 1 {
        return Err(anyhow::anyhow!("At least 1 item expected"));
    }

    let mut start_num: usize = 1;
    let mut res = create_placeholder(&mut start_num, fields_count)?;
    items_count -= 1;
    while items_count > 0 {
        let placeholder = create_placeholder(&mut start_num, fields_count)?;
        res += ", ";
        res += &placeholder;
        items_count -= 1;
    }

    Ok(res)
}

// Generates `($1, $2, $3)`
fn create_placeholder(start_num: &mut usize, mut fields_count: usize) -> anyhow::Result<String> {
    if fields_count < 1 {
        return Err(anyhow::anyhow!("At least 1 field expected"));
    }
    let mut item = format!("(${}", start_num);
    *start_num += 1;
    fields_count -= 1;
    while fields_count > 0 {
        item += &format!(", ${}", start_num);
        *start_num += 1;
        fields_count -= 1;
    }
    item += ")";
    Ok(item)
}

pub(crate) trait PrintEnum {
    fn print(&self) -> &str;
}

impl PrintEnum for ExecutionStatusView {
    fn print(&self) -> &str {
        match self {
            ExecutionStatusView::Unknown => "UNKNOWN",
            ExecutionStatusView::Failure(_) => "FAILURE",
            ExecutionStatusView::SuccessValue(_) => "SUCCESS",
            ExecutionStatusView::SuccessReceiptId(_) => "SUCCESS",
        }
    }
}

pub(crate) enum Direction {
    Inbound,
    Outbound,
}

impl PrintEnum for Direction {
    fn print(&self) -> &str {
        match self {
            Direction::Inbound => "INBOUND",
            Direction::Outbound => "OUTBOUND",
        }
    }
}

pub(crate) enum Cause {
    ValidatorsReward,
    Transaction,
    Receipt,
    ContractReward,
}

impl PrintEnum for Cause {
    fn print(&self) -> &str {
        match self {
            Cause::ValidatorsReward => "VALIDATORS_REWARD",
            Cause::Transaction => "TRANSACTION",
            Cause::Receipt => "RECEIPT",
            Cause::ContractReward => "CONTRACT_REWARD",
        }
    }
}
