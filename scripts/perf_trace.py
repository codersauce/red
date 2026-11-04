"""Completed Red timing records from legacy and current traces."""

import re

END = r"\[PERF\] (?:end id=\d+ t=\d+us )?"
EVENT = re.compile(END + r"event (?:Key\(|Paste\().*?: (\d+)us$", re.MULTILINE)
TIMING = re.compile(END + r"([\w:+-]+)(?: (.*?))?: (\d+)us$", re.MULTILINE)
STARTUP = re.compile(END + r"startup:interactive: \d+us")
