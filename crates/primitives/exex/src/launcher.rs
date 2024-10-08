use std::future::Future;
use std::sync::Arc;

use futures::{
    future::{self, BoxFuture},
    FutureExt,
};
use mp_rpc::Starknet;

use crate::{context::ExExContext, ExExHandle, ExExManager, ExExManagerHandle};

const DEFAULT_EXEX_MANAGER_CAPACITY: usize = 16;

pub struct ExExLauncher {
    extensions: Vec<(String, Box<dyn BoxedLaunchExEx>)>,
    starknet: Arc<Starknet>,
}

impl ExExLauncher {
    /// Create a new `ExExLauncher` with the given extensions.
    pub const fn new(extensions: Vec<(String, Box<dyn BoxedLaunchExEx>)>, starknet: Arc<Starknet>) -> Self {
        Self { extensions, starknet }
    }

    /// Launches all execution extensions.
    ///
    /// Spawns all extensions and returns the handle to the exex manager if any extensions are
    /// installed.
    pub async fn launch(self) -> anyhow::Result<Option<ExExManagerHandle>> {
        let Self { extensions, starknet } = self;

        if extensions.is_empty() {
            // nothing to launch
            return Ok(None);
        }

        let mut exex_handles = Vec::with_capacity(extensions.len());
        let mut exexes = Vec::with_capacity(extensions.len());

        for (id, exex) in extensions {
            // create a new exex handle
            let (handle, events, notifications) = ExExHandle::new(id.clone());
            exex_handles.push(handle);

            // create the launch context for the exex
            let context = ExExContext { starknet: starknet.clone(), events, notifications };

            exexes.push(async move {
                // init the exex
                let exex = exex.launch(context).await.unwrap();
                tokio::spawn(async move {
                    match exex.await {
                        Ok(_) => panic!("ExEx {id} finished. ExExes should run indefinitely"),
                        Err(err) => panic!("ExEx {id} crashed: {err}"),
                    }
                });
            });
        }

        future::join_all(exexes).await;

        let exex_manager = ExExManager::new(exex_handles, DEFAULT_EXEX_MANAGER_CAPACITY);
        let handle = exex_manager.handle();
        tokio::spawn(async move {
            if let Err(e) = exex_manager.await {
                eprintln!("ExExManager error: {:?}", e);
            }
        });
        Ok(Some(handle))
    }
}

/// A trait for launching an `ExEx`.
pub trait LaunchExEx: Send {
    /// Launches the `ExEx`.
    ///
    /// The `ExEx` should be able to run independently and emit events on the channels provided in
    /// the [`ExExContext`].
    fn launch(
        self,
        ctx: ExExContext,
    ) -> impl Future<Output = anyhow::Result<impl Future<Output = anyhow::Result<()>> + Send>> + Send;
}

/// A boxed exex future.
pub type BoxExEx = BoxFuture<'static, anyhow::Result<()>>;

/// A version of [`LaunchExEx`] that returns a boxed future. Makes the trait object-safe.
pub trait BoxedLaunchExEx: Send + Sync {
    /// Launches the `ExEx` and returns a boxed future.
    fn launch(self: Box<Self>, ctx: ExExContext) -> BoxFuture<'static, anyhow::Result<BoxExEx>>;
}

/// Implements [`BoxedLaunchExEx`] for any [`LaunchExEx`] that is [Send] and `'static`.
///
/// Returns a [`BoxFuture`] that resolves to a [`BoxExEx`].
impl<E> BoxedLaunchExEx for E
where
    E: LaunchExEx + Send + Sync + 'static,
{
    fn launch(self: Box<Self>, ctx: ExExContext) -> BoxFuture<'static, anyhow::Result<BoxExEx>> {
        async move {
            let exex = LaunchExEx::launch(*self, ctx).await?;
            Ok(Box::pin(exex) as BoxExEx)
        }
        .boxed()
    }
}

/// Implements `LaunchExEx` for any closure that takes an [`ExExContext`] and returns a future
/// resolving to an `ExEx`.
impl<F, Fut, E> LaunchExEx for F
where
    F: FnOnce(ExExContext) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<E>> + Send,
    E: Future<Output = anyhow::Result<()>> + Send,
{
    fn launch(
        self,
        ctx: ExExContext,
    ) -> impl Future<Output = anyhow::Result<impl Future<Output = anyhow::Result<()>> + Send>> + Send {
        self(ctx)
    }
}
