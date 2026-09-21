//! Named points in the engine where a test can interleave another actor.
//!
//! The engine always carries a [`Checkpoints`] value and calls it without any
//! conditional compilation. Outside tests the type is empty and every call is
//! a no-op, so the production struct and code paths are the ones under test.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Point {
    BeforeGuardResolution,
    AfterGuardScan,
    AfterGuardTargetRead,
    BeforeDispatch,
    AfterSpawn,
    BeforeGuardStop,
    BeforeGuardReceipt,
    AfterGuardStop,
}

/// Cheap to clone into blocking workers.
#[derive(Clone, Default)]
pub(crate) struct Checkpoints {
    #[cfg(test)]
    inner: std::sync::Arc<std::sync::Mutex<Installed>>,
}

#[cfg(test)]
#[derive(Default)]
struct Installed {
    hooks: std::collections::HashMap<Point, std::sync::Arc<dyn Fn() + Send + Sync>>,
    gates: std::collections::HashMap<Point, std::sync::Arc<tokio::sync::Notify>>,
}

impl Checkpoints {
    /// Runs the hook installed for `point`, if any.
    #[inline]
    pub(crate) fn reach(&self, _point: Point) {
        #[cfg(test)]
        {
            let hook = self.inner.lock().unwrap().hooks.get(&_point).cloned();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    /// Waits until the gate installed for `point`, if any, is released.
    #[inline]
    pub(crate) async fn pause(&self, _point: Point) {
        #[cfg(test)]
        {
            let gate = self.inner.lock().unwrap().gates.get(&_point).cloned();
            if let Some(gate) = gate {
                gate.notified().await;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn on(&self, point: Point, hook: impl Fn() + Send + Sync + 'static) {
        self.on_shared(point, std::sync::Arc::new(hook));
    }

    #[cfg(test)]
    pub(crate) fn on_shared(&self, point: Point, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.inner.lock().unwrap().hooks.insert(point, hook);
    }

    #[cfg(test)]
    pub(crate) fn clear(&self, point: Point) {
        let mut installed = self.inner.lock().unwrap();
        installed.hooks.remove(&point);
        installed.gates.remove(&point);
    }

    #[cfg(test)]
    pub(crate) fn hold(&self, point: Point, gate: std::sync::Arc<tokio::sync::Notify>) {
        self.inner.lock().unwrap().gates.insert(point, gate);
    }
}
