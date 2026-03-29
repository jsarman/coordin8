package coordin8

import (
	"context"
	"fmt"
	"sort"
	"strings"
	"sync"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// cachedConn holds an open proxy + the gRPC connection built on top of it.
type cachedConn struct {
	proxyID string
	conn    *grpc.ClientConn
	stale   bool
}

// ServiceDiscovery caches Djinn proxies by template and watches the registry
// for service changes. Repeated calls with the same template return a cached
// connection without any additional Djinn round-trips.
//
// Usage:
//
//	discovery := coordin8.NewServiceDiscovery(djinn)
//	defer discovery.Close()
//
//	conn, err := discovery.Get(ctx, coordin8.Template{"interface": "Greeter"})
//	stub := gen.NewGreeterServiceClient(conn)
type ServiceDiscovery struct {
	client  *Client
	mu      sync.Mutex
	cache   map[string]*cachedConn
	cancels map[string]context.CancelFunc
}

// NewServiceDiscovery creates a ServiceDiscovery manager backed by djinn.
func NewServiceDiscovery(client *Client) *ServiceDiscovery {
	return &ServiceDiscovery{
		client:  client,
		cache:   make(map[string]*cachedConn),
		cancels: make(map[string]context.CancelFunc),
	}
}

// Get returns a gRPC ClientConn routed through a Djinn proxy for the first
// capability matching template. Repeated calls with the same template return
// the cached connection. The ServiceDiscovery owns the connection lifetime —
// do not close it directly; call sd.Close() to tear everything down.
func (sd *ServiceDiscovery) Get(ctx context.Context, tmpl Template) (*grpc.ClientConn, error) {
	key := templateKey(tmpl)

	sd.mu.Lock()
	if entry, ok := sd.cache[key]; ok && !entry.stale {
		conn := entry.conn
		sd.mu.Unlock()
		return conn, nil
	}
	sd.mu.Unlock()

	return sd.refresh(ctx, key, tmpl)
}

func (sd *ServiceDiscovery) refresh(ctx context.Context, key string, tmpl Template) (*grpc.ClientConn, error) {
	handle, err := sd.client.Proxy().Open(ctx, tmpl)
	if err != nil {
		return nil, fmt.Errorf("service discovery: %w", err)
	}

	addr := fmt.Sprintf("localhost:%d", handle.LocalPort)
	conn, err := grpc.NewClient(addr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		_ = sd.client.Proxy().Release(ctx, handle.ProxyID)
		return nil, fmt.Errorf("service discovery: %w", err)
	}

	sd.mu.Lock()
	// Close any previously stale entry
	if old, exists := sd.cache[key]; exists {
		old.conn.Close()
		_ = sd.client.Proxy().Release(context.Background(), old.proxyID)
	}
	sd.cache[key] = &cachedConn{proxyID: handle.ProxyID, conn: conn}

	if _, watching := sd.cancels[key]; !watching {
		watchCtx, cancel := context.WithCancel(context.Background())
		sd.cancels[key] = cancel
		go sd.watch(watchCtx, key, tmpl)
	}
	sd.mu.Unlock()

	return conn, nil
}

func (sd *ServiceDiscovery) watch(ctx context.Context, key string, tmpl Template) {
	ch, err := sd.client.Registry().Watch(ctx, tmpl)
	if err != nil {
		return
	}
	for {
		select {
		case evt, ok := <-ch:
			if !ok {
				return
			}
			switch evt.Type {
			case "expired":
				sd.mu.Lock()
				if entry, exists := sd.cache[key]; exists {
					entry.stale = true
				}
				sd.mu.Unlock()
			case "registered":
				sd.mu.Lock()
				stale := false
				if entry, exists := sd.cache[key]; exists {
					stale = entry.stale
				}
				sd.mu.Unlock()
				if stale {
					_, _ = sd.refresh(ctx, key, tmpl)
				}
			}
		case <-ctx.Done():
			return
		}
	}
}

// Close releases all cached proxies and watch streams.
func (sd *ServiceDiscovery) Close() {
	sd.mu.Lock()
	defer sd.mu.Unlock()

	for _, cancel := range sd.cancels {
		cancel()
	}
	for _, entry := range sd.cache {
		entry.conn.Close()
		_ = sd.client.Proxy().Release(context.Background(), entry.proxyID)
	}
	sd.cache = make(map[string]*cachedConn)
	sd.cancels = make(map[string]context.CancelFunc)
}

func templateKey(tmpl Template) string {
	keys := make([]string, 0, len(tmpl))
	for k := range tmpl {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		parts = append(parts, k+"="+tmpl[k])
	}
	return strings.Join(parts, ",")
}
