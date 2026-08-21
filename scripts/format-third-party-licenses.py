#!/usr/bin/env python3
"""Read cargo-about JSON from stdin and emit THIRD_PARTY_LICENSES.md to stdout."""

import json
import sys


def main():
    data = json.load(sys.stdin)

    lines = []
    lines.append("# Third-Party Software Licenses")
    lines.append("")
    lines.append(
        "This file lists the licenses of third-party software statically linked"
    )
    lines.append(
        "into the handoff-mcp prebuilt binaries. These notices are provided to"
    )
    lines.append("satisfy license attribution requirements.")
    lines.append("")

    lines.append("## Summary")
    lines.append("")
    lines.append("| Crate | Version | License |")
    lines.append("|-------|---------|---------|")

    entries = []
    for lic in data["licenses"]:
        license_id = lic["id"]
        for used in lic["used_by"]:
            c = used["crate"]
            if c.get("source") and "crates.io" in c["source"]:
                entries.append(
                    {
                        "name": c["name"],
                        "version": c["version"],
                        "license": c.get("license", license_id),
                        "license_id": license_id,
                        "license_text": lic["text"],
                    }
                )

    seen = set()
    unique = []
    for e in sorted(entries, key=lambda x: x["name"].lower()):
        key = e["name"] + "-" + e["version"]
        if key not in seen:
            seen.add(key)
            unique.append(e)

    for e in unique:
        lines.append(f'| {e["name"]} | {e["version"]} | {e["license"]} |')

    lines.append("")
    lines.append("## Full License Texts")
    lines.append("")

    printed_licenses = set()
    for lic in data["licenses"]:
        lid = lic["id"]
        if lid in printed_licenses:
            continue
        printed_licenses.add(lid)

        crates = []
        for used in lic["used_by"]:
            c = used["crate"]
            if c.get("source") and "crates.io" in c["source"]:
                crates.append(f'{c["name"]} v{c["version"]}')

        if not crates:
            continue

        lines.append(f'### {lic["name"]}')
        lines.append("")
        lines.append("Used by: " + ", ".join(sorted(set(crates))))
        lines.append("")
        lines.append("```")
        lines.append(lic["text"].rstrip())
        lines.append("```")
        lines.append("")

    print("\n".join(lines))


if __name__ == "__main__":
    main()
