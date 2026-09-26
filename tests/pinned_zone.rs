use std::sync::Arc;

use context_collector::{
    InMemoryColdBacking, NoopInfoRefiner, NoopScopeSummarizer, PinnedError, Runtime, RuntimeConfig,
    RuntimeError, ScopeReport, TokenCounter, TurnObservation, Watermark,
};

struct Bytes;

impl TokenCounter for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

fn runtime(capacity: usize) -> Runtime {
    Runtime::with_counter(
        RuntimeConfig {
            watermarks: [Watermark { low: 1, high: 4 }; 5],
            pinned_capacity: capacity,
            hot_high: 100,
            processing_batch_tokens: 1024,
            max_processing_failures: 2,
        },
        Arc::new(Bytes),
        Arc::new(NoopInfoRefiner),
        Arc::new(NoopScopeSummarizer),
        Arc::new(InMemoryColdBacking::default()),
    )
    .unwrap()
}

#[tokio::test]
async fn pinned_text_is_verbatim_ordered_and_independent_of_scope_or_collection() {
    let runtime = runtime(200);
    let first = "# System\nKeep the heading.";
    let second = "<custom>Free format</custom>";
    let first_id = runtime.pin(first.into()).await.unwrap();
    let second_id = runtime.pin(second.into()).await.unwrap();

    let before = runtime.context_view(&[]).await.unwrap();
    assert_eq!(before.markdown(), format!("{first}\n\n{second}"));
    assert_eq!(before.usage.total, before.markdown().len());
    assert_eq!(before.usage.pinned.rendered_tokens, before.markdown().len());
    assert_eq!(
        before.usage.pinned.stored_tokens,
        first.len() + second.len()
    );
    assert_eq!(before.usage.pinned.capacity, 200);
    assert_eq!(runtime.pending_jobs().await, 0);

    runtime
        .complete_turn(
            "new question".into(),
            "answer".into(),
            TurnObservation {
                uses: None,
                scope: Some(ScopeReport::Transition),
            },
        )
        .await
        .unwrap();
    runtime.drain_maintenance().await;
    let after = runtime.context_view(&[]).await.unwrap();
    assert!(
        after
            .markdown()
            .starts_with(&format!("{first}\n\n{second}\n\n"))
    );
    assert_eq!(after.pinned, vec![first, second]);
    assert_eq!(after.usage.total, after.markdown().len());
    assert_eq!(runtime.view_usage().await.unwrap(), after.usage);
    assert_eq!(
        runtime
            .pinned_entries()
            .await
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![first_id, second_id]
    );
}

#[tokio::test]
async fn admission_failure_is_atomic_and_explicit_removal_releases_capacity() {
    let runtime = runtime(5);
    let first = runtime.pin("abc".into()).await.unwrap();
    assert!(matches!(
        runtime.pin("def".into()).await,
        Err(RuntimeError::Pinned(PinnedError::CapacityExceeded))
    ));
    assert!(matches!(
        runtime.pin(String::new()).await,
        Err(RuntimeError::Pinned(PinnedError::Empty))
    ));
    assert_eq!(runtime.pinned_entries().await.len(), 1);
    assert_eq!(runtime.context_view(&[]).await.unwrap().markdown(), "abc");

    runtime.unpin(first).await.unwrap();
    assert!(matches!(
        runtime.unpin(first).await,
        Err(RuntimeError::Pinned(PinnedError::Unknown(id))) if id == first
    ));
    let second = runtime.pin("defgh".into()).await.unwrap();
    assert_ne!(second, first);
    let view = runtime.context_view(&[]).await.unwrap();
    assert_eq!(view.markdown(), "defgh");
    assert_eq!(view.usage.pinned.stored_tokens, 5);
}
