package coordin8

import (
	"context"
	"testing"
	"time"

	pb "github.com/coordin8/sdk-go/gen/coordin8"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/types/known/timestamppb"
)

type fakeSpace struct {
	pb.UnimplementedSpaceServiceServer
	write *pb.WriteRequest
	read  *pb.ReadRequest
	take  *pb.TakeRequest
	tuple *pb.Tuple // returned by Read/Take; nil = no match
}

func (f *fakeSpace) Write(_ context.Context, r *pb.WriteRequest) (*pb.WriteResponse, error) {
	f.write = r
	return &pb.WriteResponse{Tuple: &pb.Tuple{
		TupleId: "t1", Attrs: r.Attrs, Payload: r.Payload,
		Lease: &pb.Lease{LeaseId: "tl1"},
		Provenance: &pb.Provenance{WrittenBy: r.WrittenBy, InputTupleId: r.InputTupleId,
			WrittenAt: timestamppb.New(time.Unix(42, 0))},
	}}, nil
}

func (f *fakeSpace) Read(_ context.Context, r *pb.ReadRequest) (*pb.ReadResponse, error) {
	f.read = r
	return &pb.ReadResponse{Tuple: f.tuple}, nil
}

func (f *fakeSpace) Take(_ context.Context, r *pb.TakeRequest) (*pb.TakeResponse, error) {
	f.take = r
	return &pb.TakeResponse{Tuple: f.tuple}, nil
}

func newSpaceClient(t *testing.T, f *fakeSpace) *SpaceClient {
	t.Helper()
	addr, _ := startFake(t, func(s *grpc.Server) { pb.RegisterSpaceServiceServer(s, f) })
	c, err := Connect(addr, WithProxyAddr(addr), WithSpaceAddr(addr), WithEventAddr(addr))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	return c.Space()
}

func TestSpaceWriteMapsOptsAndTuple(t *testing.T) {
	f := &fakeSpace{}
	sc := newSpaceClient(t, f)
	got, err := sc.Write(context.Background(), WriteOpts{
		Attrs: map[string]string{"k": "v"}, Payload: []byte("p"), TTL: 90 * time.Second,
		WrittenBy: "me", InputTupleID: "prev", TxnID: "tx",
	})
	if err != nil {
		t.Fatal(err)
	}
	w := f.write
	if w.TtlSeconds != 90 || w.WrittenBy != "me" || w.InputTupleId != "prev" || w.TxnId != "tx" || w.Attrs["k"] != "v" {
		t.Fatalf("request = %v", w)
	}
	if got.TupleID != "t1" || got.LeaseID != "tl1" || got.WrittenBy != "me" || got.InputTupleID != "prev" ||
		!got.WrittenAt.Equal(time.Unix(42, 0)) || string(got.Payload) != "p" {
		t.Fatalf("tuple = %+v", got)
	}
}

func TestSpaceReadAndTakeMapTimeoutAndReturnNilOnNoMatch(t *testing.T) {
	f := &fakeSpace{}
	sc := newSpaceClient(t, f)
	ctx := context.Background()

	r, err := sc.Read(ctx, ReadOpts{Template: map[string]string{"a": "b"}, Wait: true, Timeout: 1500 * time.Millisecond, TxnID: "x"})
	if err != nil || r != nil {
		t.Fatalf("read = %v, %v; want nil, nil", r, err)
	}
	if f.read.TimeoutMs != 1500 || !f.read.Wait || f.read.TxnId != "x" || f.read.Template["a"] != "b" {
		t.Fatalf("read req = %v", f.read)
	}

	tk, err := sc.Take(ctx, TakeOpts{Template: map[string]string{"a": "b"}, Timeout: 250 * time.Millisecond})
	if err != nil || tk != nil {
		t.Fatalf("take = %v, %v; want nil, nil", tk, err)
	}
	if f.take.TimeoutMs != 250 || f.take.Wait {
		t.Fatalf("take req = %v", f.take)
	}

	f.tuple = &pb.Tuple{TupleId: "found"} // no lease, no provenance
	tk, err = sc.Take(ctx, TakeOpts{})
	if err != nil || tk == nil || tk.TupleID != "found" || tk.LeaseID != "" || !tk.WrittenAt.IsZero() {
		t.Fatalf("take = %+v, %v", tk, err)
	}
}

func TestProtoToTupleNil(t *testing.T) {
	if protoToTuple(nil) != nil {
		t.Fatal("nil in, nil out")
	}
}
