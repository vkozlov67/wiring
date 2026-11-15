"""Internal Rust adapter for discovery filters and authorized chat responses."""

import json
import os
from urllib.request import ProxyHandler, Request, build_opener

FEED_URL = os.environ.get("WIRING_FEED_FILTER_URL", "").strip()
CHAT_URL = os.environ.get("WIRING_CHAT_THREAD_URL", "").strip()
CHAT_SEND_URL = os.environ.get("WIRING_CHAT_SEND_URL", "").strip()
CHAT_MUTATE_URL = os.environ.get("WIRING_CHAT_MUTATE_URL", "").strip()
CHAT_HISTORY_URL = os.environ.get("WIRING_CHAT_HISTORY_URL", "").strip()
CHAT_MEDIA_URL = os.environ.get("WIRING_CHAT_MEDIA_URL", "").strip()
CHAT_PAIR_URL = os.environ.get("WIRING_CHAT_PAIR_URL", "").strip()
CHAT_TRANSCRIBE_URL = os.environ.get("WIRING_CHAT_TRANSCRIBE_URL", "").strip()
CHAT_INBOX_URL = os.environ.get("WIRING_CHAT_INBOX_URL", "").strip()
CHAT_MATCHES_URL = os.environ.get("WIRING_CHAT_MATCHES_URL", "").strip()
NOTICES_URL = os.environ.get("WIRING_NOTICES_URL", "").strip()
PASSWORD_URL = os.environ.get("WIRING_PASSWORD_URL", "").strip()
MOBILE_REFRESH_URL = os.environ.get("WIRING_MOBILE_REFRESH_URL", "").strip()
MOBILE_LOGOUT_URL = os.environ.get("WIRING_MOBILE_LOGOUT_URL", "").strip()
MOBILE_ISSUE_URL = os.environ.get("WIRING_MOBILE_ISSUE_URL", "").strip()
PROFILE_SETTINGS_URL = os.environ.get("WIRING_PROFILE_SETTINGS_URL", "").strip()
FEED_UPDATE_URL = os.environ.get("WIRING_FEED_UPDATE_URL", "").strip()
FEED_CLAIM_URL = os.environ.get("WIRING_FEED_CLAIM_URL", "").strip()
FEED_SWIPE_URL = os.environ.get("WIRING_FEED_SWIPE_URL", "").strip()
CHAT_TOKEN = os.environ.get("WIRING_CHAT_INTERNAL_TOKEN", "").strip()
BACKEND_ERRORS = (OSError, ValueError)
_opener = build_opener(ProxyHandler({}))
_THREAD_KEYS = {
    "id", "from_id", "mine", "body", "photo", "photo_url", "photo_urls", "audio_url",
    "audio_duration", "transcript", "edited", "created_at", "read", "reply_to",
}


def _post(url, payload, *, headers=None, timeout=2):
    request = Request(
        url,
        data=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
        headers={"Content-Type": "application/json", **(headers or {})},
    )
    with _opener.open(request, timeout=timeout) as response:
        return json.load(response)


def feed_filter(country, intents, candidates):
    result = _post(FEED_URL, {"country": country, "intents": intents, "candidates": candidates})
    ids = [candidate["id"] for candidate in candidates]
    if not isinstance(result, list) or any(type(item) is not int for item in result):
        raise ValueError("Invalid Rust feed response")
    selected = set(result)
    if result != [candidate for candidate in ids if candidate in selected]:
        raise ValueError("Invalid Rust feed order or candidate")
    return selected


def chat_thread(payload):
    result = _post(CHAT_URL, payload)
    expected = [message["id"] for message in payload["messages"]]
    if not isinstance(result, list) or any(
        not isinstance(item, dict) or item.keys() != _THREAD_KEYS for item in result
    ):
        raise ValueError("Invalid Rust chat response")
    if [item.get("id") for item in result] != expected:
        raise ValueError("Invalid Rust chat message order")
    return result


