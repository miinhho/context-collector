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
