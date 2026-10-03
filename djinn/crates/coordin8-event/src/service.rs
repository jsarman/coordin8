use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::{debug, error};

use coordin8_core::DeliveryMode;
use coordin8_proto::coordin8::{
    event_service_server::EventService, CancelSubscriptionRequest, EmitRequest, Event,
    EventRegistration, Lease, ReceiveRequest, RenewSubscriptionRequest, SubscribeRequest,
};

use crate::manager::EventManager;

/// Map a core error to a gRPC status. `Unavailable` gets its own code (the
/// dependency isn't ready yet, safe to retry) rather than falling into the
/// generic `internal` bucket.
fn map_err(e: coordin8_core::Error) -> Status {
    match e {
        coordin8_core::Error::Unavailable(_) => Status::unavailable(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn event_record_to_proto(e: &coordin8_core::EventRecord, handback: &[u8]) -> Event {
    Event {
        event_id: e.event_id.clone(),
        source: e.source.clone(),
        event_type: e.event_type.clone(),
        seq_num: e.seq_num,
        attrs: e.attrs.clone(),
        payload: e.payload.clone(),
        handback: handback.to_vec(),
        emitted_at: Some(to_timestamp(e.emitted_at)),
    }
}

pub struct EventServiceImpl {
    manager: Arc<EventManager>,
    /// Stamped onto every `Lease` returned from `Subscribe`/`RenewSubscription`
    /// so a holder always knows where to renew — EventMgr grants its own
    /// subscription leases in-process (see
    /// `.claude/plans/distributed-leasing/PRD.md`), so this is simply
    /// EventMgr's own listening address.
    grantor_host: String,
    grantor_port: u16,
    shutdown: coordin8_core::shutdown::ShutdownSignal,
}

impl EventServiceImpl {
    pub fn new(
        manager: Arc<EventManager>,
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

/// Mailbox entries read per page by a durable `Receive`.
const MAILBOX_BATCH: usize = 64;

/// BestEffort delivery: forward matching live events; a lagged subscriber is
/// terminated with `DATA_LOSS` (it must resubscribe) rather than silently
/// skipping events.
async fn best_effort_loop(
    broadcast_rx: tokio::sync::broadcast::Receiver<coordin8_core::EventRecord>,
    tx: mpsc::Sender<Result<Event, Status>>,
    source: String,
    template: std::collections::HashMap<String, String>,
    handback: Vec<u8>,
) {
    let ops = coordin8_registry::matcher::parse_template(&template);
    let mut stream = BroadcastStream::new(broadcast_rx);

    loop {
        let result = tokio::select! {
            r = stream.next() => match r {
                Some(r) => r,
                None => return,
            },
            _ = tx.closed() => return,
        };
        let event = match result {
            Ok(e) => e,
            Err(_lagged) => {
                let _ = tx
                    .send(Err(Status::data_loss("subscriber lagged; resubscribe")))
                    .await;
                return;
            }
        };

        if event.source != source {
            continue;
        }
        let mut check_attrs = event.attrs.clone();
        check_attrs.insert("event_type".to_string(), event.event_type.clone());
        if !ops.is_empty() && !coordin8_registry::matcher::matches(&ops, &check_attrs) {
            continue;
        }

        if tx
            .send(Ok(event_record_to_proto(&event, &handback)))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Durable delivery (at-least-once): every event comes from the mailbox, in
/// per-registration seq order. An entry is acked (deleted) only after it was
/// sent AND the transport pulled it off the channel; a disconnect before that
/// leaves it in the mailbox for the next `Receive`. Broadcast messages are just
/// wake-ups; lag is harmless because the mailbox is the source of truth.
async fn durable_loop(
    manager: Arc<EventManager>,
    registration_id: String,
    broadcast_rx: tokio::sync::broadcast::Receiver<coordin8_core::EventRecord>,
    tx: mpsc::Sender<Result<Event, Status>>,
    source: String,
    handback: Vec<u8>,
    first_batch: Vec<coordin8_core::EventRecord>,
) {
    let mut wakeups = BroadcastStream::new(broadcast_rx);
    let mut cursor = 0u64;
    let mut batch = first_batch;

    loop {
        // Flush the mailbox until it is empty.
        while !batch.is_empty() {
            for event in &batch {
                let seq = event.seq_num;
                if tx
                    .send(Ok(event_record_to_proto(event, &handback)))
                    .await
                    .is_err()
                {
                    return;
                }
                // Room in the channel again => the previous event was consumed.
                if tx.reserve().await.is_err() {
                    return;
                }
                if let Err(e) = manager.ack_mailbox(&registration_id, &[seq]).await {
                    error!(registration_id, seq, error = %e, "mailbox ack failed");
                    let _ = tx.send(Err(map_err(e))).await;
                    return;
                }
                cursor = seq;
            }
            batch = match manager
                .peek_mailbox(&registration_id, cursor, MAILBOX_BATCH)
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    error!(registration_id, error = %e, "mailbox read failed");
                    let _ = tx.send(Err(map_err(e))).await;
                    return;
                }
            };
        }

        // Wait for something to be emitted for our source (or lag, or close).
        loop {
            let next = tokio::select! {
                r = wakeups.next() => r,
                _ = tx.closed() => return,
            };
            match next {
                None => return,
                Some(Ok(e)) if e.source != source => continue,
                Some(_) => break, // relevant event, or Lagged: re-read mailbox
            }
        }

        batch = match manager
            .peek_mailbox(&registration_id, cursor, MAILBOX_BATCH)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                error!(registration_id, error = %e, "mailbox read failed");
                let _ = tx.send(Err(map_err(e))).await;
                return;
            }
        };
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl EventService for EventServiceImpl {
    async fn subscribe(
        &self,
        req: Request<SubscribeRequest>,
    ) -> Result<Response<EventRegistration>, Status> {
        let r = req.into_inner();
        let delivery = match r.delivery {
            1 => DeliveryMode::BestEffort,
            _ => DeliveryMode::Durable,
        };

        let (registration_id, lease, seq_num) = self
            .manager
            .subscribe(
                r.source.clone(),
                r.template,
                delivery,
                r.ttl_seconds,
                r.handback,
            )
            .await
            .map_err(map_err)?;

        debug!(registration_id, source = %r.source, "subscribe rpc");

        Ok(Response::new(EventRegistration {
            registration_id,
            source: r.source,
            lease: Some(self.lease_to_proto(lease)),
            seq_num,
        }))
    }

    type ReceiveStream = BoxStream<Event>;

    async fn receive(
        &self,
        req: Request<ReceiveRequest>,
    ) -> Result<Response<Self::ReceiveStream>, Status> {
        let registration_id = req.into_inner().registration_id;

        let sub = self
            .manager
            .get_subscription(&registration_id)
            .await
            .map_err(map_err)?
            .ok_or_else(|| Status::not_found("subscription not found"))?;

        let handback = sub.handback.clone();
        let source = sub.source.clone();
        let template = sub.template.clone();
        let is_durable = sub.delivery == DeliveryMode::Durable;

        // Subscribe to the broadcast BEFORE reading the mailbox so an event
        // emitted in the gap still wakes us.
        let broadcast_rx = self.manager.subscribe_broadcast();

        // Capacity 1: the stream only holds one event in flight, so "the
        // channel has room again" means the previous event was pulled by the
        // transport. Durable acks key off that.
        let (tx, rx) = mpsc::channel::<Result<Event, Status>>(1);
        let manager = self.manager.clone();

        if !is_durable {
            tokio::spawn(best_effort_loop(
                broadcast_rx,
                tx,
                source,
                template,
                handback,
            ));
            return Ok(Response::new(Box::pin(ReceiverStream::new(rx))));
        }

        // A failing backlog read is surfaced to the caller, not swallowed.
        let first_batch = manager
            .peek_mailbox(&registration_id, 0, MAILBOX_BATCH)
            .await
            .map_err(map_err)?;

        debug!(
            registration_id,
            backlog = first_batch.len(),
            "receive rpc started"
        );

        tokio::spawn(durable_loop(
            manager,
            registration_id,
            broadcast_rx,
            tx,
            source,
            handback,
            first_batch,
        ));

        #[allow(clippy::result_large_err)] // tonic::Status is the gRPC error type
        let stream = self.shutdown.end_stream(ReceiverStream::new(rx), || {
            Err(Status::unavailable("server shutting down"))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn emit(&self, req: Request<EmitRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        self.manager
            .emit(r.source, r.event_type, r.attrs, r.payload)
            .await
            .map_err(map_err)?;

        Ok(Response::new(()))
    }

    async fn renew_subscription(
        &self,
        req: Request<RenewSubscriptionRequest>,
    ) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        let record = self
            .manager
            .renew_subscription(&r.registration_id, r.ttl_seconds)
            .await
            .map_err(map_err)?;

        Ok(Response::new(self.lease_to_proto(record)))
    }

    async fn cancel_subscription(
        &self,
        req: Request<CancelSubscriptionRequest>,
    ) -> Result<Response<()>, Status> {
        let registration_id = req.into_inner().registration_id;
        self.manager
            .cancel_subscription(&registration_id)
            .await
            .map_err(map_err)?;

        Ok(Response::new(()))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use std::time::Duration;

    use async_trait::async_trait;
    use chrono::Utc;
    use coordin8_core::{Error, EventRecord, EventStore, LeaseRecord, Leasing, SubscriptionRecord};
    use coordin8_provider_local::InMemoryEventStore;
    use tokio::sync::broadcast;

    struct StubLeases;

    #[async_trait]
    impl Leasing for StubLeases {
        async fn grant(
            &self,
            resource_id: &str,
            ttl_secs: u64,
        ) -> Result<coordin8_core::LeaseRecord, Error> {
            Ok(LeaseRecord {
                lease_id: uuid::Uuid::new_v4().to_string(),
                resource_id: resource_id.to_string(),
                granted_at: Utc::now(),
                expires_at: Utc::now(),
                ttl_seconds: ttl_secs,
            })
        }
        async fn renew(&self, _: &str, _: u64) -> Result<LeaseRecord, Error> {
            unimplemented!()
        }
        async fn cancel(&self, _: &str) -> Result<(), Error> {
            Ok(())
        }
    }

    fn manager(store: Arc<dyn EventStore>, broadcast_cap: usize) -> Arc<EventManager> {
        let (tx, _) = broadcast::channel(broadcast_cap);
        Arc::new(EventManager::new(store, Arc::new(StubLeases), tx))
    }

    async fn subscribe(m: &EventManager, delivery: DeliveryMode) -> String {
        m.subscribe("src".into(), HashMap::new(), delivery, 60, vec![9])
            .await
            .unwrap()
            .0
    }

    async fn emit(m: &EventManager, event_type: &str) {
        m.emit("src".into(), event_type.into(), HashMap::new(), vec![1])
            .await
            .unwrap();
    }

    async fn receive(
        svc: &EventServiceImpl,
        reg: &str,
    ) -> <EventServiceImpl as EventService>::ReceiveStream {
        svc.receive(Request::new(ReceiveRequest {
            registration_id: reg.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
    }

    /// Collect events until the stream is quiet for `quiet`.
    async fn collect(
        stream: &mut <EventServiceImpl as EventService>::ReceiveStream,
        quiet: Duration,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(Some(Ok(e))) = tokio::time::timeout(quiet, stream.next()).await {
            out.push(e);
        }
        out
    }

    // ── scenarios (run against every store) ──────────────────────────────────

    async fn two_event_types_both_arrive(store: Arc<dyn EventStore>) {
        let m = manager(store, 16);
        let reg = subscribe(&m, DeliveryMode::Durable).await;
        emit(&m, "a").await; // source+type seq 1
        emit(&m, "b").await; // source+type seq 1 as well
        let got = m.peek_mailbox(&reg, 0, 100).await.unwrap();
        let types: Vec<_> = got.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(types, ["a", "b"]);
        assert_eq!(got.iter().map(|e| e.seq_num).collect::<Vec<_>>(), [1, 2]);
    }

    async fn restart_does_not_overwrite(store: Arc<dyn EventStore>) {
        let m1 = manager(store.clone(), 16);
        let reg = subscribe(&m1, DeliveryMode::Durable).await;
        for _ in 0..3 {
            emit(&m1, "a").await;
        }
        // "Restart": fresh manager (counters reset) over the same store.
        let m2 = manager(store, 16);
        for _ in 0..2 {
            emit(&m2, "a").await;
        }
        let got = m2.peek_mailbox(&reg, 0, 100).await.unwrap();
        assert_eq!(got.len(), 5);
        assert_eq!(
            got.iter().map(|e| e.seq_num).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        let ids: BTreeSet<_> = got.iter().map(|e| e.event_id.clone()).collect();
        assert_eq!(ids.len(), 5);
    }

    async fn disconnect_mid_backlog_loses_nothing(store: Arc<dyn EventStore>) {
        let m = manager(store, 16);
        let svc = EventServiceImpl::new(m.clone(), "h", 1);
        let reg = subscribe(&m, DeliveryMode::Durable).await;
        for _ in 0..5 {
            emit(&m, "a").await;
        }

        let mut s1 = receive(&svc, &reg).await;
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for _ in 0..2 {
            seen.insert(s1.next().await.unwrap().unwrap().seq_num);
        }
        drop(s1); // client disconnects mid-backlog
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut s2 = receive(&svc, &reg).await;
        for e in collect(&mut s2, Duration::from_millis(400)).await {
            seen.insert(e.seq_num);
        }
        assert_eq!(seen, (1..=5).collect::<BTreeSet<u64>>());
    }

    async fn live_delivered_not_replayed(store: Arc<dyn EventStore>) {
        let m = manager(store, 16);
        let svc = EventServiceImpl::new(m.clone(), "h", 1);
        let reg = subscribe(&m, DeliveryMode::Durable).await;

        let mut s1 = receive(&svc, &reg).await;
        for _ in 0..3 {
            emit(&m, "a").await;
        }
        let got = collect(&mut s1, Duration::from_millis(400)).await;
        assert_eq!(got.iter().map(|e| e.seq_num).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(got[0].handback, vec![9]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(s1);

        let mut s2 = receive(&svc, &reg).await;
        assert!(collect(&mut s2, Duration::from_millis(400))
            .await
            .is_empty());
        // New events continue the sequence.
        emit(&m, "a").await;
        let got = collect(&mut s2, Duration::from_millis(400)).await;
        assert_eq!(got.iter().map(|e| e.seq_num).collect::<Vec<_>>(), [4]);
    }

    async fn durable_recovers_from_lag(store: Arc<dyn EventStore>) {
        let m = manager(store, 2); // tiny broadcast buffer
        let svc = EventServiceImpl::new(m.clone(), "h", 1);
        let reg = subscribe(&m, DeliveryMode::Durable).await;
        let mut s = receive(&svc, &reg).await;
        for _ in 0..20 {
            emit(&m, "a").await; // no yield: the receiver task lags
        }
        let got = collect(&mut s, Duration::from_millis(500)).await;
        let seqs: Vec<u64> = got.iter().map(|e| e.seq_num).collect();
        assert_eq!(seqs, (1..=20).collect::<Vec<u64>>());
    }

    // ── in-memory ─────────────────────────────────────────────────────────────

    fn mem() -> Arc<dyn EventStore> {
        Arc::new(InMemoryEventStore::new())
    }

    #[tokio::test]
    async fn mem_two_event_types_both_arrive() {
        two_event_types_both_arrive(mem()).await;
    }

    #[tokio::test]
    async fn mem_restart_does_not_overwrite() {
        restart_does_not_overwrite(mem()).await;
    }

    #[tokio::test]
    async fn mem_disconnect_mid_backlog_loses_nothing() {
        disconnect_mid_backlog_loses_nothing(mem()).await;
    }

    #[tokio::test]
    async fn mem_live_delivered_not_replayed() {
        live_delivered_not_replayed(mem()).await;
    }

    #[tokio::test]
    async fn mem_durable_recovers_from_lag() {
        durable_recovers_from_lag(mem()).await;
    }

    #[tokio::test]
    async fn best_effort_lag_ends_stream_with_data_loss() {
        let m = manager(mem(), 2);
        let svc = EventServiceImpl::new(m.clone(), "h", 1);
        let reg = subscribe(&m, DeliveryMode::BestEffort).await;
        let mut s = receive(&svc, &reg).await;
        for _ in 0..20 {
            emit(&m, "a").await;
        }
        let mut status = None;
        while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(2), s.next()).await {
            if let Err(e) = item {
                status = Some(e);
                break;
            }
        }
        let status = status.expect("stream should end with an error");
        assert_eq!(status.code(), tonic::Code::DataLoss);
        // and then the stream is over
        assert!(s.next().await.is_none());
    }

    /// Store whose enqueue always fails.
    struct FailingEnqueue(InMemoryEventStore);

    #[async_trait]
    impl EventStore for FailingEnqueue {
        async fn create_subscription(&self, s: SubscriptionRecord) -> Result<(), Error> {
            self.0.create_subscription(s).await
        }
        async fn get_subscription(&self, r: &str) -> Result<Option<SubscriptionRecord>, Error> {
            self.0.get_subscription(r).await
        }
        async fn remove_subscription(&self, r: &str) -> Result<(), Error> {
            self.0.remove_subscription(r).await
        }
        async fn remove_by_lease(&self, l: &str) -> Result<Option<SubscriptionRecord>, Error> {
            self.0.remove_by_lease(l).await
        }
        async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error> {
            self.0.list_subscriptions().await
        }
        async fn enqueue(&self, _: &str, _: EventRecord) -> Result<u64, Error> {
            Err(Error::Storage("boom".into()))
        }
        async fn peek(&self, r: &str, a: u64, l: usize) -> Result<Vec<EventRecord>, Error> {
            self.0.peek(r, a, l).await
        }
        async fn ack(&self, r: &str, s: &[u64]) -> Result<(), Error> {
            self.0.ack(r, s).await
        }
    }

    #[tokio::test]
    async fn emit_fails_when_durable_enqueue_fails() {
        let m = manager(Arc::new(FailingEnqueue(InMemoryEventStore::new())), 16);
        subscribe(&m, DeliveryMode::Durable).await;
        let err = m
            .emit("src".into(), "a".into(), HashMap::new(), vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Storage(_)));
    }

    // ── dynamo (MiniStack) ───────────────────────────────────────────────────

    async fn dynamo_store() -> (Arc<dyn EventStore>, impl std::future::Future<Output = ()>) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");
        std::env::set_var("COORDIN8_AUTO_CREATE_TABLES", "true");
        let client = coordin8_provider_dynamo::make_dynamo_client().await;
        let suffix = uuid::Uuid::new_v4();
        let sub_table = format!("coordin8_event_sub_test_{suffix}");
        let mailbox_table = format!("coordin8_event_mailbox_test_{suffix}");
        let store = coordin8_provider_dynamo::DynamoEventStore::with_tables(
            client.clone(),
            &sub_table,
            &mailbox_table,
        );
        store.init().await.expect("table creation failed");
        let cleanup = async move {
            let _ = client.delete_table().table_name(sub_table).send().await;
            let _ = client.delete_table().table_name(mailbox_table).send().await;
        };
        (Arc::new(store), cleanup)
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn dynamo_two_event_types_both_arrive() {
        let (store, cleanup) = dynamo_store().await;
        two_event_types_both_arrive(store).await;
        cleanup.await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn dynamo_restart_does_not_overwrite() {
        let (store, cleanup) = dynamo_store().await;
        restart_does_not_overwrite(store).await;
        cleanup.await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn dynamo_disconnect_mid_backlog_loses_nothing() {
        let (store, cleanup) = dynamo_store().await;
        disconnect_mid_backlog_loses_nothing(store).await;
        cleanup.await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn dynamo_live_delivered_not_replayed() {
        let (store, cleanup) = dynamo_store().await;
        live_delivered_not_replayed(store).await;
        cleanup.await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn dynamo_durable_recovers_from_lag() {
        let (store, cleanup) = dynamo_store().await;
        durable_recovers_from_lag(store).await;
        cleanup.await;
    }
}
