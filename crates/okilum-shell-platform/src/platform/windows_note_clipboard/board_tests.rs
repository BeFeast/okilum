use super::*;
use std::sync::{Arc, Mutex};

fn board(types: &[&str], values: Vec<(&str, Vec<u8>)>) -> (Board, Arc<Mutex<Vec<String>>>) {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = reads.clone();
    let values: std::collections::BTreeMap<String, Vec<u8>> = values
        .into_iter()
        .map(|(kind, data)| (kind.into(), data))
        .collect();
    let mut board = Board::from(Snapshot {
        native: Some(NativeOffer {
            sequence: 42,
            types: types.iter().map(|kind| (*kind).into()).collect(),
        }),
        ..Default::default()
    });
    board.read = Some(Arc::new(move |kind| {
        observed.lock().unwrap().push(kind.to_owned());
        Ok(values.get(kind).cloned())
    }));
    (board, reads)
}

#[test]
fn metadata_is_lazy_and_selected_payload_is_cached() {
    let (board, reads) = board(
        &[offer::UTF8_TEXT, "public.png"],
        vec![
            (offer::UTF8_TEXT, b"text".to_vec()),
            ("public.png", vec![1, 2, 3]),
        ],
    );
    assert_eq!(board.types(), [offer::UTF8_TEXT, "public.png"]);
    assert!(
        reads.lock().unwrap().is_empty(),
        "metadata must not read payloads"
    );
    assert_eq!(board.data(offer::UTF8_TEXT).unwrap(), b"text");
    assert_eq!(board.string(offer::UTF8_TEXT).as_deref(), Some("text"));
    assert_eq!(
        *reads.lock().unwrap(),
        [offer::UTF8_TEXT],
        "positive read control and no image read"
    );
}

#[test]
fn changed_offer_cannot_look_consistent_or_expose_cached_payload() {
    let (mut board, _) = board(&[offer::UTF8_TEXT], vec![]);
    board.read = Some(Arc::new(|_| Err(Error::ChangedOffer)));
    let before = board.change_count();
    assert_ne!(before, board.change_count());
    assert_eq!(board.data(offer::UTF8_TEXT), None);
    assert!(matches!(*board.error.borrow(), Some(Error::ChangedOffer)));
}

#[test]
fn file_urls_keep_order_and_metadata_frame_round_trips() {
    let urls = vec!["file:///C:/notes/one.png", "file:///C:/notes/two.pdf"];
    let (board, reads) = board(
        &[offer::FILE_URL],
        vec![(offer::FILE_URL, serde_json::to_vec(&urls).unwrap())],
    );
    assert_eq!(board.file_urls(), urls);
    assert_eq!(board.file_urls(), urls);
    assert_eq!(*reads.lock().unwrap(), [offer::FILE_URL]);
    let snapshot = Snapshot {
        native: board.native,
        ..Default::default()
    };
    let decoded = Snapshot::decode(&snapshot.encode())
        .unwrap()
        .native
        .unwrap();
    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.types, [offer::FILE_URL]);
    assert!(Snapshot::decode(r#"N{"sequence":0,"types":[]}"#).is_err());
}

#[test]
fn cfhtml_fragment_and_file_source_url_share_one_lazy_read() {
    let content = "<p>שלום <img src='image.png'></p>";
    let header = |start, end| {
        format!(
            "Version:1.0\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\nSourceURL:file:///C:/Word/msohtmlclip/\r\n"
        )
    };
    let start = header(0, 0).len();
    let raw = format!("{}{content}", header(start, start + content.len()));
    let (board, reads) = board(
        &[offer::HTML[0], offer::CHROMIUM_SOURCE_URL],
        vec![(offer::HTML[0], raw.into_bytes())],
    );
    assert_eq!(board.html_string().as_deref(), Some(content));
    assert_eq!(
        board.source_url().as_deref(),
        Some("file:///C:/Word/msohtmlclip/")
    );
    assert_eq!(*reads.lock().unwrap(), [offer::HTML[0]]);
}

#[test]
fn malformed_cfhtml_does_not_poison_the_plain_fallback() {
    for raw in [b"not a CF_HTML header".to_vec(), vec![0xff]] {
        let (board, _) = board(
            &[offer::UTF8_TEXT, offer::HTML[0]],
            vec![
                (offer::UTF8_TEXT, b"fallback".to_vec()),
                (offer::HTML[0], raw),
            ],
        );
        assert_eq!(board.html_string(), None);
        assert_eq!(board.string(offer::UTF8_TEXT).as_deref(), Some("fallback"));
        assert!(board.error.borrow().is_none());
    }
}
