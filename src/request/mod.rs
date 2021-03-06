// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

use crate::{
    backoff::{Backoff, DEFAULT_REGION_BACKOFF, OPTIMISTIC_BACKOFF, PESSIMISTIC_BACKOFF},
    transaction::HasLocks,
};
use async_trait::async_trait;
use derive_new::new;
use tikv_client_store::{HasError, Request};

pub use self::{
    plan::{
        Collect, CollectError, DefaultProcessor, Dispatch, Merge, MergeResponse, MultiRegion, Plan,
        Process, ProcessResponse, ResolveLock, RetryRegion,
    },
    plan_builder::{PlanBuilder, SingleKey},
    shard::Shardable,
};

mod plan;
mod plan_builder;
#[macro_use]
mod shard;

/// Abstracts any request sent to a TiKV server.
#[async_trait]
pub trait KvRequest: Request + Sized + Clone + Sync + Send + 'static {
    /// The expected response to the request.
    type Response: HasError + HasLocks + Clone + Send + 'static;
}

#[derive(Clone, Debug, new, Eq, PartialEq)]
pub struct RetryOptions {
    /// How to retry when there is a region error and we need to resolve regions with PD.
    pub region_backoff: Backoff,
    /// How to retry when a key is locked.
    pub lock_backoff: Backoff,
}

impl RetryOptions {
    pub const fn default_optimistic() -> RetryOptions {
        RetryOptions {
            region_backoff: DEFAULT_REGION_BACKOFF,
            lock_backoff: OPTIMISTIC_BACKOFF,
        }
    }

    pub const fn default_pessimistic() -> RetryOptions {
        RetryOptions {
            region_backoff: DEFAULT_REGION_BACKOFF,
            lock_backoff: PESSIMISTIC_BACKOFF,
        }
    }

    pub const fn none() -> RetryOptions {
        RetryOptions {
            region_backoff: Backoff::no_backoff(),
            lock_backoff: Backoff::no_backoff(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        mock::{MockKvClient, MockPdClient},
        store::store_stream_for_keys,
        Error, Key, Result,
    };
    use futures::executor;
    use grpcio::CallOption;
    use std::{
        any::Any,
        sync::{Arc, Mutex},
    };
    use tikv_client_proto::{kvrpcpb, tikvpb::TikvClient};
    use tikv_client_store::HasRegionError;

    #[test]
    fn test_region_retry() {
        #[derive(Clone)]
        struct MockRpcResponse;

        impl HasError for MockRpcResponse {
            fn error(&mut self) -> Option<Error> {
                None
            }
        }

        impl HasRegionError for MockRpcResponse {
            fn region_error(&mut self) -> Option<Error> {
                Some(Error::RegionNotFound { region_id: 1 })
            }
        }

        impl HasLocks for MockRpcResponse {}

        #[derive(Clone)]
        struct MockKvRequest {
            test_invoking_count: Arc<Mutex<usize>>,
        }

        #[async_trait]
        impl Request for MockKvRequest {
            async fn dispatch(&self, _: &TikvClient, _: CallOption) -> Result<Box<dyn Any>> {
                Ok(Box::new(MockRpcResponse {}))
            }

            fn label(&self) -> &'static str {
                "mock"
            }

            fn as_any(&self) -> &dyn Any {
                self
            }

            fn set_context(&mut self, _: kvrpcpb::Context) {
                unreachable!();
            }
        }

        #[async_trait]
        impl KvRequest for MockKvRequest {
            type Response = MockRpcResponse;
        }

        impl Shardable for MockKvRequest {
            type Shard = Vec<Vec<u8>>;

            fn shards(
                &self,
                pd_client: &std::sync::Arc<impl crate::pd::PdClient>,
            ) -> futures::stream::BoxStream<
                'static,
                crate::Result<(Self::Shard, crate::store::Store)>,
            > {
                // Increases by 1 for each call.
                let mut test_invoking_count = self.test_invoking_count.lock().unwrap();
                *test_invoking_count += 1;
                store_stream_for_keys(
                    Some(Key::from("mock_key".to_owned())).into_iter(),
                    pd_client.clone(),
                )
            }

            fn apply_shard(
                &mut self,
                _shard: Self::Shard,
                _store: &crate::store::Store,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let invoking_count = Arc::new(Mutex::new(0));

        let request = MockKvRequest {
            test_invoking_count: invoking_count.clone(),
        };

        let pd_client = Arc::new(MockPdClient::new(MockKvClient::with_dispatch_hook(
            |_: &dyn Any| Ok(Box::new(MockRpcResponse) as Box<dyn Any>),
        )));

        let plan = crate::request::PlanBuilder::new(pd_client.clone(), request)
            .resolve_lock(Backoff::no_jitter_backoff(1, 1, 3))
            .multi_region()
            .retry_region(Backoff::no_jitter_backoff(1, 1, 3))
            .plan();
        let _ = executor::block_on(async { plan.execute().await });

        // Original call plus the 3 retries
        assert_eq!(*invoking_count.lock().unwrap(), 4);
    }
}
