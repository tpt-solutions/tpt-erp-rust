//! Strongly-typed identifiers.
//!
//! [`Id<T>`] is a UUID wrapped with a [`PhantomData`] marker for the entity type `T`.
//! Because `Id<User>` and `Id<Product>` are distinct types, the compiler rejects
//! passing one where the other is expected — no more `UserId`/`ProductId` mixups.
//!
//! [`IntId<T>`] is the same idea backed by an `i64` (e.g. for databases using
//! sequence/serial columns).

use core::fmt;
use core::hash::Hash;
use core::marker::PhantomData;
use core::str::FromStr;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

/// Marker trait implemented (often as a unit struct) for each entity kind.
///
/// ```ignore
/// struct User;
/// let id: Id<User> = Id::new();
/// ```
pub trait Entity: 'static + Send + Sync {}

/// A UUID-based strong identifier for entity `T`.
///
/// ```compile_fail
/// use tpt_erp_primitives::{Id, Product, User};
/// fn takes_user(_: Id<User>) {}
/// let p: Id<Product> = Id::new();
/// takes_user(p); // cross-entity mixup — does not compile
/// ```
pub struct Id<T: Entity> {
    uuid: Uuid,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Entity> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Entity> Copy for Id<T> {}

impl<T: Entity> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.uuid == other.uuid
    }
}

impl<T: Entity> Eq for Id<T> {}

impl<T: Entity> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Entity> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.uuid.cmp(&other.uuid)
    }
}

impl<T: Entity> Hash for Id<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
    }
}

impl<T: Entity> Id<T> {
    /// Generate a new random (v4) identifier.
    pub fn new() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }

    /// Wrap an existing UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self {
            uuid,
            _marker: PhantomData,
        }
    }

    /// Parse from a UUID string.
    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self::from_uuid(Uuid::parse_str(s)?))
    }

    /// The inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.uuid
    }

    /// The UUID as a hyphenated string.
    pub fn as_str(&self) -> String {
        self.uuid.hyphenated().to_string()
    }
}

impl<T: Entity> Default for Id<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Entity> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id<{}>({})", core::any::type_name::<T>(), self.uuid)
    }
}

impl<T: Entity> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.uuid.hyphenated().to_string())
    }
}

impl<T: Entity> FromStr for Id<T> {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl<T: Entity> Serialize for Id<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.uuid.hyphenated().to_string())
    }
}

impl<'de, T: Entity> Deserialize<'de> for Id<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor<T>(PhantomData<fn() -> T>);

        impl<'de, T: Entity> Visitor<'de> for IdVisitor<T> {
            type Value = Id<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a UUID string identifying an entity")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Id<T>, E> {
                Id::<T>::parse(v).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(IdVisitor(PhantomData))
    }
}

/// An `i64`-backed strong identifier for entity `T` (serial/sequence columns).
pub struct IntId<T: Entity> {
    value: i64,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Entity> Clone for IntId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Entity> Copy for IntId<T> {}

impl<T: Entity> PartialEq for IntId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Entity> Eq for IntId<T> {}

impl<T: Entity> PartialOrd for IntId<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Entity> Ord for IntId<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}

impl<T: Entity> Hash for IntId<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IntIdError {
    #[error("invalid integer id: {0}")]
    Invalid(std::num::ParseIntError),
}

impl<T: Entity> IntId<T> {
    pub fn new(value: i64) -> Self {
        Self {
            value,
            _marker: PhantomData,
        }
    }
    pub fn value(&self) -> i64 {
        self.value
    }
}

impl<T: Entity> fmt::Debug for IntId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IntId<{}>({})", core::any::type_name::<T>(), self.value)
    }
}

impl<T: Entity> fmt::Display for IntId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl<T: Entity> FromStr for IntId<T> {
    type Err = IntIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<i64>().map(Self::new).map_err(IntIdError::Invalid)
    }
}

impl<T: Entity> Serialize for IntId<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.value)
    }
}

impl<'de, T: Entity> Deserialize<'de> for IntId<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = i64::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct User;
    impl Entity for User {}
    #[derive(Debug)]
    struct Product;
    impl Entity for Product {}

    #[test]
    fn uuid_roundtrip() {
        let id: Id<User> = Id::new();
        let s = id.as_str();
        let back = Id::<User>::parse(&s).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn serde_as_string() {
        let id: Id<Product> = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: Id<Product> = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn int_id_parse_and_serde() {
        let id: IntId<User> = IntId::new(42);
        assert_eq!(id.value(), 42);
        assert_eq!(id.to_string(), "42");
        let parsed: IntId<User> = "7".parse().unwrap();
        assert_eq!(parsed.value(), 7);
        let json = serde_json::to_string(&parsed).unwrap();
        let back: IntId<User> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, parsed);
    }

    #[test]
    fn distinct_entity_types_are_incompatible() {
        // Compile-time guarantee; this just asserts they don't accidentally unify.
        fn needs_user(_: Id<User>) {}
        let u: Id<User> = Id::new();
        needs_user(u);
        // (A function expecting Id<Product> would refuse `u` — verified by compile-fail tests.)
    }
}
