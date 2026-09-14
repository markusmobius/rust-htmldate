"""Compare Rust and Go v1.10.1 with 64 serial DOM-only passes, then check Python parity."""

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
GO_COMMIT = "0f04a39fb476a75744ed948bfcd887306ca3f187"
RUST_COMMIT = "538e08d7847af490e6c91f9aef81cb26aee44ea0"
PYTHON_COMMIT = "b8952828329abaeeb3be21387b526f2be614ce67"
COHORTS = tuple(f"document-{date}-{mode}" for date in ("original", "modified")
                for mode in ("fast", "extensive"))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_reference(arguments, fixtures):
    if arguments.python_evidence:
        evidence = json.loads(arguments.python_evidence.read_text(encoding="utf-8"))
        entries = [json.loads(line) for line in arguments.labels.read_text(encoding="utf-8-sig").splitlines() if line]
        if len(entries) != 1000 or len({entry["URL"] for entry in entries}) != 1000:
            raise RuntimeError("Expected all 1000 labelled entries without deduplication")
        if len(evidence["requests"]) != 4000 or len(evidence["expected"]) != 4000:
            raise RuntimeError("Expected all 4000 independent Python results")
        outcomes = {cohort: [] for cohort in COHORTS}
        for index, (entry, fixture) in enumerate(zip(entries, fixtures, strict=True)):
            if entry["File"] != Path(fixture["file"]).name:
                raise RuntimeError("Label and corpus order differ")
            for offset, cohort in enumerate(COHORTS):
                request = {"file": fixture["file"], "original": "-original-" in cohort,
                           "fast": cohort.endswith("-fast")}
                if evidence["requests"][4 * index + offset] != request:
                    raise RuntimeError("Python expectation and corpus order differ")
                outcomes[cohort].append(evidence["expected"][4 * index + offset])
        reference = {
            "source_commit": evidence["reference_commit"], "python": evidence["python"],
            "dependencies": evidence["dependencies"], "checked_at_utc": evidence["checked_at_utc"],
            "fixture_sha256": evidence["reused_fixture_sha256"],
            "evidence_sha256": digest(arguments.python_evidence), "labels_sha256": digest(arguments.labels),
            "uncovered_pages_checked_live": evidence["uncovered_pages_checked_live"],
            "labels": [entry["Date"] for entry in entries], "outcomes": outcomes,
        }
    else:
        previous = json.loads(arguments.reference.read_text(encoding="utf-8"))
        if previous["fixtures"] != fixtures:
            raise RuntimeError("Reference report has a different corpus")
        reference = previous["reference"]
    fixture_path = ROOT / "testdata/python-reference.json"
    if reference["source_commit"] != PYTHON_COMMIT or reference["python"] != "3.14.6":
        raise RuntimeError("Unexpected Python reference identity")
    if reference["fixture_sha256"] != digest(fixture_path):
        raise RuntimeError("Independent Python fixture changed")
    fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
    if reference["dependencies"] != fixture["dependencies"] or len(reference["labels"]) != 1000:
        raise RuntimeError("Python dependencies or labels differ")
    known = {(case["file"], case["options"]["original"], case["options"]["fast"]): case
             for case in fixture["cases"] if case["kind"] == "file"}
    for cohort in COHORTS:
        for page, expected in zip(fixtures, reference["outcomes"][cohort], strict=True):
            case = known.get((page["file"], "-original-" in cohort, cohort.endswith("-fast")))
            if case is None:
                if page["file"] not in reference["uncovered_pages_checked_live"]:
                    raise RuntimeError("Missing independent Python expectation")
                continue
            if (case["expected"] != {"date": expected, "error": ""}
                    or fixture["corpus_sha256_lf"][page["file"]] != page["sha256"]
                    or case["options"]["min"] != "1995-01-01T00:00:00Z"
                    or case["options"]["max"] != "2026-09-13T23:59:59.999999999Z"):
                raise RuntimeError("Stored Python expectation or input differs")
    return reference


