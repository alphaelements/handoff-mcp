#!/usr/bin/env python3
"""Normalize volatile values in a load_context JSON dump (golden fixture helper).

usage: normalize-load-context-golden.py <raw.json> <out.json>

Volatile values are replaced by placeholders (keys are kept so the shape is
still compared). Object key order is preserved as emitted (by_status order is
NOT stable across runs in v0.47.2; compare semantically, order-independent).
"""
import json
import re
import sys

ID_KEYS = {"agent_id": "<agent_id>"}
TS_KEYS = {"ended_at", "started_at", "created_at", "updated_at", "completed_at", "timestamp", "last_heartbeat"}
RE_TS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})")
RE_SID = re.compile(r"\bs-\d{8}-\d{6}-\d+\b")
RE_AID = re.compile(r"\ba-\d{8}-\d{6}-\d+\b")


def norm_str(s):
    s = RE_TS.sub("<ts>", s)
    s = RE_SID.sub("<session_id>", s)
    return RE_AID.sub("<agent_id>", s)


def walk(o, key=None, parent_key=None):
    if isinstance(o, dict):
        return {k: walk(v, k, key) for k, v in o.items()}
    if isinstance(o, list):
        return [walk(v, key, parent_key) for v in o]
    if isinstance(o, str):
        if key in ID_KEYS:
            return ID_KEYS[key]
        if key in TS_KEYS:
            return "<ts>"
        if key == "id" and parent_key == "previous_session":
            return "<session_id>"
        return norm_str(o)
    return o


def main():
    raw = open(sys.argv[1], encoding="utf-8").read()
    data = walk(json.loads(raw))
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()
