use std::{error::Error, fmt};

/// Handle a fatal error by logging it and exiting the process.
#[inline]
pub fn handle_fatal<T, E: Error>(res: Result<T, E>) -> T {
    handle_fatal_msg(res, "Fatal error encountered")
}

/// Handle a fatal error by logging it and exiting the process.
pub fn handle_fatal_msg<T, E: Error>(res: Result<T, E>, msg: &str) -> T {
    match res {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(err = %Chain(&e), "{msg}");
            std::process::exit(1);
        },
    }
}

/// A helper type for preventing sensitive strings from being printed.
#[derive(
    Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize,
)]
#[repr(transparent)]
pub struct PrivateString(pub String);

impl std::fmt::Debug for PrivateString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("<private>") }
}

impl std::ops::Deref for PrivateString {
    type Target = String;

    #[inline]
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl std::ops::DerefMut for PrivateString {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl From<String> for PrivateString {
    #[inline]
    fn from(value: String) -> Self { Self(value) }
}

impl From<PrivateString> for String {
    #[inline]
    fn from(PrivateString(value): PrivateString) -> Self { value }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Chain<'a, E>(pub &'a E);

impl<E: Error> fmt::Display for Chain<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;

        enum IndentState {
            NumberStart(usize), // Numbered and indented initial line
            PlainStart,         // Plain indented initial line
            HangStart,          // Hanging indent for successive lines
            MidLine,            // No indent, not at line start
        }

        struct Indented<'a, F> {
            f: &'a mut F,
            state: IndentState,
        }

        impl<F: Write> Indented<'_, F> {
            fn write_pad(&mut self) -> fmt::Result {
                match std::mem::replace(&mut self.state, IndentState::MidLine) {
                    IndentState::NumberStart(i) => write!(self.f, "{i: >5}: "),
                    IndentState::PlainStart => write!(self.f, "    "),
                    IndentState::HangStart => write!(self.f, "      "),
                    IndentState::MidLine => Ok(()),
                }
            }
        }

        impl<F: Write> Write for Indented<'_, F> {
            fn write_str(&mut self, mut s: &str) -> fmt::Result {
                while let Some((head, tail)) = s.split_once('\n') {
                    if !head.is_empty() {
                        self.write_pad()?;
                    }
                    self.f.write_str(head)?;
                    self.f.write_char('\n')?;
                    self.state = IndentState::HangStart;
                    s = tail;
                }

                let trail = !s.is_empty();
                if trail {
                    self.write_pad()?;
                }
                self.f.write_str(s)?;
                self.state = if trail {
                    IndentState::MidLine
                } else {
                    IndentState::HangStart
                };
                Ok(())
            }
        }

        let Self(err) = *self;

        write!(f, "{err}")?;

        if let src @ Some(_) = err.source() {
            let mut multi_src = false;

            for (i, src) in std::iter::successors(src, |e| (*e).source()).enumerate() {
                if i == 0 {
                    write!(f, "\nCaused by:")?;
                    multi_src = src.source().is_some();
                }

                writeln!(f)?;
                write!(
                    Indented {
                        f,
                        state: if multi_src {
                            IndentState::NumberStart(i)
                        } else {
                            IndentState::PlainStart
                        },
                    },
                    "{src}"
                )?;
            }
        }

        Ok(())
    }
}

/// Aborts a task when dropped, so cancelling a future also cancels the tasks it spawned.
#[derive(Debug)]
pub(crate) struct AbortOnDrop(pub(crate) tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

pub mod stop {
    //! A oneshot channel for requesting a task to stop.

    use std::pin::Pin;

    use tokio::sync::oneshot;

    /// A signal that a task has received a request to stop.
    #[derive(Debug, Default)]
    #[must_use = "Consider calling .as_unit()"]
    #[allow(missing_copy_implementations)]
    #[repr(transparent)]
    pub struct StopCode(());

    impl StopCode {
        /// Convert the stop code into a unit value.
        #[inline]
        pub fn as_unit(self) { let Self(()) = self; }
    }

    /// A sender for requesting a task to stop.
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct StopTx(oneshot::Sender<StopCode>);

    impl StopTx {
        /// Send a stop signal, or run the provided closure if the receiver has
        /// already been dropped.
        #[inline]
        pub fn send_or_else<F: FnOnce()>(self, f: F) {
            self.0.send(StopCode(())).unwrap_or_else(|StopCode(())| f());
        }

        /// Send a stop signal, or do nothing if the receiver has already been
        /// dropped.
        #[inline]
        pub fn maybe_send(self) { self.send_or_else(|| ()); }
    }

    /// A receiver for a stop signal.
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct StopRx(oneshot::Receiver<StopCode>);

    impl StopRx {
        /// Convert the receiver into a future that resolves when the task
        /// should stop.
        pub fn as_unit(self) -> impl std::future::Future<Output = ()> + Send + Sync + 'static {
            use futures_util::FutureExt;
            self.0.map(|r| r.map_or((), |StopCode(())| ()))
        }
    }

    impl std::future::Future for StopRx {
        type Output = StopCode;

        fn poll(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            Pin::new(&mut self.get_mut().0)
                .poll(cx)
                .map(|r| r.unwrap_or(StopCode(())))
        }
    }

    /// Create a new stop transmitter and receiver pair.
    #[must_use]
    pub fn channel() -> (StopTx, StopRx) {
        let (tx, rx) = oneshot::channel();
        (StopTx(tx), StopRx(rx))
    }
}
