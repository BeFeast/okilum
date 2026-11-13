use super::super::*;

fn changed(source: &str) -> String {
    match format("xml", source) {
        Formatted::Changed(text) => text,
        Formatted::Unchanged(reason) => panic!("XML must format: {reason}\n{source}"),
    }
}

fn unchanged(source: &str) -> String {
    match format("xml", source) {
        Formatted::Unchanged(reason) => reason,
        Formatted::Changed(text) => panic!("must not change:\n{source}\n→\n{text}"),
    }
}

const POM_LINE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><project xmlns=\"http://maven.apache.org/POM/4.0.0\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><modelVersion>4.0.0</modelVersion><groupId>com.example</groupId><dependencies><dependency><groupId>junit</groupId><artifactId>junit</artifactId><version>4.13.2</version><scope>test</scope></dependency></dependencies><!-- build --><build/></project>";

const POM_PRETTY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<project xmlns=\"http://maven.apache.org/POM/4.0.0\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <dependencies>
    <dependency>
      <groupId>junit</groupId>
      <artifactId>junit</artifactId>
      <version>4.13.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
  <!-- build -->
  <build/>
</project>";

/// Oleg, 13.11: a one-line file was never formatted. Before #2912 every XML
/// input was refused with «No safe built-in formatter for xml».
#[test]
fn xml2912_single_line_pom_is_indented_one_element_per_line() {
    assert_eq!(changed(POM_LINE), POM_PRETTY);
    // The final newline follows the source.
    assert_eq!(changed(&format!("{POM_LINE}\n")), format!("{POM_PRETTY}\n"));
    assert_eq!(
        validation_after(POM_PRETTY),
        Some(Ok(())),
        "the result is well-formed"
    );
}

fn validation_after(text: &str) -> Option<Result<(), Invalid>> {
    xml_validation_controlled(text, &Control::default())
}

#[test]
fn xml2912_formatting_is_idempotent() {
    for source in [
        POM_LINE.to_string(),
        format!("{POM_LINE}\r\n").replace("><", ">\r\n<"),
        "<a>\n\n\n<b/>\n\n<c>t</c><d><e/></d></a>\n".into(),
        "<r><p>Mixed <b>bold</b> text</p><pre xml:space=\"preserve\">  <x/>  </pre></r>".into(),
    ] {
        let first = match format("xml", &source) {
            Formatted::Changed(text) => text,
            Formatted::Unchanged(reason) => panic!("{reason}"),
        };
        assert_eq!(
            format("xml", &first),
            Formatted::Unchanged("Already formatted.".into()),
            "{first}"
        );
    }
}

/// An already formatted file (multi-line tags, a multi-line comment, blank
/// lines between sections, text leaves) round-trips byte for byte.
#[test]
fn xml2912_already_formatted_file_round_trips_bytes() {
    let source = "<?xml version=\"1.0\"?>
<!--
  Licensed under the Apache License.
-->
<project name=\"demo\"
         default=\"jar\">

  <property name=\"src\" value=\"src\"/>

  <target name=\"jar\" depends=\"compile\">
    <jar destfile=\"demo.jar\"   basedir='build'/>
    <echo>Built   ${src}</echo>
  </target>
</project>
";
    assert_eq!(unchanged(source), "Already formatted.");
    let crlf = source.replace('\n', "\r\n");
    assert_eq!(unchanged(&crlf), "Already formatted.");
    let bom = format!("\u{feff}{source}");
    assert_eq!(unchanged(&bom), "Already formatted.");
}

