"""Beta-only WSGI entry point and gateway to the Rust city search."""

import os

from flask import jsonify

from app import app, tr
import places_backend


def rust_search(query: str, country: str, include_countries: bool):
    try:
        places = places_backend.search("http://places:5081/search", query, country, include_countries)
    except places_backend.SEARCH_ERRORS:
        app.logger.warning("Beta Rust city search unavailable")
        return jsonify({"ok": False, "error": tr("сервер временно недоступен")}), 503

    response = jsonify({"ok": True, "places": places})
    response.headers["X-Wiring-Beta-Places"] = "rust"
    return response


backend = os.environ.get("BETA_PLACES_BACKEND", "flask")
if backend == "rust":
    app.config["BETA_PLACES_SEARCH"] = rust_search
elif backend != "flask":
    raise ValueError("BETA_PLACES_BACKEND must be flask or rust")
