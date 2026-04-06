/// EventMgr demo — runs in-process, no gRPC needed.
///
/// Demonstrates the full cycle:
///   Subscribe (durable) → Emit × 3 → Receive (drain backlog) → Emit while live → Receive live
use std::sync::Arc;

use coordin8_core::DeliveryMode;
use coordin8_event::EventManager;
use coordin8_lease::LeaseManager;
use coordin8_provider_local::{InMemoryEventStore, InMemoryLeaseStore};
use tokio::sync::broadcast;

fn make_manager() -> Arc<EventManager> {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let lease_manager = Arc::new(LeaseManager::new(
        lease_store,
        coordin8_core::LeaseConfig::default(),
    ));
    let event_store = Arc::new(InMemoryEventStore::new());
    let (event_tx, _) = broadcast::channel(256);
    Arc::new(EventManager::new(event_store, lease_manager, event_tx))
}

// ── Test 1: durable mailbox — emit before receive, drain backlog ─────────────

#[tokio::test]
async fn durable_mailbox_drains_backlog() {
    let mgr = make_manager();

    let (reg_id, _, seq_at_sub) = mgr
        .subscribe(
            "market.signals".into(),
            Default::default(), // empty template = match all
            DeliveryMode::Durable,
            60,
            b"my-handback".to_vec(),
        )
        .await
        .unwrap();

    println!("\n[demo] Subscribed: reg_id={reg_id}  seq_at_subscribe={seq_at_sub}");

    // Emit 3 events before anyone is receiving
    for i in 1..=3u64 {
        let event = mgr
            .emit(
                "market.signals".into(),
                "price.update".into(),
                [("ticker".into(), "AAPL".into())].into(),
                format!("{{\"price\": {}}}", 150 + i).into_bytes(),
            )
            .await
            .unwrap();
        println!(
            "[demo] Emitted:    seq={} event_id={}",
            event.seq_num,
            &event.event_id[..8]
        );
    }

    // Now drain the mailbox
    let backlog = mgr.drain_mailbox(&reg_id).await.unwrap();
    println!("[demo] Drained {} events from mailbox:", backlog.len());
    for e in &backlog {
        println!(
            "       seq={}  type={}  payload={}",
            e.seq_num,
            e.event_type,
            String::from_utf8_lossy(&e.payload)
        );
    }

    assert_eq!(backlog.len(), 3);
    assert_eq!(backlog[0].seq_num, 1);
    assert_eq!(backlog[2].seq_num, 3);
    assert_eq!(backlog[0].source, "market.signals");

    // Second drain should be empty
    let empty = mgr.drain_mailbox(&reg_id).await.unwrap();
    assert!(empty.is_empty());
    println!("[demo] Second drain: empty ✓");
}

// ── Test 2: best-effort — no mailbox, live only ──────────────────────────────

#[tokio::test]
async fn best_effort_live_only() {
    let mgr = make_manager();

    let (reg_id, _, _) = mgr
        .subscribe(
            "sensors".into(),
            Default::default(),
            DeliveryMode::BestEffort,
            60,
            vec![],
        )
        .await
        .unwrap();

    // Emit before anyone drains — mailbox should stay empty (best-effort)
    mgr.emit(
        "sensors".into(),
        "temp".into(),
        [("unit".into(), "C".into())].into(),
        b"22.4".to_vec(),
    )
    .await
    .unwrap();

    let queue = mgr.drain_mailbox(&reg_id).await.unwrap();
    assert!(queue.is_empty());
    println!("\n[demo] BestEffort: mailbox empty after emit (as expected) ✓");
}

// ── Test 3: template filtering — only matching events enqueued ───────────────

