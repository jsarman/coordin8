package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/coordin8/sdk-go/coordin8"
	"github.com/spf13/cobra"
)

var (
	registryAddr string
	token        string
)

func main() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

var rootCmd = &cobra.Command{
	Use:   "coordin8",
	Short: "Coordin8 CLI — inspect and interact with the Djinn",
}

func init() {
	rootCmd.PersistentFlags().StringVar(&registryAddr, "registry", "localhost:9002", "Registry address (host:port) — every other Djinn service is looked up through it")
	rootCmd.PersistentFlags().StringVar(&token, "token", "", "Bearer token for an auth-enabled Djinn (default: $COORDIN8_TOKEN; see 'coordin8 auth mint-token')")
	rootCmd.AddCommand(leaseCmd)
	rootCmd.AddCommand(registryCmd)
	rootCmd.AddCommand(spaceCmd)
}

// effectiveToken returns --token if set, else $COORDIN8_TOKEN. Harmless
// against a Djinn with no COORDIN8_JWT_SECRET configured — it just adds
// metadata a no-op interceptor never inspects.
func effectiveToken() string {
	if token != "" {
		return token
	}
	return os.Getenv("COORDIN8_TOKEN")
}

func connect() (*coordin8.Client, error) {
	var opts []coordin8.ConnectOption
	if t := effectiveToken(); t != "" {
		opts = append(opts, coordin8.WithToken(t))
	}
	return coordin8.Connect(registryAddr, opts...)
}

// ── lease ─────────────────────────────────────────────────────────────────────
//
// There is no single "the" LeaseMgr anymore — Registry, Space, and EventMgr
// each grant their own leases (see .claude/plans/distributed-leasing/PRD.md).
// These commands dial the grantor directly via --grantor (its address is
// printed by `registry register` / a Write / a Subscribe response as
// grantor_host:grantor_port), rather than going through the shared Client.

var leaseCmd = &cobra.Command{
	Use:   "lease",
	Short: "Manage leases (dials the grantor directly — see --grantor)",
}

var (
	leaseTTL        int64
	leaseResourceID string
	leaseID         string
	leaseGrantor    string
)

// dialLeaseGrantor dials whichever service granted the lease being
// renewed/cancelled/watched. Defaults to --registry (the most common case —
// every self-registration's lease is Registry-granted); pass --grantor
// explicitly for a Space- or EventMgr-granted lease.
func dialLeaseGrantor() (*coordin8.LeaseClient, error) {
	addr := leaseGrantor
	if addr == "" {
		addr = registryAddr
	}
	var opts []coordin8.LeaseDialOption
	if t := effectiveToken(); t != "" {
		opts = append(opts, coordin8.WithLeaseToken(t))
	}
	return coordin8.DialLease(addr, opts...)
}

var leaseGrantCmd = &cobra.Command{
	Use:   "grant",
	Short: "Grant a new lease directly against --grantor",
	RunE: func(cmd *cobra.Command, args []string) error {
		lc, err := dialLeaseGrantor()
		if err != nil {
			return err
		}
		defer lc.Close()

		record, err := lc.Grant(context.Background(), leaseResourceID, time.Duration(leaseTTL)*time.Second)
		if err != nil {
			return err
		}
		fmt.Printf("lease_id:    %s\n", record.LeaseID)
		fmt.Printf("resource_id: %s\n", record.ResourceID)
		fmt.Printf("expires_at:  %s\n", record.ExpiresAt.Format(time.RFC3339))
		return nil
	},
}

var leaseRenewCmd = &cobra.Command{
	Use:   "renew",
	Short: "Renew an existing lease against --grantor",
	RunE: func(cmd *cobra.Command, args []string) error {
		lc, err := dialLeaseGrantor()
		if err != nil {
			return err
		}
		defer lc.Close()

		record, err := lc.Renew(context.Background(), leaseID, time.Duration(leaseTTL)*time.Second)
		if err != nil {
			return err
		}
		fmt.Printf("lease_id:    %s\n", record.LeaseID)
		fmt.Printf("resource_id: %s\n", record.ResourceID)
		fmt.Printf("expires_at:  %s  (extended)\n", record.ExpiresAt.Format(time.RFC3339))
		return nil
	},
}

var leaseCancelCmd = &cobra.Command{
	Use:   "cancel",
	Short: "Cancel a lease against --grantor",
	RunE: func(cmd *cobra.Command, args []string) error {
		lc, err := dialLeaseGrantor()
		if err != nil {
			return err
		}
		defer lc.Close()

		if err := lc.Cancel(context.Background(), leaseID); err != nil {
			return err
		}
		fmt.Printf("cancelled: %s\n", leaseID)
		return nil
	},
}

var leaseWatchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Watch for lease expiry events on --grantor (Ctrl+C to stop)",
	RunE: func(cmd *cobra.Command, args []string) error {
		lc, err := dialLeaseGrantor()
		if err != nil {
			return err
		}
		defer lc.Close()

		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		ch, err := lc.Watch(ctx, leaseResourceID)
		if err != nil {
			return err
		}

		filter := leaseResourceID
		if filter == "" {
			filter = "*"
		}
		fmt.Printf("watching expiry events for resource: %s\n\n", filter)

		for evt := range ch {
			reason := "expired"
			if evt.Cancelled {
				reason = "cancelled"
			}
			fmt.Printf("[%s] %s  lease_id=%s  resource_id=%s\n",
				evt.ExpiredAt.Format(time.RFC3339), reason, evt.LeaseID, evt.ResourceID)
		}
		return nil
	},
}

