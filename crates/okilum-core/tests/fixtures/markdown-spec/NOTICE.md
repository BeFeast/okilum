# Markdown spec test data (#650)

Test input only. These files are not compiled into or shipped with Okilum.

## commonmark-0.31.2.json

The examples of the CommonMark Spec, version 0.31.2 (2024-01-28), byte for byte
as published at <https://spec.commonmark.org/0.31.2/spec.json>
(SHA-256 `d431b29d97b6f73e69d547109cf5081578fac931e72afe95639ebe766c1b2a20`).

Copyright John MacFarlane. Licensed under the Creative Commons
Attribution-ShareAlike 4.0 International License (CC BY-SA 4.0),
<https://creativecommons.org/licenses/by-sa/4.0/>.

## gfm-0.29.json

The examples of the GitHub Flavored Markdown Spec, version 0.29-gfm
(2019-04-06), extracted from `test/spec.txt` of
<https://github.com/github/cmark-gfm> at commit
`27d942c8b0a62d192f616e5bf3578f4b6a89e180` (spec.txt SHA-256
`7d8e5814befec287ac116786d81ff14e0adc9b13295b4494649e995408fd871c`) with
`scripts/markdown-spec-json.py`. Changes from the source: the examples are
converted to JSON in the format of the CommonMark project's spec.json, `→` is
replaced by a tab as the spec prescribes, each example records the extensions
its fence names, and the prose of the spec is omitted.

Copyright GitHub, Inc. and John MacFarlane. Licensed under the Creative
Commons Attribution-ShareAlike 4.0 International License (CC BY-SA 4.0),
<https://creativecommons.org/licenses/by-sa/4.0/>. This JSON is an adaptation
of that spec and is distributed under the same license.

## known-failures.txt

Okilum's own list of the examples it fails and why. MIT, like the rest of
Okilum.
