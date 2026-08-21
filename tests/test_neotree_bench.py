import unittest

from scripts.neotree_bench import percentile, summarize, timing_samples


class NeoTreeBenchmarkTest(unittest.TestCase):
    def test_timing_samples_preserve_request_names(self) -> None:
        samples = timing_samples(
            "[PERF] drain ListDirectory: 42us\n"
            "[PERF] drain UpdateTreePanel: 7us\n"
            "[PERF] event Key: 11us\n"
        )

        self.assertEqual(samples["drain ListDirectory"], [42])
        self.assertEqual(samples["drain UpdateTreePanel"], [7])
        self.assertEqual(samples["event"], [11])

    def test_percentile_handles_empty_and_orders_samples(self) -> None:
        self.assertEqual(percentile([], 95), 0)
        self.assertEqual(percentile([9, 1, 3], 50), 3)

    def test_summary_requires_every_sample_to_reach_the_last_entry(self) -> None:
        base = {
            "entries": 512,
            "open_ms": 20,
            "directory_ms": 5,
            "navigation_p95_us": 100,
            "rss_delta_kib": 1024,
            "idle_cpu_ms": 0,
        }
        result = summarize(
            [
                {**base, "target_reachable": True, "truncation_marker": False},
                {**base, "target_reachable": False, "truncation_marker": True},
            ]
        )

        self.assertEqual(len(result), 1)
        self.assertFalse(result[0]["complete"])
        self.assertTrue(result[0]["truncation_marker"])


if __name__ == "__main__":
    unittest.main()
