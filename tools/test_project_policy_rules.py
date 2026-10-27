"""Documentation-only policy consistency checks. Never imported by server runtime."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
RULES = ROOT / "docs" / "POLICY_RULES.md"
FIELDS = ("目的", "適用範囲", "必須事項", "禁止事項", "検証方法", "例外条件")
EXPECTED_COUNTS = {
    "LANG": 2,
    "QUAL": 4,
    "GIT": 7,
    "AI": 6,
    "QA": 6,
    "DATA": 4,
    "SEC": 3,
    "COMP": 6,
}


class ProjectPolicyRulesTests(unittest.TestCase):
    """Protect rule IDs, structure and AI discoverability."""

    @classmethod
    def setUpClass(cls):
        cls.text = RULES.read_text(encoding="utf-8")

    def test_rule_ids_are_unique_and_have_all_six_fields(self):
        matches = list(re.finditer(
            r"^### ([A-Z]+-[0-9]+) — .+（P[01]）\s*$",
            self.text,
            flags=re.MULTILINE,
        ))
        ids = [match.group(1) for match in matches]
        self.assertEqual(len(ids), sum(EXPECTED_COUNTS.values()))
        self.assertEqual(len(set(ids)), len(ids), "Duplicate policy rule IDs")
        self.assertEqual(
            {prefix: sum(rule.startswith(prefix + "-") for rule in ids)
             for prefix in EXPECTED_COUNTS},
            EXPECTED_COUNTS,
        )

        for index, match in enumerate(matches):
            end = matches[index + 1].start() if index + 1 < len(matches) else len(self.text)
            block = self.text[match.end():end]
            for field in FIELDS:
                self.assertEqual(
                    len(re.findall(r"^- \*\*" + field + r"\*\*：", block, re.MULTILINE)),
                    1,
                    f"{match.group(1)} missing or repeated {field}",
                )

    def test_merge_risk_and_exception_process_are_present(self):
        for token in (
            "自動マージの危険度マトリクス",
            "L（低）",
            "M（中）",
            "H（高）",
            "明示承認",
            "例外・判断保留の申請様式",
        ):
            self.assertIn(token, self.text)

    def test_linked_from_agents_and_readme(self):
        self.assertTrue(RULES.is_file())
        for path in ("AGENTS.md", "README.md"):
            with self.subTest(path=path):
                self.assertIn("POLICY_RULES.md", (ROOT / path).read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
