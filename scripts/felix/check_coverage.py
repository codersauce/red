"""Validate the reference trace against the active M1 coverage inventory."""
import argparse
import json
from pathlib import Path
import tomllib


def check_bindings(cases, commands, mode, counted):
    missing = {tuple(command.split()) for command in commands}
    if len(missing) != len(commands):
        raise ValueError(f"duplicate binding in {mode} inventory")
    for case in cases:
        events = case["events"]
        keys = [event["key"] for event in events]
        before = {"mode": "Normal", "count": None, "pending": []}
        for index, event in enumerate(events):
            if before["mode"] == mode and not before["pending"] and (not counted or before["count"]):
                missing.difference_update([command for command in missing
                                           if tuple(keys[index:index + len(command)]) == command])
            before = event
    if missing:
        raise ValueError(f"{mode}: missing {'counted ' if counted else ''}commands {sorted(missing)}")


def validate(inventory, suite):
    if inventory["schema"] != 1 or suite["commit"] != inventory["reference"]:
        raise ValueError("unexpected coverage schema or Helix revision")
    for area in inventory["area"]:
        if area["remaining"]:
            raise ValueError(f"{area['name']}: open M1 cases {area['remaining']}")
    seed = suite["seed"]
    expected = [case.format(seed=seed) for area in inventory["area"] for case in area["cases"]]
    names = [case["name"] for case in suite["cases"]]
    if len(set(expected)) != len(expected) or len(set(names)) != len(names):
        raise ValueError("duplicate case in coverage inventory or trace")
    if set(names) != set(expected):
        raise ValueError(f"missing cases {set(expected) - set(names)}, unexpected cases {set(names) - set(expected)}")
    required = set(inventory["fields"])
    for case in suite["cases"]:
        if not isinstance(case["text"], str) or not case["events"]:
            raise ValueError(f"{case['name']}: missing initial text or events")
        for index, event in enumerate(case["events"], 1):
            missing = required - event.keys()
            if missing:
                raise ValueError(f"{case['name']} input {index}: missing fields {sorted(missing)}")
            if event["key"] == "<paste>" and not isinstance(event.get("paste"), str):
                raise ValueError(f"{case['name']} input {index}: missing paste text")
            if case["name"].startswith("prompt-"):
                if "prompt" not in event:
                    raise ValueError(f"{case['name']} input {index}: missing prompt state")
                prompt = event["prompt"]
                if prompt is not None:
                    candidates, selected = prompt["candidates"], prompt["selected"]
                    if candidates != sorted(set(candidates)) or (selected is not None and not 0 <= selected < len(candidates)):
                        raise ValueError(f"{case['name']} input {index}: invalid completion list or selection")
                    line, cursor = prompt["line"].encode(), prompt["cursor"]
                    if not 0 <= cursor <= len(line):
                        raise ValueError(f"{case['name']} input {index}: invalid prompt cursor")
                    line[:cursor].decode()  # The cursor is a UTF-8 byte offset.
            if case["name"].startswith("repeat-") and not isinstance(event.get("last_insert"), list):
                raise ValueError(f"{case['name']} input {index}: missing insert recipe")
            if case["name"].startswith("page-") or case["name"] in ("search-viewport", "search-wrapped-viewport"):
                view = event.get("viewport", {})
                if not (isinstance(view.get("anchor"), int) and 0 <= view["anchor"] <= len(event["text"])
                        and isinstance(view.get("height"), int) and view["height"] > 0):
                    raise ValueError(f"{case['name']} input {index}: missing or invalid viewport")
            ranges = event["ranges"]
            if not ranges or not 0 <= event["primary"] < len(ranges):
                raise ValueError(f"{case['name']} input {index}: invalid primary selection")

    for mode in ("Normal", "Select"):
        check_bindings(suite["cases"], inventory["bindings"]["counted_normal_select"], mode, True)
    check_bindings(suite["cases"], inventory["bindings"]["insert"], "Insert", False)


def validate_terminal(inventory, terminal):
    from reference_smoke import verify_records

    if terminal["commit"] != inventory["reference"]:
        raise ValueError("terminal capture used a different Helix revision")
    groups = {name.replace("-", "_") for area in inventory["area"] for name in area.get("terminal", [])}
    for name in groups:
        for key in (name, "red_" + name):
            frames = terminal.get(key)
            if not isinstance(frames, list) or not frames:
                raise ValueError(f"missing terminal checkpoints for {key}")
    records = terminal["events"]
    verify_records(records)
    # Prove that a dropped observation and a reversed primary selection are
    # distinguishable even when both editors paint the same text.
    reversed_primary = dict(records[5], ranges=[[3, 0], *records[5]["ranges"][1:]])
    for fault in (records[:-1], [*records[:5], reversed_primary, *records[6:]]):
        try:
            verify_records(fault)
        except ValueError:
            continue
        raise ValueError("reference comparison accepted a deliberately damaged capture")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--terminal", type=Path, help="paired release and candidate terminal checkpoints")
    args = parser.parse_args()
    inventory = tomllib.loads(Path(__file__).with_name("coverage.toml").read_text())
    suite = json.loads(args.trace.read_text())
    validate(inventory, suite)
    if args.terminal:
        validate_terminal(inventory, json.loads(args.terminal.read_text()))
    print(f"Compared inventory with {len(suite['cases'])} cases and "
          f"{sum(len(case['events']) for case in suite['cases'])} recorded inputs.")
    for area in inventory["area"]:
        if area["remaining"]:
            print(f"- {area['name']}: " + ", ".join(area["remaining"]))


if __name__ == "__main__":
    main()
