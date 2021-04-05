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
	docker build -t eg_postgresql .
	docker-compose up -d
	cargo test postgres -- --ignored

test-mysql:
	cargo test mysql -- --ignored

stop:
	docker-compose down
	
advanced: test-pg test-mysql