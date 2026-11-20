.PHONY: check fmt test test-s3 ui docker stack-up stack-down

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

# Local S3 + Vault stack in a podman pod (no compose needed); see dev/stack.sh.
stack-up: docker
	dev/stack.sh up

stack-down:
	dev/stack.sh down

# S3 storage conformance against a throwaway SeaweedFS container.
test-s3:
	docker rm -f gc-test-s3 >/dev/null 2>&1 || true
	docker run -d --name gc-test-s3 -p 19000:9000 docker.io/chrislusf/seaweedfs:latest server -dir=/data -s3 -s3.port=9000 -ip.bind=0.0.0.0
	until curl -s -o /dev/null -w '%{http_code}' -X PUT http://127.0.0.1:19000/gc-test | grep -qE '200|409'; do sleep 1; done
	GC_TEST_S3_ENDPOINT=http://127.0.0.1:19000 GC_TEST_S3_BUCKET=gc-test AWS_ACCESS_KEY_ID=any AWS_SECRET_ACCESS_KEY=any \
		cargo test --lib storage::conformance; status=$$?; docker rm -f gc-test-s3 >/dev/null; exit $$status
