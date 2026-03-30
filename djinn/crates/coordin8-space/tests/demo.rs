/// Space demo — runs in-process, no gRPC needed.
///
/// Demonstrates the full cycle:
///   out → read → take → blocking take → watch → lease expiry
use std::sync::Arc;
use std::time::Duration;

use coordin8_core::{SpaceEventKind, TupleRecord};
use coordin8_lease::LeaseManager;
use coordin8_provider_local::{InMemoryLeaseStore, InMemorySpaceStore};
use coordin8_space::SpaceManager;
use tokio::sync::broadcast;

fn make_manager() -> (Arc<SpaceManager>, broadcast::Sender<TupleRecord>) {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let lease_manager = Arc::new(LeaseManager::new(lease_store, coordin8_core::LeaseConfig::default()));
    let space_store = Arc::new(InMemorySpaceStore::new());
    let (tuple_tx, _) = broadcast::channel(256);
    let (expiry_tx, _) = broadcast::channel(256);
    let mgr = Arc::new(SpaceManager::new(
        space_store,
        lease_manager,
        tuple_tx.clone(),
        expiry_tx,
    ));
    (mgr, tuple_tx)
}

fn make_manager_with_reaper() -> Arc<SpaceManager> {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let lease_manager = Arc::new(LeaseManager::new(lease_store.clone(), coordin8_core::LeaseConfig::default()));
    let space_store = Arc::new(InMemorySpaceStore::new());
    let (tuple_tx, _) = broadcast::channel(256);
    let (expiry_tx, _) = broadcast::channel(256);

    let mgr = Arc::new(SpaceManager::new(
        space_store,
        Arc::clone(&lease_manager),
        tuple_tx,
        expiry_tx,
    ));

    // Start reaper + expiry listener
    let (reaper_expiry_tx, _) = broadcast::channel::<coordin8_core::LeaseRecord>(256);
    let reaper_mgr = Arc::clone(&lease_manager);
    let reaper_tx = reaper_expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_mgr, reaper_tx, Duration::from_millis(100)).await;
    });

    let space_expiry_mgr = Arc::clone(&mgr);
    let mut space_expiry_rx = reaper_expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = space_expiry_rx.recv().await {
            if lease.resource_id.starts_with("space:") {
                space_expiry_mgr.on_tuple_expired(&lease.lease_id).await;
            } else if lease.resource_id.starts_with("space-watch:") {
                space_expiry_mgr.on_watch_expired(&lease.lease_id).await;
            }
        }
    });

    mgr
}

// ── Test 1: out and read ────────────────────────────────────────────────────

#[tokio::test]
async fn out_and_read() {
    let (mgr, _) = make_manager();

    let (record, _lease) = mgr
        .out(
            [("kind".into(), "price".into()), ("ticker".into(), "AAPL".into())].into(),
            b"{\"price\": 189}".to_vec(),
            60,
            "test".into(),
            None,
        )
        .await
        .unwrap();

    println!("\n[demo] Out: tuple_id={}", record.tuple_id);

    // Read by template
    let found = mgr
        .read([("kind".into(), "price".into())].into(), false, 0)
        .await
        .unwrap();

    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.tuple_id, record.tuple_id);
    assert_eq!(found.attrs["ticker"], "AAPL");
    println!("[demo] Read: found tuple_id={} ✓", found.tuple_id);
}

// ── Test 2: out and take ────────────────────────────────────────────────────

