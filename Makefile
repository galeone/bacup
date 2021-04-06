format:
	cargo fmt

test:
	cargo test

lint:
	cargo clippy

build:
	cargo build 

basic: format test lint build

test-pg:
	docker-compose up -d
	sleep 1
	cargo test postgres -- --ignored
	docker-compose down

advanced: test-pg