#[tokio::test]
async fn template_filtering_enqueues_only_matches() {
    let mgr = make_manager();

    // Subscribe only to TSLA events
    let (reg_id, _, _) = mgr
        .subscribe(
            "market.signals".into(),
            [("ticker".into(), "TSLA".into())].into(),
            DeliveryMode::Durable,
            60,
            vec![],
        )
        .await
        .unwrap();

    // Emit one AAPL (no match) and one TSLA (match)
    mgr.emit(
        "market.signals".into(),
        "price.update".into(),
        [("ticker".into(), "AAPL".into())].into(),
        b"150".to_vec(),
    )
    .await
    .unwrap();

    mgr.emit(
        "market.signals".into(),
        "price.update".into(),
        [("ticker".into(), "TSLA".into())].into(),
        b"242".to_vec(),
    )
    .await
    .unwrap();

    let backlog = mgr.drain_mailbox(&reg_id).await.unwrap();
    println!(
        "\n[demo] Template filter: got {} event(s) (expected 1)",
        backlog.len()
    );
    for e in &backlog {
        println!(
            "       ticker={:?}  payload={}",
            e.attrs.get("ticker"),
            String::from_utf8_lossy(&e.payload)
        );
    }

    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0].attrs["ticker"], "TSLA");
}

// ── Test 4: sequence numbers monotonically increase per (source, type) ───────

#[tokio::test]
async fn sequence_numbers_are_monotonic_per_source_type() {
    let mgr = make_manager();

    let mut seqs = vec![];
    for _ in 0..5 {
        let e = mgr
            .emit("feed".into(), "tick".into(), Default::default(), vec![])
            .await
            .unwrap();
        seqs.push(e.seq_num);
    }

    // Different type resets to its own counter
    let other = mgr
        .emit(
            "feed".into(),
            "heartbeat".into(),
            Default::default(),
            vec![],
        )
        .await
        .unwrap();

    println!("\n[demo] Seq numbers (feed::tick): {:?}", seqs);
    println!("[demo] Seq number  (feed::heartbeat): {}", other.seq_num);

    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    assert_eq!(other.seq_num, 1); // independent counter
}

// ── Test 5: cancel subscription ──────────────────────────────────────────────

#[tokio::test]
async fn cancel_removes_subscription() {
    let mgr = make_manager();

    let (reg_id, _, _) = mgr
        .subscribe(
            "events".into(),
            Default::default(),
            DeliveryMode::Durable,
            60,
            vec![],
        )
        .await
        .unwrap();

    mgr.cancel_subscription(&reg_id).await.unwrap();

    let sub = mgr.get_subscription(&reg_id).await.unwrap();
    assert!(sub.is_none());
    println!("\n[demo] Cancel: subscription gone ✓");
}

// ── Test 6: subscription lease expiry cascade ────────────────────────────

#[tokio::test]
async fn subscription_lease_expiry_cascade() {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let lease_manager = Arc::new(LeaseManager::new(
        lease_store,
        coordin8_core::LeaseConfig::default(),
    ));
    let event_store = Arc::new(InMemoryEventStore::new());
    let (event_tx, _) = broadcast::channel(256);
    let mgr = Arc::new(EventManager::new(
        event_store,
        Arc::clone(&lease_manager),
        event_tx,
    ));

    // Subscribe with a 1-second TTL
    let (reg_id, lease, _) = mgr
        .subscribe(
            "test.source".into(),
            Default::default(),
            DeliveryMode::Durable,
            1,
            vec![],
        )
        .await
        .unwrap();

    // Subscription exists
    assert!(mgr.get_subscription(&reg_id).await.unwrap().is_some());
    println!(
        "\n[demo] Subscription created: reg_id={reg_id}  lease_id={}",
        lease.lease_id
    );

    // Simulate what the reaper + cascade does: unsubscribe by lease
    let removed = mgr.unsubscribe_by_lease(&lease.lease_id).await.unwrap();
    assert_eq!(removed, Some(reg_id.clone()));

    // Subscription should be gone
    assert!(mgr.get_subscription(&reg_id).await.unwrap().is_none());

    // Mailbox should also be gone (drain returns error)
    let drain_result = mgr.drain_mailbox(&reg_id).await;
    assert!(drain_result.is_err());
    println!("[demo] Lease expiry cascade: subscription + mailbox cleaned up ✓");
}