/// Comments, CDATA, processing instructions, DOCTYPE with an internal subset,
/// entity references, attribute order/quotes/spacing are copied as written.
#[test]
fn xml2912_markup_is_copied_byte_for_byte() {
    let doctype = "<!DOCTYPE note [\n  <!ENTITY co \"Example > Inc\">\n  <!-- a [bracket] -->\n]>";
    let comment = "<!--  keep   <b>this</b> -- spacing  -->";
    let pi = "<?xml-stylesheet type=\"text/xsl\" href=\"s.xsl\"?>";
    let tag = "<note  z='1'   a=\"2\"\n      m=\"&amp;&#x41;&co;\">";
    let cdata = "<![CDATA[ if (a < b && c) { } ]]>";
    let source = format!(
        "<?xml version='1.0' encoding='utf-8' standalone=\"yes\"?>{pi}{doctype}{tag}{comment}<to>&co; &lt;Tove&gt;</to><script>{cdata}</script><?app run?></note >"
    );
    let result = changed(&source);
    assert_eq!(
        result,
        format!(
            "<?xml version='1.0' encoding='utf-8' standalone=\"yes\"?>\n{pi}\n{doctype}\n{tag}\n  {comment}\n  <to>&co; &lt;Tove&gt;</to>\n  <script>{cdata}</script>\n  <?app run?>\n</note >"
        )
    );
    for part in [doctype, comment, pi, tag, cdata, "&co; &lt;Tove&gt;"] {
        assert!(result.contains(part), "{part}");
    }
    assert!(
        !result.contains("Example > Inc</to>"),
        "no entity expansion"
    );
}

/// Mixed content (XHTML, DocBook) and xml:space="preserve" keep every byte,
/// whitespace between their children included.
#[test]
fn xml2912_mixed_content_and_preserved_space_are_untouched() {
    let para = "<para>See <emphasis>this</emphasis>  and\n   <link href=\"x\">that</link>.</para>";
    let pre = "<pre xml:space=\"preserve\">\n    <code>  a  </code>\n<b/></pre>";
    let leaf = "<title>  Spaced   title  </title>";
    let source = format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{para}{pre}{leaf}<p></p><br/></body></html>"
    );
    assert_eq!(
        changed(&source),
        format!(
            "<html xmlns=\"http://www.w3.org/1999/xhtml\">\n  <body>\n    {para}\n    {pre}\n    {leaf}\n    <p></p>\n    <br/>\n  </body>\n</html>"
        )
    );
    // A mixed root is copied whole: nothing to re-indent.
    assert_eq!(unchanged(para), "Already formatted.");
    // xml:space="default" or another value does not freeze the subtree.
    assert_eq!(
        changed("<a xml:space=\"default\"><b/></a>"),
        "<a xml:space=\"default\">\n  <b/>\n</a>"
    );
}

/// Negative control: whitespace-only text and the layout of verbatim parts
/// are the only things Format may touch; tags and text never change.
#[test]
fn xml2912_only_whitespace_between_markup_changes() {
    let source = "<r>\n\t\t<a k=\"v\">text</a>   <b/><!--c--></r>";
    let result = changed(source);
    let strip = |s: &str| {
        s.split(|c: char| c.is_ascii_whitespace())
            .collect::<String>()
    };
    assert_eq!(strip(&result), strip(source));
    assert_eq!(
        result,
        "<r>\n  <a k=\"v\">text</a>\n  <b/>\n  <!--c-->\n</r>"
    );
    // Every other language still refuses; Repair has nothing for XML.
    assert!(matches!(
        format("html", "<p><b>x</b></p>"),
        Formatted::Unchanged(reason) if reason.contains("No safe built-in formatter")
    ));
    assert!(repair("xml", "<a>").unwrap_err().contains("xml"));
}

#[test]
fn xml2912_invalid_xml_is_unchanged_with_the_validate_position() {
    for (source, line, column) in [
        ("<a><b></a>", 1, 7),
        ("<a>\n  <b>\n</a>", 3, 1),
        ("<a/><b/>", 1, 5),
        ("<a x=\"1\" x=\"2\"/>", 1, 10),
        ("<a x=1/>", 1, 6),
        ("<a>&nope</a>", 1, 4),
        ("text<a/>", 1, 1),
        ("<a/>\ntrail", 2, 1),
        ("<a>", 1, 1),
        ("<a><!-- open</a>", 1, 4),
        ("", 1, 1),
        (" <?xml version=\"1.0\"?><a/>", 1, 2),
        ("<a>]]></a>", 1, 4),
        ("<a b=\"<\"/>", 1, 7),
        ("{ a: 1", 1, 1),
    ] {
        let reason = unchanged(source);
        assert_eq!(reason, xml_invalid_notice(line, column), "{source:?}");
        assert!(reason.starts_with("XML is invalid at "), "{reason}");
        let error = validation_after(source).unwrap().unwrap_err();
        assert_eq!((error.line, error.column), (line, column), "{source:?}");
    }
}

