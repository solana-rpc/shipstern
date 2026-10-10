//! A handle for talking to a [`Runtime`](crate::Runtime) after it has started.

use std::sync::{Arc, Mutex, PoisonError};

use shipstern_core::Filters;
use tokio::sync::watch;

/// Why a filter update never reached the source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterUpdateError {
    /// The runtime has stopped, or was dropped without being run, so nothing
    /// is left to apply the set.
    #[error("the runtime is no longer running")]
    Closed,
    /// The set names a parser no pipeline is registered for, so nothing is sent.
    /// Only the name is checked: a prefilter of the wrong kind still reaches the
    /// wire, and the runtime drops its updates.
    #[error("no pipeline registered for parser `{0}`")]
    UnknownParser(String),
}

/// Filter state shared by every handle onto one runtime.
///
/// The `watch` slot is both the transport to the source and the record of the
/// last set handed off, so the two can never disagree.
#[derive(Debug)]
pub(crate) struct FilterState {
    /// The set the pipelines were built with. Also the source of truth for
    /// which parser IDs an update may name.
    initial: Filters,
    /// Latest set for the source. A newer set replaces one the source has not
    /// read yet, which is what the servers do with successive requests anyway.
    filter_updates_tx: watch::Sender<Filters>,
    /// Serialises read-modify-write sequences across handles, so two
    /// concurrent edits cannot interleave and lose one another. Never held
    /// across an await.
    update_lock: Mutex<()>,
}

impl FilterState {
    pub(crate) fn new(filter_updates_tx: watch::Sender<Filters>) -> Self {
        let initial = filter_updates_tx.borrow().clone();

        Self {
            initial,
            filter_updates_tx,
            update_lock: Mutex::new(()),
        }
    }

    fn snapshot(&self) -> Filters { self.filter_updates_tx.borrow().clone() }

    /// Refuse a set naming a parser that has no registered pipeline.
    fn validate(&self, filters: &Filters) -> Result<(), FilterUpdateError> {
        let Some(unknown) = filters
            .parser_ids()
            .find(|id| self.initial.get(id).is_none())
        else {
            return Ok(());
        };

        Err(FilterUpdateError::UnknownParser(unknown.to_owned()))
    }
}

/// A handle onto a [`Runtime`](crate::Runtime) for changing its subscription
/// while it runs.
///
/// Take it with [`Runtime::handle`](crate::Runtime::handle) before running, since
/// the run methods consume the runtime. Only sources implementing
/// [`FilterUpdateSource`](crate::sources::FilterUpdateSource) have one. Handles are
/// cheap to clone, share one view of the filters, and never await.
///
/// ```rust, ignore
/// let runtime = Runtime::builder()
///     .account(Pipeline::new(TokenProgramAccParser, [Handler]))
///     .try_build::<YellowstoneGrpcSource>(config)?;
///
/// let handle = runtime.handle();
/// tokio::spawn(runtime.run_async());
///
/// // Widen the account subscription with one more owner.
/// let extra = Prefilter::builder().account_owners([new_mint]).build()?;
/// handle.update_filters(|filters| filters.merge(TokenProgramAccParser.id(), extra))?;
///
/// // Back to what the pipelines were built with.
/// handle.reset_filters()?;
/// ```
///
/// # Semantics
///
/// - Every update sends the complete set and replaces the live subscription.
/// - Keys are parser IDs with a registered pipeline. Instruction parsers share one
///   entry under [`InstructionPipeline::ID`](crate::instruction::InstructionPipeline::ID).
///   A key dropped by [`Filters::remove`] can be merged back.
/// - `Ok(())` means the source has the set, not that the server applied it.
///   Handlers see the old set until the queued backlog drains.
/// - Only the newest set is sent. A set rejected between connections is retried
///   once the stream recovers, or dropped with a warning if auto-reconnect is off.
/// - A refusal arrives on the stream. A terminal code ends the run; a
///   recoverable one, like `ResourceExhausted`, resubscribes with the same set
///   and can keep looping.
#[derive(Debug, Clone)]
pub struct RuntimeHandle {
    state: Arc<FilterState>,
}

impl RuntimeHandle {
    pub(crate) fn new(state: Arc<FilterState>) -> Self { Self { state } }

    /// The last filter set published for the source, seeded from the registered
    /// pipelines. It can run ahead of what the server serves, so judge a live
    /// subscription by what the handlers receive.
    ///
    /// ```rust, ignore
    /// let owners = handle
    ///     .filters()
    ///     .get(&TokenProgramAccParser.id())
    ///     .and_then(|prefilter| prefilter.account.as_ref())
    ///     .map(|account| account.owners.clone());
    /// ```
    ///
    #[must_use]
    pub fn filters(&self) -> Filters { self.state.snapshot() }

    /// Edit the live filter set in place and send the result.
    ///
    /// Concurrent calls apply one after another, so no edit is lost. `edit` runs
    /// under that lock, so calling `update_filters` from inside it deadlocks.
    ///
    /// ```rust, ignore
    /// handle.update_filters(|filters| {
    ///     filters.merge(TokenProgramAccParser.id(), extra_owner);
    ///     filters.remove(InstructionPipeline::ID);
    /// })?;
    /// ```
    ///
    /// # Errors
    ///
    /// [`FilterUpdateError::UnknownParser`] leaves the set untouched, and
    /// nothing is sent. See [`FilterUpdateError`] for the rest.
    ///
    pub fn update_filters<F>(&self, edit: F) -> Result<(), FilterUpdateError>
    where F: FnOnce(&mut Filters) {
        // The guarded section only copies and republishes a set, so a panic
        // partway through leaves no torn state behind and the next caller can
        // take the lock as if nothing happened.
        let _serialised = self
            .state
            .update_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let mut next = self.state.snapshot();
        edit(&mut next);
        self.state.validate(&next)?;

        // Every `watch` send becomes a fresh subscribe request, so skip an edit that
        // changed nothing. Reading first is safe because the update lock is held.
        if *self.state.filter_updates_tx.borrow() == next {
            return if self.state.filter_updates_tx.receiver_count() == 0 {
                Err(FilterUpdateError::Closed)
            } else {
                Ok(())
            };
        }

        // A failed send leaves the slot untouched, so `filters()` never
        // reports a set that went nowhere.
        self.state
            .filter_updates_tx
            .send(next)
            .map_err(|_| FilterUpdateError::Closed)
    }

    /// Replace the whole subscription with `filters`. For edits that start from the
    /// current set use [`Self::update_filters`]: reading [`Self::filters`] and
    /// sending it back can lose a concurrent edit.
    ///
    /// ```rust, ignore
    /// handle.send_filter_update(Filters::new(rebuilt))?;
    /// ```
    ///
    /// # Errors
    ///
    /// See [`FilterUpdateError`].
    ///
    pub fn send_filter_update(&self, filters: Filters) -> Result<(), FilterUpdateError> {
        self.update_filters(|current| *current = filters)
    }

    /// Restore the set the pipelines were built with.
    ///
    /// ```rust, ignore
    /// handle.reset_filters()?;
    /// ```
    ///
    /// # Errors
    ///
    /// See [`FilterUpdateError`].
    ///
    pub fn reset_filters(&self) -> Result<(), FilterUpdateError> {
        self.send_filter_update(self.state.initial.clone())
    }
}
