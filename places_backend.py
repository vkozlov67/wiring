"""Client for the optional Rust place catalog."""

import json
import os
from urllib.error import URLError
from urllib.request import ProxyHandler, Request, build_opener

from cities import normalize_city
from i18n import get_language
from world_places import key

_opener = build_opener(ProxyHandler({}))


def search(url: str, query: str, country: str, include_countries: bool) -> list[dict[str, str]]:
    needle = key(query)
    body = json.dumps({
        "needle": needle,
        "normalized_city": normalize_city(query) if needle else "",
        "country": country,
        "include_countries": include_countries,
        "english": get_language() == "en",
    }).encode()
    request = Request(url, body, {"Content-Type": "application/json"})
    with _opener.open(request, timeout=2) as response:
        places = json.load(response)
    if not isinstance(places, list) or len(places) > 60 or any(
        not isinstance(place, dict)
        or place.keys() != {"value", "country", "label"}
        or not all(isinstance(value, str) for value in place.values())
        for place in places
    ):
        raise ValueError("Invalid Rust place catalog response")
    return places


def catalog(url: str) -> dict:
    with _opener.open(url, timeout=2) as response:
        payload = json.load(response)
    if not isinstance(payload, dict) or not {
        "neuro", "vibe", "looking_for", "genders", "intents", "places", "city_search",
        "country_search", "prompts", "report_reasons", "default_albums", "limits",
    } <= payload.keys():
        raise ValueError("Invalid Rust public catalog response")
    return payload


SEARCH_ERRORS = (URLError, TimeoutError, OSError, ValueError)
SEARCH_URL = os.environ.get("WIRING_PLACES_SEARCH_URL", "").strip()
CATALOG_URL = os.environ.get("WIRING_PLACES_CATALOG_URL", "").strip()
