FROM node:20-bookworm-slim AS frontend
WORKDIR /app
COPY package*.json ./
RUN npm ci
COPY tsconfig.json vite*.ts ./
COPY src ./src
COPY locales ./locales
RUN npm run build

FROM python:3.12-slim AS python
WORKDIR /app

FROM python AS catalog
COPY catalog.py cities.py glossary.py i18n.py world_places.py ./
COPY locales ./locales
COPY resources/places ./resources/places
COPY deploy/beta/export_places.py ./
RUN mkdir /catalog && python export_places.py /catalog

FROM python AS documents
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt
COPY catalog.py cities.py glossary.py i18n.py legal_pages.py public_documents.py site_branding.py world_places.py ./
COPY locales ./locales
COPY templates ./templates
COPY public/manifest.webmanifest ./public/
COPY resources/places ./resources/places
COPY deploy/beta/export_public.py ./
RUN python export_public.py /documents

FROM rust:1.90-bookworm AS rust
WORKDIR /build
COPY deploy/beta/places ./
RUN cargo build --locked --release --jobs 2

FROM rust AS rust-test
RUN cargo test --locked --release --jobs 2

FROM debian:bookworm-slim AS places
ENV WIRING_PLACES_BIND=0.0.0.0:5081
COPY --from=rust /build/target/release/wiring-beta-places /usr/local/bin/
COPY --from=catalog /catalog /catalog
COPY --from=documents /documents /documents
USER 10001:10001
CMD ["/usr/local/bin/wiring-beta-places"]

FROM python AS flask
WORKDIR /app
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt
COPY *.py ./
COPY scripts ./scripts
COPY public ./public
COPY templates ./templates
COPY locales ./locales
COPY resources ./resources
COPY deploy/beta/wsgi.py ./beta_wsgi.py
COPY --from=frontend /app/public/dist ./public/dist
RUN mkdir /state && chown 10001:10001 /state
USER 10001:10001
CMD ["gunicorn", "--preload", "--bind", "0.0.0.0:5070", "--workers", "1", "--threads", "4", "beta_wsgi:app"]
