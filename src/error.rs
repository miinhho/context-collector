use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Error supplied by an application-owned task or backing implementation.
/// Its concrete type remains available through the error source chain.
pub type ExternalError = Arc<dyn Error + Send + Sync + 'static>;

/// Object-safe asynchronous result for application-owned work.
pub type TaskFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExternalError>> + Send + 'a>>;
