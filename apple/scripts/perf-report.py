#!/usr/bin/env python3
"""Reads the scroll measurements out of perf.sh's result bundles as a Markdown table, and holds
them to the budget.

    perf-report.py --budget MS_PER_S [--tolerance MS_PER_S] PLATFORM=RESULT...

Prints, per screen and platform, the hitch time ratio of its scrolling: the mean over the
measured iterations, and the worst one, in ms of hitches per s of scrolling, with the frame
rate. A screen whose test passed with no scrolling recorded is too short to scroll. Exits 1
when a mean is over the budget by more than the tolerance, or a screen's test failed.
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


def screen_of(identifier):
    """`ScrollPerfTests/testTranscript()` → `Transcript`."""
    return identifier.split("/")[-1].removeprefix("test").removesuffix("()")


def outcomes(result):
    """{screen: (whether its test passed, its first failure)} of a result bundle."""
    found = {}

    def walk(node):
        if node.get("nodeType") == "Test Case":
            failures = [child["name"] for child in node.get("children", [])
                        if child.get("nodeType") == "Failure Message"]
            found[screen_of(node["nodeIdentifier"])] = (node.get("result") == "Passed", next(iter(failures), ""))
        for child in node.get("children", []):
            walk(child)

    for node in xcresult("tests", result)["testNodes"]:
        walk(node)
    return found


def metrics(result):
    """{screen: {metric's display name, lowercased: [measurements]}} of a result bundle."""
    screens = {}
    for test in xcresult("metrics", result):
        for run in test["testRuns"]:
            screens[screen_of(test["testIdentifier"])] = {
                metric["displayName"].lower(): metric["measurements"] for metric in run["metrics"]
            }
    return screens


def find(measured, name):
    """The measurements of the metric whose display name contains `name`."""
    return next((values for key, values in measured.items() if name in key), None)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--budget", type=float, required=True)
    parser.add_argument("--tolerance", type=float, default=0)
    parser.add_argument("results", nargs="+")
    args = parser.parse_args()

    platforms = [result.split("=", 1) for result in args.results]
    tests = {platform: outcomes(path) for platform, path in platforms}
    measured = {platform: metrics(path) for platform, path in platforms}
    screens = sorted({screen for by_screen in tests.values() for screen in by_screen})
    limit = args.budget + args.tolerance

    over = []
    print(f"Hitch time ratio while scrolling, ms/s: mean (worst), at the frame rate measured. "
          f"Budget {args.budget:g} ms/s" + (f" + {args.tolerance:g} tolerance" if args.tolerance else "") + ".\n")
    print("| Screen | " + " | ".join(platform for platform, _ in platforms) + " |")
    print("|---|" + "---|" * len(platforms))
    for screen in screens:
        cells = []
        for platform, _ in platforms:
            passed, failure = tests[platform].get(screen, (False, "not run"))
            values = measured[platform].get(screen, {})
            ratio, rate = find(values, RATIO), find(values, RATE)
            if not passed:
                cells.append("failed")
                over.append(f"{screen} on {platform} failed: {failure}")
                continue
            if not ratio:
                cells.append("nothing to scroll")
                continue
            mean = sum(ratio) / len(ratio)
            cell = f"{mean:.1f} ({max(ratio):.1f})"
            if rate:
                cell += f" at {sum(rate) / len(rate):.0f} fps"
            if mean > limit:
                cell = f"**{cell}** ✗"
                over.append(f"{screen} on {platform}: {mean:.1f} ms/s")
            cells.append(cell)
        print(f"| {screen} | " + " | ".join(cells) + " |")

    if over:
        print(f"\nOver {limit:g} ms/s, or failed: " + "; ".join(over) + ".")
        sys.exit(1)


if __name__ == "__main__":
    main()
