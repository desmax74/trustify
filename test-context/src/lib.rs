#![allow(clippy::expect_used)]

use futures::Stream;
use peak_alloc::PeakAlloc;
use postgresql_embedded::PostgreSQL;
use std::env;
use std::env::current_dir;
use std::io::{ErrorKind, Read, Seek};
use std::path::PathBuf;
use test_context::AsyncTestContext;
use tokio::io::AsyncReadExt;
use tokio_util::bytes::Bytes;
use tokio_util::io::{ReaderStream, SyncIoBridge};
use tracing::instrument;
use trustify_common as common;
use trustify_common::db;
use trustify_common::hashing::{Digests, HashingRead};
use trustify_module_ingestor::graph::Graph;
use trustify_module_ingestor::model::IngestResult;
use trustify_module_ingestor::service::{Format, IngestorService};
use trustify_module_storage::service::fs::FileSystemBackend;

#[allow(dead_code)]
pub struct TrustifyContext {
    pub db: common::db::Database,
    pub graph: Graph,
    pub storage: FileSystemBackend,
    pub ingestor: IngestorService,
    pub mem_limit_mb: f32,
    postgresql: Option<PostgreSQL>,
}

#[global_allocator]
static PEAK_ALLOC: PeakAlloc = PeakAlloc;

impl TrustifyContext {
    async fn new(db: common::db::Database, postgresql: impl Into<Option<PostgreSQL>>) -> Self {
        let (storage, _) = FileSystemBackend::for_test()
            .await
            .expect("initializing the storage backend");
        let graph = Graph::new(db.clone());
        let ingestor = IngestorService::new(graph.clone(), storage.clone());
        let mem_limit_mb = env::var("MEM_LIMIT_MB")
            .unwrap_or("500".into())
            .parse()
            .expect("a numerical value");

        Self {
            db,
            graph,
            storage,
            ingestor,
            mem_limit_mb,
            postgresql: postgresql.into(),
        }
    }

    pub async fn ingest_documents<'a, P: IntoIterator<Item = &'a str>>(
        &self,
        paths: P,
    ) -> Result<Vec<IngestResult>, anyhow::Error> {
        let mut results = Vec::new();
        for path in paths {
            results.push(self.ingest_document(path).await?);
        }
        Ok(results)
    }

    pub async fn ingest_document(&self, path: &str) -> Result<IngestResult, anyhow::Error> {
        let bytes = document_bytes(path).await?;
        Ok(self
            .ingestor
            .ingest(&bytes, Format::Unknown, ("source", "TrustifyContext"), None)
            .await?)
    }

    pub async fn ingest_read<R: Read>(&self, mut read: R) -> Result<IngestResult, anyhow::Error> {
        let mut bytes = Vec::new();
        read.read_to_end(&mut bytes)?;

        Ok(self
            .ingestor
            .ingest(&bytes, Format::Unknown, ("source", "TrustifyContext"), None)
            .await?)
    }
}

impl AsyncTestContext for TrustifyContext {
    #[instrument]
    #[allow(clippy::expect_used)]
    async fn setup() -> TrustifyContext {
        if env::var("EXTERNAL_TEST_DB").is_ok() {
            log::warn!("Using external database from 'DB_*' env vars");
            let config = common::config::Database::from_env().expect("DB config from env");

            let db = if env::var("EXTERNAL_TEST_DB_BOOTSTRAP").is_ok() {
                common::db::Database::bootstrap(&config).await
            } else {
                common::db::Database::new(&config).await
            }
            .expect("Configuring the database");

            return TrustifyContext::new(db, None).await;
        }

        let (db, postgresql) = db::embedded::create()
            .await
            .expect("Create an embedded database");

        TrustifyContext::new(db, postgresql).await
    }

    async fn teardown(self) {
        let peak_mem = PEAK_ALLOC.peak_usage_as_mb();
        if peak_mem > self.mem_limit_mb {
            log::error!("Too much RAM used: {peak_mem} MB");
        }
        PEAK_ALLOC.reset_peak_usage();
    }
}

fn find_workspace_root() -> Result<PathBuf, anyhow::Error> {
    let current_dir = current_dir()?;
    let mut i = Some(current_dir.as_path());
    while let Some(cur) = i {
        if cur.join("rust-toolchain.toml").exists() {
            return Ok(cur.to_path_buf());
        }
        i = cur.parent();
    }
    Err(std::io::Error::new(ErrorKind::NotFound, "damnit").into())
}

fn absolute(path: &str) -> Result<PathBuf, anyhow::Error> {
    let workspace_root = find_workspace_root()?;
    let test_data = workspace_root.join("etc").join("test-data");
    Ok(test_data.join(path))
}

pub async fn document_bytes(path: &str) -> Result<Bytes, anyhow::Error> {
    let mut file = tokio::fs::File::open(absolute(path)?).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    if path.ends_with(".xz") {
        bytes = liblzma::decode_all(&*bytes)?;
    }
    Ok(bytes.into())
}

pub async fn document_stream(
    path: &str,
) -> Result<impl Stream<Item = Result<Bytes, std::io::Error>>, anyhow::Error> {
    let file = tokio::fs::File::open(absolute(path)?).await?;
    Ok(ReaderStream::new(file))
}

pub async fn document_read(path: &str) -> Result<impl Read + Seek, anyhow::Error> {
    Ok(std::fs::File::open(absolute(path)?)?)
}

pub async fn document<T>(path: &str) -> Result<(T, Digests), anyhow::Error>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let file = tokio::fs::File::open(absolute(path)?).await?;
    let mut reader = HashingRead::new(SyncIoBridge::new(file));
    let f = || match serde_json::from_reader(&mut reader) {
        Ok(v) => match reader.finish() {
            Ok(digests) => Ok((v, digests)),
            Err(e) => Err(anyhow::Error::new(e)),
        },
        Err(e) => Err(anyhow::Error::new(e)),
    };
    tokio::task::spawn_blocking(f).await?
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::StreamExt;
    use test_context::test_context;
    use test_log::test;

    #[test_context(TrustifyContext)]
    #[test(tokio::test)]
    async fn ingest_documents(ctx: &TrustifyContext) -> Result<(), anyhow::Error> {
        let result = ctx
            .ingest_documents(["zookeeper-3.9.2-cyclonedx.json"])
            .await?;

        let ingestion_result = &result[0];

        assert!(!ingestion_result.document_id.is_empty());

        Ok(())
    }

    #[test(tokio::test)]
    async fn test_document_bytes() {
        let bytes = document_bytes("zookeeper-3.9.2-cyclonedx.json")
            .await
            .unwrap();
        assert!(!bytes.is_empty());
    }

    #[test(tokio::test)]
    async fn test_document_stream() {
        let stream = document_stream("zookeeper-3.9.2-cyclonedx.json")
            .await
            .unwrap();
        assert!(Box::pin(stream).next().await.is_some());
    }

    #[test(tokio::test)]
    async fn test_document_struct() {
        use hex::ToHex;
        use osv::schema::Vulnerability;

        let (osv, digests): (Vulnerability, _) =
            document("osv/RUSTSEC-2021-0079.json").await.unwrap();

        assert_eq!(osv.id, "RUSTSEC-2021-0079");
        assert_eq!(
            digests.sha256.encode_hex::<String>(),
            "d113c2bd1ad6c3ac00a3a8d3f89d3f38de935f8ede0d174a55afe9911960cf51"
        );
    }
}
