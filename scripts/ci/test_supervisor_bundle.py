"""Behaviour of scripts/ci/supervisor-bundle.sh that does not need a Mac: the Team ID
derivation, run the way the release runs it (set -Eeuo pipefail, sourced), with a stand-in
for `security`. The packaging itself is exercised on a real Mac by check-supervisor-bundle.sh."""
import pathlib
import re
import subprocess
import unittest

SCRIPT = pathlib.Path(__file__).with_name("supervisor-bundle.sh")
KEYCHAIN = (
    '  1) 1111111111111111111111111111111111111111 "Developer ID Application: Example Person (ABCDE12345)"\n'
    '  2) 2222222222222222222222222222222222222222 "Apple Development: Someone Else (ZZZZZ99999)"\n'
    "     2 valid identities found\n"
)


def team_id(identity, configured=None, listing=KEYCHAIN, how="direct", keychain=None):
    """Run supervisor_team_id; `how` is a direct call or inside a command substitution."""
    export = f"export OKILUM_SIGNING_TEAM_ID={configured}\n" if configured else ""
    call = (
        'supervisor_team_id "$IDENTITY"'
        if how == "direct"
        else 'value="$(supervisor_team_id "$IDENTITY")"; printf "%s\\n" "$value"'
    )
    program = f"""set -Eeuo pipefail
security() {{
    if [[ -n $EXPECTED_KEYCHAIN ]]; then
        [[ $# == 5 && $5 == "$EXPECTED_KEYCHAIN" ]] || return 91
    else
        [[ $# == 4 ]] || return 92
    fi
    printf '%s' "$LISTING"
}}
source {SCRIPT}
{export}{call}
echo reached-the-end
"""
    environment = {"PATH": "/usr/bin:/bin", "IDENTITY": identity, "LISTING": listing,
                   "EXPECTED_KEYCHAIN": keychain or ""}
    if keychain is not None:
        environment["OKILUM_SIGNING_KEYCHAIN"] = keychain
    done = subprocess.run(
        ["/bin/bash", "-c", program],
        env=environment,
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

    def test_unset_and_explicit_keychain_preserve_identity_arguments(self):
        for keychain in (None, "/tmp/explicit keychain.keychain-db"):
            for how in ("direct", "substitution"):
                code, out, err = team_id("1111111111111111111111111111111111111111",
                                         keychain=keychain, how=how)
                self.assertEqual((code, out.splitlines()[0] if out else ""),
                                 (0, "ABCDE12345"), (keychain, how, err))

    def test_supervisor_sign_omits_empty_keychain_and_preserves_explicit_path(self):
        for keychain in (None, "/tmp/explicit keychain.keychain-db"):
            environment = {"PATH": "/usr/bin:/bin"}
            if keychain is not None:
                environment["OKILUM_SIGNING_KEYCHAIN"] = keychain
            script = ('set -Eeuo pipefail\n'
                      'codesign() { printf "arg:%s\\n" "$@"; }\n'
                      f'source "{SCRIPT}"\n'
                      'supervisor_sign "/tmp/Test App.app" "fake identity" com.example.app --timestamp\n')
            result = subprocess.run(["/bin/bash", "-c", script], env=environment,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = result.stdout.splitlines()
            self.assertNotIn("arg:", args)
            self.assertEqual("arg:--keychain" in args, keychain is not None)
            if keychain is not None:
                self.assertIn("arg:" + keychain, args)
            self.assertIn("arg:--timestamp", args)
            self.assertIn("arg:/tmp/Test App.app/Contents/MacOS/okilum-sync-supervisor", args)

    def test_every_added_optional_array_expansion_handles_empty_under_system_bash(self):
        repo = SCRIPT.parents[2]
        found = 0
        for name in ["scripts/ci/supervisor-bundle.sh", "scripts/updater/sign-bundle.sh",
                     "scripts/updater/make-dmg.sh"]:
            text = (repo / name).read_text()
            # Run each actual guarded expansion with nounset, not a rewritten approximation.
            expressions = re.findall(r'\$\{(keychain_args|[A-Z_]+KEYCHAIN_ARGS)\[@\]\+"\$\{\1\[@\]\}"\}', text)
            self.assertTrue(expressions, name)
            for variable in set(expressions):
                expression = '${' + variable + '[@]+"${' + variable + '[@]}"}'
                for values, expected in [('()', ['0']), ('(--keychain "/tmp/a b.keychain-db")',
                                                          ['2', '--keychain', '/tmp/a b.keychain-db'])]:
                    program = f'set -Eeuo pipefail; {variable}={values}; set -- {expression}; printf "%s\\n" "$#" "$@"'
                    result = subprocess.run(["/bin/bash", "-c", program], capture_output=True, text=True)
                    self.assertEqual((result.returncode, result.stdout.splitlines()), (0, expected),
                                     (name, variable, values, result.stderr))
                found += 1
        self.assertGreaterEqual(found, 6)

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
