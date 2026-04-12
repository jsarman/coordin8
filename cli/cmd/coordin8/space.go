package main

import (
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"os/signal"
	"time"

	"github.com/coordin8/sdk-go/coordin8"
	"github.com/spf13/cobra"
)

var spaceCmd = &cobra.Command{
	Use:   "space",
	Short: "Interact with the Space (tuple store)",
}

// ── write ────────────────────────────────────────────────────────────────────

var (
	spaceWriteAttrs     map[string]string
	spaceWritePayload   string
	spaceWriteTTL       int64
	spaceWriteWrittenBy string
	spaceWriteInputID   string
	spaceWriteTxnID     string
)

var spaceWriteCmd = &cobra.Command{
	Use:   "write",
	Short: "Write a tuple into the Space",
	Example: `  coordin8 space write --attr ticker=AAPL --payload '{"price":150.25}' --ttl 60
  coordin8 space write --attr kind=task --attr priority=high --payload "do stuff" --written-by worker-1`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		t, err := c.Space().Write(context.Background(), coordin8.WriteOpts{
			Attrs:        spaceWriteAttrs,
			Payload:      []byte(spaceWritePayload),
			TTL:          time.Duration(spaceWriteTTL) * time.Second,
			WrittenBy:    spaceWriteWrittenBy,
			InputTupleID: spaceWriteInputID,
			TxnID:        spaceWriteTxnID,
		})
		if err != nil {
			return err
		}

		fmt.Printf("tuple_id:   %s\n", t.TupleID)
		fmt.Printf("lease_id:   %s\n", t.LeaseID)
		for k, v := range t.Attrs {
			fmt.Printf("  %-16s %s\n", k+":", v)
		}
		if len(t.Payload) > 0 {
			fmt.Printf("payload:    %s\n", string(t.Payload))
		}
		return nil
	},
}

// ── read ─────────────────────────────────────────────────────────────────────

var (
	spaceReadTemplate map[string]string
	spaceReadWait     bool
	spaceReadTimeout  int64
	spaceReadTxnID    string
)

var spaceReadCmd = &cobra.Command{
	Use:   "read",
	Short: "Read a tuple by template (non-destructive)",
	Example: `  coordin8 space read --match ticker=AAPL
  coordin8 space read --match kind=task --wait --timeout 10`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		t, err := c.Space().Read(context.Background(), coordin8.ReadOpts{
			Template: spaceReadTemplate,
			Wait:     spaceReadWait,
			Timeout:  time.Duration(spaceReadTimeout) * time.Second,
			TxnID:    spaceReadTxnID,
		})
		if err != nil {
			return err
		}
		if t == nil {
			fmt.Println("no match")
			return nil
		}
		printTuple(t)
		return nil
	},
}

// ── take ─────────────────────────────────────────────────────────────────────

var (
	spaceTakeTemplate map[string]string
	spaceTakeWait     bool
	spaceTakeTimeout  int64
	spaceTakeTxnID    string
)

var spaceTakeCmd = &cobra.Command{
	Use:   "take",
	Short: "Atomically take (claim + remove) a tuple by template",
	Example: `  coordin8 space take --match kind=task
  coordin8 space take --match kind=task --wait --timeout 30`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		t, err := c.Space().Take(context.Background(), coordin8.TakeOpts{
			Template: spaceTakeTemplate,
			Wait:     spaceTakeWait,
			Timeout:  time.Duration(spaceTakeTimeout) * time.Second,
			TxnID:    spaceTakeTxnID,
		})
		if err != nil {
			return err
		}
		if t == nil {
			fmt.Println("no match")
			return nil
		}
		printTuple(t)
		return nil
	},
}

// ── contents ─────────────────────────────────────────────────────────────────

var (
	spaceContentsTemplate map[string]string
	spaceContentsTxnID    string
)

var spaceContentsCmd = &cobra.Command{
	Use:   "contents",
	Short: "List all tuples matching a template",
	Example: `  coordin8 space contents --match ticker=AAPL
  coordin8 space contents`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		tuples, err := c.Space().Contents(context.Background(), spaceContentsTemplate, spaceContentsTxnID)
		if err != nil {
			return err
		}

		if len(tuples) == 0 {
			fmt.Println("no tuples")
			return nil
		}

		for _, t := range tuples {
			printTupleOneline(t)
		}
		fmt.Printf("\n%d tuple(s)\n", len(tuples))
		return nil
	},
}

