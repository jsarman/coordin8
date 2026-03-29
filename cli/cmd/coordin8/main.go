package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/coordin8/sdk-go/coordin8"
	"github.com/spf13/cobra"
)

var djinnHost string

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
	rootCmd.PersistentFlags().StringVar(&djinnHost, "host", "localhost", "Djinn host")
	rootCmd.AddCommand(leaseCmd)
	rootCmd.AddCommand(registryCmd)
}

func connect() (*coordin8.Client, error) {
	return coordin8.Connect(djinnHost)
}

// ── lease ─────────────────────────────────────────────────────────────────────

var leaseCmd = &cobra.Command{
	Use:   "lease",
	Short: "Manage leases",
}

var (
	leaseTTL        int64
	leaseResourceID string
	leaseID         string
)

var leaseGrantCmd = &cobra.Command{
	Use:   "grant",
	Short: "Grant a new lease",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		record, err := c.Leases().Grant(context.Background(), leaseResourceID, time.Duration(leaseTTL)*time.Second)
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
	Short: "Renew an existing lease",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		record, err := c.Leases().Renew(context.Background(), leaseID, time.Duration(leaseTTL)*time.Second)
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
	Short: "Cancel a lease",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		if err := c.Leases().Cancel(context.Background(), leaseID); err != nil {
			return err
		}
		fmt.Printf("cancelled: %s\n", leaseID)
		return nil
	},
}

var leaseWatchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Watch for lease expiry events (Ctrl+C to stop)",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()

		ch, err := c.Leases().Watch(ctx, leaseResourceID)
		if err != nil {
			return err
		}

		filter := leaseResourceID
		if filter == "" {
			filter = "*"
		}
		fmt.Printf("watching expiry events for resource: %s\n\n", filter)

		for evt := range ch {
			fmt.Printf("[%s] expired  lease_id=%s  resource_id=%s\n",
				evt.ExpiredAt.Format(time.RFC3339), evt.LeaseID, evt.ResourceID)
		}
		return nil
	},
}

func init() {
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

		leaseID, err := c.Registry().Register(context.Background(), reg)
		if err != nil {
			return err
		}
		fmt.Printf("registered  interface=%-20s  lease_id=%s\n", registryInterface, leaseID)
		fmt.Printf("lease_id:   %s\n", leaseID)
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

		fmt.Println("watching registry events...\n")
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
