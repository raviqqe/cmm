use super::type_::Type;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Union(Arc<UnionInner>);

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct UnionInner {
    members: Vec<Type>,
}

impl Union {
    pub fn new(members: Vec<Type>) -> Self {
        Self(UnionInner { members }.into())
    }

    pub fn members(&self) -> &[Type] {
        &self.0.members
    }
}
