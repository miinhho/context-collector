pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

#[derive(Default)]
pub struct TiktokenCounter;

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        tiktoken_rs::o200k_base_singleton()
            .encode_ordinary(text)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::{TiktokenCounter, TokenCounter};

    #[test]
    fn counts_o200k_tokens_instead_of_bytes() {
        let counter = TiktokenCounter;
        assert_eq!(counter.count("hello world"), 2);
        assert!(counter.count("안녕하세요") < "안녕하세요".len());
    }
}
