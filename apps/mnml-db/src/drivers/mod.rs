//! Driver adapters — one submodule per engine.
//!
//! Each submodule is gated on its feature flag, and wraps the
//! engine-specific driver crate's concrete types into the neutral
//! `crate::driver::Driver` trait.

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "mariadb")]
pub mod mariadb;

#[cfg(feature = "clickhouse")]
pub mod clickhouse;

#[cfg(feature = "redshift")]
pub mod redshift;

#[cfg(feature = "docdb")]
pub mod docdb;

#[cfg(feature = "dynamodb")]
pub mod dynamodb;

use anyhow::{Result, anyhow};

use crate::connection::ConnectionSpec;
use crate::driver::Driver;

/// Connect to a spec and return a boxed neutral `Driver`. This is
/// the single funnel where the engine name becomes a concrete type
/// — nothing above this line ever branches on the engine.
pub async fn connect(spec: &ConnectionSpec) -> Result<Box<dyn Driver>> {
    match spec.engine.as_str() {
        #[cfg(feature = "postgres")]
        "postgres" => {
            let d = postgres::PgAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "redis")]
        "redis" => {
            let d = redis::RedisAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "mariadb")]
        "mariadb" => {
            let d = mariadb::MariaAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        // Accept `mysql` as an alias — MariaDB's driver crate speaks
        // the MySQL wire protocol; both server flavors work through
        // the same adapter.
        #[cfg(feature = "mariadb")]
        "mysql" => {
            let d = mariadb::MariaAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "clickhouse")]
        "clickhouse" => {
            let d = clickhouse::ClickHouseAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "redshift")]
        "redshift" => {
            let d = redshift::RedshiftAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "docdb")]
        "docdb" => {
            let d = docdb::DocDbAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        // `mongodb` is an alias — same adapter, since DocumentDB is
        // MongoDB-wire-compatible.
        #[cfg(feature = "docdb")]
        "mongodb" => {
            let d = docdb::DocDbAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        #[cfg(feature = "dynamodb")]
        "dynamodb" => {
            let d = dynamodb::DynamoDbAdapter::connect(spec).await?;
            Ok(Box::new(d))
        }
        other => Err(anyhow!(
            "no driver compiled in for engine `{other}` — enable the corresponding feature"
        )),
    }
}

/// The list of engine names this build supports. Used by the config
/// validator to reject a spec whose engine isn't compiled in.
pub fn supported_engines() -> &'static [&'static str] {
    &[
        #[cfg(feature = "postgres")]
        "postgres",
        #[cfg(feature = "redis")]
        "redis",
        #[cfg(feature = "mariadb")]
        "mariadb",
        #[cfg(feature = "mariadb")]
        "mysql",
        #[cfg(feature = "clickhouse")]
        "clickhouse",
        #[cfg(feature = "redshift")]
        "redshift",
        #[cfg(feature = "docdb")]
        "docdb",
        #[cfg(feature = "docdb")]
        "mongodb",
        #[cfg(feature = "dynamodb")]
        "dynamodb",
    ]
}
