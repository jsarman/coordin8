/// Space demo — runs in-process, no gRPC needed.
///
/// Demonstrates the full cycle:
///   write → read → take → blocking take → notify → contents → lease expiry
///   transactional isolation: uncommitted writes, commit, abort
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

// ── Test 1: write and read ──────────────────────────────────────────────────

#[tokio::test]
async fn write_and_read() {
    let (mgr, _) = make_manager();

    let (record, _lease) = mgr
        .write(
            [("kind".into(), "price".into()), ("ticker".into(), "AAPL".into())].into(),
            b"{\"price\": 189}".to_vec(),
            60,
            "test".into(),
            None,
            None,
        )
        .await
        .unwrap();

    println!("\n[demo] Write: tuple_id={}", record.tuple_id);

    // Read by template
    let found = mgr
        .read([("kind".into(), "price".into())].into(), false, 0, None)
        .await
        .unwrap();

    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.tuple_id, record.tuple_id);
    assert_eq!(found.attrs["ticker"], "AAPL");
    println!("[demo] Read: found tuple_id={} ✓", found.tuple_id);
}

// ── Test 2: write and take ──────────────────────────────────────────────────

#[tokio::test]
async fn write_and_take() {
    let (mgr, _) = make_manager();

    mgr.write(
        [("kind".into(), "task".into()), ("name".into(), "build".into())].into(),
        vec![],
        60,
        "producer".into(),
        None,
        None,
    )
    .await
    .unwrap();

    // Take it
    let taken = mgr
        .take([("kind".into(), "task".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(taken.is_some());
    println!("\n[demo] Take: got tuple ✓");

    // Second take should return None
    let second = mgr
        .take([("kind".into(), "task".into())].into(), false, 0, None)
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
        .read([("kind".into(), "nope".into())].into(), false, 0, None)
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
        .take([("kind".into(), "nope".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(result.is_none());
    println!("\n[demo] Take non-blocking empty: None ✓");
}

// ── Test 5: blocking take unblocks on write ─────────────────────────────────

#[tokio::test]
async fn take_blocking_unblocks_on_write() {
    let (mgr, _) = make_manager();
    let mgr2 = Arc::clone(&mgr);

    // Spawn a blocking taker
    let taker = tokio::spawn(async move {
        mgr2.take([("kind".into(), "job".into())].into(), true, 5000, None)
            .await
            .unwrap()
    });

    // Give the taker time to subscribe
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Write a matching tuple
    mgr.write(
        [("kind".into(), "job".into()), ("id".into(), "42".into())].into(),
        vec![],
        60,
        "dispatcher".into(),
        None,
        None,
    )
    .await
    .unwrap();

    let result = taker.await.unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().attrs["id"], "42");
    println!("\n[demo] Blocking take unblocked on write ✓");
}

// ── Test 6: read blocking timeout ───────────────────────────────────────────

#[tokio::test]
async fn read_blocking_timeout() {
    let (mgr, _) = make_manager();

    let start = std::time::Instant::now();
    let result = mgr
        .read([("kind".into(), "ghost".into())].into(), true, 100, None)
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

    mgr.write(
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
        None,
    )
    .await
    .unwrap();

    // Exact match
    let exact = mgr
        .read([("kind".into(), "sensor".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(exact.is_some());

    // contains:
    let contains = mgr
        .read(
            [("metrics".into(), "contains:humidity".into())].into(),
            false,
            0,
            None,
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
            None,
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
            None,
        )
        .await
        .unwrap();
    assert!(any.is_some());

    // No match
    let miss = mgr
        .read([("kind".into(), "camera".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(miss.is_none());

    println!("\n[demo] Template matching: exact, contains, starts_with, wildcard, miss ✓");
}

// ── Test 8: tuple lease expiry ──────────────────────────────────────────────

#[tokio::test]
async fn tuple_lease_expiry() {
    let mgr = make_manager_with_reaper();

    mgr.write(
        [("kind".into(), "ephemeral".into())].into(),
        vec![],
        1, // 1 second TTL
        "test".into(),
        None,
        None,
    )
    .await
    .unwrap();

    // Should exist immediately
    let exists = mgr
        .read([("kind".into(), "ephemeral".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(exists.is_some());

    // Wait for expiry
    tokio::time::sleep(Duration::from_secs(2)).await;

    let gone = mgr
        .read([("kind".into(), "ephemeral".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(gone.is_none());
    println!("\n[demo] Tuple lease expiry: gone after TTL ✓");
}

// ── Test 9: notify appearance ───────────────────────────────────────────────

#[tokio::test]
async fn notify_appearance() {
    let (mgr, _) = make_manager();
    let mgr2 = Arc::clone(&mgr);

    // Create a notification
    let (_watch_id, _lease_id) = mgr
        .notify(
            [("kind".into(), "alert".into())].into(),
            SpaceEventKind::Appearance,
            60,
            b"my-handback".to_vec(),
        )
        .await
        .unwrap();

    // Subscribe to the broadcast to verify events flow
    let mut rx = mgr.subscribe_tuple_broadcast();

    // Write a matching tuple
    mgr2.write(
        [("kind".into(), "alert".into()), ("level".into(), "critical".into())].into(),
        b"disk full".to_vec(),
        60,
        "monitor".into(),
        None,
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
    println!("\n[demo] Notify appearance: received event ✓");
}

// ── Test 10: concurrent take — one winner ───────────────────────────────────

#[tokio::test]
async fn take_race_one_winner() {
    let (mgr, _) = make_manager();

    mgr.write(
        [("kind".into(), "token".into())].into(),
        vec![],
        60,
        "test".into(),
        None,
        None,
    )
    .await
    .unwrap();

    let mgr1 = Arc::clone(&mgr);
    let mgr2 = Arc::clone(&mgr);

    let t1 = tokio::spawn(async move {
        mgr1.take([("kind".into(), "token".into())].into(), false, 0, None)
            .await
            .unwrap()
    });
    let t2 = tokio::spawn(async move {
        mgr2.take([("kind".into(), "token".into())].into(), false, 0, None)
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
        .write(
            [("kind".into(), "temp".into())].into(),
            vec![],
            60,
            "test".into(),
            None,
            None,
        )
        .await
        .unwrap();

    mgr.cancel(&record.tuple_id).await.unwrap();

    let gone = mgr
        .read([("kind".into(), "temp".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(gone.is_none());
    println!("\n[demo] Cancel tuple: removed ✓");
}

// ── Test 12: provenance tracking ────────────────────────────────────────────

#[tokio::test]
async fn provenance_tracking() {
    let (mgr, _) = make_manager();

    // Write an input tuple
    let (input, _lease) = mgr
        .write(
            [("kind".into(), "raw".into())].into(),
            b"raw data".to_vec(),
            60,
            "sensor".into(),
            None,
            None,
        )
        .await
        .unwrap();

    // Write a derived tuple with lineage
    let (derived, _lease) = mgr
        .write(
            [("kind".into(), "processed".into())].into(),
            b"processed data".to_vec(),
            60,
            "pipeline".into(),
            Some(input.tuple_id.clone()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(derived.written_by, "pipeline");
    assert_eq!(derived.input_tuple_id, Some(input.tuple_id));
    println!("\n[demo] Provenance: lineage tracked ✓");
}

// ── Test 13: contents — bulk read ───────────────────────────────────────────

#[tokio::test]
async fn contents_bulk_read() {
    let (mgr, _) = make_manager();

    // Write 3 price tuples and 1 task tuple
    for ticker in &["AAPL", "TSLA", "GOOG"] {
        mgr.write(
            [("kind".into(), "price".into()), ("ticker".into(), ticker.to_string())].into(),
            vec![],
            60,
            "feed".into(),
            None,
            None,
        )
        .await
        .unwrap();
    }
    mgr.write(
        [("kind".into(), "task".into())].into(),
        vec![],
        60,
        "scheduler".into(),
        None,
        None,
    )
    .await
    .unwrap();

    // Contents with price template should return 3
    let prices = mgr
        .contents([("kind".into(), "price".into())].into(), None)
        .await
        .unwrap();
    assert_eq!(prices.len(), 3);
    println!("\n[demo] Contents: got {} price tuples ✓", prices.len());

    // Contents with empty template should return all 4
    let all = mgr
        .contents(Default::default(), None)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
    println!("[demo] Contents: got {} total tuples ✓", all.len());

    // Contents with no-match template should return 0
    let none = mgr
        .contents([("kind".into(), "nope".into())].into(), None)
        .await
        .unwrap();
    assert!(none.is_empty());
    println!("[demo] Contents: empty for no-match ✓");
}

// ══════════════════════════════════════════════════════════════════════════════
// Transactional isolation tests
// ══════════════════════════════════════════════════════════════════════════════

// ── Test 14: write under txn → read without txn → None (isolation) ──────────

#[tokio::test]
async fn txn_write_invisible_without_txn() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-001".to_string();

    // Write under transaction
    mgr.write(
        [("kind".into(), "secret".into())].into(),
        b"hidden".to_vec(),
        60,
        "writer".into(),
        None,
        Some(txn_id.clone()),
    )
    .await
    .unwrap();

    // Read WITHOUT txn → should NOT see the tuple
    let result = mgr
        .read([("kind".into(), "secret".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(result.is_none(), "uncommitted tuple should be invisible without txn");

    // Take WITHOUT txn → should NOT see the tuple
    let result = mgr
        .take([("kind".into(), "secret".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(result.is_none(), "uncommitted tuple should not be takeable without txn");

    println!("\n[demo] Txn isolation: uncommitted write invisible to non-txn reads ✓");
}

// ── Test 15: write under txn → read WITH txn → found ───────────────────────

#[tokio::test]
async fn txn_write_visible_with_txn() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-002".to_string();

    // Write under transaction
    mgr.write(
        [("kind".into(), "secret".into()), ("data".into(), "classified".into())].into(),
        b"payload".to_vec(),
        60,
        "writer".into(),
        None,
        Some(txn_id.clone()),
    )
    .await
    .unwrap();

    // Read WITH same txn → should see it
    let result = mgr
        .read(
            [("kind".into(), "secret".into())].into(),
            false,
            0,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    assert!(result.is_some(), "uncommitted tuple should be visible within its own txn");
    assert_eq!(result.unwrap().attrs["data"], "classified");

    println!("\n[demo] Txn isolation: uncommitted write visible to own txn ✓");
}

// ── Test 16: write under txn → commit → read without txn → found ───────────

#[tokio::test]
async fn txn_commit_makes_visible() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-003".to_string();

    // Write under transaction
    mgr.write(
        [("kind".into(), "committed".into())].into(),
        vec![],
        60,
        "writer".into(),
        None,
        Some(txn_id.clone()),
    )
    .await
    .unwrap();

    // Not visible before commit
    let before = mgr
        .read([("kind".into(), "committed".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(before.is_none());

    // Commit
    mgr.commit_space_txn(&txn_id).await.unwrap();

    // Now visible
    let after = mgr
        .read([("kind".into(), "committed".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(after.is_some(), "tuple should be visible after commit");

    println!("\n[demo] Txn commit: tuple visible after commit ✓");
}

// ── Test 17: write under txn → abort → read without txn → None ─────────────

#[tokio::test]
async fn txn_abort_discards_writes() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-004".to_string();

    // Write under transaction
    mgr.write(
        [("kind".into(), "doomed".into())].into(),
        vec![],
        60,
        "writer".into(),
        None,
        Some(txn_id.clone()),
    )
    .await
    .unwrap();

    // Abort
    mgr.abort_space_txn(&txn_id).await.unwrap();

    // Should not exist anywhere
    let result = mgr
        .read([("kind".into(), "doomed".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(result.is_none(), "aborted tuple should be gone");

    // Also not visible under the (now-dead) txn
    let result = mgr
        .read(
            [("kind".into(), "doomed".into())].into(),
            false,
            0,
            Some(txn_id),
        )
        .await
        .unwrap();
    assert!(result.is_none(), "aborted tuple should be gone even under txn");

    println!("\n[demo] Txn abort: uncommitted writes discarded ✓");
}

// ── Test 18: take under txn → commit → gone ────────────────────────────────

#[tokio::test]
async fn txn_take_then_commit() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-005".to_string();

    // Write under txn, then take under same txn
    mgr.write(
        [("kind".into(), "takeable".into())].into(),
        vec![],
        60,
        "writer".into(),
        None,
        Some(txn_id.clone()),
    )
    .await
    .unwrap();

    let taken = mgr
        .take(
            [("kind".into(), "takeable".into())].into(),
            false,
            0,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    assert!(taken.is_some(), "should be able to take own uncommitted tuple");

    // Commit
    mgr.commit_space_txn(&txn_id).await.unwrap();

    // Should be gone (was taken before commit, nothing to flush)
    let result = mgr
        .read([("kind".into(), "takeable".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(result.is_none(), "taken-then-committed tuple should be gone");

    println!("\n[demo] Txn take+commit: tuple consumed ✓");
}

// ── Test 19: isolation between different transactions ───────────────────────

#[tokio::test]
async fn txn_isolation_between_txns() {
    let (mgr, _) = make_manager();

    // Two transactions writing different tuples
    mgr.write(
        [("kind".into(), "alpha".into())].into(),
        vec![],
        60,
        "txn-a".into(),
        None,
        Some("txn-A".to_string()),
    )
    .await
    .unwrap();

    mgr.write(
        [("kind".into(), "beta".into())].into(),
        vec![],
        60,
        "txn-b".into(),
        None,
        Some("txn-B".to_string()),
    )
    .await
    .unwrap();

    // txn-A can see alpha but not beta
    let sees_alpha = mgr
        .read(
            [("kind".into(), "alpha".into())].into(),
            false,
            0,
            Some("txn-A".to_string()),
        )
        .await
        .unwrap();
    assert!(sees_alpha.is_some(), "txn-A should see its own write");

    let sees_beta = mgr
        .read(
            [("kind".into(), "beta".into())].into(),
            false,
            0,
            Some("txn-A".to_string()),
        )
        .await
        .unwrap();
    assert!(sees_beta.is_none(), "txn-A should NOT see txn-B's write");

    // txn-B can see beta but not alpha
    let sees_beta = mgr
        .read(
            [("kind".into(), "beta".into())].into(),
            false,
            0,
            Some("txn-B".to_string()),
        )
        .await
        .unwrap();
    assert!(sees_beta.is_some(), "txn-B should see its own write");

    let sees_alpha = mgr
        .read(
            [("kind".into(), "alpha".into())].into(),
            false,
            0,
            Some("txn-B".to_string()),
        )
        .await
        .unwrap();
    assert!(sees_alpha.is_none(), "txn-B should NOT see txn-A's write");

    println!("\n[demo] Txn isolation: transactions can't see each other's writes ✓");
}

// ── Test 20: take from committed under txn → abort → restored ──────────────

#[tokio::test]
async fn txn_take_committed_abort_restores() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-006".to_string();

    // Write a committed tuple (no txn)
    mgr.write(
        [("kind".into(), "restoreable".into())].into(),
        vec![],
        60,
        "writer".into(),
        None,
        None,
    )
    .await
    .unwrap();

    // Take it under a transaction
    let taken = mgr
        .take(
            [("kind".into(), "restoreable".into())].into(),
            false,
            0,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    assert!(taken.is_some(), "should take committed tuple under txn");

    // Non-transactional read should NOT see it (it's been taken)
    let gone = mgr
        .read([("kind".into(), "restoreable".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(gone.is_none(), "taken tuple should be invisible");

    // Abort the transaction — tuple should be restored
    mgr.abort_space_txn(&txn_id).await.unwrap();

    // Now it should be visible again
    let restored = mgr
        .read([("kind".into(), "restoreable".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(restored.is_some(), "tuple should be restored after abort");

    println!("\n[demo] Txn abort: committed take restored ✓");
}

// ── Test 21: the full user scenario ────────────────────────────────────────
// "write with transaction → read without → nothing → take without → nothing
//  → read with txn → found → take with txn → commit → read → nothing"

#[tokio::test]
async fn txn_full_isolation_scenario() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-full".to_string();

    // 1. Write a tuple under the transaction
    let (record, _) = mgr
        .write(
            [("kind".into(), "order".into()), ("id".into(), "42".into())].into(),
            b"order payload".to_vec(),
            60,
            "producer".into(),
            None,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    println!("\n[demo] 1. Wrote tuple {} under txn", record.tuple_id);

    // 2. Read WITHOUT txn → Nothing
    let r = mgr
        .read([("kind".into(), "order".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "step 2: non-txn read should see nothing");
    println!("[demo] 2. Read without txn → None ✓");

    // 3. Take WITHOUT txn → Nothing
    let r = mgr
        .take([("kind".into(), "order".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "step 3: non-txn take should see nothing");
    println!("[demo] 3. Take without txn → None ✓");

    // 4. Read WITH txn → Found
    let r = mgr
        .read(
            [("kind".into(), "order".into())].into(),
            false,
            0,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    assert!(r.is_some(), "step 4: txn read should find it");
    assert_eq!(r.unwrap().attrs["id"], "42");
    println!("[demo] 4. Read with txn → found ✓");

    // 5. Take WITH txn → Gets the tuple
    let r = mgr
        .take(
            [("kind".into(), "order".into())].into(),
            false,
            0,
            Some(txn_id.clone()),
        )
        .await
        .unwrap();
    assert!(r.is_some(), "step 5: txn take should get it");
    println!("[demo] 5. Take with txn → got it ✓");

    // 6. Commit
    mgr.commit_space_txn(&txn_id).await.unwrap();
    println!("[demo] 6. Committed ✓");

    // 7. Read → Nothing (taken before commit, nothing flushed)
    let r = mgr
        .read([("kind".into(), "order".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "step 7: should be gone after take+commit");
    println!("[demo] 7. Read after commit → None ✓");

    println!("[demo] Full txn isolation scenario passed ✓");
}

// ── Test 22: cross-process shared txn ────────────────────────────────────────
// Process A and B share the same transaction. A writes, B reads/takes — both
// under the same txn_id. Outsiders see nothing until commit.

#[tokio::test]
async fn txn_cross_process_shared_txn() {
    let (mgr, _) = make_manager();
    let txn = "txn-shared".to_string();

    // ── Process A: write under the shared transaction ───────────────────────
    let (record, _) = mgr
        .write(
            [("kind".into(), "transfer".into()), ("amount".into(), "500".into())].into(),
            b"bank transfer".to_vec(),
            60,
            "process-a".into(),
            None,
            Some(txn.clone()),
        )
        .await
        .unwrap();
    println!("\n[demo] Process A: wrote tuple {} under shared txn", record.tuple_id);

    // ── Outsider: read with no transaction → nothing ────────────────────────
    let r = mgr
        .read([("kind".into(), "transfer".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "outsider read should see nothing");
    println!("[demo] Outsider: read (no txn) → None ✓");

    // ── Outsider: take with no transaction → nothing ────────────────────────
    let r = mgr
        .take([("kind".into(), "transfer".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "outsider take should see nothing");
    println!("[demo] Outsider: take (no txn) → None ✓");

    // ── Process B: read with the SAME transaction → found ───────────────────
    let r = mgr
        .read(
            [("kind".into(), "transfer".into())].into(),
            false,
            0,
            Some(txn.clone()),
        )
        .await
        .unwrap();
    assert!(r.is_some(), "Process B should see tuple under shared txn");
    assert_eq!(r.unwrap().attrs["amount"], "500");
    println!("[demo] Process B: read (shared txn) → found ✓");

    // ── Process B: take with the SAME transaction → claims it ───────────────
    let r = mgr
        .take(
            [("kind".into(), "transfer".into())].into(),
            false,
            0,
            Some(txn.clone()),
        )
        .await
        .unwrap();
    assert!(r.is_some(), "Process B should take tuple under shared txn");
    println!("[demo] Process B: take (shared txn) → got it ✓");

    // ── Outsider still sees nothing ─────────────────────────────────────────
    let r = mgr
        .read([("kind".into(), "transfer".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "outsider still sees nothing before commit");
    println!("[demo] Outsider: read (no txn) → None (still uncommitted) ✓");

    // ── Commit the shared transaction ───────────────────────────────────────
    mgr.commit_space_txn(&txn).await.unwrap();
    println!("[demo] Committed shared txn ✓");

    // ── Final: tuple is gone (written + taken under same txn, committed) ────
    let r = mgr
        .read([("kind".into(), "transfer".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "tuple consumed — written and taken in same txn");
    println!("[demo] Final: read → None (tuple consumed) ✓");

    println!("[demo] Cross-process shared txn passed ✓");
}

// ── Test 23: shared txn abort — writer writes, taker takes, txn aborts ──────
// Both operate under the same txn. Abort = tuple never existed. The taker's
// application must handle the abort (retry, compensate, etc).

#[tokio::test]
async fn txn_shared_abort_voids_everything() {
    let (mgr, _) = make_manager();
    let txn = "txn-doomed".to_string();

    // ── Writer writes under shared txn ──────────────────────────────────────
    mgr.write(
        [("kind".into(), "payment".into()), ("id".into(), "99".into())].into(),
        b"$500".to_vec(),
        60,
        "writer".into(),
        None,
        Some(txn.clone()),
    )
    .await
    .unwrap();
    println!("\n[demo] Writer: wrote payment under shared txn");

    // ── Taker takes under same txn → gets it ────────────────────────────────
    let taken = mgr
        .take(
            [("kind".into(), "payment".into())].into(),
            false,
            0,
            Some(txn.clone()),
        )
        .await
        .unwrap();
    assert!(taken.is_some(), "taker should get tuple under shared txn");
    assert_eq!(taken.unwrap().attrs["id"], "99");
    println!("[demo] Taker: took payment under shared txn ✓");

    // ── Abort the shared transaction ────────────────────────────────────────
    // Writer decided to abort. Everything under this txn evaporates.
    mgr.abort_space_txn(&txn).await.unwrap();
    println!("[demo] Shared txn aborted ✓");

    // ── Tuple never existed — nothing to see anywhere ───────────────────────
    let r = mgr
        .read([("kind".into(), "payment".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "tuple never existed after abort");

    let r = mgr
        .read(
            [("kind".into(), "payment".into())].into(),
            false,
            0,
            Some(txn.clone()),
        )
        .await
        .unwrap();
    assert!(r.is_none(), "tuple gone even under the aborted txn");

    println!("[demo] Final: tuple never existed — taker's work is void ✓");
    println!("[demo] Shared txn abort passed ✓");
}

// ── Test 24: separate txns — writer commits, taker aborts → tuple restored ──
// Writer writes + commits (tuple visible). Taker takes under its own txn, then
// aborts → tuple goes back. The take never happened.

#[tokio::test]
async fn txn_taker_abort_restores_committed_tuple() {
    let (mgr, _) = make_manager();
    let writer_txn = "txn-writer".to_string();
    let taker_txn = "txn-taker".to_string();

    // ── Writer writes under its txn and commits ─────────────────────────────
    mgr.write(
        [("kind".into(), "invoice".into()), ("id".into(), "777".into())].into(),
        b"invoice data".to_vec(),
        60,
        "billing".into(),
        None,
        Some(writer_txn.clone()),
    )
    .await
    .unwrap();

    // Not visible yet
    let r = mgr
        .read([("kind".into(), "invoice".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "not visible before writer commits");
    println!("\n[demo] Writer: wrote invoice, not yet visible ✓");

    // Writer commits
    mgr.commit_space_txn(&writer_txn).await.unwrap();
    println!("[demo] Writer: committed ✓");

    // Now visible
    let r = mgr
        .read([("kind".into(), "invoice".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_some(), "visible after writer commits");
    println!("[demo] Invoice now visible ✓");

    // ── Taker takes under its own txn ───────────────────────────────────────
    let taken = mgr
        .take(
            [("kind".into(), "invoice".into())].into(),
            false,
            0,
            Some(taker_txn.clone()),
        )
        .await
        .unwrap();
    assert!(taken.is_some(), "taker claims invoice under its own txn");
    println!("[demo] Taker: took invoice under taker txn ✓");

    // Tuple invisible to outsiders (taken, pending taker's commit)
    let r = mgr
        .read([("kind".into(), "invoice".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "invisible while taken under txn");
    println!("[demo] Outsider: can't see it (taken, pending) ✓");

    // ── Taker aborts — tuple restored to committed store ────────────────────
    mgr.abort_space_txn(&taker_txn).await.unwrap();
    println!("[demo] Taker: aborted ✓");

    // Tuple is back
    let r = mgr
        .read([("kind".into(), "invoice".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_some(), "tuple restored after taker abort");
    assert_eq!(r.unwrap().attrs["id"], "777");
    println!("[demo] Invoice restored — take never happened ✓");

    println!("[demo] Taker abort restores committed tuple passed ✓");
}

// ── Test 25: blocked taker unblocks when txn take aborts ────────────────────
// Writer writes (committed). Taker 1 takes under txn. Taker 2 blocks (no match).
// Taker 1 aborts → tuple restored → taker 2 wakes up and claims it.

#[tokio::test]
async fn txn_abort_unblocks_waiting_taker() {
    let (mgr, _) = make_manager();
    let taker1_txn = "txn-taker1".to_string();

    // ── Writer writes a committed tuple ─────────────────────────────────────
    mgr.write(
        [("kind".into(), "job".into()), ("id".into(), "101".into())].into(),
        vec![],
        60,
        "dispatcher".into(),
        None,
        None,
    )
    .await
    .unwrap();
    println!("\n[demo] Writer: wrote job 101 (committed)");

    // ── Taker 1: takes under its txn ────────────────────────────────────────
    let taken = mgr
        .take(
            [("kind".into(), "job".into())].into(),
            false,
            0,
            Some(taker1_txn.clone()),
        )
        .await
        .unwrap();
    assert!(taken.is_some());
    println!("[demo] Taker 1: took job under txn ✓");

    // ── Taker 2: blocking take (no txn) — nothing available, will block ────
    let mgr2 = Arc::clone(&mgr);
    let taker2 = tokio::spawn(async move {
        mgr2.take([("kind".into(), "job".into())].into(), true, 5000, None)
            .await
            .unwrap()
    });

    // Give taker 2 time to subscribe to the broadcast
    tokio::time::sleep(Duration::from_millis(50)).await;
    println!("[demo] Taker 2: blocking, waiting for a job...");

    // ── Taker 1: aborts → tuple restored → broadcast → taker 2 wakes ───────
    mgr.abort_space_txn(&taker1_txn).await.unwrap();
    println!("[demo] Taker 1: aborted ✓");

    // ── Taker 2: should wake up and get the tuple ───────────────────────────
    let result = tokio::time::timeout(Duration::from_secs(2), taker2)
        .await
        .expect("taker 2 should not timeout")
        .unwrap();

    assert!(result.is_some(), "taker 2 should get the restored tuple");
    assert_eq!(result.unwrap().attrs["id"], "101");
    println!("[demo] Taker 2: woke up and got job 101 ✓");

    // ── Tuple is now gone (taker 2 took it) ─────────────────────────────────
    let r = mgr
        .read([("kind".into(), "job".into())].into(), false, 0, None)
        .await
        .unwrap();
    assert!(r.is_none(), "tuple consumed by taker 2");
    println!("[demo] Final: job consumed by taker 2 ✓");

    println!("[demo] Abort-unblocks-waiting-taker passed ✓");
}

// ── Test 26: contents includes uncommitted under txn ────────────────────────

#[tokio::test]
async fn txn_contents_includes_uncommitted() {
    let (mgr, _) = make_manager();
    let txn_id = "txn-contents".to_string();

    // Write 2 committed tuples
    mgr.write(
        [("kind".into(), "item".into()), ("id".into(), "1".into())].into(),
        vec![], 60, "w".into(), None, None,
    ).await.unwrap();
    mgr.write(
        [("kind".into(), "item".into()), ("id".into(), "2".into())].into(),
        vec![], 60, "w".into(), None, None,
    ).await.unwrap();

    // Write 1 uncommitted under txn
    mgr.write(
        [("kind".into(), "item".into()), ("id".into(), "3".into())].into(),
        vec![], 60, "w".into(), None, Some(txn_id.clone()),
    ).await.unwrap();

    // Contents without txn → 2
    let without = mgr.contents([("kind".into(), "item".into())].into(), None).await.unwrap();
    assert_eq!(without.len(), 2, "contents without txn should see 2");

    // Contents with txn → 3
    let with = mgr.contents([("kind".into(), "item".into())].into(), Some(txn_id)).await.unwrap();
    assert_eq!(with.len(), 3, "contents with txn should see 3");

    println!("\n[demo] Txn contents: includes uncommitted ✓");
}
