PROTO_DIR  := proto
SDK_GO_GEN := sdks/go/gen
GO         := $(HOME)/go-install/go/bin/go

.PHONY: proto build build-examples test lint clean

proto: ## Regenerate proto code for all languages
	@mkdir -p $(SDK_GO_GEN)
	protoc \
		--go_out=$(SDK_GO_GEN) --go_opt=paths=source_relative \
		--go-grpc_out=$(SDK_GO_GEN) --go-grpc_opt=paths=source_relative \
		-I $(PROTO_DIR) \
		$(PROTO_DIR)/coordin8/lease.proto \
		$(PROTO_DIR)/coordin8/registry.proto \
		$(PROTO_DIR)/coordin8/common.proto
	@mkdir -p examples/hello-coordin8/gen
	protoc \
		--go_out=examples/hello-coordin8/gen --go_opt=paths=source_relative \
		--go-grpc_out=examples/hello-coordin8/gen --go-grpc_opt=paths=source_relative \
		-I examples/hello-coordin8/proto \
		examples/hello-coordin8/proto/greeter.proto

build: ## Build the Djinn and CLI
	cd djinn && cargo build --release
	cd cli && $(GO) build -o ../djinn/coordin8 ./cmd/coordin8/

build-examples: ## Build example binaries into their own directories
	cd examples/hello-coordin8 && \
		$(GO) build -o bin/greeter-service ./greeter_service/ && \
		$(GO) build -o bin/greeter-client  ./greeter_client/
	@echo "examples/hello-coordin8/bin/greeter-service"
	@echo "examples/hello-coordin8/bin/greeter-client"

test: ## Run all tests
	cd djinn && cargo test --all
	cd sdks/go && $(GO) test ./...

lint: ## Lint all components
	cd djinn && cargo clippy --all -- -D warnings && cargo fmt --all --check
	cd sdks/go && $(GO) vet ./...
	cd cli && $(GO) vet ./...
	cd examples/hello-coordin8 && $(GO) vet ./...

clean:
	cd djinn && cargo clean
	rm -f djinn/coordin8
	rm -rf examples/hello-coordin8/bin
