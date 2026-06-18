import tempfile
import unittest
from pathlib import Path

from analyze_zakura_sync import analyze_database, build_cache


FIXTURE_DIR = Path(__file__).parent / "fixtures"


class ZakuraTraceAnalysisTest(unittest.TestCase):
    def run_fixture_analysis(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            out_dir = Path(tmpdir)
            db_path = build_cache(FIXTURE_DIR, out_dir, refresh_cache=True)
            result = analyze_database(db_path, out_dir, 60)
            expected_outputs = [
                "summary.md",
                "rates.csv",
                "state_intervals.csv",
                "sync_throughput.png",
                "sync_backlog.png",
                "bottleneck_timeline.png",
                "download_budget_utilization.png",
                "blocking_breakdown.png",
            ]
            for filename in expected_outputs:
                self.assertTrue((out_dir / filename).is_file(), filename)
            return result

    def test_download_and_finalize_rates(self):
        result = self.run_fixture_analysis()
        rates = result["rates"].set_index("bucket_start_s")

        self.assertIn(0, rates.index)
        self.assertIn(60, rates.index)

        first_bucket = rates.loc[0]
        self.assertEqual(first_bucket["downloaded_raw_blocks"], 3)
        self.assertEqual(first_bucket["downloaded_unique_blocks"], 2)
        self.assertEqual(first_bucket["downloaded_raw_bytes"], 500)
        self.assertEqual(first_bucket["downloaded_unique_bytes"], 300)
        self.assertEqual(first_bucket["finalized_blocks"], 2)
        self.assertEqual(first_bucket["finalized_known_bytes"], 300)
        self.assertEqual(first_bucket["finalized_missing_blocks"], 0)

        second_bucket = rates.loc[60]
        self.assertEqual(second_bucket["downloaded_raw_blocks"], 1)
        self.assertEqual(second_bucket["downloaded_unique_blocks"], 1)
        self.assertEqual(second_bucket["downloaded_unique_bytes"], 300)
        self.assertEqual(second_bucket["finalized_blocks"], 2)
        self.assertEqual(second_bucket["finalized_known_bytes"], 300)
        self.assertEqual(second_bucket["finalized_missing_blocks"], 1)

    def test_state_interval_classification(self):
        result = self.run_fixture_analysis()
        intervals = result["state_intervals"]

        self.assertEqual(intervals.loc[0, "start_s"], 0)
        self.assertEqual(intervals.loc[0, "end_s"], 10)
        self.assertEqual(intervals.loc[0, "duration_s"], 10)
        self.assertTrue(intervals.loc[0, "hol_gap_active"])
        self.assertEqual(intervals.loc[0, "hol_gap_reorder_blocks"], 5)
        self.assertEqual(
            intervals["blocking_class"].tolist(),
            [
                "hol_gap",
                "commit_backpressure",
                "download_starved",
                "peer_unavailable",
                "header_limited",
            ],
        )


if __name__ == "__main__":
    unittest.main()