def chat_send(payload, *, url=None):
    result = _post(
        url or CHAT_SEND_URL, payload,
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict) or result.get("status") not in {
        "sent", "duplicate", "blocked", "no_match", "reply_missing",
    }:
        raise ValueError("Invalid Rust message write outcome")
    if result["status"] in {"sent", "duplicate"} and (
        type(result.get("id")) is not int or result["id"] <= 0
    ):
        raise ValueError("Invalid Rust message ID")
    if result["status"] == "sent" and type(result.get("updated_notice")) is not bool:
        raise ValueError("Invalid Rust notification outcome")
    return result


def chat_mutate(action, uid, message_id, changed_at, **fields):
    result = _post(
        CHAT_TRANSCRIBE_URL if action == "transcribe" else CHAT_MUTATE_URL,
        {"action": action, "uid": uid, "message_id": message_id, "changed_at": changed_at, **fields},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust chat mutation outcome")
    status = result.get("status")
    if status == "unavailable" or (action == "edit" and status in ("edited", "voice")):
        return result
    if action == "transcribe" and status == "transcribed" and isinstance(result.get("transcript"), str):
        return result
    if action == "delete" and status == "deleted":
        uploads = result.get("uploads")
        if isinstance(uploads, list) and all(isinstance(path, str) for path in uploads):
            return result
    if action == "react" and status == "reacted":
        reactions = result.get("reactions")
        if isinstance(reactions, list) and all(
            isinstance(reaction, dict) and reaction.keys() == {"emoji", "count", "mine"}
            and isinstance(reaction["emoji"], str) and type(reaction["count"]) is int
            and reaction["count"] > 0 and type(reaction["mine"]) is bool
            for reaction in reactions
        ):
            return result
    raise ValueError("Invalid Rust chat mutation outcome")


def chat_history(payload):
    result = _post(
        CHAT_HISTORY_URL, payload,
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust chat history outcome")
    status = result.get("status")
    if status in ("blocked", "no_match"):
        return result
    messages = result.get("messages")
    if status == "available" and type(result.get("has_more")) is bool and isinstance(messages, list) and all(
        isinstance(message, dict) and message.keys() == _THREAD_KEYS | {"reactions"}
        for message in messages
    ):
        return result
    raise ValueError("Invalid Rust chat history response")


def feed_update(action, uid, now, **fields):
    result = _post(
        FEED_UPDATE_URL, {"action": action, "uid": uid, "now": now, **fields},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust feed update outcome")
    status = result.get("status")
    if action == "view" and status in ("viewed", "not_delivered"):
        return result
    if action == "reset" and (
        status == "premium_required" or (status == "reset" and type(result.get("reset")) is bool
                                         and type(result.get("generation")) is int)
    ):
        return result
    raise ValueError("Invalid Rust feed update outcome")


def feed_claim(uid, candidates, limit, include_delivered, now):
    result = _post(
        FEED_CLAIM_URL,
        {"uid": uid, "candidates": list(candidates), "limit": limit, "include_delivered": include_delivered, "now": now},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict) or type(result.get("has_more")) is not bool or type(result.get("generation")) is not int:
        raise ValueError("Invalid Rust feed page")
    ids = result.get("ids")
    if not isinstance(ids, list) or len(ids) > limit or any(type(item) is not int or item not in candidates for item in ids):
        raise ValueError("Invalid Rust feed page candidates")
    if len(ids) != len(set(ids)):
        raise ValueError("Duplicate Rust feed page candidates")
    return [candidates[item] for item in ids], result["has_more"], result["generation"]


def feed_swipe(uid, target_id, direction, seed_direction, now):
    result = _post(
        FEED_SWIPE_URL,
        {"uid": uid, "target_id": target_id, "direction": direction, "seed_direction": seed_direction, "now": now},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust swipe outcome")
    status = result.get("status")
    if status in ("blocked", "not_found", "premium_required", "already_matched"):
        return result
    if status == "snoozed" and type(result.get("until")) is int:
        return result
    if status == "applied" and type(result.get("liked")) is int and all(
        type(result.get(key)) is bool for key in ("matched", "was_match")
    ):
        return result
    raise ValueError("Invalid Rust swipe outcome")


def chat_pair(action, uid, other_id, now, **fields):
    result = _post(
        CHAT_PAIR_URL, {"action": action, "uid": uid, "other_id": other_id, "now": now, **fields},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust chat pair outcome")
    status = result.get("status")
    if (action == "unmatch" and status in ("hidden", "no_match")) or (
        action == "block" and status in ("hidden", "not_found")
    ):
        return result
    if action == "report" and (status == "not_found" or (
        status == "reported" and type(result.get("id")) is int and result["id"] > 0
        and type(result.get("newly_banned")) is bool
    )):
        return result
    raise ValueError("Invalid Rust chat pair outcome")


def chat_inbox(uid):
    result = _post(CHAT_INBOX_URL, {"uid": uid}, headers={"X-Wiring-Chat-Token": CHAT_TOKEN})
    if not isinstance(result, dict) or result.keys() != {"likes_in", "unread", "unread_chats"} or any(
        type(count) is not int or count < 0 for count in result.values()
    ):
        raise ValueError("Invalid Rust inbox counters")
    return result


def chat_matches(uid, voice_label, photo_label):
    result = _post(
        CHAT_MATCHES_URL, {"uid": uid, "voice_label": voice_label, "photo_label": photo_label},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN},
    )
    numbers = {"id", "last_at", "last_from_id", "matched_at", "unread"}
    if not isinstance(result, list) or any(
        not isinstance(item, dict) or item.keys() != numbers | {"last_message"}
        or not isinstance(item["last_message"], str) or any(type(item[key]) is not int for key in numbers)
        for item in result
    ):
        raise ValueError("Invalid Rust chat summaries")
    return result


def notices(action, uid, **fields):
    result = _post(
        NOTICES_URL, {"action": action, "uid": uid, **fields},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN},
    )
    if not isinstance(result, list) or any(
        not isinstance(item, dict) or item.keys() != {"id", "kind", "from_id", "body", "created_at"}
        or any(type(item[key]) is not int for key in ("id", "from_id", "created_at"))
        or not isinstance(item["kind"], str) or not isinstance(item["body"], str)
        for item in result
    ) or (action == "mark_read" and result):
        raise ValueError("Invalid Rust notification response")
    return result


def verify_password(password_hash, password):
    result = _post(
        PASSWORD_URL, {"hash": password_hash, "password": password},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=5,
    )
    if not isinstance(result, dict) or result.get("status") not in ("valid", "invalid", "unsupported"):
        raise ValueError("Invalid Rust password verification response")
    return {"valid": True, "invalid": False, "unsupported": None}[result["status"]]


def rotate_mobile_refresh(old_hash, new_hash, now, expires_at):
    result = _post(
        MOBILE_REFRESH_URL, {"old_hash": old_hash, "new_hash": new_hash, "now": now, "expires_at": expires_at},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict):
        raise ValueError("Invalid Rust refresh outcome")
    if result.get("status") == "invalid":
        return None
    uid, fingerprint = result.get("user_id"), result.get("fingerprint")
    if result.get("status") != "rotated" or type(uid) is not int or uid <= 0 or not isinstance(fingerprint, str) or len(fingerprint) != 16:
        raise ValueError("Invalid Rust refresh credentials")
    return uid, fingerprint


def save_profile_setting(action, uid, **fields):
    result = _post(
        PROFILE_SETTINGS_URL, {"action": action, "uid": uid, **fields},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict) or type(result.get("updated")) is not bool:
        raise ValueError("Invalid Rust profile settings response")
    return result["updated"]


def logout_mobile(token_hash, push_token, now):
    result = _post(
        MOBILE_LOGOUT_URL, {"token_hash": token_hash, "push_token": push_token, "now": now},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict) or result.get("ok") is not True:
        raise ValueError("Invalid Rust mobile logout outcome")


def issue_mobile_refresh(uid, token_hash, family, fingerprint, device, now, expires_at):
    result = _post(
        MOBILE_ISSUE_URL,
        {"uid": uid, "token_hash": token_hash, "family": family, "fingerprint": fingerprint,
         "device": device, "now": now, "expires_at": expires_at},
        headers={"X-Wiring-Chat-Token": CHAT_TOKEN}, timeout=12,
    )
    if not isinstance(result, dict) or type(result.get("issued")) is not bool:
        raise ValueError("Invalid Rust mobile issuance outcome")
    return result["issued"]
