"""Export the existing curated city data and Python's Unicode search keys."""

import gzip
import json
from pathlib import Path
import sys

from cities import PLACES, _preserve_legacy, country_search_index
from catalog import catalog_payload
from i18n import tr
from world_places import ROOT, key


def place(value: str, country: str, *, world: bool = False):
    label = "Энгельс · Россия" if world and value == "Engels · Россия" else value
    return value, country, label, tr(label, language="en")


def export(destination: Path):
    (destination / "catalog.json").write_text(
        json.dumps(catalog_payload(), ensure_ascii=False), encoding="utf-8"
    )
    tokens = country_search_index()
    countries = []
    cities = []
    for block in PLACES:
        country = block["country"]
        countries.append((
            place(country, country), " ".join(tokens.get(country, [])),
            key(country), key(tr(country, language="en")),
        ))
        for city in block["cities"]:
            cities.append((place(city, country), key(city)))
    (destination / "curated.json").write_text(json.dumps((countries, cities), ensure_ascii=False), encoding="utf-8")

    with gzip.open(ROOT / "world-cities.jsonl.gz", "rt", encoding="utf-8") as source:
        with (destination / "world.jsonl").open("w", encoding="utf-8") as target:
            for line in source:
                value, name, country, aliases, _ = json.loads(line)
                canonical = _preserve_legacy(value)
                target.write(json.dumps((place(canonical, country, world=True), key(name), f"|{aliases}|"), ensure_ascii=False) + "\n")


if __name__ == "__main__":
    export(Path(sys.argv[1]))