#[tokio::test]
async fn out_and_take() {
    let (mgr, _) = make_manager();

    mgr.out(
        [("kind".into(), "task".into()), ("name".into(), "build".into())].into(),
        vec![],
        60,
        "producer".into(),
        None,
    )
    .await
    .unwrap();

    // Take it
    let taken = mgr
        .take([("kind".into(), "task".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(taken.is_some());
    println!("\n[demo] Take: got tuple ✓");

    // Second take should return None
    let second = mgr
        .take([("kind".into(), "task".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(second.is_none());
    println!("[demo] Second take: None ✓");
}

// ── Test 3: read nonblocking empty ──────────────────────────────────────────

#[tokio::test]
async fn read_nonblocking_empty() {
    let (mgr, _) = make_manager();

    let result = mgr
        .read([("kind".into(), "nope".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(result.is_none());
    println!("\n[demo] Read non-blocking empty: None ✓");
}

// ── Test 4: take nonblocking empty ──────────────────────────────────────────

#[tokio::test]
async fn take_nonblocking_empty() {
    let (mgr, _) = make_manager();

    let result = mgr
        .take([("kind".into(), "nope".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(result.is_none());
    println!("\n[demo] Take non-blocking empty: None ✓");
}

// ── Test 5: blocking take unblocks on out ───────────────────────────────────

#[tokio::test]
async fn take_blocking_unblocks_on_out() {
    let (mgr, _) = make_manager();
    let mgr2 = Arc::clone(&mgr);

    // Spawn a blocking taker
    let taker = tokio::spawn(async move {
        mgr2.take([("kind".into(), "job".into())].into(), true, 5000)
            .await
            .unwrap()
    });

    // Give the taker time to subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Out a matching tuple
    mgr.out(
        [("kind".into(), "job".into()), ("id".into(), "42".into())].into(),
        vec![],
        60,
        "dispatcher".into(),
        None,
    )
    .await
    .unwrap();

    let result = taker.await.unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().attrs["id"], "42");
    println!("\n[demo] Blocking take unblocked on out ✓");
}

// ── Test 6: read blocking timeout ───────────────────────────────────────────

#[tokio::test]
async fn read_blocking_timeout() {
    let (mgr, _) = make_manager();

    let start = std::time::Instant::now();
    let result = mgr
        .read([("kind".into(), "ghost".into())].into(), true, 100)
        .await
        .unwrap();

    assert!(result.is_none());
    assert!(start.elapsed() >= Duration::from_millis(80));
    println!("\n[demo] Read blocking timeout: None after {:?} ✓", start.elapsed());
}

// ── Test 7: template matching operators ─────────────────────────────────────

#[tokio::test]
async fn template_matching() {
    let (mgr, _) = make_manager();

    mgr.out(
        [
            ("kind".into(), "sensor".into()),
            ("location".into(), "tampa-east-7".into()),
            ("metrics".into(), "wind,humidity,temp".into()),
        ]
        .into(),
        vec![],
        60,
        "test".into(),
        None,
    )
    .await
    .unwrap();

    // Exact match
    let exact = mgr
        .read([("kind".into(), "sensor".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(exact.is_some());

    // contains:
    let contains = mgr
        .read(
            [("metrics".into(), "contains:humidity".into())].into(),
            false,
            0,
        )
        .await
        .unwrap();
    assert!(contains.is_some());

    // starts_with:
    let starts = mgr
        .read(
            [("location".into(), "starts_with:tampa".into())].into(),
            false,
            0,
        )
        .await
        .unwrap();
    assert!(starts.is_some());

    // Wildcard (Any)
    let any = mgr
        .read(
            [("kind".into(), "sensor".into()), ("location".into(), "*".into())].into(),
            false,
            0,
        )
        .await
        .unwrap();
    assert!(any.is_some());

    // No match
    let miss = mgr
        .read([("kind".into(), "camera".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(miss.is_none());

    println!("\n[demo] Template matching: exact, contains, starts_with, wildcard, miss ✓");
}

// ── Test 8: tuple lease expiry ──────────────────────────────────────────────

#[tokio::test]
async fn tuple_lease_expiry() {
    let mgr = make_manager_with_reaper();

    mgr.out(
        [("kind".into(), "ephemeral".into())].into(),
        vec![],
        1, // 1 second TTL
        "test".into(),
        None,
    )
    .await
    .unwrap();

    // Should exist immediately
    let exists = mgr
        .read([("kind".into(), "ephemeral".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(exists.is_some());

    // Wait for expiry
    tokio::time::sleep(Duration::from_secs(2)).await;

    let gone = mgr
        .read([("kind".into(), "ephemeral".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(gone.is_none());
    println!("\n[demo] Tuple lease expiry: gone after TTL ✓");
}

// ── Test 9: watch appearance ────────────────────────────────────────────────

#[tokio::test]
async fn watch_appearance() {
    let (mgr, _) = make_manager();
    let mgr2 = Arc::clone(&mgr);

    // Create a watch
    let (_watch_id, _lease_id) = mgr
        .watch(
            [("kind".into(), "alert".into())].into(),
            SpaceEventKind::Appearance,
            60,
        )
        .await
        .unwrap();

    // Subscribe to the broadcast to verify events flow
    let mut rx = mgr.subscribe_tuple_broadcast();

    // Out a matching tuple
    mgr2.out(
        [("kind".into(), "alert".into()), ("level".into(), "critical".into())].into(),
        b"disk full".to_vec(),
        60,
        "monitor".into(),
        None,
    )
    .await
    .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(event.attrs["kind"], "alert");
    assert_eq!(event.attrs["level"], "critical");
    println!("\n[demo] Watch appearance: received event ✓");
}

// ── Test 10: concurrent take — one winner ───────────────────────────────────

#[tokio::test]
async fn take_race_one_winner() {
    let (mgr, _) = make_manager();

    mgr.out(
        [("kind".into(), "token".into())].into(),
        vec![],
        60,
        "test".into(),
        None,
    )
    .await
    .unwrap();

    let mgr1 = Arc::clone(&mgr);
    let mgr2 = Arc::clone(&mgr);

    let t1 = tokio::spawn(async move {
        mgr1.take([("kind".into(), "token".into())].into(), false, 0)
            .await
            .unwrap()
    });
    let t2 = tokio::spawn(async move {
        mgr2.take([("kind".into(), "token".into())].into(), false, 0)
            .await
            .unwrap()
    });

    let (r1, r2) = tokio::join!(t1, t2);
    let r1 = r1.unwrap();
    let r2 = r2.unwrap();

    // Exactly one should win
    let winners = [r1.is_some(), r2.is_some()]
        .iter()
        .filter(|&&w| w)
        .count();
    assert_eq!(winners, 1);
    println!("\n[demo] Take race: exactly one winner ✓");
}

// ── Test 11: cancel tuple removes it ────────────────────────────────────────

#[tokio::test]
async fn cancel_tuple_removes() {
    let (mgr, _) = make_manager();

    let (record, _lease) = mgr
        .out(
            [("kind".into(), "temp".into())].into(),
            vec![],
            60,
            "test".into(),
            None,
        )
        .await
        .unwrap();

    mgr.cancel_tuple(&record.tuple_id).await.unwrap();

    let gone = mgr
        .read([("kind".into(), "temp".into())].into(), false, 0)
        .await
        .unwrap();
    assert!(gone.is_none());
    println!("\n[demo] Cancel tuple: removed ✓");
}

// ── Test 12: provenance tracking ────────────────────────────────────────────

#[tokio::test]
async fn provenance_tracking() {
    let (mgr, _) = make_manager();

    // Out an input tuple
    let (input, _lease) = mgr
        .out(
            [("kind".into(), "raw".into())].into(),
            b"raw data".to_vec(),
            60,
            "sensor".into(),
            None,
        )
        .await
        .unwrap();

    // Out a derived tuple with lineage
    let (derived, _lease) = mgr
        .out(
            [("kind".into(), "processed".into())].into(),
            b"processed data".to_vec(),
            60,
            "pipeline".into(),
            Some(input.tuple_id.clone()),
        )
        .await
        .unwrap();

    assert_eq!(derived.written_by, "pipeline");
    assert_eq!(derived.input_tuple_id, Some(input.tuple_id));
    println!("\n[demo] Provenance: lineage tracked ✓");
}
