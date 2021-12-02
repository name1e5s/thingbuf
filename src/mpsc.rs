//! Multi-producer, single-consumer channels using [`ThingBuf`](crate::ThingBuf).
//!
//! The default MPSC channel returned by the [`channel`] function is
//! _asynchronous_: receiving from the channel is an `async fn`, and the
//! receiving task willwait when there are no messages in the channel.
//!
//! If the "std" feature flag is enabled, this module also provides a
//! synchronous channel, in the [`sync`] module. The synchronous  receiver will
//! instead wait for new messages by blocking the current thread. Naturally,
//! this requires the Rust standard library. A synchronous channel
//! can be constructed using the [`sync::channel`] function.

use crate::{
    loom::atomic::AtomicUsize,
    util::wait::{Notify, WaitCell},
    Ref, ThingBuf,
};
use core::fmt;

#[cfg(feature = "alloc")]
use crate::util::wait::WaitQueue;

#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError<T = ()> {
    Full(T),
    Closed(T),
}

#[derive(Debug)]
pub struct Closed<T = ()>(T);

#[derive(Debug)]
struct Inner<T, N: Notify> {
    thingbuf: ThingBuf<T>,
    rx_wait: WaitCell<N>,
    tx_count: AtomicUsize,
    #[cfg(feature = "alloc")]
    tx_wait: WaitQueue<N>,
}

struct SendRefInner<'a, T, N: Notify> {
    inner: &'a Inner<T, N>,
    slot: Ref<'a, T>,
}

// ==== impl TrySendError ===

impl TrySendError {
    fn with_value<T>(self, value: T) -> TrySendError<T> {
        match self {
            Self::Full(()) => TrySendError::Full(value),
            Self::Closed(()) => TrySendError::Closed(value),
        }
    }
}

// ==== impl Inner ====
impl<T, N: Notify> Inner<T, N> {
    #[cfg(not(test))]
    const fn new(thingbuf: ThingBuf<T>) -> Self {
        Self {
            thingbuf,
            rx_wait: WaitCell::new(),
            tx_count: AtomicUsize::new(1),
            #[cfg(feature = "alloc")]
            tx_wait: WaitQueue::new(),
        }
    }

    #[cfg(test)]
    fn new(thingbuf: ThingBuf<T>) -> Self {
        Self {
            thingbuf,
            rx_wait: WaitCell::new(),
            tx_count: AtomicUsize::new(1),
            #[cfg(feature = "alloc")]
            tx_wait: WaitQueue::new(),
        }
    }
}

impl<T: Default, N: Notify> Inner<T, N> {
    fn try_send_ref(&self) -> Result<SendRefInner<'_, T, N>, TrySendError> {
        self.thingbuf
            .push_ref()
            .map(|slot| SendRefInner { inner: self, slot })
            .map_err(|_| {
                if self.rx_wait.is_rx_closed() {
                    TrySendError::Closed(())
                } else {
                    self.rx_wait.notify();
                    TrySendError::Full(())
                }
            })
    }

    fn try_send(&self, val: T) -> Result<(), TrySendError<T>> {
        match self.try_send_ref() {
            Ok(mut slot) => {
                slot.with_mut(|slot| *slot = val);
                Ok(())
            }
            Err(e) => Err(e.with_value(val)),
        }
    }
}

impl<T, N: Notify> SendRefInner<'_, T, N> {
    #[inline]
    pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
        self.slot.with(f)
    }

    #[inline]
    pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
        self.slot.with_mut(f)
    }
}

impl<T, N: Notify> Drop for SendRefInner<'_, T, N> {
    #[inline]
    fn drop(&mut self) {
        test_println!("drop SendRef<T, {}>", std::any::type_name::<N>());
        self.inner.rx_wait.notify();
    }
}

impl<T: fmt::Debug, N: Notify> fmt::Debug for SendRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Debug::fmt(val, f))
    }
}

impl<T: fmt::Display, N: Notify> fmt::Display for SendRefInner<'_, T, N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.with(|val| fmt::Display::fmt(val, f))
    }
}

impl<T: fmt::Write, N: Notify> fmt::Write for SendRefInner<'_, T, N> {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.with_mut(|val| val.write_str(s))
    }

    #[inline]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.with_mut(|val| val.write_char(c))
    }

    #[inline]
    fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
        self.with_mut(|val| val.write_fmt(f))
    }
}

macro_rules! impl_send_ref {
    (pub struct $name:ident<$notify:ty>;) => {
        pub struct $name<'sender, T>(SendRefInner<'sender, T, $notify>);

        impl<T> $name<'_, T> {
            #[inline]
            pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
                self.0.with(f)
            }

            #[inline]
            pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
                self.0.with_mut(f)
            }
        }

        impl<T: fmt::Debug> fmt::Debug for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: fmt::Display> fmt::Display for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: fmt::Write> fmt::Write for $name<'_, T> {
            #[inline]
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0.write_str(s)
            }

            #[inline]
            fn write_char(&mut self, c: char) -> fmt::Result {
                self.0.write_char(c)
            }

            #[inline]
            fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
                self.0.write_fmt(f)
            }
        }
    };
}

macro_rules! impl_recv_ref {
    (pub struct $name:ident<$notify:ty>;) => {
        pub struct $name<'recv, T> {
            slot: Ref<'recv, T>,
            inner: &'recv Inner<T, $notify>,
        }

        impl<T> $name<'_, T> {
            #[inline]
            pub fn with<U>(&self, f: impl FnOnce(&T) -> U) -> U {
                self.slot.with(f)
            }

            #[inline]
            pub fn with_mut<U>(&mut self, f: impl FnOnce(&mut T) -> U) -> U {
                self.slot.with_mut(f)
            }
        }

        impl<T: fmt::Debug> fmt::Debug for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.slot.fmt(f)
            }
        }

        impl<T: fmt::Display> fmt::Display for $name<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.slot.fmt(f)
            }
        }

        impl<T: fmt::Write> fmt::Write for $name<'_, T> {
            #[inline]
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.slot.write_str(s)
            }

            #[inline]
            fn write_char(&mut self, c: char) -> fmt::Result {
                self.slot.write_char(c)
            }

            #[inline]
            fn write_fmt(&mut self, f: fmt::Arguments<'_>) -> fmt::Result {
                self.slot.write_fmt(f)
            }
        }

        impl<T> Drop for RecvRef<'_, T> {
            fn drop(&mut self) {
                test_println!("drop RecvRef<T, {}>", stringify!($notify));
                if let Some(lock) = self.inner.tx_wait.lock() {
                    lock.notify();
                }
            }
        }
    };
}

mod async_impl;
pub use self::async_impl::*;

feature! {
    #![feature = "std"]
    pub mod sync;
}

#[cfg(test)]
mod tests;
