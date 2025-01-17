use cap_sdk_core::transaction::IndefiniteEvent;
use std::cell::RefCell;

use crate::{env::CapEnv, InsertTransactionError, TransactionId};

thread_local! {
    pub(crate) static PENDING: RefCell<Vec<IndefiniteEvent>> = RefCell::new(vec![]);
}

/// Inserts a transaction into the contract's history.
///
/// # Examples
///
/// ### Inserting an event from a builder.
///
/// See also: [`IndefiniteEventBuilder`][crate::IndefiniteEventBuilder], [`IntoDetails`][crate::IntoDetails], [`TryFromDetails`][crate::TryFromDetails].
/// ```rust
/// use cap_sdk_core::transaction::EventStatus;
//# use ic_kit::Principal;
//# use crate::{insert, IndefiniteEventBuilder, IntoDetails, TryFromDetails};
/// pub struct TransactionDetails {
///     foo: String,
///     bar: u64,
/// }
//# impl TryFromDetails for TransactionDetails {
//# fn try_from_details(
//#    details: &Vec<(String, cap_sdk_core::transaction::DetailValue)>,
//# ) -> Result<Self, ()> {
//#    Ok(Self {
//#        foo: String::from(
//#            "Peek behind the curtain and you might regret what you find...",
//#        ),
//#        bar: 42,
//#    })
//# }
//# }
//# impl IntoDetails for TransactionDetails {
//# fn into_details(self) -> Vec<(String, cap_sdk_core::transaction::DetailValue)> {
//#     vec![]
//# }
//# }
///
/// let transaction_details = TransactionDetails {
///     foo: String::from("foo"),
///     bar: 42
/// };
///
/// // Construct the event which accompanies our details.
/// // `IndefiniteEventBuilder` allows any `IntoDetails` type
/// // to be used in a call to `IndefiniteEventBuilder::details`.
/// //
/// // This is useful when dealing with the `cap-standards` types,
/// // but also works with a dynamically-constructed details vec with
/// // the type signature of `Vec<(String, DetailValue)>`.
/// let event = IndefiniteEventBuilder::new()
///     .caller(Principal::anonymous())
///     .operation(String::from("transfer"))
///     .status(EventStatus::Completed)
///     .details(transaction_details)
///     .build()
///     .unwrap();
///
///
/// // Insert the transaction with `insert`. It takes any type
/// // that implements `Into<IndefiniteEvent>`, this includes
/// // types like `TypedIndefiniteEvent` as well as `Vec<(String, DetailValue)>`
/// insert(event).await.unwrap();
/// ```
pub async fn insert(
    transaction: impl Into<IndefiniteEvent>,
) -> Result<TransactionId, InsertTransactionError> {
    insert_many(vec![transaction].into_iter()).await
}

/// Insert many transactions using one write to Cap.
pub async fn insert_many<T: Into<IndefiniteEvent>>(
    events: impl Iterator<Item = T>,
) -> Result<TransactionId, InsertTransactionError> {
    let events = events.map(|x| x.into()).collect::<Vec<_>>();

    let (events, offset) = PENDING.with(|p| {
        let mut r = p.borrow_mut();
        if r.is_empty() {
            (Some(events), 0)
        } else {
            r.extend(events.into_iter());
            (None, r.len() as u64)
        }
    });

    if offset > 0 {
        match flush_to_cap().await {
            Ok(id) => Ok(id + offset - 1),
            Err(e) => {
                PENDING.with(|p| {
                    p.borrow_mut().remove(offset as usize - 1);
                });
                Err(e)
            }
        }
    } else {
        CapEnv::get()
            .await
            .root
            .insert_many(&events.unwrap())
            .await
            .map_err(|(code, details)| match details.as_str() {
                "The method can only be invoked by one of the writers." => {
                    InsertTransactionError::CantWrite
                }
                _ => InsertTransactionError::Unexpected(code, details),
            })
    }
}

/// Insert a transaction into Cap without needing an await, this method guarantees finality of
/// the transactions and can handle insertion errors that might happen on the root bucket. (e.g
/// if the root bucket has gone out of cycles)
///
/// It works by having a local pending transactions buffer, in the heap storage, which it uses
/// to track failed transactions.
///
/// If you're using this method, be sure you store/restore the pending transactions during
/// upgrades. You can use [pending_transactions] and [restore_pending_transactions].
pub fn insert_sync(event: impl Into<IndefiniteEvent>) {
    PENDING.with(|p| {
        p.borrow_mut().push(event.into());
    });

    ic_cdk::block_on(async {
        let _ = flush_to_cap().await;
    });
}

/// Like [insert_sync], but allows you to insert more than one transaction at a time.
pub fn insert_many_sync<T: Into<IndefiniteEvent>>(events: impl Iterator<Item = T>) {
    PENDING.with(|p| {
        p.borrow_mut().extend(events.map(|e| e.into()));
    });

    ic_cdk::block_on(async {
        let _ = flush_to_cap().await;
    });
}

/// Return the array of pending transactions. Can be used in a pre-upgrade hook.
pub fn pending_transactions() -> Vec<IndefiniteEvent> {
    PENDING.with(|p| p.borrow().iter().cloned().collect::<Vec<_>>())
}

/// Restore the transactions, it keeps the previous pending transactions as well.
pub fn restore_pending_transactions(mut events: Vec<IndefiniteEvent>) {
    PENDING.with(|p| {
        events.extend(p.take());
        p.replace(events);
    });
}

/// Force a flush of pending transactions to Cap.
pub async fn flush_to_cap() -> Result<TransactionId, InsertTransactionError> {
    let context = CapEnv::get().await;
    let mut events = PENDING.with(|p| p.take());

    let id = context
        .root
        .insert_many(&events)
        .await
        .map_err(|(code, details)| match details.as_str() {
            "The method can only be invoked by one of the writers." => {
                InsertTransactionError::CantWrite
            }
            _ => InsertTransactionError::Unexpected(code, details),
        })
        .map_err(|e| {
            // TODO(qti3e) Is ordering preserved this way?
            // need to be double checked.
            PENDING.with(|p| {
                events.extend(p.take());
                p.replace(events);
            });

            e
        })?;

    Ok(id)
}
