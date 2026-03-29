// double-entry — Coordin8 TransactionMgr 2PC demo
//
// Simulates a double-entry bookkeeping transfer between two accounts:
//   - "ledger-debit"  and "ledger-credit" each run their own gRPC participant server
//   - Both enlist in a transaction managed by the Djinn
//
// Two scenarios are run back-to-back:
//
//	Scenario A — Happy path: both participants prepared → COMMITTED
//	Scenario B — Veto abort: ledger-debit vetoes → ABORTED, no state change
//
// Run against a live Djinn:
//
//	go run . [djinn-host:port]   (default: localhost:9004)
package main

import (
	"context"
	"fmt"
	"log"
	"net"
	"os"
	"sync/atomic"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/emptypb"
)

// ── Participant server ────────────────────────────────────────────────────────

type ledger struct {
	pb.UnimplementedParticipantServiceServer
	name      string
	veto      bool       // if true, Prepare returns VOTE_ABORTED
	committed atomic.Bool
	aborted   atomic.Bool
}

func (l *ledger) Prepare(_ context.Context, req *pb.ParticipantRequest) (*pb.PrepareResponse, error) {
	if l.veto {
		fmt.Printf("    [%s] Prepare txn=%s → VOTE_ABORTED (vetoing!)\n", l.name, req.TxnId)
		return &pb.PrepareResponse{Vote: pb.PrepareVote_VOTE_ABORTED}, nil
	}
	fmt.Printf("    [%s] Prepare txn=%s → VOTE_PREPARED\n", l.name, req.TxnId)
	return &pb.PrepareResponse{Vote: pb.PrepareVote_VOTE_PREPARED}, nil
}

func (l *ledger) Commit(_ context.Context, req *pb.ParticipantRequest) (*emptypb.Empty, error) {
	l.committed.Store(true)
	fmt.Printf("    [%s] Commit  txn=%s ✓\n", l.name, req.TxnId)
	return &emptypb.Empty{}, nil
}

func (l *ledger) Abort(_ context.Context, req *pb.ParticipantRequest) (*emptypb.Empty, error) {
	l.aborted.Store(true)
	fmt.Printf("    [%s] Abort   txn=%s ✓\n", l.name, req.TxnId)
	return &emptypb.Empty{}, nil
}

func (l *ledger) PrepareAndCommit(_ context.Context, req *pb.ParticipantRequest) (*pb.PrepareResponse, error) {
	if l.veto {
		fmt.Printf("    [%s] PrepareAndCommit txn=%s → VOTE_ABORTED\n", l.name, req.TxnId)
		return &pb.PrepareResponse{Vote: pb.PrepareVote_VOTE_ABORTED}, nil
	}
	l.committed.Store(true)
	fmt.Printf("    [%s] PrepareAndCommit txn=%s → VOTE_PREPARED (committed)\n", l.name, req.TxnId)
	return &pb.PrepareResponse{Vote: pb.PrepareVote_VOTE_PREPARED}, nil
}

// spawnLedger starts a participant gRPC server on an ephemeral port.
// Binds on 0.0.0.0 so Docker-hosted Djinn can call back; advertises
// advertiseHost (see resolveAdvertiseHost) so the enlisted endpoint is reachable.
func spawnLedger(name string, veto bool, advertiseHost string) (string, *ledger) {
	l := &ledger{name: name, veto: veto}
	lis, err := net.Listen("tcp", "0.0.0.0:0")
	if err != nil {
		log.Fatalf("listen %s: %v", name, err)
	}
	port := lis.Addr().(*net.TCPAddr).Port
	srv := grpc.NewServer()
	pb.RegisterParticipantServiceServer(srv, l)
	go func() { _ = srv.Serve(lis) }()
	time.Sleep(20 * time.Millisecond) // let it bind
	return fmt.Sprintf("%s:%d", advertiseHost, port), l
}

// resolveAdvertiseHost returns the host to advertise to the Djinn for 2PC callbacks.
// Defaults to "host.docker.internal" so a Docker-hosted Djinn can reach back to
// participant servers running on the host. Override with ADVERTISE_HOST if needed.
func resolveAdvertiseHost() string {
	if h := os.Getenv("ADVERTISE_HOST"); h != "" {
		return h
	}
	return "host.docker.internal"
}

// ── Main ──────────────────────────────────────────────────────────────────────

