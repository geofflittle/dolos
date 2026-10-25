pub use dolos_core::*;

use dolos_core::config::StorageVersion;

use miette::Diagnostic;
use std::fmt::Display;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),

    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error("client error: {0}")]
    ClientError(String),

    #[error("parse error: {0}")]
    ParseError(String),

    #[error("server error: {0}")]
    ServerError(String),

    #[error("storage error: {0}")]
    StorageError(String),

    /// The configuration declares a storage version this binary does not read.
    ///
    /// Both versions are carried rather than formatted away, because the
    /// comparison is between two configuration strings and a caller that wants
    /// to report or repair it needs the pair.
    #[error(
        "the configuration declares storage version `{declared}` and this dolos reads \
         `{supported}`. That is a comparison of two configuration strings, and no store was \
         read. If the stores were written by a dolos that reads `{supported}`, set \
         `storage.version` to `{supported}`. Running `dolos init` writes a fresh configuration \
         and bootstraps again, which discards whatever the stores hold. The bootstrap guide is \
         at {guide}"
    )]
    StorageVersionMismatch {
        declared: StorageVersion,
        supported: StorageVersion,
        guide: &'static str,
    },

    /// The state store holds a cursor at an older revision than this binary reads.
    #[error(
        "the state store holds chain data at state revision {} and this dolos reads revision \
         {required} or later. Reseed it with `dolos bootstrap --force`, which clears the stores \
         and bootstraps again",
        .stored.map_or("none".to_string(), |revision| revision.to_string())
    )]
    StateRevisionTooOld { stored: Option<u64>, required: u64 },

    #[error("wal error: {0}")]
    WalError(#[from] WalError),

    #[error("chain error: {0}")]
    ArchiveError(#[from] ArchiveError),

    #[error("state error: {0}")]
    StateError(#[from] StateError),

    #[error("mempool error: {0}")]
    MempoolError(#[from] MempoolError),

    #[error("{0}")]
    Message(String),

    #[error("{0}")]
    Custom(String),
}

impl Error {
    pub fn config(text: impl Display) -> Error {
        Error::ConfigError(text.to_string())
    }

    pub fn client(error: impl Display) -> Error {
        Error::ClientError(error.to_string())
    }

    pub fn parse(error: impl Display) -> Error {
        Error::ParseError(error.to_string())
    }

    pub fn server(error: impl Display) -> Error {
        Error::ServerError(error.to_string())
    }

    pub fn message(text: impl Into<String>) -> Error {
        Error::Message(text.into())
    }

    pub fn custom(error: impl Display) -> Error {
        Error::Custom(error.to_string())
    }
}

impl From<Box<dyn std::error::Error>> for Error {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        Error::custom(err)
    }
}

#[derive(Clone, Default)]
pub struct CancelTokenImpl(pub tokio_util::sync::CancellationToken);

impl CancelToken for CancelTokenImpl {
    async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

/// The crawler for the first of `points` local history holds, or `None` when it
/// holds none of them.
///
/// The failure is a value because a session serving a client has somewhere to
/// report it, and the archive can hold blocks the log has no way to continue
/// from.
pub fn start_crawler<D: Domain>(
    domain: &D,
    points: &[ChainPoint],
) -> Result<Option<(dolos_core::crawl::ChainCrawler<D>, ChainPoint)>, Error> {
    dolos_core::crawl::ChainCrawler::<D>::start(domain, points).map_err(Error::server)
}

#[cfg(test)]
mod tests {
    use dolos_core::{ArchiveStore as _, ArchiveWriter as _, SyncExt as _};
    use dolos_testing::blocks::make_conway_block;
    use dolos_testing::toy_domain::ToyDomain;

    use super::*;

    /// The must-fire case. A crawl asked to start at the last block the archive
    /// holds has nowhere to continue, and a session that serves a client has to
    /// answer rather than end the process.
    #[test]
    fn a_start_with_nowhere_to_continue_is_returned_and_not_a_panic() {
        let domain = ToyDomain::new(None, None);

        // One block the archive holds and the wal never saw, so the crawl can
        // start on it and the page after it is empty on both stores.
        let writer = domain.archive().start_writer().unwrap();
        let (point, block) = make_conway_block(100);
        writer.apply(&point, &block).unwrap();
        writer.commit().unwrap();

        let Err(error) = start_crawler(&domain, &[point]) else {
            panic!("the start answered with a crawler for a page nothing continues");
        };

        assert!(
            matches!(&error, Error::ServerError(text) if text.contains("archive")),
            "{error}"
        );
    }

    /// The must-not case for a point local history holds, which is every
    /// ordinary intersect.
    #[test]
    fn a_start_the_log_can_continue_from_answers_with_a_crawler() {
        let domain = ToyDomain::new(None, None);

        for slot in 0..=10u64 {
            let (_, block) = make_conway_block(slot * 10);
            domain.roll_forward(block).unwrap();
        }

        let (point, _) = make_conway_block(50);
        let started = start_crawler(&domain, std::slice::from_ref(&point)).unwrap();

        assert!(matches!(started, Some((_, found)) if found == point));
    }

    /// The must-not case for a point nothing holds. No intersect is not a
    /// failure, and a session answers it with its own refusal.
    #[test]
    fn a_start_at_a_point_nothing_holds_answers_with_no_crawler() {
        let domain = ToyDomain::new(None, None);

        for slot in 0..=10u64 {
            let (_, block) = make_conway_block(slot * 10);
            domain.roll_forward(block).unwrap();
        }

        let (point, _) = make_conway_block(9999);

        assert!(start_crawler(&domain, &[point]).unwrap().is_none());
    }
}
