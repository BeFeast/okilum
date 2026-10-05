//! Standalone same-host raw parser cost probe; not a native latency gate.
use comrak::{parse_document, Arena, Options};
use std::time::Instant;
fn main() {
    for (name, pattern) in [
        ("plain", "plain text "),
        ("emphasis", "***nested*** ~~strike~~ "),
        ("brackets", "[[[[[[[[[[[[[[[[[[[[[[[["),
        ("backticks", "` `` ``` ```` ````` "),
        ("destinations", "[x](a(b(c(d(e(f(g(h)))))))) "),
        ("tables", "|a|b|\n|-|-|\n|**x**|y|\n\n"),
    ] {
        let source = pattern.repeat(65536 / pattern.len());
        let mut samples = Vec::new();
        let mut nodes = 0;
        for _ in 0..5 {
            let arena = Arena::new();
            let mut options = Options::default();
            options.extension.strikethrough = true;
            options.extension.table = true;
            options.extension.wikilinks_title_after_pipe = true;
            options.extension.front_matter_delimiter = Some("---".into());
            let start = Instant::now();
            let root = parse_document(&arena, &source, &options);
            samples.push(start.elapsed().as_micros());
            nodes = root.descendants().count();
        }
        println!(
            "name={name} bytes={} nodes={nodes} parse_us={samples:?}",
            source.len()
        );
    }
}
