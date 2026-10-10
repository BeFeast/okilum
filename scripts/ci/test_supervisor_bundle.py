"""Behaviour of scripts/ci/supervisor-bundle.sh that does not need a Mac: the Team ID
derivation, run the way the release runs it (set -Eeuo pipefail, sourced), with a stand-in
for `security`. The packaging itself is exercised on a real Mac by check-supervisor-bundle.sh."""
import pathlib
import subprocess
import unittest

SCRIPT = pathlib.Path(__file__).with_name("supervisor-bundle.sh")
KEYCHAIN = (
    '  1) 1111111111111111111111111111111111111111 "Developer ID Application: Example Person (ABCDE12345)"\n'
    '  2) 2222222222222222222222222222222222222222 "Apple Development: Someone Else (ZZZZZ99999)"\n'
    "     2 valid identities found\n"
)


def team_id(identity, configured=None, listing=KEYCHAIN, how="direct"):
    """Run supervisor_team_id; `how` is a direct call or inside a command substitution."""
    export = f"export OKILUM_SIGNING_TEAM_ID={configured}\n" if configured else ""
    call = (
        'supervisor_team_id "$IDENTITY"'
        if how == "direct"
        else 'value="$(supervisor_team_id "$IDENTITY")"; printf "%s\\n" "$value"'
    )
    program = f"""set -Eeuo pipefail
security() {{ printf '%s' "$LISTING"; }}
source {SCRIPT}
{export}{call}
echo reached-the-end
"""
    done = subprocess.run(
        ["bash", "-c", program],
        env={"PATH": "/usr/bin:/bin", "IDENTITY": identity, "LISTING": listing},
        capture_output=True,
        text=True,
    )
    return done.returncode, done.stdout.strip(), done.stderr.strip()


class TeamId(unittest.TestCase):
    def test_derived_from_the_identity_by_name_or_by_hash(self):
        for identity in (
            "Developer ID Application: Example Person (ABCDE12345)",
            "1111111111111111111111111111111111111111",
        ):
            for how in ("direct", "substitution"):
                code, out, err = team_id(identity, how=how)
                self.assertEqual((code, out.splitlines()[0]), (0, "ABCDE12345"), (identity, how, err))

    def test_a_configured_team_must_agree_with_the_identity(self):
        code, out, _ = team_id("Developer ID Application: Example Person (ABCDE12345)", configured="ABCDE12345")
        self.assertEqual(code, 0)
        self.assertTrue(out.startswith("ABCDE12345"))
        code, out, err = team_id("Developer ID Application: Example Person (ABCDE12345)", configured="QQQQQ11111")
        self.assertNotEqual(code, 0)
        self.assertIn("differs", err)
        self.assertNotIn("reached-the-end", out)

    def test_an_identity_missing_from_the_keychain_falls_back_to_the_configured_team(self):
        # The case the pipeline's `grep` fails on: under pipefail it must not abort the
        # function before the fallback, whether called directly or in a substitution.
        for how in ("direct", "substitution"):
            code, out, err = team_id("no such identity", configured="ABCDE12345", how=how)
            self.assertEqual((code, out.splitlines()[0]), (0, "ABCDE12345"), (how, err))

    def test_with_no_source_it_refuses_and_says_why(self):
        for how in ("direct", "substitution"):
            code, out, err = team_id("no such identity", how=how)
            self.assertNotEqual(code, 0, how)
            self.assertIn("cannot determine the Team ID", err)
            self.assertNotIn("reached-the-end", out)

    def test_malformed_values_are_never_returned(self):
        for bad in ("abcde12345", "ABCDE1234", "ABCDE123456", "ABCDE 1234", "ABCDE1234;"):
            code, out, err = team_id("no such identity", configured=bad)
            self.assertNotEqual(code, 0, bad)
            self.assertNotIn("reached-the-end", out, bad)
        # A listing line whose parentheses do not hold a Team ID yields nothing either.
        listing = '  1) 3333333333333333333333333333333333333333 "Developer ID Application: Odd (not-a-team)"\n'
        code, _, err = team_id("Developer ID Application: Odd (not-a-team)", listing=listing)
        self.assertNotEqual(code, 0)
        self.assertIn("cannot determine the Team ID", err)


if __name__ == "__main__":
    unittest.main()
