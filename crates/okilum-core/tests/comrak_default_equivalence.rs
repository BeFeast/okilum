mod cases {
    include!("fixtures/comrak_api_cases.rs");
}

#[test]
fn default_parse_ast_and_all_outputs_match_pinned_upstream_receipt() {
    let mut results = Vec::new();
    for source in cases::cases() {
        let options = cases::options(&source);
        let arena = comrak::Arena::new();
        let root = comrak::parse_document(&arena, &source, &options);
        results.push((source, cases::output(root, &options)));
    }
    let receipt = serde_json::to_vec_pretty(&results).unwrap();
    if let Ok(path) = std::env::var("OKILUM_COMRAK_UPSTREAM_RECEIPT") {
        assert_eq!(
            receipt,
            std::fs::read(path).unwrap(),
            "default upstream equivalence"
        );
    }
    if let Ok(path) = std::env::var("OKILUM_COMRAK_OUTPUT_RECEIPT") {
        std::fs::write(path, receipt).unwrap();
    }
}