func init() {
	leaseCmd.PersistentFlags().StringVar(&leaseGrantor, "grantor", "", "Address (host:port) of the service that granted this lease — Registry, Space, or EventMgr (default: --registry, the common case)")

	leaseGrantCmd.Flags().StringVar(&leaseResourceID, "resource", "", "Resource ID to lease (required)")
	leaseGrantCmd.Flags().Int64Var(&leaseTTL, "ttl", 30, "TTL in seconds")
	leaseGrantCmd.MarkFlagRequired("resource")

	leaseRenewCmd.Flags().StringVar(&leaseID, "id", "", "Lease ID to renew (required)")
	leaseRenewCmd.Flags().Int64Var(&leaseTTL, "ttl", 30, "New TTL in seconds")
	leaseRenewCmd.MarkFlagRequired("id")

	leaseCancelCmd.Flags().StringVar(&leaseID, "id", "", "Lease ID to cancel (required)")
	leaseCancelCmd.MarkFlagRequired("id")

	leaseWatchCmd.Flags().StringVar(&leaseResourceID, "resource", "", "Resource ID to watch (empty = all)")

	leaseCmd.AddCommand(leaseGrantCmd, leaseRenewCmd, leaseCancelCmd, leaseWatchCmd)
}

// ── registry ──────────────────────────────────────────────────────────────────

var registryCmd = &cobra.Command{
	Use:   "registry",
	Short: "Inspect the registry",
}

var registryListCmd = &cobra.Command{
	Use:   "list",
	Short: "List all registered capabilities",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		caps, err := c.Registry().LookupAll(context.Background(), coordin8.Template{})
		if err != nil {
			return err
		}

		if len(caps) == 0 {
			fmt.Println("no registered capabilities")
			return nil
		}

		for _, cap := range caps {
			fmt.Printf("%-40s  interface=%-20s", cap.CapabilityID, cap.Interface)
			for k, v := range cap.Attrs {
				fmt.Printf("  %s=%s", k, v)
			}
			if cap.Transport != nil {
				fmt.Printf("  transport=%s", cap.Transport.Type)
			}
			fmt.Println()
		}
		return nil
	},
}

var (
	registryInterface     string
	registryAttrs         map[string]string
	registryTransportType string
	registryTransportHost string
	registryTTL           int64
)

var registryRegisterCmd = &cobra.Command{
	Use:   "register",
	Short: "Register a service capability",
	Example: `  coordin8 registry register --interface WeatherStation \
    --attr region=tampa-east --attr metrics=wind,humidity,temp \
    --transport grpc --host sensor-7.internal --ttl 60`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		reg := coordin8.Registration{
			Interface: registryInterface,
			Attrs:     registryAttrs,
			TTL:       time.Duration(registryTTL) * time.Second,
		}
		if registryTransportType != "" {
			reg.Transport = &coordin8.TransportDescriptor{
				Type:   registryTransportType,
				Config: map[string]string{"host": registryTransportHost},
			}
		}

		result, err := c.Registry().Register(context.Background(), reg)
		if err != nil {
			return err
		}
		fmt.Printf("registered  interface=%-20s  capability_id=%s\n", registryInterface, result.CapabilityID)
		fmt.Printf("lease_id:   %s\n", result.LeaseID)
		fmt.Println("(renew this lease to stay registered; let it expire to disappear)")
		return nil
	},
}

var registryLookupTemplate map[string]string

var registryLookupCmd = &cobra.Command{
	Use:   "lookup",
	Short: "Look up a capability by template",
	Example: `  coordin8 registry lookup --match interface=WeatherStation --match region=tampa-east
  coordin8 registry lookup --match interface=WeatherStation --match metrics=contains:humidity`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		cap, err := c.Registry().Lookup(context.Background(), coordin8.Template(registryLookupTemplate))
		if err != nil {
			return err
		}

		fmt.Printf("capability_id: %s\n", cap.CapabilityID)
		fmt.Printf("interface:     %s\n", cap.Interface)
		for k, v := range cap.Attrs {
			fmt.Printf("  %-16s %s\n", k+":", v)
		}
		if cap.Transport != nil {
			fmt.Printf("transport:     %s\n", cap.Transport.Type)
			for k, v := range cap.Transport.Config {
				fmt.Printf("  %-16s %s\n", k+":", v)
			}
		}
		return nil
	},
}

var registryWatchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Watch for registry changes (Ctrl+C to stop)",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		ch, err := c.Registry().Watch(ctx, coordin8.Template(registryLookupTemplate))
		if err != nil {
			return err
		}

		fmt.Println("watching registry events...")
		for evt := range ch {
			fmt.Printf("[%-12s] %s  interface=%s\n",
				evt.Type, evt.Capability.CapabilityID, evt.Capability.Interface)
		}
		return nil
	},
}

func init() {
	registryLookupTemplate = make(map[string]string)

	registryRegisterCmd.Flags().StringVar(&registryInterface, "interface", "", "Service interface name (required)")
	registryRegisterCmd.Flags().StringToStringVar(&registryAttrs, "attr", nil, "Service attributes (key=value, repeatable)")
	registryRegisterCmd.Flags().StringVar(&registryTransportType, "transport", "", "Transport type (grpc, kafka, tcp, ...)")
	registryRegisterCmd.Flags().StringVar(&registryTransportHost, "host", "", "Transport host")
	registryRegisterCmd.Flags().Int64Var(&registryTTL, "ttl", 60, "Registration TTL in seconds")
	registryRegisterCmd.MarkFlagRequired("interface")

	registryLookupCmd.Flags().StringToStringVar(&registryLookupTemplate, "match", nil, "Template fields (key=value)")
	registryWatchCmd.Flags().StringToStringVar(&registryLookupTemplate, "match", nil, "Template fields (key=value)")

	registryCmd.AddCommand(registryRegisterCmd, registryListCmd, registryLookupCmd, registryWatchCmd)
}
