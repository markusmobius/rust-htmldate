"""Generate independent results from pinned upstream Python htmldate."""

import argparse
from collections import Counter
from datetime import datetime
from functools import partial
import hashlib
import importlib.metadata
import json
import logging
from pathlib import Path
import platform


ROOT = Path(__file__).resolve().parents[1]
REQUIREMENTS = dict(line.split("==") for line in (ROOT / "tools/python-requirements.txt").read_text(encoding="utf-8").splitlines() if line)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--go-source", type=Path, required=True)
    parser.add_argument("--input", type=Path, action="append", default=[])
    parser.add_argument("--output", type=Path, default=ROOT / "target/python-historical-comparison.json")
    parser.add_argument("--go-fixture", type=Path)
    parser.add_argument("--probe", action="store_true")
    args = parser.parse_args()
    versions = {package: importlib.metadata.version(package) for package in REQUIREMENTS}
    for package, required in REQUIREMENTS.items():
        if versions[package] != required:
            parser.error(f"expected {package} {required}, found {versions[package]}")

    import htmldate
    import htmldate.extractors as extractors
    from lxml import etree
    from htmldate import find_date
    from htmldate.extractors import (
        EXTERNAL_PARSER, JSON_PUBLISHED, custom_parse, extract_url_date,
        regex_parse, try_date_expr,
    )
    from htmldate.utils import Extractor, load_html
    from htmldate.validators import validate_and_convert

    logging.disable(logging.CRITICAL)
    minimum = datetime(1995, 1, 1)
    maximum = datetime(2026, 9, 13, 23, 59, 59, 999999)
    frozen = datetime(2026, 9, 13, 12)
    EXTERNAL_PARSER._settings.RELATIVE_BASE = frozen
    extractors.dateutil_parse = partial(extractors.dateutil_parse, default=frozen.replace(hour=0))
    if args.probe:
        print(json.dumps({"python": platform.python_version(), "dependencies": versions}, indent=2))
        examples = [
            '<html><head><script type="application/ld+json">{"dateCreated":"2020-01-01T12:00:00Z","datePublished":"2020-01-02T13:00:00Z"}</script></head><body></body></html>',
            '<html><head><script type="application/ld+json">{"datePublished":"2020-02-02T12:00:00Z","other":{"datePublished":"2020-01-01T13:00:00Z"}}</script></head><body></body></html>',
        ]
        for example in examples:
            print(json.dumps({"input": example, "date": find_date(example, original_date=True, extensive_search=False, min_date=minimum, max_date=maximum)}))
        for filename in ("test-files/comparison/chicagotribune.com-Biden.html", "test-files/mediacloud/1727473717.html"):
            content = (args.go_source / filename).read_bytes()
            tree = load_html(content)
            matches = []
            for element in tree.xpath('.//script[@type="application/ld+json" or @type="application/settings+json"]'):
                if not element.text or '"date' not in element.text:
                    continue
                match = JSON_PUBLISHED.search(element.text)
                if match:
                    start = match.start(1)
                    end = element.text.find('"', start)
                    matches.append(element.text[start:end])
            dates = Counter(find_date(content, original_date=True, extensive_search=False, min_date=minimum, max_date=maximum) for _ in range(20))
            print(json.dumps({"file": filename, "dates": dates, "published_matches_in_order": matches}))
        return

    package_root = Path(htmldate.__file__).parent
    output = {
        "python": platform.python_version(),
        "dependencies": versions,
        "libxml": ".".join(map(str, etree.LIBXML_VERSION)),
        "source_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(package_root.glob("*.py"))},
        "current_time": frozen.isoformat(),
        "cases": [],
        "unsupported": [],
    }
    differences = []
    go_cases = []
    counts = Counter()
    for input_path in args.input:
        fixture = json.loads(input_path.read_text(encoding="utf-8"))
        for index, case in enumerate(fixture["cases"]):
            kind = case["kind"]
            identity = {"fixture": input_path.name, "index": index, "kind": kind}
            if kind == "time":
                output["unsupported"].append({**identity, "reason": "Go-only time/timezone extraction API"})
                continue
            options = case["options"]
            earliest = datetime.fromisoformat(options["min"]).replace(tzinfo=None)
            latest = datetime.fromisoformat(options["max"]).replace(tzinfo=None)
            extensive = not options["fast"]
            settings = Extractor(extensive, latest, earliest, options["original"], "%Y-%m-%d")
            text = case["input"]
            if kind == "file":
                text = (args.go_source / case["file"]).read_bytes()
                if hashlib.sha256(text).hexdigest() != case["sha256"]:
                    raise ValueError(f"corpus file changed: {case['file']}")
            error = ""
            try:
                if kind in ("html", "file"):
                    date = find_date(text, extensive_search=extensive, original_date=options["original"], min_date=earliest, max_date=latest, url=options.get("url") or None, deferred_url_extractor=options.get("defer", False))
                elif kind == "fast":
                    date = custom_parse(text, "%Y-%m-%d", earliest, latest)
                elif kind == "regex":
                    date = validate_and_convert(regex_parse(text), "%Y-%m-%d", earliest=earliest, latest=latest)
                elif kind == "url":
                    date = extract_url_date(text, settings)
                elif kind == "try":
                    date = try_date_expr(text, "%Y-%m-%d", extensive, earliest, latest)
                else:
                    raise ValueError(f"unmapped case kind: {kind}")
            except Exception as exception:
                date = None
                error = f"{type(exception).__name__}: {exception}"
            actual = {"date": date or "", "error": error}
            go_cases.append({"kind": kind, "input": case["input"], "file": case.get("file", ""), "sha256": case.get("sha256", ""), "options": options, "expected": actual})
            output["cases"].append({**identity, **actual})
            counts[kind] += 1
            expected = case["expected"]["date"].split("T")[0]
            if actual["date"] != expected or error != case["expected"]["error"]:
                differences.append({**identity, "file": case.get("file", ""), "input": case["input"], "options": options, "python": actual, "go": {"date": expected, "error": case["expected"]["error"]}})
            if (index + 1) % 500 == 0:
                print(f"{input_path.name}: {index + 1}/{len(fixture['cases'])}", flush=True)
    output["counts"] = counts
    output["differences"] = differences
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")
    if args.go_fixture:
        go_fixture = {key: value for key, value in output.items() if key not in ("cases", "differences", "unsupported")}
        go_fixture["cases"] = go_cases
        go_fixture["go_only_cases"] = len(output["unsupported"])
        args.go_fixture.parent.mkdir(parents=True, exist_ok=True)
        args.go_fixture.write_text(json.dumps(go_fixture, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"compared": counts, "unsupported": len(output["unsupported"]), "differences": len(differences), "output": str(args.output)}, indent=2))


if __name__ == "__main__":
    main()