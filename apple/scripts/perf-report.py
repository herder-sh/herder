#!/usr/bin/env python3
"""Reads what perf.sh measured out of its result bundles and the apps' logs as a Markdown table,
and holds the scrolling to the budget.

    perf-report.py --budget MS_PER_S [--tolerance MS_PER_S] [--log PLATFORM=LOG]... PLATFORM=RESULT...

Prints, per test and platform:

- the hitch time ratio of a screen's scrolling: the mean over the measured iterations, and the
  worst one, in ms of hitches per s of scrolling, with the frame rate. A simulator and a virtual
  machine measure no frames, which it says in place of a ratio of 0;
- when a launch showed the Board, since its process started (the app's `herder-launch` line);
- the main thread's hangs while the test measured: its turns of 100 ms or longer (the app's
  `herder-hang` lines between the test's `herder-perf begin` and `end`), their sum, how much of
  it the thread ran rather than waited, and the longest.

A platform takes several result bundles, and its log as `log show --style ndjson` wrote it. A
screen whose test passed with no scrolling recorded is too short to scroll. Exits 1 when a mean
is over the budget by more than the tolerance, a test failed, or a platform ran none.
"""

import argparse
import json
import subprocess
import sys

RATIO = "hitch time ratio"
RATE = "frame rate"


def xcresult(command, result):
    out = subprocess.run(
        ["xcrun", "xcresulttool", "get", "test-results", command, "--path", result],
        check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def test_of(identifier):
    """`ScrollPerfTests/testTranscript()` → `Transcript`."""
    return identifier.split("/")[-1].removeprefix("test").removesuffix("()")


def outcomes(result):
    """{test: (whether it passed, its first failure, whether it measures scrolling)} of a result
    bundle."""
    found = {}

    def walk(node):
        if node.get("nodeType") == "Test Case":
            failures = [child["name"] for child in node.get("children", [])
                        if child.get("nodeType") == "Failure Message"]
            identifier = node["nodeIdentifier"]
            found[test_of(identifier)] = (
                node.get("result") == "Passed", next(iter(failures), ""), "ScrollPerfTests/" in identifier)
        for child in node.get("children", []):
            walk(child)

    for node in xcresult("tests", result)["testNodes"]:
        walk(node)
    return found


def metrics(result):
    """{test: {metric's display name, lowercased: [measurements]}} of a result bundle."""
    tests = {}
    for test in xcresult("metrics", result):
        for run in test["testRuns"]:
            tests[test_of(test["testIdentifier"])] = {
                metric["displayName"].lower(): metric["measurements"] for metric in run["metrics"]
            }
    return tests


def find(measured, name):
    """The measurements of the metric whose display name contains `name`."""
    return next((values for key, values in measured.items() if name in key), None)


def measured_lines(log):
    """{test: [(line's name, {its numbers})]}: the app's lines while each test measured."""
    tests, current = {}, None
    with open(log, errors="replace") as lines:
        for line in lines:
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            words = (entry.get("eventMessage") or "").split() if isinstance(entry, dict) else []
            if words[:2] == ["herder-perf", "begin"] and len(words) == 3:
                current = tests.setdefault(words[2], [])
            elif words[:2] == ["herder-perf", "end"]:
                current = None
            elif current is not None and words and words[0] in ("herder-hang", "herder-launch"):
                numbers = dict(word.split("=", 1) for word in words[1:] if "=" in word)
                current.append((words[0], {key: int(value) for key, value in numbers.items() if value.isdigit()}))
    return tests


def scrolling(values, limit):
    """A screen's scrolling as (its part of the cell, the mean when it is over `limit`)."""
    ratio, rate = find(values, RATIO), find(values, RATE)
    if not ratio:
        return None, None
    if rate is not None and not any(rate):
        return "no frames measured", None
    mean = sum(ratio) / len(ratio)
    part = f"{mean:.1f} ({max(ratio):.1f})"
    if rate:
        part += f" at {sum(rate) / len(rate):.0f} fps"
    if mean > limit:
        return f"**{part}** ✗", mean
    return part, None


def launch(lines):
    """When the launch among a test's lines showed the Board."""
    for name, numbers in lines:
        if name == "herder-launch" and "content" in numbers and "shown" in numbers:
            return f"Board after {max(numbers['content'], numbers['shown'])} ms"
    return None


def hangs(lines):
    turns = [numbers for name, numbers in lines if name == "herder-hang" and "ms" in numbers]
    if not turns:
        return "no hangs"
    total = sum(turn["ms"] for turn in turns)
    ran = sum(turn.get("cpu", 0) for turn in turns)
    count = "1 hang" if len(turns) == 1 else f"{len(turns)} hangs"
    return f"{count}, {total} ms ({ran} ms on the CPU), longest {max(turn['ms'] for turn in turns)} ms"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--budget", type=float, required=True)
    parser.add_argument("--tolerance", type=float, default=0)
    parser.add_argument("--log", action="append", default=[])
    parser.add_argument("results", nargs="+")
    args = parser.parse_args()

    platforms, tests, measured = [], {}, {}
    for platform, path in (result.split("=", 1) for result in args.results):
        if platform not in platforms:
            platforms.append(platform)
        tests.setdefault(platform, {}).update(outcomes(path))
        measured.setdefault(platform, {}).update(metrics(path))
    logs = {platform: measured_lines(path) for platform, path in (log.split("=", 1) for log in args.log)}
    names = sorted({test for by_test in tests.values() for test in by_test})
    limit = args.budget + args.tolerance

    over = [f"no test ran on {platform}" for platform in platforms if not tests[platform]]
    print(f"Per test: the hitch time ratio while scrolling, ms/s, mean (worst), at the frame rate measured, "
          f"budget {args.budget:g} ms/s" + (f" + {args.tolerance:g} tolerance" if args.tolerance else "")
          + "; when a launch showed the Board; and the main thread's hangs while the test measured, "
          "its turns of 100 ms or longer.\n")
    print("| Test | " + " | ".join(platforms) + " |")
    print("|---|" + "---|" * len(platforms))
    for test in names:
        cells = []
        for platform in platforms:
            if test not in tests[platform]:
                cells.append("")
                continue
            passed, failure, scrolls = tests[platform][test]
            if not passed:
                cells.append("failed")
                over.append(f"{test} on {platform} failed: {failure}")
                continue
            parts = []
            scrolled, mean = scrolling(measured[platform].get(test, {}), limit)
            if scrolls:
                parts.append(scrolled or "nothing to scroll")
            if mean is not None:
                over.append(f"{test} on {platform}: {mean:.1f} ms/s")
            if platform in logs:
                lines = logs[platform].get(test)
                if lines is None:
                    parts.append("nothing in the log")
                else:
                    parts += [part for part in (launch(lines), hangs(lines)) if part]
            cells.append("; ".join(parts))
        print(f"| {test} | " + " | ".join(cells) + " |")

    if over:
        print(f"\nOver {limit:g} ms/s, or failed: " + "; ".join(over) + ".")
        sys.exit(1)


if __name__ == "__main__":
    main()
