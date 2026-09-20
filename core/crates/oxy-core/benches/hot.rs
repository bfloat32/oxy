//! The per-keystroke path, benched: parse → rows → merge → sort.
//! Run with `cargo bench -p oxy-core`; keep inputs realistic — a launcher's
//! cost shows up in the shapes scripts actually emit.

use std::collections::HashSet;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

use oxy_core::model::query::Query;
use oxy_core::model::row::{Row, to_row, to_row_owned};
use oxy_core::registry::Extension;
use oxy_core::support::rank;

fn known() -> HashSet<String> {
    [
        "file", "format", "in", "music", "year", "calc", "web", "emoji", "ssh",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn ext() -> Extension {
    Extension {
        id: "emoji".into(),
        title: "Emoji".into(),
        subtitle: "Emoji".into(),
        glyph: "😀".into(),
        tier: 6,
        view: "emoji".into(),
        max_rows: 60,
        ..Default::default()
    }
}

/// A row the way `emoji:` emits it: a dozen fields plus a passthrough
/// `remember` object and three actions.
fn emoji_raw(i: usize) -> Value {
    json!({
        "id": format!("emoji-{i}"),
        "title": format!("face with tears of joy {i}"),
        "glyph": "😂",
        "subtitle": "Emoji",
        "copyText": "😂",
        "exec": "printf %s '😂' | wl-copy",
        "score": 90000 - i as i64 * 100,
        "remember": {"file": "emoji-recent", "value": "😂", "keep": 24},
        "actions": [
            {"title": "Copy Emoji", "shortcut": "↵", "exec": "printf %s '😂' | wl-copy"},
            {"title": "Type It", "exec": "sleep 0.2; wtype 😂"},
            {"title": "Copy Name", "exec": "printf %s 'face' | wl-copy"},
        ]
    })
}

fn bench_parse(c: &mut Criterion) {
    let k = known();
    c.bench_function("query_parse/scoped", |b| {
        b.iter(|| {
            Query::parse(
                black_box("emoji:crying laughing face"),
                1,
                Some(black_box(&k)),
            )
        })
    });
    c.bench_function("query_parse/filters", |b| {
        b.iter(|| {
            Query::parse(
                black_box("file:report format:pdf in:~/Sync budget review"),
                1,
                Some(black_box(&k)),
            )
        })
    });
}

fn bench_to_row(c: &mut Criterion) {
    let e = ext();
    let raw = emoji_raw(0);
    c.bench_function("to_row/borrowed", |b| {
        b.iter(|| to_row(black_box(&e), black_box(&raw), 0))
    });
    // The fresh-run path consumes the raw map — one less deep copy per row.
    c.bench_function("to_row/owned", |b| {
        b.iter_batched(
            || emoji_raw(0),
            |raw| to_row_owned(black_box(&e), raw, 0),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_merge(c: &mut Criterion) {
    let e = ext();
    // Six buckets of ten rows: a mid-flight merged page.
    let buckets: Vec<(&str, Vec<Arc<Row>>)> = ["a", "b", "c", "d", "e", "f"]
        .iter()
        .map(|id| {
            let rows: Vec<Arc<Row>> = (0..10)
                .map(|i| Arc::new(to_row(&e, &emoji_raw(i), i).unwrap()))
                .collect();
            (*id, rows)
        })
        .collect();
    let refs: Vec<(&str, &[Arc<Row>])> = buckets
        .iter()
        .map(|(id, rows)| (*id, rows.as_slice()))
        .collect();
    c.bench_function("merge60", |b| {
        b.iter(|| rank::merge(black_box(&refs), black_box(""), 60))
    });
}

/// The extras map a worker builds per routed ask.
fn bench_extras(c: &mut Criterion) {
    let k = known();
    let q = Query::parse("file:report format:pdf in:~/Sync", 1, Some(&k));
    c.bench_function("extras", |b| {
        b.iter(|| black_box(&q).extras(black_box("file"), black_box(&[])))
    });
}

criterion_group!(hot, bench_parse, bench_to_row, bench_merge, bench_extras);
criterion_main!(hot);
