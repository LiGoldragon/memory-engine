//! A sequence that cannot be empty, owned by this crate.
//!
//! The engine's commit log is atomic by construction: an entry carries
//! one or more operations, never zero. [`NonEmpty`] is how that is said
//! in the type — the empty case is unrepresentable, so no caller and no
//! replay path needs an emptiness check.
//!
//! The type was previously taken from `signal-frame`. It is reproduced
//! here field-for-field — `head` then `tail`, with the same rkyv derives
//! — because [`crate::CommitLogEntry`] and
//! [`crate::VersionedCommitLogEntry`] are archived to durable storage
//! through it, and the archived layout must not move. Only what this
//! crate uses is carried over; nothing was added.

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use thiserror::Error;

/// A sequence guaranteed non-empty at the type level.
///
/// The empty case is unrepresentable: every value has a `head`. Multi-element
/// sequences extend through `tail`. Construction from an external sequence
/// fails closed with [`NonEmptyError::Empty`].
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NonEmpty<Value> {
    head: Value,
    tail: Vec<Value>,
}

impl<Value> NonEmpty<Value> {
    /// The one-element sequence.
    pub fn single(head: Value) -> Self {
        Self {
            head,
            tail: Vec::new(),
        }
    }

    /// The sequence built from its first element and the rest.
    pub fn from_head_and_tail(head: Value, tail: Vec<Value>) -> Self {
        Self { head, tail }
    }

    /// The sequence an ordinary vector carries, or [`NonEmptyError::Empty`]
    /// when that vector holds nothing. Order is preserved.
    pub fn try_from_vec(mut values: Vec<Value>) -> Result<Self, NonEmptyError> {
        if values.is_empty() {
            return Err(NonEmptyError::Empty);
        }
        let head = values.remove(0);
        Ok(Self { head, tail: values })
    }

    /// The first element, which always exists.
    pub fn head(&self) -> &Value {
        &self.head
    }

    /// Every element after the first, possibly none.
    pub fn tail(&self) -> &[Value] {
        &self.tail
    }

    /// The element count, never zero.
    pub fn len(&self) -> usize {
        1 + self.tail.len()
    }

    /// Always `false`. Present so that a reader of `len` finds the
    /// companion it expects, and so the guarantee is stated in code.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Borrowing iteration in order: head first, then the tail.
    pub fn iter(&self) -> NonEmptyIterator<'_, Value> {
        NonEmptyIterator {
            head: Some(&self.head),
            tail: self.tail.iter(),
        }
    }

    /// The elements as an ordinary vector, in order.
    pub fn into_vec(self) -> Vec<Value> {
        let mut values = Vec::with_capacity(1 + self.tail.len());
        values.push(self.head);
        values.extend(self.tail);
        values
    }
}

impl<Value> IntoIterator for NonEmpty<Value> {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'value, Value> IntoIterator for &'value NonEmpty<Value> {
    type Item = &'value Value;
    type IntoIter = NonEmptyIterator<'value, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The borrowing iterator [`NonEmpty::iter`] hands out.
pub struct NonEmptyIterator<'value, Value> {
    head: Option<&'value Value>,
    tail: std::slice::Iter<'value, Value>,
}

impl<'value, Value> Iterator for NonEmptyIterator<'value, Value> {
    type Item = &'value Value;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(head) = self.head.take() {
            return Some(head);
        }
        self.tail.next()
    }
}

/// The one way constructing a [`NonEmpty`] can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NonEmptyError {
    #[error("cannot construct a NonEmpty from an empty sequence")]
    Empty,
}
