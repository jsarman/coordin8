PROTO_DIR  := proto
SDK_GO_GEN := sdks/go/gen
# Use mise-managed `go` when active; fall back to the legacy non-standard install.
GO         ?= $(HOME)/go-install/go/bin/go

.PHONY: proto build build-examples test lint clean

proto: ## Regenerate proto code for all languages
	@mkdir -p $(SDK_GO_GEN)
	protoc \
		--go_out=$(SDK_GO_GEN) --go_opt=paths=source_relative \
		--go-grpc_out=$(SDK_GO_GEN) --go-grpc_opt=paths=source_relative \
		-I $(PROTO_DIR) \
		$(PROTO_DIR)/coordin8/lease.proto \
		$(PROTO_DIR)/coordin8/registry.proto \
		$(PROTO_DIR)/coordin8/common.proto \
		$(PROTO_DIR)/coordin8/event.proto \
		$(PROTO_DIR)/coordin8/transaction.proto
	@mkdir -p examples/hello-coordin8/go/gen
	protoc \
		--go_out=examples/hello-coordin8/go/gen --go_opt=paths=source_relative \
		--go-grpc_out=examples/hello-coordin8/go/gen --go-grpc_opt=paths=source_relative \
		-I examples/hello-coordin8/go/proto \
		examples/hello-coordin8/go/proto/greeter.proto
	cd sdks/node && npm run proto
	cd examples/hello-coordin8/node && \
		protoc \
		--plugin=protoc-gen-ts_proto=./node_modules/.bin/protoc-gen-ts_proto \
		--ts_proto_out=gen \
		--ts_proto_opt=outputServices=grpc-js,esModuleInterop=true,env=node \
		-I ../go/proto \
		../go/proto/greeter.proto

build: ## Build the Djinn and CLI
	cd djinn && cargo build --release
	cd cli && $(GO) build -o ../djinn/coordin8 ./cmd/coordin8/

build-examples: ## Build example binaries into their own directories
	cd examples/hello-coordin8/go && \
		$(GO) build -o bin/greeter-service ./greeter_service/ && \
		$(GO) build -o bin/greeter-client  ./greeter_client/
	@echo "examples/hello-coordin8/go/bin/greeter-service"
	@echo "examples/hello-coordin8/go/bin/greeter-client"
	cd examples/hello-coordin8/java && ./gradlew assemble -q
	@echo "examples/hello-coordin8/java/build/distributions/ (run via ./gradlew run)"
	cd examples/hello-coordin8/node && npm run build
	@echo "examples/hello-coordin8/node/dist/ (run via npm start)"
	cd examples/market-watch && $(GO) build -o bin/market-watch .
	@echo "examples/market-watch/bin/market-watch"
	cd examples/double-entry && $(GO) build -o bin/double-entry .
	@echo "examples/double-entry/bin/double-entry"

test: ## Run all tests
	cd djinn && cargo test --all
	cd sdks/go && $(GO) test ./...

lint: ## Lint all components
	cd djinn && cargo clippy --all -- -D warnings && cargo fmt --all --check
	cd sdks/go && $(GO) vet ./...
	cd cli && $(GO) vet ./...
	cd examples/hello-coordin8/go && $(GO) vet ./...
	cd examples/market-watch && $(GO) vet ./...
	cd examples/double-entry && $(GO) vet ./...

clean:
	cd djinn && cargo clean
	rm -f djinn/coordin8
	rm -rf examples/hello-coordin8/go/bin
	rm -rf examples/market-watch/bin examples/double-entry/bin
	cd examples/hello-coordin8/java && ./gradlew clean -q
	cd sdks/java && ./gradlew clean -q
	rm -rf sdks/node/dist examples/hello-coordin8/node/dist
