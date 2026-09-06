package main

import (
	"fmt"
	"os"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/spf13/cobra"
)

// ── auth ──────────────────────────────────────────────────────────────────────
//
// Static, offline token minting — Decision 2 of
// .claude/plans/grpc-security/PRD.md. There is no live issuance service in
// v1: a service (or this CLI) reads its own token from COORDIN8_TOKEN at
// startup, the same way it already reads COORDIN8_REGISTRY, so there's no
// discovery cycle to bootstrap through. mint-token is the only way tokens
// get created; it never talks to a Djinn.

var authCmd = &cobra.Command{
	Use:   "auth",
	Short: "Mint JWTs for a Coordin8 deployment with COORDIN8_JWT_SECRET set",
}

var (
	authSecret string
	authSub    string
	authTTL    time.Duration
	authScope  string
	authIssuer string
)

// authClaims mirrors coordin8-auth's Rust `Claims` struct field-for-field
// (sub/exp/iat/scope/iss) so a token minted here decodes identically on
// every service, regardless of which language validates it.
type authClaims struct {
	jwt.RegisteredClaims
	Scope string `json:"scope,omitempty"`
}

var authMintTokenCmd = &cobra.Command{
	Use:   "mint-token",
	Short: "Mint a signed HS256 token for a caller identity (offline, no live Djinn needed)",
	Example: `  coordin8 auth mint-token --sub greeter --ttl 720h
  COORDIN8_JWT_SECRET=... coordin8 auth mint-token --sub settlement-engine --scope admin`,
	RunE: func(cmd *cobra.Command, args []string) error {
		secret := authSecret
		if secret == "" {
			secret = os.Getenv("COORDIN8_JWT_SECRET")
		}
		if secret == "" {
			return fmt.Errorf("no signing secret: pass --secret or set COORDIN8_JWT_SECRET")
		}

		now := time.Now()
		claims := authClaims{
			RegisteredClaims: jwt.RegisteredClaims{
				Subject:   authSub,
				IssuedAt:  jwt.NewNumericDate(now),
				ExpiresAt: jwt.NewNumericDate(now.Add(authTTL)),
				Issuer:    authIssuer,
			},
			Scope: authScope,
		}

		signed, err := jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString([]byte(secret))
		if err != nil {
			return fmt.Errorf("sign token: %w", err)
		}
		fmt.Println(signed)
		return nil
	},
}

func init() {
	authMintTokenCmd.Flags().StringVar(&authSecret, "secret", "", "HS256 signing secret (default: $COORDIN8_JWT_SECRET)")
	authMintTokenCmd.Flags().StringVar(&authSub, "sub", "", "Caller identity this token authenticates as (required)")
	authMintTokenCmd.Flags().DurationVar(&authTTL, "ttl", 24*time.Hour, "Token lifetime")
	authMintTokenCmd.Flags().StringVar(&authScope, "scope", "", "Optional scope claim, for a future authorization layer")
	authMintTokenCmd.Flags().StringVar(&authIssuer, "issuer", "", "Optional issuer claim")
	authMintTokenCmd.MarkFlagRequired("sub")

	authCmd.AddCommand(authMintTokenCmd)
	rootCmd.AddCommand(authCmd)
}