func main() {
	djinnAddr := "localhost:9004"
	if len(os.Args) > 1 {
		djinnAddr = os.Args[1]
	}

	conn, err := grpc.NewClient(djinnAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatalf("connect %s: %v", djinnAddr, err)
	}
	defer conn.Close()

	txn := pb.NewTransactionServiceClient(conn)
	ctx := context.Background()

	advertiseHost := resolveAdvertiseHost()

	banner("double-entry — Coordin8 TransactionMgr 2PC demo")
	fmt.Printf("  djinn:          %s\n", djinnAddr)
	fmt.Printf("  advertise host: %s\n", advertiseHost)

	// ═════════════════════════════════════════════════════════════════════════
	// Scenario A — Happy path: both participants prepared → COMMITTED
	// ═════════════════════════════════════════════════════════════════════════
	section("Scenario A — happy path (both prepared → COMMITTED)")

	debitAddrA, debitA := spawnLedger("ledger-debit ", false, advertiseHost)
	creditAddrA, creditA := spawnLedger("ledger-credit", false, advertiseHost)
	fmt.Printf("  ledger-debit  listening on %s\n", debitAddrA)
	fmt.Printf("  ledger-credit listening on %s\n\n", creditAddrA)

	txnA, err := txn.Begin(ctx, &pb.BeginRequest{TtlSeconds: 30})
	if err != nil {
		log.Fatalf("begin: %v", err)
	}
	fmt.Printf("[A1] Transaction started: %s\n\n", txnA.TxnId)

	fmt.Println("[A2] Enlisting participants...")
	must(txn.Enlist(ctx, &pb.EnlistRequest{TxnId: txnA.TxnId, ParticipantEndpoint: debitAddrA}))
	must(txn.Enlist(ctx, &pb.EnlistRequest{TxnId: txnA.TxnId, ParticipantEndpoint: creditAddrA}))
	fmt.Println("     both enlisted ✓\n")

	fmt.Println("[A3] Committing (2PC — prepare then commit)...")
	_, err = txn.Commit(ctx, &pb.CommitRequest{TxnId: txnA.TxnId})
	if err != nil {
		log.Printf("     commit error: %v", err)
	}
	fmt.Println()

	stateA, _ := txn.GetState(ctx, &pb.GetStateRequest{TxnId: txnA.TxnId})
	fmt.Printf("[A4] Final state: %s\n", stateA.State)
	fmt.Printf("     debit.committed=%v  credit.committed=%v\n\n",
		debitA.committed.Load(), creditA.committed.Load())

	if stateA.State == pb.TransactionState_COMMITTED && debitA.committed.Load() && creditA.committed.Load() {
		fmt.Println("     PASS — transfer committed on both ledgers ✓")
	} else {
		fmt.Println("     FAIL — unexpected state")
	}

	// ═════════════════════════════════════════════════════════════════════════
	// Scenario B — Veto: ledger-debit vetoes → ABORTED, no state change
	// ═════════════════════════════════════════════════════════════════════════
	section("Scenario B — veto abort (debit vetoes → ABORTED)")

	debitAddrB, debitB := spawnLedger("ledger-debit ", true /* veto */, advertiseHost)
	creditAddrB, creditB := spawnLedger("ledger-credit", false, advertiseHost)
	fmt.Printf("  ledger-debit  listening on %s (will VETO)\n", debitAddrB)
	fmt.Printf("  ledger-credit listening on %s\n\n", creditAddrB)

	txnB, err := txn.Begin(ctx, &pb.BeginRequest{TtlSeconds: 30})
	if err != nil {
		log.Fatalf("begin: %v", err)
	}
	fmt.Printf("[B1] Transaction started: %s\n\n", txnB.TxnId)

	fmt.Println("[B2] Enlisting participants...")
	must(txn.Enlist(ctx, &pb.EnlistRequest{TxnId: txnB.TxnId, ParticipantEndpoint: debitAddrB}))
	must(txn.Enlist(ctx, &pb.EnlistRequest{TxnId: txnB.TxnId, ParticipantEndpoint: creditAddrB}))
	fmt.Println("     both enlisted ✓\n")

	fmt.Println("[B3] Attempting commit (debit will veto during prepare)...")
	_, err = txn.Commit(ctx, &pb.CommitRequest{TxnId: txnB.TxnId})
	if err != nil {
		fmt.Printf("     commit returned error (expected): %v\n", err)
	}
	fmt.Println()

	stateB, _ := txn.GetState(ctx, &pb.GetStateRequest{TxnId: txnB.TxnId})
	fmt.Printf("[B4] Final state: %s\n", stateB.State)
	fmt.Printf("     debit.committed=%v  debit.aborted=%v\n",
		debitB.committed.Load(), debitB.aborted.Load())
	fmt.Printf("     credit.committed=%v credit.aborted=%v\n\n",
		creditB.committed.Load(), creditB.aborted.Load())

	if stateB.State == pb.TransactionState_ABORTED &&
		!debitB.committed.Load() && !creditB.committed.Load() {
		fmt.Println("     PASS — transaction aborted, no state change on either ledger ✓")
	} else {
		fmt.Println("     FAIL — unexpected state")
	}

	banner("fin")
}

func must(_ interface{}, err error) {
	if err != nil {
		log.Fatalf("rpc: %v", err)
	}
}

func banner(s string) {
	line := "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
	fmt.Printf("\n%s\n  %s\n%s\n", line, s, line)
}

func section(s string) {
	fmt.Printf("\n── %s ──\n\n", s)
}
