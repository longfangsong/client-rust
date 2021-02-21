// Copyright 2021 TiKV Project Authors. Licensed under Apache-2.0.

use crate::{backoff::Backoff, pd::PdClient, request::{
    Dispatch, KvRequest, Merge, MergeResponse, MultiRegionPlan, Plan, Process, ProcessResponse,
    ResolveLockPlan, RetryRegionPlan, Shardable,
}, store::Store, transaction::{HasLocks, TransactionStatus}, Result};
use std::{marker::PhantomData, sync::Arc};
use tikv_client_store::HasError;
use crate::request::plan::HeartbeatPlan;
use std::sync::RwLock;

/// Builder type for plans (see that module for more).
pub struct PlanBuilder<PdC: PdClient, P: Plan, Ph: PlanBuilderPhase> {
    pd_client: Arc<PdC>,
    plan: P,
    phantom: PhantomData<Ph>,
}

/// Used to ensure that a plan has a designated target or targets, a target is
/// a particular TiKV server.
pub trait PlanBuilderPhase {}
pub struct NoTarget;
impl PlanBuilderPhase for NoTarget {}
pub struct Targetted;
impl PlanBuilderPhase for Targetted {}

impl<PdC: PdClient, Req: KvRequest> PlanBuilder<PdC, Dispatch<Req>, NoTarget> {
    pub fn new(pd_client: Arc<PdC>, request: Req) -> Self {
        PlanBuilder {
            pd_client,
            plan: Dispatch {
                request,
                kv_client: None,
            },
            phantom: PhantomData,
        }
    }
}

impl<PdC: PdClient, P: Plan> PlanBuilder<PdC, P, Targetted> {
    /// Return the built plan, note that this can only be called once the plan
    /// has a target.
    pub fn plan(self) -> P {
        self.plan
    }
}

impl<PdC: PdClient, P: Plan, Ph: PlanBuilderPhase> PlanBuilder<PdC, P, Ph> {
    /// If there is a lock error, then resolve the lock and retry the request.
    pub fn resolve_lock(self, backoff: Backoff) -> PlanBuilder<PdC, ResolveLockPlan<P, PdC>, Ph>
    where
        P::Result: HasLocks,
    {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: ResolveLockPlan {
                inner: self.plan,
                backoff,
                pd_client: self.pd_client,
            },
            phantom: PhantomData,
        }
    }

    /// If there is a region error, re-shard the request and re-resolve regions, then retry.
    ///
    /// Note that this plan must wrap a multi-region plan if the request should be re-sharded.
    pub fn retry_region(self, backoff: Backoff) -> PlanBuilder<PdC, RetryRegionPlan<P, PdC>, Ph>
    where
        P::Result: HasError,
    {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: RetryRegionPlan {
                inner: self.plan,
                backoff,
                pd_client: self.pd_client,
            },
            phantom: PhantomData,
        }
    }

    /// Merge the results of a request. Usually used where a request is sent to multiple regions
    /// to combine the responses from each region.
    pub fn merge<In, M: Merge<In>>(self, merge: M) -> PlanBuilder<PdC, MergeResponse<P, In, M>, Ph>
    where
        In: Clone + Send + Sync + 'static,
        P: Plan<Result = Vec<Result<In>>>,
    {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: MergeResponse {
                inner: self.plan,
                merge,
                phantom: PhantomData,
            },
            phantom: PhantomData,
        }
    }

    /// Apply a processing step to a response (usually only needed if the request is sent to a
    /// single region because post-porcessing can be incorporated in the merge step for multi-region
    /// requests).
    pub fn post_process(self) -> PlanBuilder<PdC, ProcessResponse<P, P::Result>, Ph>
        where
            P: Plan<Result: Process>,
    {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: ProcessResponse {
                inner: self.plan,
                phantom: PhantomData,
            },
            phantom: PhantomData,
        }
    }

    /// spawn a heartbeat request.
    pub fn heart_beat(self, status: Arc<RwLock<TransactionStatus>>) -> PlanBuilder<PdC, HeartbeatPlan<P>, Ph>
    where
        P: Plan<Result: HasError>,
    {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: HeartbeatPlan {
                inner: self.plan,
                status,
            },
            phantom: PhantomData,
        }
    }
}

impl<PdC: PdClient, P: Plan + Shardable> PlanBuilder<PdC, P, NoTarget>
where
    P::Result: HasError,
{
    /// Split the request into shards sending a request to the region of each shard.
    pub fn multi_region(self) -> PlanBuilder<PdC, MultiRegionPlan<P, PdC>, Targetted> {
        PlanBuilder {
            pd_client: self.pd_client.clone(),
            plan: MultiRegionPlan {
                inner: self.plan,
                pd_client: self.pd_client,
            },
            phantom: PhantomData,
        }
    }
}

impl<PdC: PdClient, R: KvRequest + SingleKey> PlanBuilder<PdC, Dispatch<R>, NoTarget> {
    /// Target the request at a single region.
    pub async fn single_region(self) -> Result<PlanBuilder<PdC, Dispatch<R>, Targetted>> {
        let key = self.plan.request.key();
        let store = self.pd_client.clone().store_for_key(key.into()).await?;
        set_single_region_store(self.plan, store, self.pd_client)
    }
}

impl<PdC: PdClient, R: KvRequest> PlanBuilder<PdC, Dispatch<R>, NoTarget> {
    /// Target the request at a single region; caller supplies the store to target.
    pub async fn single_region_with_store(
        self,
        store: Store,
    ) -> Result<PlanBuilder<PdC, Dispatch<R>, Targetted>> {
        set_single_region_store(self.plan, store, self.pd_client)
    }
}

fn set_single_region_store<PdC: PdClient, R: KvRequest>(
    mut plan: Dispatch<R>,
    store: Store,
    pd_client: Arc<PdC>,
) -> Result<PlanBuilder<PdC, Dispatch<R>, Targetted>> {
    plan.request.set_context(store.region.context()?);
    plan.kv_client = Some(store.client);
    Ok(PlanBuilder {
        plan,
        pd_client,
        phantom: PhantomData,
    })
}

/// Indicates that a request operates on a single key.
pub trait SingleKey {
    #[allow(clippy::ptr_arg)]
    fn key(&self) -> &Vec<u8>;
}
