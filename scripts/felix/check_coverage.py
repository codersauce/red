"""Validate the reference trace against the active M1 coverage inventory."""
import argparse
import json
from pathlib import Path
import tomllib


def validate(inventory, suite):
    if inventory["schema"] != 1 or suite["commit"] != inventory["reference"]:
        raise ValueError("unexpected coverage schema or Helix revision")
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
            ranges = event["ranges"]
            if not ranges or not 0 <= event["primary"] < len(ranges):
                raise ValueError(f"{case['name']} input {index}: invalid primary selection")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    args = parser.parse_args()
    inventory = tomllib.loads(Path(__file__).with_name("coverage.toml").read_text())
    suite = json.loads(args.trace.read_text())
    validate(inventory, suite)
    print(f"Compared inventory with {len(suite['cases'])} cases and "
          f"{sum(len(case['events']) for case in suite['cases'])} recorded inputs.")
    for area in inventory["area"]:
        print(f"- {area['name']}: " + ", ".join(area["remaining"]))


if __name__ == "__main__":
    main()
