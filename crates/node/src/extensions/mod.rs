mod pragma_dispatch;

use futures::future::BoxFuture;
use mp_exex::{BoxExEx, BoxedLaunchExEx, ExExContext};
use pragma_dispatch::exex_pragma_dispatch;

// Helper function to create a boxed ExEx
fn box_exex<F, Fut>(f: F) -> Box<dyn BoxedLaunchExEx>
where
    F: FnOnce(ExExContext) -> Fut + Send + Sync + 'static,
    Fut: futures::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    Box::new(move |ctx| {
        Box::pin(async move { Ok(Box::pin(f(ctx)) as BoxExEx) }) as BoxFuture<'static, anyhow::Result<BoxExEx>>
    })
}

/// List of all ExEx that will be ran along Madara.
pub fn madara_exexs() -> Vec<(String, Box<dyn BoxedLaunchExEx>)> {
    vec![("Pragma Dispatch ExEx".to_string(), box_exex(exex_pragma_dispatch))]
}
