use context_collector::{TiktokenCounter, TokenCounter};

#[test]
fn counts_o200k_tokens_instead_of_bytes() {
    let counter = TiktokenCounter;
    assert_eq!(counter.count("hello world"), 2);
    assert!(counter.count("안녕하세요") < "안녕하세요".len());
}
