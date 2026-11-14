package coordin8

import (
	"context"
	"io"
)

// Stream is a server stream delivered over a channel: range over C, and
// once it closes, Err says why. Err is nil for a clean end or when the
// caller's context was cancelled; otherwise it is the gRPC status the server
// ended with, so the cases a bare closed channel can't tell apart are
// distinguishable:
//
//   - codes.DataLoss: the subscriber fell behind and events were dropped;
//     resubscribe (and re-snapshot).
//   - codes.Unavailable: the server is shutting down; reconnect.
//   - codes.NotFound: the subscription or watch is gone (its lease lapsed).
type Stream[T any] struct {
	C    <-chan T
	done chan struct{}
	err  error
}

// Err reports why C closed. Nil until then.
func (s *Stream[T]) Err() error {
	select {
	case <-s.done:
		return s.err
	default:
		return nil
	}
}

// Done is closed when the stream has ended (just before C closes).
func (s *Stream[T]) Done() <-chan struct{} { return s.done }

// pump runs recv until the stream ends, sending each converted message on
// the returned Stream. convert may drop a message (ok=false). delivered, if
// set, runs after a message was handed to the caller. buffer is C's capacity
// (0: a message counts as delivered only once the caller has taken it).
func pump[M, T any](
	ctx context.Context,
	buffer int,
	recv func() (M, error),
	convert func(M) (T, bool),
	delivered func(T),
) *Stream[T] {
	ch := make(chan T, buffer)
	s := &Stream[T]{C: ch, done: make(chan struct{})}
	go func() {
		defer close(ch)
		defer close(s.done)
		for {
			msg, err := recv()
			if err != nil {
				if err != io.EOF && ctx.Err() == nil {
					s.err = err
				}
				return
			}
			v, ok := convert(msg)
			if !ok {
				continue
			}
			select {
			case ch <- v:
				if delivered != nil {
					delivered(v)
				}
			case <-ctx.Done():
				return
			}
		}
	}()
	return s
}
