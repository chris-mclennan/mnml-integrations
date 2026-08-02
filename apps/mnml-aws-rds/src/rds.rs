//! `aws rds describe-db-instances` / `describe-db-clusters` shell-outs
//! + structured response models. Pure CLI — no SDK dep.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbEndpoint {
    #[serde(rename = "Address", default)]
    pub address: Option<String>,
    #[serde(rename = "Port", default)]
    pub port: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbInstance {
    #[serde(rename = "DBInstanceIdentifier")]
    pub identifier: String,
    #[serde(rename = "DBInstanceArn", default)]
    pub arn: String,
    #[serde(rename = "DBInstanceClass", default)]
    pub instance_class: Option<String>,
    #[serde(rename = "Engine", default)]
    pub engine: Option<String>,
    #[serde(rename = "EngineVersion", default)]
    pub engine_version: Option<String>,
    #[serde(rename = "DBInstanceStatus", default)]
    pub status: Option<String>,
    #[serde(rename = "Endpoint", default)]
    pub endpoint: Option<DbEndpoint>,
    #[serde(rename = "AllocatedStorage", default)]
    pub allocated_storage: Option<u32>,
    #[serde(rename = "StorageType", default)]
    pub storage_type: Option<String>,
    #[serde(rename = "MultiAZ", default)]
    pub multi_az: Option<bool>,
    #[serde(rename = "AvailabilityZone", default)]
    pub az: Option<String>,
    #[serde(rename = "PubliclyAccessible", default)]
    pub publicly_accessible: Option<bool>,
    #[serde(rename = "DBClusterIdentifier", default)]
    pub cluster_identifier: Option<String>,
    #[serde(rename = "InstanceCreateTime", default)]
    pub create_time: Option<String>,
    #[serde(rename = "MasterUsername", default)]
    pub master_username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbCluster {
    #[serde(rename = "DBClusterIdentifier")]
    pub identifier: String,
    #[serde(rename = "DBClusterArn", default)]
    pub arn: String,
    #[serde(rename = "Engine", default)]
    pub engine: Option<String>,
    #[serde(rename = "EngineVersion", default)]
    pub engine_version: Option<String>,
    #[serde(rename = "EngineMode", default)]
    pub engine_mode: Option<String>,
    #[serde(rename = "Status", default)]
    pub status: Option<String>,
    #[serde(rename = "Endpoint", default)]
    pub endpoint: Option<String>,
    #[serde(rename = "ReaderEndpoint", default)]
    pub reader_endpoint: Option<String>,
    #[serde(rename = "Port", default)]
    pub port: Option<u32>,
    #[serde(rename = "MultiAZ", default)]
    pub multi_az: Option<bool>,
    #[serde(rename = "MasterUsername", default)]
    pub master_username: Option<String>,
    #[serde(rename = "AllocatedStorage", default)]
    pub allocated_storage: Option<u32>,
    #[serde(rename = "DatabaseName", default)]
    pub database_name: Option<String>,
    #[serde(rename = "ClusterCreateTime", default)]
    pub create_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListInstancesResponse {
    #[serde(rename = "DBInstances")]
    db_instances: Vec<DbInstance>,
    #[serde(rename = "Marker", default)]
    marker: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListClustersResponse {
    #[serde(rename = "DBClusters")]
    db_clusters: Vec<DbCluster>,
    #[serde(rename = "Marker", default)]
    marker: Option<String>,
}

/// Unified focused-item type so the renderer + list code work
/// across `instances` and `clusters` tabs.
#[derive(Debug, Clone)]
pub enum Item {
    Instance(DbInstance),
    Cluster(DbCluster),
}

impl Item {
    pub fn primary_label(&self) -> &str {
        match self {
            Item::Instance(i) => &i.identifier,
            Item::Cluster(c) => &c.identifier,
        }
    }
    pub fn secondary_label(&self) -> String {
        match self {
            Item::Instance(i) => {
                let engine = i.engine.as_deref().unwrap_or("?");
                let status = i.status.as_deref().unwrap_or("?");
                format!("{engine} · {status}")
            }
            Item::Cluster(c) => {
                let engine = c.engine.as_deref().unwrap_or("?");
                let status = c.status.as_deref().unwrap_or("?");
                format!("{engine} · {status}")
            }
        }
    }
    pub fn arn(&self) -> &str {
        match self {
            Item::Instance(i) => &i.arn,
            Item::Cluster(c) => &c.arn,
        }
    }
    pub fn endpoint(&self) -> Option<String> {
        match self {
            Item::Instance(i) => i.endpoint.as_ref().and_then(|e| {
                e.address.as_ref().map(|a| match e.port {
                    Some(p) => format!("{a}:{p}"),
                    None => a.clone(),
                })
            }),
            Item::Cluster(c) => c.endpoint.as_ref().map(|a| match c.port {
                Some(p) => format!("{a}:{p}"),
                None => a.clone(),
            }),
        }
    }
}

pub fn list_db_instances(region: Option<&str>) -> Result<Vec<DbInstance>> {
    let mut all = Vec::new();
    let mut marker: Option<String> = None;

    loop {
        let mut cmd = Command::new("aws");
        cmd.args(["rds", "describe-db-instances", "--output", "json"]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(m) = &marker {
            cmd.args(["--starting-token", m]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws rds describe-db-instances`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws rds describe-db-instances failed: {}",
                stderr.trim()
            ));
        }
        let resp: ListInstancesResponse = serde_json::from_slice(&output.stdout)
            .with_context(|| "parse describe-db-instances JSON")?;
        all.extend(resp.db_instances);
        match resp.marker {
            Some(m) if !m.is_empty() => marker = Some(m),
            _ => break,
        }
    }

    all.sort_by_key(|i| i.identifier.to_lowercase());
    Ok(all)
}

pub fn list_db_clusters(region: Option<&str>) -> Result<Vec<DbCluster>> {
    let mut all = Vec::new();
    let mut marker: Option<String> = None;

    loop {
        let mut cmd = Command::new("aws");
        cmd.args(["rds", "describe-db-clusters", "--output", "json"]);
        if let Some(r) = region {
            cmd.args(["--region", r]);
        }
        if let Some(m) = &marker {
            cmd.args(["--starting-token", m]);
        }
        let output = cmd
            .output()
            .with_context(|| "spawn `aws rds describe-db-clusters`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "aws rds describe-db-clusters failed: {}",
                stderr.trim()
            ));
        }
        let resp: ListClustersResponse = serde_json::from_slice(&output.stdout)
            .with_context(|| "parse describe-db-clusters JSON")?;
        all.extend(resp.db_clusters);
        match resp.marker {
            Some(m) if !m.is_empty() => marker = Some(m),
            _ => break,
        }
    }

    all.sort_by_key(|c| c.identifier.to_lowercase());
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_describe_db_instances_response() {
        let json = r#"{
            "DBInstances": [
                {
                    "DBInstanceIdentifier": "prod-postgres",
                    "DBInstanceArn": "arn:aws:rds:us-east-1:1:db:prod-postgres",
                    "DBInstanceClass": "db.r6g.xlarge",
                    "Engine": "postgres",
                    "EngineVersion": "16.4",
                    "DBInstanceStatus": "available",
                    "Endpoint": {
                        "Address": "prod.cluster-xyz.us-east-1.rds.amazonaws.com",
                        "Port": 5432
                    },
                    "AllocatedStorage": 200,
                    "StorageType": "gp3",
                    "MultiAZ": true,
                    "MasterUsername": "admin"
                }
            ]
        }"#;
        let resp: ListInstancesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.db_instances.len(), 1);
        let i = &resp.db_instances[0];
        assert_eq!(i.engine.as_deref(), Some("postgres"));
        assert_eq!(i.multi_az, Some(true));
        let item = Item::Instance(i.clone());
        let endpoint = item.endpoint().expect("instance has endpoint");
        assert!(endpoint.contains("rds.amazonaws.com"));
        assert!(endpoint.ends_with(":5432"));
    }

    #[test]
    fn parses_describe_db_clusters_response() {
        let json = r#"{
            "DBClusters": [
                {
                    "DBClusterIdentifier": "aurora-prod",
                    "DBClusterArn": "arn:aws:rds:us-east-1:1:cluster:aurora-prod",
                    "Engine": "aurora-postgresql",
                    "EngineVersion": "15.4",
                    "Status": "available",
                    "Endpoint": "aurora-prod.cluster-abc.us-east-1.rds.amazonaws.com",
                    "ReaderEndpoint": "aurora-prod.cluster-ro-abc.us-east-1.rds.amazonaws.com",
                    "Port": 5432
                }
            ]
        }"#;
        let resp: ListClustersResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.db_clusters.len(), 1);
        let item = Item::Cluster(resp.db_clusters[0].clone());
        let endpoint = item.endpoint().expect("cluster has endpoint");
        assert!(endpoint.ends_with(":5432"));
    }

    #[test]
    fn secondary_label_combines_engine_and_status() {
        let i = DbInstance {
            identifier: "x".into(),
            arn: "arn".into(),
            instance_class: None,
            engine: Some("postgres".into()),
            engine_version: None,
            status: Some("available".into()),
            endpoint: None,
            allocated_storage: None,
            storage_type: None,
            multi_az: None,
            az: None,
            publicly_accessible: None,
            cluster_identifier: None,
            create_time: None,
            master_username: None,
        };
        let label = Item::Instance(i).secondary_label();
        assert!(label.contains("postgres"));
        assert!(label.contains("available"));
    }

    #[test]
    fn endpoint_falls_back_to_address_when_no_port() {
        let i = DbInstance {
            identifier: "x".into(),
            arn: "".into(),
            instance_class: None,
            engine: None,
            engine_version: None,
            status: None,
            endpoint: Some(DbEndpoint {
                address: Some("host.x".into()),
                port: None,
            }),
            allocated_storage: None,
            storage_type: None,
            multi_az: None,
            az: None,
            publicly_accessible: None,
            cluster_identifier: None,
            create_time: None,
            master_username: None,
        };
        assert_eq!(Item::Instance(i).endpoint(), Some("host.x".into()));
    }
}
