FROM node:20-bookworm-slim AS frontend
WORKDIR /app
COPY package*.json ./
RUN npm ci
COPY tsconfig.json vite*.ts ./
COPY src ./src
COPY locales ./locales
RUN npm run build
COPY public ./public

FROM python:3.12-slim AS python
WORKDIR /app

FROM python AS catalog
COPY catalog.py cities.py glossary.py i18n.py world_places.py ./
COPY locales ./locales
COPY resources/places ./resources/places
COPY deploy/beta/export_places.py ./
RUN mkdir /catalog && python export_places.py /catalog

FROM python AS documents
COPY requirements-build.txt ./
RUN pip install --no-cache-dir -r requirements-build.txt
COPY catalog.py cities.py glossary.py i18n.py legal_pages.py public_documents.py site_branding.py world_places.py ./
COPY locales ./locales
COPY templates ./templates
COPY public/manifest.webmanifest ./public/
COPY resources/places ./resources/places
COPY deploy/beta/export_public.py ./
RUN python export_public.py /documents

FROM rust:1.90-bookworm AS rust
WORKDIR /build
COPY deploy/beta/places/Cargo.toml deploy/beta/places/Cargo.lock ./
COPY deploy/beta/places/src ./src
COPY deploy/beta/places/schema ./schema
RUN cargo build --locked --release --jobs 2

FROM rust AS rust-test
RUN cargo test --locked --release --jobs 2 --bins

FROM debian:bookworm-slim AS places
ENV WIRING_PLACES_BIND=0.0.0.0:5081
COPY --from=rust /build/target/release/wiring-beta-places /usr/local/bin/
COPY --from=catalog /catalog /catalog
COPY --from=documents /documents /documents
USER 10001:10001
CMD ["/usr/local/bin/wiring-beta-places"]

FROM debian:bookworm-slim AS local-runtime
RUN apt-get update && apt-get install --no-install-recommends -y curl ffmpeg openssl \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 10001 -g 10001 -m 755 /state/uploads /state/thumbs
COPY --from=places /usr/local/bin/wiring-beta-places /usr/local/bin/
COPY --from=places /catalog /catalog
COPY --from=places /documents /documents
COPY --from=frontend /app/public /var/www/wiring-static
USER 10001:10001
CMD ["/usr/local/bin/wiring-beta-places"]

FROM nginx:stable-alpine AS local-proxy
COPY --from=frontend /app/public /var/www/wiring-static
COPY deploy/local/nginx.conf /etc/nginx/conf.d/default.conf

FROM rust AS api-test
COPY deploy/beta/places/tests ./tests
RUN cargo test --locked --release --jobs 2 --tests --no-run
CMD ["cargo", "test", "--offline", "--locked", "--release", "--tests", "--", "--test-threads=1"]
