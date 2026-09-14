"""Verify the independent Python fixture and package its hash-checked saved pages."""

import argparse
from datetime import datetime
from functools import partial
import gzip
import hashlib
import importlib.metadata
import io
import json
import logging
import os
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import time
from types import SimpleNamespace
import warnings


ROOT = Path(__file__).resolve().parents[2]
GO_COMMIT = "0f04a39fb476a75744ed948bfcd887306ca3f187"
UPSTREAM = "b8952828329abaeeb3be21387b526f2be614ce67"
SOURCES = {
    "__init__.py": "a19a9fb3210b69fb3786b22a355a804e14abdc1e21f9403051f9e2e0a499869c",
    "cli.py": "88f577f0ad1b585955d4f9391b5c0b635e006ab00c9847cfbc443a3754aa6ccb",
    "core.py": "a77ff0dcd3533da0a64c1fde9cf100e5665b93836acb0e3a1e8204a3f80e9d03",
    "extractors.py": "389685498353db3a9b5235b394e6221171ac1075b89467fb1798e510a534111e",
    "meta.py": "55760f7a0c702e84debbd30bc15c22fd05d11ce759f3d0fea4515a12ab96155d",
    "settings.py": "e803e13bb05d958a208c06ad24e06958edf634571f637bd301d2871bb8efb3ef",
    "utils.py": "4a8a35b69ea26b57fff312108f648c0551b708b6550d53a9482780f90f58e2dd",
    "validators.py": "37c8a952e5b240c24ee75765e76a6faabb692209e11528b0da22411531264900",
}