def scores(labels, outcomes):
    counts = dict(TP=0, FP=0, FN=0, TN=0)
    for label, date in zip(labels, outcomes, strict=True):
        if not date:
            counts["TN" if not label else "FN"] += 1
        else:
            counts["TP" if date == label else "FP"] += 1
    positives = counts["TP"]
    return counts | {
        "precision": positives / (positives + counts["FP"]),
        "recall": positives / (positives + counts["FN"]),
        "accuracy": (positives + counts["TN"]) / len(labels),
        "f_score": 2 * positives / (2 * positives + counts["FP"] + counts["FN"]),
    }


def summarize(samples):
    summary = {}
    for cohort in COHORTS:
        summary[cohort] = {}
        for engine in ("Go", "Rust"):
            timings = [sample["pass_ms"] for sample in samples
                       if sample["cohort"] == cohort and sample["engine"] == engine]
            if len(timings) != 8:
                raise RuntimeError("Expected exactly eight timed passes per cell")
            summary[cohort][engine] = {
                "median_ms": statistics.median(timings),
                "range_ms": [min(timings), max(timings)], "samples_ms": timings,
            }
        summary[cohort]["go_over_rust_time"] = (
            summary[cohort]["Go"]["median_ms"] / summary[cohort]["Rust"]["median_ms"]
        )
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--go", type=Path, required=True, help="Go HtmlDate checkout with its comparison runner")
    parser.add_argument("--cpu", type=int, default=2)
    parser.add_argument("--cargo", default=str(Path.home() / ".cargo/bin/cargo"))
    parser.add_argument("--reference", type=Path, default=ROOT / "tools/benchmark-v1.10.1.json")
    parser.add_argument("--python-evidence", type=Path, help="initial independently checked Python results")
    parser.add_argument("--labels", type=Path, help="initial ordered Go comparison JSONL label manifest")
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    arguments.go = arguments.go.resolve()
    if bool(arguments.python_evidence) != bool(arguments.labels):
        parser.error("supply --python-evidence and --labels together")
    if arguments.output.exists():
        parser.error("output already exists; choose a new report path")
    if not hasattr(os, "sched_getaffinity") or arguments.cpu not in os.sched_getaffinity(0):
        parser.error("run under Linux/WSL with an available CPU")
    sys.dont_write_bytecode = True
    runner_path = arguments.go / "scripts/comparison/benchmark.py"
    spec = importlib.util.spec_from_file_location("go_benchmark", runner_path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    environment = os.environ.copy()
    settings = {
        "GOTOOLCHAIN": "go1.27.1", "GOWORK": "off", "GOOS": "linux", "GOARCH": "amd64",
        "CGO_ENABLED": "0", "GOAMD64": "v1", "GOMAXPROCS": "1", "TZ": "UTC",
        "GOGC": "100", "GOMEMLIMIT": "off", "GOFLAGS": "", "GOEXPERIMENT": "", "GODEBUG": "",
        "LC_ALL": "C.UTF-8", "CARGO_BUILD_JOBS": "1", "RUST_TEST_THREADS": "1",
        "RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": "",
    }
    environment.update(settings)
    environment.pop("CARGO_BUILD_TARGET", None)
    environment["CARGO_TARGET_DIR"] = str(ROOT / "target")
    goroot = helper.capture(["go", "env", "GOROOT"], arguments.go, environment).strip()
    environment["ZONEINFO"] = str(Path(goroot) / "lib/time/zoneinfo.zip")
    helper.capture(["git", "diff", "--ignore-cr-at-eol", "--exit-code", RUST_COMMIT, "--", "Cargo.toml", "Cargo.lock",
                    "rust-toolchain.toml", "src", "data"], ROOT, environment)
    started = time.monotonic()
    deadline = started + 360
    signal.signal(signal.SIGALRM, helper.timeout_expired)
    signal.signal(signal.SIGTERM, helper.timeout_expired)
    report = {
        "measured_at_utc": datetime.now(timezone.utc).isoformat(), "platform": platform.platform(),
        "cpu": next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                    if line.startswith("model name")),
        "cpu_affinity": [arguments.cpu], "environment": settings, "status": "in-progress",
        "runs_per_engine_and_cohort": 8, "total_timed_passes": 64,
        "processes": "One persistent process per engine; all 1000 DOMs parsed once per process.",
        "warmup": "One untimed DOM pass per engine/mode; eight warmups before timing.",
        "timing": "Extraction only; file reads, initial HTML parsing, formatting and validation excluded.",
        "corpus_normalization": "CRLF to LF for both engines; no inputs removed or deduplicated.",
        "corpus_storage": "Identical saved pages staged on the native Linux temporary filesystem before parsing.",
        "saved_pages_archive_sha256": digest(ROOT / "testdata/python-pages.tar.gz"),
        "zoneinfo_sha256": digest(Path(environment["ZONEINFO"])),
        "runner_sha256": {"Rust/" + path.relative_to(ROOT).as_posix(): digest(path)
                          for path in (Path(__file__).resolve(), ROOT / "examples/benchmark.rs")},
        "source_references": {}, "fixtures": [], "warmup_metadata": {}, "dom_outcomes": {}, "samples": [],
    }
    report["runner_sha256"].update({
        "Go/" + path.relative_to(arguments.go).as_posix(): digest(path)
        for path in [runner_path, *sorted((arguments.go / "scripts/comparison").glob("*.go"))]
    })
    helper.save_report(arguments.output, report)
    workers = {}
    try:
        with tempfile.TemporaryDirectory(prefix="rust-htmldate-benchmark-") as directory:
            work = Path(directory)
            corpus = work / "corpus"
            corpus.mkdir()
            with tarfile.open(ROOT / "testdata/python-pages.tar.gz") as archive:
                archive.extractall(corpus, filter="data")
            inventory = json.loads((arguments.go / "scripts/comparison/benchmark-v1.9.3-v1.10.1.json").read_text(encoding="utf-8"))
            report["fixtures"] = inventory["fixtures"]
            for fixture in report["fixtures"]:
                destination = corpus / fixture["file"]
                if (not destination.is_file()
                        or hashlib.sha256(destination.read_bytes().replace(b"\r\n", b"\n")).hexdigest() != fixture["sha256"]):
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(arguments.go / fixture["file"], destination)
                content = destination.read_bytes().replace(b"\r\n", b"\n")
                if len(content) != fixture["bytes"] or hashlib.sha256(content).hexdigest() != fixture["sha256"]:
                    raise RuntimeError(f"Canonical corpus changed: {fixture['file']}")
            print("Build pinned Go v1.10.1 and Rust v1.10.1 release benchmark", flush=True)
            go_binary, report["source_references"]["Go"] = helper.build(
                "Go-v1.10.1", GO_COMMIT, work, environment, "git",
            )
            build = [arguments.cargo, "+1.98.1", "build", "--release", "--locked", "--offline", "--example", "benchmark"]
            subprocess.run(build, cwd=ROOT, env=environment, check=True, timeout=180)
            rust_binary = ROOT / "target/release/examples/benchmark"
            report["source_references"]["Rust"] = {
                "commit": RUST_COMMIT, "cargo_lock_sha256": digest(ROOT / "Cargo.lock"),
                "cargo_toml_sha256": digest(ROOT / "Cargo.toml"), "binary_sha256": digest(rust_binary),
                "build_command": build,
                "rustc": helper.capture([str(Path(arguments.cargo).with_name("rustc")), "+1.98.1", "--version", "--verbose"],
                                         ROOT, environment).strip(),
            }
            commands = {
                "Go": [str(go_binary), "-benchmark", "document", "-passes", "1", "-corpus-root", str(corpus),
                       "-min-date", "1995-01-01", "-max-date", "2026-09-13", "-current-time", "2026-09-13T12:00:00Z"],
                "Rust": [str(rust_binary), str(corpus), str(work / "fixtures.json")],
            }
            for engine, command in commands.items():
                print(f"Load and parse {engine} corpus once", flush=True)
                workers[engine] = subprocess.Popen(
                    ["taskset", "-c", str(arguments.cpu), *command], cwd=ROOT, env=environment,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1,
                )
                ready = helper.read_message(workers[engine], deadline)
                if ready["metadata"]["cases"] != 1000 or (report["fixtures"] and ready["fixtures"] != report["fixtures"]):
                    raise RuntimeError("Engines received different corpus bytes")
                report["fixtures"] = ready["fixtures"]
                (work / "fixtures.json").write_text(json.dumps(report["fixtures"]), encoding="utf-8")
            report["reference"] = load_reference(arguments, report["fixtures"])
            for cohort in COHORTS:
                for engine, worker in workers.items():
                    print(f"Warmup {engine} {cohort}", flush=True)
                    result = helper.measure(worker, cohort, True, deadline)
                    report["warmup_metadata"][f"{engine}/{cohort}"] = result["metadata"]
                    report["dom_outcomes"][f"{engine}/{cohort}"] = [outcome["date"] for outcome in result["results"]]
                    helper.save_report(arguments.output, report)
            for round_index in range(8):
                cohorts = list(COHORTS[round_index % 4:] + COHORTS[:round_index % 4])
                engines = list(workers)
                if round_index % 2:
                    cohorts.reverse()
                    engines.reverse()
                for cohort in cohorts:
                    for engine in engines:
                        result = helper.measure(workers[engine], cohort, False, deadline)
                        if result["metadata"] != report["warmup_metadata"][f"{engine}/{cohort}"]:
                            raise RuntimeError("Outputs or settings changed during timing")
                        result.pop("results", None)
                        result.update(round=round_index + 1, engine=engine, cohort=cohort)
                        report["samples"].append(result)
                        helper.save_report(arguments.output, report)
                        print(f"{len(report['samples'])}/64 {engine} {cohort}: {result['pass_ms']:.2f} ms/pass", flush=True)
            report["summary"] = summarize(report["samples"])
            report["benchmark_elapsed_seconds"] = time.monotonic() - started
            accuracy = {"cases": 4000, "path": "from_reader, outside the timed benchmark", "outcomes": {}, "differences": [], "summary": {}}
            for cohort in COHORTS:
                print(f"Check Rust/Python full-input accuracy: {cohort}", flush=True)
                worker = workers["Rust"]
                worker.stdin.write(json.dumps({"cohort": cohort, "warmup": True, "accuracy": True}) + "\n")
                worker.stdin.flush()
                result = helper.read_message(worker, deadline)
                if result["metadata"]["cohort"] != cohort or not result["accuracy"] or len(result["results"]) != 1000:
                    raise RuntimeError("Unexpected accuracy response")
                actual = [outcome["date"] for outcome in result["results"]]
                expected = report["reference"]["outcomes"][cohort]
                accuracy["outcomes"][cohort] = actual
                for index, (rust_date, python_date) in enumerate(zip(actual, expected, strict=True)):
                    if rust_date != python_date:
                        accuracy["differences"].append({"index": index, "file": report["fixtures"][index]["file"],
                                                        "cohort": cohort, "Rust": rust_date, "Python": python_date})
                if "-original-" in cohort:
                    accuracy["summary"][cohort] = {
                        engine: scores(report["reference"]["labels"], dates)
                        for engine, dates in (("Python reference", expected), ("Rust v1.10.1", actual))
                    }
                report["accuracy"] = accuracy
                helper.save_report(arguments.output, report)
            accuracy["matching_outputs"] = 4000 - len(accuracy["differences"])
            report["dom_differences"] = {
                cohort: [{"index": index, "file": report["fixtures"][index]["file"], "Go": go_date, "Rust": rust_date}
                         for index, (go_date, rust_date) in enumerate(zip(report["dom_outcomes"][f"Go/{cohort}"],
                                                                        report["dom_outcomes"][f"Rust/{cohort}"], strict=True))
                         if go_date != rust_date] for cohort in COHORTS
            }
        report.update(status="complete", elapsed_seconds=time.monotonic() - started)
        helper.save_report(arguments.output, report)
    except BaseException as error:
        report.update(status="incomplete", error=str(error), elapsed_seconds=time.monotonic() - started)
        helper.save_report(arguments.output, report)
        raise
    finally:
        for worker in workers.values():
            worker.stdin.close()
        for worker in workers.values():
            try:
                worker.wait(timeout=5)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait()
    for cohort, result in report["summary"].items():
        print(f"{cohort}: Go {result['Go']['median_ms']:.2f} ms, Rust {result['Rust']['median_ms']:.2f} ms, "
              f"Go/Rust {result['go_over_rust_time']:.3f}x", flush=True)
    print(f"Completed 64 timed passes in {report['benchmark_elapsed_seconds']:.1f}s; "
          f"Rust/Python agreement {report['accuracy']['matching_outputs']}/4000", flush=True)
    print(json.dumps(report["accuracy"]["summary"], indent=2), flush=True)


if __name__ == "__main__":
    main()