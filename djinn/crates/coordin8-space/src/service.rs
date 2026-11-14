use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_core::SpaceEventKind;
use coordin8_proto::coordin8::{
    space_service_server::SpaceService, CancelTupleRequest, ContentsRequest, Lease, NotifyRequest,
    Provenance, ReadRequest, ReadResponse, RenewTupleRequest, SpaceEvent, SpaceEventType,
    TakeRequest, TakeResponse, Tuple, WriteRequest, WriteResponse,
};
use coordin8_registry::matcher::{matches, parse_template};

use crate::manager::SpaceManager;

/// Map a core error to a gRPC status. `Unavailable` gets its own code (the
/// dependency isn't ready yet, safe to retry) rather than falling into the
/// generic `internal` bucket.
fn map_err(e: coordin8_core::Error) -> Status {
    use coordin8_core::Error as E;
    match e {
        E::Unavailable(_) => Status::unavailable(e.to_string()),
        E::TupleNotFound(_) | E::WatchNotFound(_) | E::LeaseNotFound(_) => {
            Status::not_found(e.to_string())
        }
        E::LeaseExpired(_) => Status::failed_precondition(e.to_string()),
        E::InvalidArgument(_) => Status::invalid_argument(e.to_string()),
        E::PermissionDenied(_) => Status::permission_denied(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn tuple_record_to_proto(r: &coordin8_core::TupleRecord, lease: Option<Lease>) -> Tuple {
    Tuple {
        tuple_id: r.tuple_id.clone(),
        attrs: r.attrs.clone(),
        payload: r.payload.clone(),
        provenance: Some(Provenance {
            written_by: r.written_by.clone(),
            written_at: Some(to_timestamp(r.written_at)),
            input_tuple_id: r.input_tuple_id.clone().unwrap_or_default(),
        }),
        lease,
    }
}

/// Parse txn_id from proto: empty string → None.
fn parse_txn_id(txn_id: String) -> Option<String> {
    if txn_id.is_empty() {
        None
    } else {
        Some(txn_id)
    }
}

pub struct SpaceServiceImpl {
    manager: Arc<SpaceManager>,
    /// Stamped onto every `Lease` returned from `Write`/`Notify`/`RenewTuple`
    /// so a holder always knows where to renew — Space grants its own tuple
    /// and watch leases in-process (see
    /// `.claude/plans/distributed-leasing/PRD.md`), so this is simply
    /// Space's own listening address.
    grantor_host: String,
    grantor_port: u16,
    shutdown: coordin8_core::shutdown::ShutdownSignal,
}

impl SpaceServiceImpl {
    pub fn new(
        manager: Arc<SpaceManager>,
        grantor_host: impl Into<String>,
        grantor_port: u16,
    ) -> Self {
        Self {
            manager,
            grantor_host: grantor_host.into(),
            grantor_port,
            shutdown: Default::default(),
        }
    }

    /// End this service's long-lived server streams when `shutdown` fires
    /// (graceful drain), with `UNAVAILABLE` so client reconnect loops
    /// reconnect elsewhere instead of seeing a clean EOF.
    pub fn with_shutdown(mut self, shutdown: coordin8_core::shutdown::ShutdownSignal) -> Self {
        self.shutdown = shutdown;
        self
    }

    fn lease_to_proto(&self, lease: coordin8_core::LeaseRecord) -> Lease {
        Lease {
            lease_id: lease.lease_id,
            resource_id: lease.resource_id,
            granted_at: Some(to_timestamp(lease.granted_at)),
            expires_at: Some(to_timestamp(lease.expires_at)),
            ttl_seconds: lease.ttl_seconds,
            grantor_host: self.grantor_host.clone(),
            grantor_port: self.grantor_port as u32,
        }
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl SpaceService for SpaceServiceImpl {
    async fn write(&self, req: Request<WriteRequest>) -> Result<Response<WriteResponse>, Status> {
        let r = req.into_inner();
        let input_tuple_id = if r.input_tuple_id.is_empty() {
            None
        } else {
            Some(r.input_tuple_id)
        };
        let txn_id = parse_txn_id(r.txn_id);

        let (record, lease_record) = self
            .manager
            .write(
                r.attrs,
                r.payload,
                r.ttl_seconds,
                r.written_by,
                input_tuple_id,
                txn_id,
            )
            .await
            .map_err(map_err)?;

        debug!(tuple_id = %record.tuple_id, "write rpc");

        let lease = Some(self.lease_to_proto(lease_record));

        Ok(Response::new(WriteResponse {
            tuple: Some(tuple_record_to_proto(&record, lease)),
        }))
    }

    async fn read(&self, req: Request<ReadRequest>) -> Result<Response<ReadResponse>, Status> {
        let r = req.into_inner();
        let txn_id = parse_txn_id(r.txn_id);

        let result = self
            .manager
            .read(r.template, r.wait, r.timeout_ms, txn_id)
            .await
            .map_err(map_err)?;

        Ok(Response::new(ReadResponse {
            tuple: result.map(|rec| tuple_record_to_proto(&rec, None)),
        }))
    }

    async fn take(&self, req: Request<TakeRequest>) -> Result<Response<TakeResponse>, Status> {
        let r = req.into_inner();
        let txn_id = parse_txn_id(r.txn_id);

        let result = self
            .manager
            .take(r.template, r.wait, r.timeout_ms, txn_id)
            .await
            .map_err(map_err)?;

        Ok(Response::new(TakeResponse {
            tuple: result.map(|rec| tuple_record_to_proto(&rec, None)),
        }))
    }

    type NotifyStream = BoxStream<SpaceEvent>;

    async fn notify(
        &self,
        req: Request<NotifyRequest>,
    ) -> Result<Response<Self::NotifyStream>, Status> {
        let r = req.into_inner();
        let on = match r.on {
            x if x == SpaceEventType::Expiration as i32 => SpaceEventKind::Expiration,
            _ => SpaceEventKind::Appearance,
        };

        let (_, lease, mut reclaimed) = self
            .manager
            .notify(
                r.template.clone(),
                on.clone(),
                r.ttl_seconds,
                r.handback.clone(),
            )
            .await
            .map_err(map_err)?;

        // Subscribe to the right broadcast BEFORE returning the stream.
        let broadcast_rx = match on {
            SpaceEventKind::Appearance => self.manager.subscribe_tuple_broadcast(),
            SpaceEventKind::Expiration => self.manager.subscribe_expiry_broadcast(),
        };

        let event_type = match on {
            SpaceEventKind::Appearance => SpaceEventType::Appearance as i32,
            SpaceEventKind::Expiration => SpaceEventType::Expiration as i32,
        };

        let template = r.template;
        let handback = r.handback;
        let (tx, rx) = mpsc::channel::<Result<SpaceEvent, Status>>(64);
        // First message: the watch lease, so the client can renew it before
        // any tuple matches.
        let lease_id = lease.lease_id.clone();
        tx.send(Ok(SpaceEvent {
            r#type: SpaceEventType::Registered as i32,
            tuple: None,
            occurred_at: Some(to_timestamp(chrono::Utc::now())),
            handback: handback.clone(),
            lease: Some(self.lease_to_proto(lease)),
        }))
        .await
        .map_err(|_| Status::internal("notify stream closed"))?;

        let manager = Arc::clone(&self.manager);
        tokio::spawn(async move {
            let ops = parse_template(&template);
            let mut stream = BroadcastStream::new(broadcast_rx);

            loop {
                let result = tokio::select! {
                    r = stream.next() => match r {
                        Some(r) => r,
                        None => break,
                    },
                    // The watch lease lapsed or was cancelled: the watch is
                    // gone, so is the stream.
                    _ = &mut reclaimed => {
                        let _ = tx
                            .send(Err(Status::not_found("watch lease reclaimed")))
                            .await;
                        return;
                    }
                    // Client gone (noticed even if nothing ever matches):
                    // release the watch.
                    _ = tx.closed() => break,
                };
                let tuple = match result {
                    Ok(t) => t,
                    Err(BroadcastStreamRecvError::Lagged(_)) => {
                        let _ = tx
                            .send(Err(Status::data_loss(
                                "watcher lagged; resubscribe and re-snapshot",
                            )))
                            .await;
                        break;
                    }
                };

                if !ops.is_empty() && !matches(&ops, &tuple.attrs) {
                    continue;
                }

                let event = SpaceEvent {
                    r#type: event_type,
                    tuple: Some(tuple_record_to_proto(&tuple, None)),
                    occurred_at: Some(to_timestamp(chrono::Utc::now())),
                    handback: handback.clone(),
                    lease: None,
                };

                if tx.send(Ok(event)).await.is_err() {
                    break; // client disconnected
                }
            }
            manager.end_watch(&lease_id).await;
        });

        #[allow(clippy::result_large_err)] // tonic::Status is the gRPC error type
        let stream = self.shutdown.end_stream(ReceiverStream::new(rx), || {
            Err(Status::unavailable("server shutting down"))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    type ContentsStream = BoxStream<Tuple>;

    async fn contents(
        &self,
        req: Request<ContentsRequest>,
    ) -> Result<Response<Self::ContentsStream>, Status> {
        let r = req.into_inner();
        let txn_id = parse_txn_id(r.txn_id);

        let tuples = self
            .manager
            .contents(r.template, txn_id)
            .await
            .map_err(map_err)?;

        let (tx, rx) = mpsc::channel::<Result<Tuple, Status>>(64);

        tokio::spawn(async move {
            for record in &tuples {
                let proto = tuple_record_to_proto(record, None);
                if tx.send(Ok(proto)).await.is_err() {
                    return;
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn renew(&self, req: Request<RenewTupleRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        let record = self
            .manager
            .renew(&r.tuple_id, r.ttl_seconds)
            .await
            .map_err(map_err)?;

        Ok(Response::new(self.lease_to_proto(record)))
    }

    async fn cancel(&self, req: Request<CancelTupleRequest>) -> Result<Response<()>, Status> {
        let tuple_id = req.into_inner().tuple_id;
        self.manager.cancel(&tuple_id).await.map_err(map_err)?;

        Ok(Response::new(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordin8_core::{Leasing, TupleRecord};
    use coordin8_lease::LeaseManager;
    use coordin8_provider_local::{InMemoryLeaseStore, InMemorySpaceStore};
    use tokio::sync::broadcast;

    /// A blocked Take/Read (unary, wait forever) must end with UNAVAILABLE
    /// when shutdown begins, or it holds the graceful drain for the whole
    /// grace period.
    #[tokio::test]
    async fn blocked_take_and_read_end_on_shutdown() {
        let lease_manager: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            coordin8_core::LeaseConfig::default(),
            broadcast::channel(16).0,
        ));
        let (trigger, signal) = coordin8_core::shutdown::channel();
        let manager = Arc::new(
            SpaceManager::new(
                Arc::new(InMemorySpaceStore::new()),
                lease_manager,
                broadcast::channel::<TupleRecord>(8).0,
                broadcast::channel::<TupleRecord>(8).0,
            )
            .with_shutdown(signal),
        );
        let svc = Arc::new(SpaceServiceImpl::new(manager, "127.0.0.1", 9006));
        let template = std::collections::HashMap::from([("k".to_string(), "v".to_string())]);

        let (s1, t1) = (Arc::clone(&svc), template.clone());
        let take = tokio::spawn(async move {
            s1.take(Request::new(TakeRequest {
                template: t1,
                wait: true,
                timeout_ms: 0,
                txn_id: String::new(),
            }))
            .await
        });
        let (s2, t2) = (Arc::clone(&svc), template);
        let read = tokio::spawn(async move {
            s2.read(Request::new(ReadRequest {
                template: t2,
                wait: true,
                timeout_ms: 0,
                txn_id: String::new(),
            }))
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!take.is_finished() && !read.is_finished());

        trigger.trigger();
        let within = std::time::Duration::from_secs(1);
        let take = tokio::time::timeout(within, take).await.unwrap().unwrap();
        let read = tokio::time::timeout(within, read).await.unwrap().unwrap();
        assert_eq!(take.unwrap_err().code(), tonic::Code::Unavailable);
        assert_eq!(read.unwrap_err().code(), tonic::Code::Unavailable);
    }

    /// A Notify watcher that falls behind the broadcast must be told
    /// (DATA_LOSS), not silently handed a stream with a gap in it.
    #[tokio::test]
    async fn notify_stream_that_lags_ends_with_data_loss() {
        let lease_manager: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            coordin8_core::LeaseConfig::default(),
            broadcast::channel(16).0,
        ));
        let (tuple_tx, _) = broadcast::channel::<TupleRecord>(2);
        let (expiry_tx, _) = broadcast::channel::<TupleRecord>(2);
        let manager = Arc::new(SpaceManager::new(
            Arc::new(InMemorySpaceStore::new()),
            lease_manager,
            tuple_tx,
            expiry_tx,
        ));
        let svc = SpaceServiceImpl::new(manager.clone(), "127.0.0.1", 9006);

        let mut stream = svc
            .notify(Request::new(NotifyRequest {
                template: Default::default(),
                on: SpaceEventType::Appearance as i32,
                ttl_seconds: 60,
                handback: vec![],
                txn_id: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();

        // Single-threaded runtime: the forwarding task has not polled yet, so
        // these overflow the 2-slot broadcast before it drains anything.
        for _ in 0..10 {
            manager
                .write(Default::default(), vec![], 60, "t".into(), None, None)
                .await
                .unwrap();
        }

        let mut saw_loss = false;
        while let Some(item) = stream.next().await {
            if let Err(status) = item {
                assert_eq!(status.code(), tonic::Code::DataLoss);
                saw_loss = true;
                break;
            }
        }
        assert!(saw_loss, "lagged watcher must receive DATA_LOSS");
        assert!(stream.next().await.is_none(), "stream ends after DATA_LOSS");
    }

    /// Manager + its own landlord with a fast reaper and the watch cascade,
    /// wired like services.rs.
    fn watch_rig() -> (Arc<SpaceManager>, Arc<LeaseManager>) {
        let (expiry, _) = broadcast::channel(64);
        let lm = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            coordin8_core::LeaseConfig::default(),
            expiry,
        ));
        let manager = Arc::new(SpaceManager::new(
            Arc::new(InMemorySpaceStore::new()),
            lm.clone(),
            broadcast::channel::<TupleRecord>(16).0,
            broadcast::channel::<TupleRecord>(16).0,
        ));
        let (reaper_lm, reaper_tx) = (lm.clone(), lm.expiry_tx());
        tokio::spawn(async move {
            coordin8_lease::reaper::run_reaper(
                reaper_lm,
                reaper_tx,
                std::time::Duration::from_millis(100),
            )
            .await;
        });
        let (m, mut rx) = (manager.clone(), lm.expiry_tx().subscribe());
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if ev.record.resource_id.starts_with("space-watch:") {
                    m.on_watch_expired(&ev.record.lease_id).await;
                }
            }
        });
        (manager, lm)
    }

    async fn open_watch(
        svc: &SpaceServiceImpl,
        ttl_seconds: u64,
    ) -> <SpaceServiceImpl as SpaceService>::NotifyStream {
        svc.notify(Request::new(NotifyRequest {
            template: [("never".to_string(), "matches".to_string())].into(),
            on: SpaceEventType::Appearance as i32,
            ttl_seconds,
            handback: vec![7],
            txn_id: String::new(),
        }))
        .await
        .unwrap()
        .into_inner()
    }

    /// The watch lease comes first, and the watch lives exactly as long as
    /// it: unrenewed, the stream ends NOT_FOUND; renewed, it stays.
    #[tokio::test]
    async fn notify_stream_carries_and_ends_with_its_lease() {
        let (manager, lm) = watch_rig();
        let svc = SpaceServiceImpl::new(manager, "127.0.0.1", 9006);

        let mut lapsing = open_watch(&svc, 1).await;
        let first = lapsing.next().await.unwrap().unwrap();
        assert_eq!(first.r#type, SpaceEventType::Registered as i32);
        assert!(first.tuple.is_none());
        assert_eq!(first.handback, vec![7]);
        let lease = first.lease.expect("registered event carries the lease");
        assert!(lease.resource_id.starts_with("space-watch:"));
        assert_eq!(lease.grantor_port, 9006);

        let mut renewed = open_watch(&svc, 1).await;
        let kept = renewed.next().await.unwrap().unwrap().lease.unwrap();

        let end = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                lm.renew(&kept.lease_id, 1).await.unwrap();
                tokio::select! {
                    item = lapsing.next() => return item,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
                }
            }
        })
        .await
        .expect("unrenewed watch stream must end");
        assert_eq!(end.unwrap().unwrap_err().code(), tonic::Code::NotFound);

        // The renewed one is still open.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), renewed.next())
                .await
                .is_err()
        );
    }

    /// A client that goes away releases its watch even if nothing ever
    /// matches (the forwarding task no longer leaks).
    #[tokio::test]
    async fn dropped_notify_stream_releases_its_watch() {
        let (manager, lm) = watch_rig();
        let svc = SpaceServiceImpl::new(manager, "127.0.0.1", 9006);
        let mut s = open_watch(&svc, 60).await;
        let lease = s.next().await.unwrap().unwrap().lease.unwrap();
        drop(s);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            lm.renew(&lease.lease_id, 60).await.is_err(),
            "watch lease should be cancelled when the client leaves"
        );
    }
}

#[cfg(test)]
mod status_tests {
    use super::map_err;
    use coordin8_core::Error;
    use tonic::Code;

    #[test]
    fn gone_is_not_found_not_internal() {
        assert_eq!(
            map_err(Error::TupleNotFound("t".into())).code(),
            Code::NotFound
        );
        assert_eq!(
            map_err(Error::LeaseExpired("l".into())).code(),
            Code::FailedPrecondition
        );
        assert_eq!(map_err(Error::Storage("x".into())).code(), Code::Internal);
    }
}