def import_fixture(source):
    encoded = subprocess.check_output(["git", "-C", str(source), "show", f"{GO_COMMIT}:test-files/python-reference.json"])
    fixture = json.loads(encoded)
    if fixture["source_commit"] != UPSTREAM or fixture["source_sha256"] != SOURCES or len(fixture["cases"]) != 9614:
        raise RuntimeError("unexpected independent fixture provenance")
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
            for filename, expected in sorted(fixture["corpus_sha256_lf"].items()):
                content = (source / filename).read_bytes().replace(b"\r\n", b"\n")
                if hashlib.sha256(content).hexdigest() != expected:
                    raise RuntimeError(f"saved page changed: {filename}")
                entry = tarfile.TarInfo(filename)
                entry.size = len(content)
                entry.mode = 0o644
                archive.addfile(entry, io.BytesIO(content))
    for filename, content in (("python-reference.json", encoded), ("python-pages.tar.gz", output.getvalue())):
        (ROOT / "testdata" / filename).write_bytes(content)
        print(f"{filename}: {len(content)} bytes, SHA-256 {hashlib.sha256(content).hexdigest()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--import-go", type=Path)
    mode.add_argument("--check", action="store_true")
    parser.add_argument("--kind", action="append")
    parser.add_argument("--context", choices=("windows_utc", "windows_eastern"))
    args = parser.parse_args()
    if args.import_go:
        import_fixture(args.import_go)
        return
    timezone = "EST5EDT" if args.context == "windows_eastern" else "UTC"
    if os.environ.get("TZ") != timezone:
        raise SystemExit(subprocess.run([sys.executable, *sys.argv], env=os.environ | {"TZ": timezone}).returncode)
    if platform.python_implementation() != "CPython" or platform.python_version() != "3.14.6":
        raise RuntimeError("expected CPython 3.14.6")
    requirements = dict(line.split("==") for line in (ROOT / "tools/python-requirements.txt").read_text().splitlines() if line)
    if {package: importlib.metadata.version(package) for package in requirements} != requirements:
        raise RuntimeError("unexpected Python dependency versions")

    import dateutil.parser._parser as parser_module
    import dateutil.tz.tz as timezone_module
    import htmldate
    import htmldate.extractors as extractors
    from htmldate import find_date
    from htmldate.utils import Extractor
    from htmldate.validators import is_valid_date, validate_and_convert

    hashes = {path.name: hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()
              for path in sorted(Path(htmldate.__file__).parent.glob("*.py"))}
    if hashes != SOURCES:
        raise RuntimeError("installed HtmlDate source differs from the pinned upstream commit")
    expected_timestamp = 1730611800 if args.context == "windows_eastern" else 1730597400
    if datetime(2024, 11, 3, 1, 30).timestamp() != expected_timestamp:
        raise RuntimeError("unexpected C datetime timestamp environment")
    fixture = json.loads((ROOT / "testdata/python-reference.json").read_bytes())
    if fixture["source_commit"] != UPSTREAM or fixture["source_sha256"] != SOURCES or len(fixture["cases"]) != 9614:
        raise RuntimeError("unexpected independent fixture provenance")
    pages = {}
    if not args.context and (not args.kind or "file" in args.kind):
        with tarfile.open(ROOT / "testdata/python-pages.tar.gz", "r:gz") as archive:
            for entry in archive:
                content = archive.extractfile(entry).read()
                if hashlib.sha256(content).hexdigest() != fixture["corpus_sha256_lf"][entry.name]:
                    raise RuntimeError(f"saved page changed: {entry.name}")
                pages[entry.name] = content
        if pages.keys() != fixture["corpus_sha256_lf"].keys():
            raise RuntimeError("saved-page inventory changed")
    logging.disable(logging.CRITICAL)
    warnings.simplefilter("ignore")
    checked = 0
    for index, case in enumerate(fixture["cases"]):
        context = case.get("audit_id", "").split("-", 1)[0] if case.get("environment") else None
        if context != args.context or (args.kind and case["kind"] not in args.kind):
            continue
        environment = case.get("environment", fixture["timezone_context"])
        names, offsets = environment["names"], environment["offsets"]
        snapshot = SimpleNamespace(tzname=names, timezone=-offsets[0], altzone=-offsets[1],
                                   daylight=int(offsets[0] != offsets[1]), localtime=time.localtime)
        parser_module.time = timezone_module.time = snapshot
        parser_module.DEFAULTPARSER.info._year = environment["parser_year"]
        parser_module.DEFAULTPARSER.info._century = environment["parser_year"] // 100 * 100
        current = datetime.fromisoformat(case.get("current_time", fixture["current_time"]))
        extractors.EXTERNAL_PARSER._settings.RELATIVE_BASE = current.replace(tzinfo=None)
        extractors.dateutil_parse = partial(parser_module.parse, default=current.replace(
            tzinfo=None, hour=0, minute=0, second=0, microsecond=0, fold=int(case.get("default_fold", False))))
        is_valid_date.cache_clear()
        extractors.try_date_expr.cache_clear()
        options = case["options"]
        earliest, latest = datetime.fromisoformat(options["min"]), datetime.fromisoformat(options["max"])
        extensive = not options["fast"]
        kind, text = case["kind"], pages[case["file"]] if case["kind"] == "file" else case["input"]
        error = ""
        try:
            if kind in ("html", "file"):
                value = find_date(text, extensive_search=extensive, original_date=options["original"],
                                  min_date=earliest, max_date=latest, url=options.get("url") or None,
                                  deferred_url_extractor=options.get("defer", False))
            elif kind == "fast":
                value = extractors.custom_parse(text, "%Y-%m-%d", earliest, latest)
            elif kind == "try":
                value = extractors.try_date_expr(text, "%Y-%m-%d", extensive, earliest, latest)
            elif kind == "regex":
                value = validate_and_convert(extractors.regex_parse(text), "%Y-%m-%d", earliest, latest)
            elif kind == "url":
                value = extractors.extract_url_date(text, Extractor(extensive, latest, earliest, options["original"], "%Y-%m-%d"))
            else:
                raise RuntimeError(f"unexpected kind: {kind}")
        except Exception as exception:
            value, error = None, f"{type(exception).__name__}: {exception}"
        actual = {"date": value or "", "error": error}
        if actual != case["expected"]:
            raise RuntimeError(f"Python case {index} changed: {actual} != {case['expected']}")
        checked += 1
    print(f"Verified {checked} independent Python cases ({args.context or 'utc'}); fixture bytes unchanged", flush=True)
    if not args.context and (not args.kind or "fast" in args.kind):
        for context in ("windows_utc", "windows_eastern"):
            command = [sys.executable, __file__, "--check", "--context", context]
            for kind in args.kind or []:
                command.extend(["--kind", kind])
            subprocess.run(command, check=True, env=os.environ | {"TZ": "EST5EDT" if context == "windows_eastern" else "UTC"})


if __name__ == "__main__":
    main()