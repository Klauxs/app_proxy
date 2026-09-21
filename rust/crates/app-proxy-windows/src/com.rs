//! COM apartment lifetime for the synchronous worker threads that use COM.
use crate::{Error, Result};
use std::marker::PhantomData;
use windows::Win32::System::Com::{COINIT, CoInitializeEx, CoUninitialize};

/// A successful `CoInitializeEx` on the current thread, balanced on drop.
///
/// The guard cannot leave its thread. Interfaces created while it is alive must
/// be dropped before it: keep it as the last field of a struct, or declare it
/// before the interfaces in a function body.
pub(crate) struct Apartment(PhantomData<*const ()>);

impl Apartment {
    /// `operation` labels the failure; COM descriptions are never forwarded.
    pub(crate) fn enter(model: COINIT, operation: &'static str) -> Result<Self> {
        // SAFETY: no pointers are passed. Success, including S_FALSE for a
        // thread that is already initialized, must be balanced exactly once on
        // this thread, which the returned guard does.
        let code = unsafe { CoInitializeEx(None, model) };
        if code.is_err() {
            return Err(Error::Windows {
                operation,
                code: code.0 as u32,
            });
        }
        Ok(Self(PhantomData))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful initialization made by `enter` on
        // this same thread; the guard is neither Send nor Sync.
        unsafe { CoUninitialize() }
    }
}
