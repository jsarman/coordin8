//! Participant-endpoint allowlist: service-level behavior (status codes,
//! nothing stored, nothing dialed) and the bundled-mode implicit Space allow.
use std::sync::Arc;

use tonic::{Code, Request};

use coordin8_core::{Leasing, TransactionState, TxnStore};
use coordin8_lease::LeaseManager;
use coordin8_proto::coordin8::{
    transaction_service_server::TransactionService, BeginRequest, CommitRequest, EnlistRequest,
};
use coordin8_provider_local::{InMemoryLeaseStore, InMemoryTxnStore};
use coordin8_txn::{ParticipantAllowlist, TxnManager, TxnServiceImpl};

fn make(allow: ParticipantAllowlist) -> (TxnServiceImpl, Arc<InMemoryTxnStore>) {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let (tx, _) = tokio::sync::broadcast::channel(256);
    let lm: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        lease_store,
        coordin8_core::LeaseConfig::default(),
        tx,
    ));
    let store = Arc::new(InMemoryTxnStore::new());
    let mgr = Arc::new(TxnManager::new(store.clone(), lm).with_participant_allowlist(allow));
    (TxnServiceImpl::new(mgr, "127.0.0.1", 9004), store)
}

async fn begin(svc: &TxnServiceImpl) -> String {
    svc.begin(Request::new(BeginRequest { ttl_seconds: 60 }))
        .await
        .unwrap()
        .into_inner()
        .txn_id
}

async fn enlist(svc: &TxnServiceImpl, txn_id: &str, ep: &str) -> Result<(), tonic::Status> {
    svc.enlist(Request::new(EnlistRequest {
        txn_id: txn_id.into(),
        participant_endpoint: ep.into(),
        crash_count: 0,
    }))
    .await
    .map(|_| ())
}

#[tokio::test]
async fn disallowed_enlist_is_permission_denied_and_not_stored() {
    let (svc, store) = make(ParticipantAllowlist::parse("space:9006").unwrap());
    let txn_id = begin(&svc).await;

    // 127.0.0.1:9 would fail to connect (and hang/err) if it were dialed on commit.
    let err = enlist(&svc, &txn_id, "127.0.0.1:9").await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);

    let rec = store.get(&txn_id).await.unwrap().unwrap();
    assert!(rec.participants.is_empty(), "rejected endpoint was stored");

    // Zero participants: commit succeeds without any outbound call.
    svc.commit(Request::new(CommitRequest {
        txn_id: txn_id.clone(),
        wait_millis: 0,
    }))
    .await
    .unwrap();
    let rec = store.get(&txn_id).await.unwrap().unwrap();
    assert_eq!(rec.state, TransactionState::Committed);
}

#[tokio::test]
async fn allowed_enlist_is_stored() {
    let (svc, store) = make(ParticipantAllowlist::parse("127.0.0.0/8, space:9006").unwrap());
    let txn_id = begin(&svc).await;
    enlist(&svc, &txn_id, "127.0.0.1:5555").await.unwrap();
    enlist(&svc, &txn_id, "space:9006").await.unwrap();
    assert_eq!(
        enlist(&svc, &txn_id, "space:9007")
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
    let rec = store.get(&txn_id).await.unwrap().unwrap();
    assert_eq!(rec.participants.len(), 2);
}

#[tokio::test]
async fn malformed_endpoint_is_invalid_argument() {
    for allow in ["", "space:9006"] {
        let (svc, store) = make(ParticipantAllowlist::parse(allow).unwrap());
        let txn_id = begin(&svc).await;
        for bad in [
            "",
            "no-port",
            "host:",
            "host:notaport",
            "http://x:80",
            "::1:80",
        ] {
            let err = enlist(&svc, &txn_id, bad).await.unwrap_err();
            assert_eq!(
                err.code(),
                Code::InvalidArgument,
                "{bad:?} (allow={allow:?})"
            );
        }
        assert!(store
            .get(&txn_id)
            .await
            .unwrap()
            .unwrap()
            .participants
            .is_empty());
    }
}

#[tokio::test]
async fn unset_allowlist_allows_any_well_formed_endpoint() {
    let (svc, _) = make(ParticipantAllowlist::allow_all());
    let txn_id = begin(&svc).await;
    enlist(&svc, &txn_id, "anything.example:1234")
        .await
        .unwrap();
}

/// Bundled mode: the Djinn's own Space participant endpoint is implicitly
/// allowed even when the operator's allowlist doesn't mention it.
#[tokio::test]
async fn bundled_space_endpoint_is_implicitly_allowed() {
    let allow = ParticipantAllowlist::parse("*.corp.internal")
        .unwrap()
        .with_implicit_endpoint("127.0.0.1:9006")
        .unwrap();
    let (svc, _) = make(allow);
    let txn_id = begin(&svc).await;
    enlist(&svc, &txn_id, "127.0.0.1:9006").await.unwrap();
    enlist(&svc, &txn_id, "a.corp.internal:7000").await.unwrap();
    // Only that exact host:port, not the whole host.
    assert_eq!(
        enlist(&svc, &txn_id, "127.0.0.1:9004")
            .await
            .unwrap_err()
            .code(),
        Code::PermissionDenied
    );
}