// ── notify ───────────────────────────────────────────────────────────────────

var (
	spaceNotifyTemplate map[string]string
	spaceNotifyOn       string
	spaceNotifyTTL      int64
	spaceNotifyHandback string
)

var spaceNotifyCmd = &cobra.Command{
	Use:   "notify",
	Short: "Watch for tuple events (Ctrl+C to stop)",
	Example: `  coordin8 space notify --match ticker=AAPL --on appearance
  coordin8 space notify --on expiration --ttl 120`,
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		var eventType coordin8.SpaceEventType
		switch spaceNotifyOn {
		case "expiration":
			eventType = coordin8.Expiration
		default:
			eventType = coordin8.Appearance
		}

		ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
		defer cancel()

		ch, err := c.Space().Notify(ctx, coordin8.NotifyOpts{
			Template: spaceNotifyTemplate,
			On:       eventType,
			TTL:      time.Duration(spaceNotifyTTL) * time.Second,
			Handback: []byte(spaceNotifyHandback),
		})
		if err != nil {
			return err
		}

		fmt.Printf("watching %s events... (Ctrl+C to stop)\n\n", spaceNotifyOn)

		for evt := range ch {
			label := "APPEARANCE"
			if evt.Type == coordin8.Expiration {
				label = "EXPIRATION"
			}
			if evt.Tuple != nil {
				fmt.Printf("[%s] %s  tuple_id=%s", evt.OccurredAt.Format(time.RFC3339), label, evt.Tuple.TupleID)
				for k, v := range evt.Tuple.Attrs {
					fmt.Printf("  %s=%s", k, v)
				}
				fmt.Println()
			} else {
				fmt.Printf("[%s] %s\n", evt.OccurredAt.Format(time.RFC3339), label)
			}
		}
		return nil
	},
}

// ── cancel ───────────────────────────────────────────────────────────────────

var spaceCancelTupleID string

var spaceCancelCmd = &cobra.Command{
	Use:   "cancel",
	Short: "Cancel (remove) a tuple by ID",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		if err := c.Space().CancelTuple(context.Background(), spaceCancelTupleID); err != nil {
			return err
		}
		fmt.Printf("cancelled: %s\n", spaceCancelTupleID)
		return nil
	},
}

// ── renew ────────────────────────────────────────────────────────────────────

var (
	spaceRenewTupleID string
	spaceRenewTTL     int64
)

var spaceRenewCmd = &cobra.Command{
	Use:   "renew",
	Short: "Renew a tuple's lease",
	RunE: func(cmd *cobra.Command, args []string) error {
		c, err := connect()
		if err != nil {
			return err
		}
		defer c.Close()

		lease, err := c.Space().RenewTuple(context.Background(), spaceRenewTupleID, time.Duration(spaceRenewTTL)*time.Second)
		if err != nil {
			return err
		}
		fmt.Printf("lease_id:   %s\n", lease.LeaseID)
		fmt.Printf("expires_at: %s  (extended)\n", lease.ExpiresAt.Format(time.RFC3339))
		return nil
	},
}

// ── helpers ──────────────────────────────────────────────────────────────────

func printTuple(t *coordin8.TupleRecord) {
	fmt.Printf("tuple_id:   %s\n", t.TupleID)
	if t.LeaseID != "" {
		fmt.Printf("lease_id:   %s\n", t.LeaseID)
	}
	for k, v := range t.Attrs {
		fmt.Printf("  %-16s %s\n", k+":", v)
	}
	if len(t.Payload) > 0 {
		if isPrintable(t.Payload) {
			fmt.Printf("payload:    %s\n", string(t.Payload))
		} else {
			fmt.Printf("payload:    (base64) %s\n", base64.StdEncoding.EncodeToString(t.Payload))
		}
	}
	if t.WrittenBy != "" {
		fmt.Printf("written_by: %s\n", t.WrittenBy)
	}
	if !t.WrittenAt.IsZero() {
		fmt.Printf("written_at: %s\n", t.WrittenAt.Format(time.RFC3339))
	}
	if t.InputTupleID != "" {
		fmt.Printf("input:      %s\n", t.InputTupleID)
	}
}