#[test]
fn xml2912_line_endings_bom_and_indent_unit_follow_the_source_and_editor() {
    let crlf = changed("\u{feff}<a><b>x</b></a>\r\n");
    assert_eq!(crlf, "\u{feff}<a>\r\n  <b>x</b>\r\n</a>\r\n");
    // A single-line body inherits the owning document's newline.
    assert_eq!(
        format_with_line_ending("xml", "<a><b/></a>", LineEnding::Crlf),
        Formatted::Changed("<a>\r\n  <b/>\r\n</a>".into())
    );
    assert_eq!(
        unchanged("<a>\r\n<b/>\n</a>"),
        "Mixed line endings; source unchanged."
    );
    assert_eq!(
        format_indented(
            "xml",
            "<a><b><c/></b></a>",
            LineEnding::Lf,
            "\t",
            &Control::default()
        ),
        Some(Formatted::Changed(
            "<a>\n\t<b>\n\t\t<c/>\n\t</b>\n</a>".into()
        ))
    );
}

#[test]
fn xml2912_blank_lines_before_children_survive_once() {
    assert_eq!(
        changed("\n\n<a>\n\n\n  <b/>\n\n\n\n  <c/>\n\n</a>\n\n"),
        "<a>\n\n  <b/>\n\n  <c/>\n</a>\n"
    );
    assert_eq!(
        changed("<?xml version=\"1.0\"?>\n\n<!-- c -->\n<a><b/></a>"),
        "<?xml version=\"1.0\"?>\n\n<!-- c -->\n<a>\n  <b/>\n</a>"
    );
}

#[test]
fn xml2912_large_input_formats_linearly_and_cancels_without_a_result() {
    let item = "<item id=\"1\" kind='a'><name>n &amp; m</name><value>42</value></item>";
    let source = format!("<items>{}</items>", item.repeat(150_000));
    assert!(source.len() > 8 * 1024 * 1024);
    let started = std::time::Instant::now();
    let result = changed(&source);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "linear format took {:?}",
        started.elapsed()
    );
    assert_eq!(
        result.matches("\n    <name>n &amp; m</name>").count(),
        150_000
    );
    assert_eq!(
        format("xml", &result),
        Formatted::Unchanged("Already formatted.".into())
    );
    // Cancelled before start, and mid-run after real scanning work.
    let control = Control::default();
    control.cancel();
    assert!(format_controlled("xml", &source, LineEnding::Lf, &control).is_none());
    assert!(xml_validation_controlled(&source, &control).is_none());
    let calls = std::cell::Cell::new(0);
    let stop = || {
        calls.set(calls.get() + 1);
        calls.get() > 300
    };
    assert!(super::format(&source, LineEnding::Lf, "  ", &stop).is_none());
    assert!(calls.get() > 300, "positive control: the scan polled");
}

#[test]
fn xml2912_output_and_input_limits_refuse_without_invalid_claims() {
    // 20 000 nested levels: a 140 KB input whose indentation would exceed
    // the 64 MiB output ceiling.
    let deep = format!("{}{}", "<a>".repeat(20_000), "</a>".repeat(20_000));
    let reason = unchanged(&deep);
    assert!(reason.contains("results up to 64 MiB"), "{reason}");
    assert!(!reason.to_lowercase().contains("invalid"));
    assert_eq!(validation_after(&deep), Some(Ok(())));
    let huge = format!("<a>{}</a>", " ".repeat(MAX_BYTES));
    let reason = unchanged(&huge);
    assert!(reason.contains("64 MiB"), "{reason}");
}

#[test]
fn xml2912_many_attributes_are_checked_for_duplicates_linearly() {
    let attributes: String = (0..50_000).map(|i| format!(" a{i}=\"{i}\"")).collect();
    let source = format!("<r{attributes}><c/></r>");
    let started = std::time::Instant::now();
    assert!(matches!(format("xml", &source), Formatted::Changed(_)));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let duplicate = format!("<r{attributes} a49999=\"x\"/>");
    let error = validation_after(&duplicate).unwrap().unwrap_err();
    assert!(error.message.contains("duplicate attribute a49999"));
}
