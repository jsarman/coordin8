package coordin8

import (
	"context"

	"google.golang.org/grpc"
)

// bearerTokenCredentials attaches a static bearer token to every RPC as
// `authorization: Bearer <token>` metadata — gRPC's call-credentials layer,
// distinct from (and composable with) transport/channel credentials. See
// `coordin8-auth` on the Rust side and `.claude/plans/grpc-security/PRD.md`
// for the full design this implements the client half of.
//
// RequireTransportSecurity is false: Coordin8 has no TLS channel credentials
// yet (transport encryption is explicitly deferred, see the PRD's
// Non-Goals), and requiring it here would make WithToken unusable against
// every deployment this project currently supports.
type bearerTokenCredentials struct {
	token string
}

func (c bearerTokenCredentials) GetRequestMetadata(ctx context.Context, uri ...string) (map[string]string, error) {
	return map[string]string{"authorization": "Bearer " + c.token}, nil
}

func (c bearerTokenCredentials) RequireTransportSecurity() bool {
	return false
}

// perRPCTokenOption builds the grpc.DialOption that attaches token to every
// call on a connection. Shared by Connect and DialLease so both dial paths
// authenticate identically.
func perRPCTokenOption(token string) grpc.DialOption {
	return grpc.WithPerRPCCredentials(bearerTokenCredentials{token: token})
}

// PerRPCToken returns a grpc.DialOption attaching token to every call —
// exported for callers dialing a Djinn service directly with grpc.NewClient
// instead of going through Connect/DialLease (e.g. an example talking
// straight to EventMgr or TransactionMgr).
func PerRPCToken(token string) grpc.DialOption {
	return perRPCTokenOption(token)
}