func printTupleOneline(t *coordin8.TupleRecord) {
	fmt.Printf("%-40s", t.TupleID)
	for k, v := range t.Attrs {
		fmt.Printf("  %s=%s", k, v)
	}
	if len(t.Payload) > 0 && isPrintable(t.Payload) {
		fmt.Printf("  payload=%s", string(t.Payload))
	}
	fmt.Println()
}

func isPrintable(b []byte) bool {
	for _, c := range b {
		if c < 0x20 || c > 0x7e {
			return false
		}
	}
	return true
}

// ── init ─────────────────────────────────────────────────────────────────────

func init() {
	// write
	spaceWriteCmd.Flags().StringToStringVar(&spaceWriteAttrs, "attr", nil, "Tuple attributes (key=value, repeatable)")
	spaceWriteCmd.Flags().StringVar(&spaceWritePayload, "payload", "", "Tuple payload (string)")
	spaceWriteCmd.Flags().Int64Var(&spaceWriteTTL, "ttl", 30, "TTL in seconds")
	spaceWriteCmd.Flags().StringVar(&spaceWriteWrittenBy, "written-by", "cli", "Writer identity (provenance)")
	spaceWriteCmd.Flags().StringVar(&spaceWriteInputID, "input", "", "Input tuple ID (lineage)")
	spaceWriteCmd.Flags().StringVar(&spaceWriteTxnID, "txn", "", "Transaction ID (optional)")

	// read
	spaceReadCmd.Flags().StringToStringVar(&spaceReadTemplate, "match", nil, "Template fields (key=value)")
	spaceReadCmd.Flags().BoolVar(&spaceReadWait, "wait", false, "Block until a match arrives")
	spaceReadCmd.Flags().Int64Var(&spaceReadTimeout, "timeout", 0, "Wait timeout in seconds (0 = forever)")
	spaceReadCmd.Flags().StringVar(&spaceReadTxnID, "txn", "", "Transaction ID (optional)")

	// take
	spaceTakeCmd.Flags().StringToStringVar(&spaceTakeTemplate, "match", nil, "Template fields (key=value)")
	spaceTakeCmd.Flags().BoolVar(&spaceTakeWait, "wait", false, "Block until a match arrives")
	spaceTakeCmd.Flags().Int64Var(&spaceTakeTimeout, "timeout", 0, "Wait timeout in seconds (0 = forever)")
	spaceTakeCmd.Flags().StringVar(&spaceTakeTxnID, "txn", "", "Transaction ID (optional)")

	// contents
	spaceContentsCmd.Flags().StringToStringVar(&spaceContentsTemplate, "match", nil, "Template fields (key=value)")
	spaceContentsCmd.Flags().StringVar(&spaceContentsTxnID, "txn", "", "Transaction ID (optional)")

	// notify
	spaceNotifyCmd.Flags().StringToStringVar(&spaceNotifyTemplate, "match", nil, "Template fields (key=value)")
	spaceNotifyCmd.Flags().StringVar(&spaceNotifyOn, "on", "appearance", "Event type: appearance or expiration")
	spaceNotifyCmd.Flags().Int64Var(&spaceNotifyTTL, "ttl", 60, "Watch lease TTL in seconds")
	spaceNotifyCmd.Flags().StringVar(&spaceNotifyHandback, "handback", "", "Opaque handback data (echoed with events)")

	// cancel
	spaceCancelCmd.Flags().StringVar(&spaceCancelTupleID, "id", "", "Tuple ID to cancel (required)")
	spaceCancelCmd.MarkFlagRequired("id")

	// renew
	spaceRenewCmd.Flags().StringVar(&spaceRenewTupleID, "id", "", "Tuple ID to renew (required)")
	spaceRenewCmd.Flags().Int64Var(&spaceRenewTTL, "ttl", 30, "New TTL in seconds")
	spaceRenewCmd.MarkFlagRequired("id")

	spaceCmd.AddCommand(spaceWriteCmd, spaceReadCmd, spaceTakeCmd, spaceContentsCmd, spaceNotifyCmd, spaceCancelCmd, spaceRenewCmd)
}
