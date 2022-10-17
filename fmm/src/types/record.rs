use super::type_::Type;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Record(Arc<RecordInner>);

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RecordInner {
    fields: Vec<Type>,
    hash: u64,
}

impl Record {
    pub fn new(fields: Vec<Type>) -> Self {
        let mut hasher = DefaultHasher::new();

        fields.hash(&mut hasher);

        Self(
            RecordInner {
                fields,
                hash: hasher.finish(),
            }
            .into(),
        )
    }

    pub fn fields(&self) -> &[Type] {
        &self.0.fields
    }
}

impl Hash for Record {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.0.hash.hash(hasher);
    }
}
