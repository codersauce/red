import unittest

from perf_trace import STARTUP, TIMING


class PerformanceTraceTest(unittest.TestCase):
    def test_only_completed_records_count_toward_latency(self):
        log = """[PERF] begin id=1 t=4us thread=1 startup:interactive
[PERF] end id=1 t=20us startup:interactive: 16us
[PERF] begin id=2 t=21us thread=1 event Key(x)
[PERF] end id=2 t=44us event Key(x): 23us
[PERF] event Key(y): 11us
"""
        self.assertTrue(STARTUP.search(log))
        self.assertEqual([(m[1], m[3]) for m in TIMING.finditer(log)],
                         [("startup:interactive", "16"), ("event", "23"), ("event", "11")])
        self.assertFalse(STARTUP.search(log.splitlines()[0]))


if __name__ == "__main__":
    unittest.main()
