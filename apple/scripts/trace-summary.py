#!/usr/bin/env python3
"""Summarises an Instruments trace of the app (see profile-mac.sh) as plain text.

    trace-summary.py TRACE PID

Prints, for the process PID: its hitches, the SwiftUI views that updated most and longest, and
the main thread's heaviest frames in herder's own code.
"""

import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict


def rows(trace, schema):
    """Yields each row of a table as {column mnemonic: shown value}, resolving xctrace's refs."""
    xpath = f'/trace-toc/run[@number="1"]/data/table[@schema="{schema}"]'
    export = subprocess.Popen(
        ["xcrun", "xctrace", "export", "--input", trace, "--xpath", xpath],
        stdout=subprocess.PIPE,
    )
    columns, values = [], {}
    for event, element in ET.iterparse(export.stdout, events=("start", "end")):
        if event == "start":
            continue
        if element.tag == "mnemonic":
            columns.append(element.text)
        elif element.tag == "row":
            row = {}
            for column, cell in zip(columns, element):
                row[column] = shown(cell, values)
            yield row
            element.clear()
        if "id" in element.attrib and element.tag != "row":
            values[element.attrib["id"]] = shown(element, values)
    export.wait()


def shown(cell, values):
    """A cell's value: its own, or the one it refers to; a backtrace as its frames' names."""
    if "ref" in cell.attrib:
        return values.get(cell.attrib["ref"], "")
    if cell.tag.endswith("backtrace"):
        return [shown(frame, values) for frame in cell if frame.tag == "frame"]
    if cell.tag == "frame":
        binary = next((shown(child, values) for child in cell if child.tag == "binary"), "")
        return (cell.attrib.get("name", ""), binary)
    return cell.attrib.get("fmt", cell.attrib.get("name", cell.text or ""))


def nanos(text):
    """A duration as xctrace shows it ("9.50 µs", "275.00 ms") in nanoseconds."""
    number, _, unit = text.partition(" ")
    scale = {"ns": 1, "µs": 1e3, "ms": 1e6, "s": 1e9}.get(unit, 0)
    try:
        return float(number.replace(",", "")) * scale
    except ValueError:
        return 0


def ms(value):
    return f"{value / 1e6:9.1f} ms"


def main():
    trace, pid = sys.argv[1], sys.argv[2]
    ours = f"({pid})"

    hitches = [nanos(row["duration"]) for row in rows(trace, "hitches") if ours in row.get("process", "")]
    print(f"Hitches: {len(hitches)}, {ms(sum(hitches)).strip()} in all, longest {ms(max(hitches, default=0)).strip()}")

    count, time = Counter(), defaultdict(float)
    for row in rows(trace, "swiftui-updates"):
        if ours not in row.get("process", ""):
            continue
        view = row.get("view-name") or ""
        if not view or view.startswith("["):
            view = row.get("description", "?")
        count[view] += 1
        time[view] += nanos(row.get("duration", ""))
    print(f"\nSwiftUI updates: {sum(count.values())}, {ms(sum(time.values())).strip()} in all")
    print("  by time:")
    for view, total in sorted(time.items(), key=lambda item: -item[1])[:25]:
        print(f"  {ms(total)} {count[view]:8} {view[:140]}")

    frames, samples = Counter(), 0
    for row in rows(trace, "time-profile"):
        if ours not in row.get("process", "") or "Main Thread" not in row.get("thread", ""):
            continue
        samples += 1
        stack = row.get("stack") or []
        # Each of herder's own frames once per sample, however deep it recurs.
        for name in dict.fromkeys(name for name, binary in stack if binary == "herder"):
            frames[name] += 1
    print(f"\nMain thread samples: {samples}; herder's frames by share of them:")
    for name, hits in frames.most_common(25):
        print(f"  {100 * hits / max(samples, 1):5.1f}% {name[:150]}")


if __name__ == "__main__":
    main()
