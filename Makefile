.PHONY: check fmt test ui docker

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
	cd game-cacher-ui && npm ci && npx tsc -b && npx eslint . && npx vite build

fmt:
	cargo fmt

test:
	cargo test

ui:
	cd game-cacher-ui && npm ci && npx vite build

docker:
	docker build -f docker/Dockerfile -t game-cacher:dev .
