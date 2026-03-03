//! Topic synthesis: addressable, living context documents maintained by the cortex.

pub mod store;

pub use store::{Topic, TopicCriteria, TopicStatus, TopicStore, TopicVersion};
