"""Build the public HTML and localized JSON files served by Rust."""
import json
import sys
from pathlib import Path

from jinja2 import Environment, FileSystemLoader, select_autoescape

from catalog import GENDER_IDS, INTENT_IDS, LEGACY_VIBE_IDS, NEURO_IDS, VIBE_IDS, catalog_payload
from cities import CITY_SET, _ALIASES, _LEGACY_CITY_INDEX
from i18n import catalog_copy, editorial_html, tr, use_language
from legal_pages import SUPPORT_HTML
from public_documents import APPLE_APP_SITE_ASSOCIATION, LANDING_THEMES, PAGES
from site_branding import SITE_META_DESCRIPTION, SITE_OG_DESCRIPTION, SITE_PAGE_TITLE


def export(destination):
    env = Environment(loader=FileSystemLoader("templates"), autoescape=select_autoescape())
    env.filters["localized_html"] = editorial_html
    env.globals["_"] = tr
    template = env.get_template("legal.html")
    spa = env.get_template("index.html")
    landing = env.get_template("landing.html")
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "admin.html").write_bytes(Path("templates/admin.html").read_bytes())
    (destination / "admin-header.html").write_bytes(Path("templates/components/header.html").read_bytes())
    (destination / "admin-i18n-en.json").write_bytes(Path("locales/en.json").read_bytes())
    (destination / "place-countries.json").write_bytes(Path("resources/places/countries.json").read_bytes())
    (destination / "profile-rules.json").write_text(json.dumps({
        "cities": sorted(CITY_SET), "city_keys": sorted(_LEGACY_CITY_INDEX.keys() | _ALIASES.keys()),
        "genders": sorted(GENDER_IDS), "intents": sorted(INTENT_IDS),
        "neuro": sorted(NEURO_IDS), "vibe": sorted(VIBE_IDS), "retired_vibe": sorted(LEGACY_VIBE_IDS),
    }, ensure_ascii=False), encoding="utf-8")
    (destination / "apple-app-site-association.json").write_text(
        json.dumps(APPLE_APP_SITE_ASSOCIATION), encoding="utf-8",
    )
    for language in ("ru", "en"):
        with use_language(language):
            catalog = catalog_payload()
            (destination / f"api-catalog-{language}.json").write_text(
                json.dumps({"ok": True, **(catalog_copy(catalog) if language == "en" else catalog)}, ensure_ascii=False),
                encoding="utf-8",
            )
            for suffix, analytics in (("", ""), ("-analytics", "__WIRING_GOOGLE_ANALYTICS__")):
                for name, notice, error in (("support-form", "", "__WIRING_SUPPORT_ERROR__"),
                                            ("support-done", "__WIRING_SUPPORT_NOTICE__", "")):
                    (destination / f"{name}-{language}{suffix}.html").write_text(template.render(
                        language=language, title=tr("Поддержка"), body=SUPPORT_HTML, support_form=True,
                        description=tr("Написать в поддержку WIRING. Почту указывать не обязательно."),
                        me={"name": "__WIRING_SUPPORT_NAME__", "email": "__WIRING_SUPPORT_EMAIL__"},
                        notice=notice, error=error, path="/support", site_url="__WIRING_SITE_ORIGIN__",
                        google_analytics_id=analytics, yandex_metrika_id="__WIRING_YANDEX_METRIKA__" if analytics else "",
                    ), encoding="utf-8")
                (destination / f"spa-{language}{suffix}.html").write_text(spa.render(
                    language=language, site_url="__WIRING_SITE_ORIGIN__", base_path="",
                    page_title=SITE_PAGE_TITLE, meta_description=SITE_META_DESCRIPTION,
                    og_description=SITE_OG_DESCRIPTION, registration_created=False,
                    registration_managed=False, google_analytics_id=analytics,
                    yandex_metrika_id="__WIRING_YANDEX_METRIKA__" if analytics else "",
                ), encoding="utf-8")
                for theme in LANDING_THEMES:
                    for ios in (False, True):
                        for android in (False, True):
                            name = f"landing-{theme}-{int(ios)}{int(android)}"
                            (destination / f"{name}-{language}{suffix}.html").write_text(landing.render(
                                language=language, site_url="__WIRING_SITE_ORIGIN__", path="/m",
                                description=tr(SITE_META_DESCRIPTION), theme=theme,
                                themes=[{"id": key, "label": tr(value)} for key, value in LANDING_THEMES.items()],
                                testflight_url="__WIRING_TESTFLIGHT__" if ios else "#testflight",
                                testflight_ready=ios, apk_url="__WIRING_APK__" if android else "#android-apk",
                                apk_ready=android, google_analytics_id=analytics,
                                yandex_metrika_id="__WIRING_YANDEX_METRIKA__" if analytics else "",
                            ), encoding="utf-8")
            manifest = json.loads(Path("public/manifest.webmanifest").read_text(encoding="utf-8"))
            manifest["description"] = tr(manifest.get("description", ""))
            manifest["lang"] = language
            (destination / f"manifest-{language}.json").write_text(
                json.dumps(manifest, ensure_ascii=False), encoding="utf-8",
            )
            for path, (title, description, body) in PAGES.items():
                name = path[1:]
                for suffix, analytics in (("", ""), ("-analytics", "__WIRING_GOOGLE_ANALYTICS__")):
                    (destination / f"{name}-{language}{suffix}.html").write_text(template.render(
                        language=language, title=tr(title), description=tr(description), body=body,
                        path=path, site_url="__WIRING_SITE_ORIGIN__",
                        google_analytics_id=analytics,
                        yandex_metrika_id="__WIRING_YANDEX_METRIKA__" if analytics else "",
                    ), encoding="utf-8")


if __name__ == "__main__":
    export(Path(sys.argv[1]))
